// The recording view: runs `mc --json capture --stdin-commands` and shows its NDJSON events.
import { jobWrite, newJobId, noteImageSave, runJob, spawnMc } from "./api";
import { byId, clock, el, show } from "./dom";
import type { LiveMemory } from "./livememory";

export interface CaptureResult {
  sessionDir: string | null;
  code: number | null;
  error: string | null;
}

const MAX_CAPTIONS = 60;
const MAX_ASR = 12;
const MAX_EVENTS = 200;
const NEAR_BOTTOM_PX = 24;

type Event = Record<string, unknown> & { event: string };

function addEvent(text: string, warn = false): void {
  const list = byId("events");
  list.prepend(el("li", { className: warn ? "warn" : "", textContent: `${new Date().toLocaleTimeString()}  ${text}` }));
  while (list.children.length > MAX_EVENTS) list.lastElementChild?.remove();
}

/** Applies `change` to a list and keeps it at the bottom unless the reader scrolled up. */
function follow(id: string, change: (list: HTMLElement) => void): void {
  const list = byId(id);
  const atBottom = list.scrollHeight - list.scrollTop - list.clientHeight < NEAR_BOTTOM_PX;
  change(list);
  if (atBottom) list.scrollTop = list.scrollHeight;
  show(byId(`${id}-end`), !atBottom);
}

function wireFollow(id: string): void {
  const list = byId(id);
  const jump = byId(`${id}-end`);
  show(jump, false);
  jump.onclick = () => {
    list.scrollTop = list.scrollHeight;
    show(jump, false);
  };
  list.onscroll = () => {
    if (list.scrollHeight - list.scrollTop - list.clientHeight < NEAR_BOTTOM_PX) show(jump, false);
  };
}

function line(speaker: string, text: string, className = ""): HTMLLIElement {
  return el("li", { className }, el("span", { className: "speaker", textContent: speaker }), text);
}

// Captions name the speaker and are the source of truth: confirmed lines first, the ones Teams
// may still correct after them in grey; Whisper lines live apart so they never push captions out.
function addCaption(speaker: string, text: string): void {
  follow("captions", (list) => {
    const firstOpen = list.querySelector("li.open");
    list.insertBefore(line(speaker, text), firstOpen);
    const confirmed = list.querySelectorAll("li:not(.open)");
    for (let i = 0; i < confirmed.length - MAX_CAPTIONS; i++) confirmed[i].remove();
  });
}

/** Open lines come as `[Speaker]: text`, one per line still open. */
function showOpen(lines: unknown, people: LiveMemory): void {
  follow("captions", (list) => {
    for (const old of list.querySelectorAll("li.open")) old.remove();
    for (const raw of Array.isArray(lines) ? lines : []) {
      const match = /^\[(.*?)\]:\s*(.*)$/.exec(String(raw));
      const [speaker, text] = match ? [match[1], match[2]] : ["", String(raw)];
      if (speaker) people.see(speaker);
      list.append(line(speaker, text, "open"));
    }
  });
}

function addAsr(speaker: string, text: string): void {
  follow("asr-lines", (list) => {
    list.append(line(speaker, text, "asr"));
    while (list.children.length > MAX_ASR) list.firstElementChild?.remove();
  });
}

function setState(text: string): void {
  byId("capture-state").textContent = text;
}

function counts(value: unknown): string {
  if (!value || typeof value !== "object") return "";
  return Object.entries(value as Record<string, unknown>)
    .map(([k, v]) => `${k} ${v}`)
    .join(", ");
}

/** Shows one event; returns the session folder when the event announces it. */
function handle(e: Event, people: LiveMemory): string | null {
  switch (e.event) {
    case "started":
      setState("Esperando la reunión…");
      break;
    case "session":
      addEvent(`Grabando en ${e.session_dir}`);
      return String(e.session_dir);
    case "source":
      if (e.state === "attached") setState("Conectado a la reunión.");
      else if (e.state === "searching") setState("Buscando la ventana de la reunión…");
      else setState("La ventana de la reunión se cerró.");
      break;
    case "segment":
      people.see(String(e.speaker ?? ""));
      addCaption(String(e.speaker ?? ""), String(e.text ?? ""));
      break;
    case "asr":
      addAsr(e.ch === "mic" ? "Tú ~" : "Reunión ~", String(e.text ?? ""));
      break;
    case "no_captions":
      addEvent(`Sin subtítulos desde hace ${e.secs} s: ¿están activados?`, true);
      break;
    case "rec":
      addEvent(e.recording ? "Audio: grabando" : "Audio: detenido");
      break;
    case "ledger":
      addEvent(`Analista: ${counts(e.counts)}`);
      break;
    case "note":
      addEvent(`Nota: ${e.text}`);
      break;
    case "mark":
      addEvent(`Marca en ${clock(Number(e.mono_ms ?? 0))}`);
      break;
    case "fixed":
      addEvent(`Corrección: «${e.heard}» es «${e.correct}»`);
      break;
    case "named":
      addEvent(`${e.from} es ${e.to}`);
      break;
    case "unclean_session":
      addEvent(`Una sesión anterior no se cerró bien: ${e.session_dir}`, true);
      break;
    case "closing":
      setState("Grabación cerrada. El analista termina la última parte (hasta 30 s)…");
      break;
    case "ended":
      setState("Guardando la sesión…");
      break;
    case "open":
      showOpen(e.lines, people);
      break;
    default:
      if (typeof e.message === "string") addEvent(e.message, true);
  }
  return null;
}

function parse(text: string): Event | null {
  try {
    const value = JSON.parse(text);
    return value && typeof value.event === "string" ? value : null;
  } catch {
    return null;
  }
}

function errorOf(text: string): string {
  try {
    const err = JSON.parse(text).error;
    if (err) return err.hint ? `${err.message} (${err.hint})` : err.message;
  } catch {
    // Plain text.
  }
  return text;
}

/** An image pasted or dropped on the note box, waiting for «Nota». */
class PendingImage {
  private file: File | null = null;

  constructor() {
    const box = byId<HTMLInputElement>("note-text");
    box.onpaste = (event) => this.take(event.clipboardData?.files);
    box.ondragover = (event) => event.preventDefault();
    box.ondrop = (event) => {
      event.preventDefault();
      this.take(event.dataTransfer?.files);
    };
    byId("note-image-clear").onclick = () => this.clear();
    this.clear();
  }

  private take(files: FileList | undefined | null): void {
    const image = Array.from(files ?? []).find((f) => f.type.startsWith("image/"));
    if (!image) return;
    this.file = image;
    byId<HTMLImageElement>("note-image-preview").src = URL.createObjectURL(image);
    show(byId("note-image"), true);
  }

  clear(): void {
    this.file = null;
    show(byId("note-image"), false);
  }

  /** Saves the image in the session folder; its path relative to the session, or null. */
  async save(sessionDir: string | null): Promise<string | null> {
    if (!this.file) return null;
    if (!sessionDir) throw new Error("la sesión aún no empieza: la imagen se guarda cuando haya reunión");
    const ext = this.file.type.split("/")[1]?.replace("jpeg", "jpg") || "png";
    const bytes = Array.from(new Uint8Array(await this.file.arrayBuffer()));
    const saved = await noteImageSave(sessionDir, ext, bytes);
    this.clear();
    return saved;
  }
}

function wireButtons(id: string, sessionDir: () => string | null, people: LiveMemory): void {
  const note = byId<HTMLInputElement>("note-text");
  const image = new PendingImage();
  const send = (text: string) => jobWrite(id, text).catch((err) => addEvent(String(err), true));
  people.attach(send);
  byId("note").onclick = async () => {
    const text = note.value.replace(/\s+/g, " ").trim();
    let saved: string | null = null;
    try {
      saved = await image.save(sessionDir());
    } catch (err) {
      addEvent(String(err instanceof Error ? err.message : err), true);
      return;
    }
    if (!text && !saved) return note.focus();
    send(`n ${[text, saved ? `[imagen: ${saved}]` : ""].filter(Boolean).join(" ")}`);
    note.value = "";
  };
  note.onkeydown = (event) => {
    if (event.key === "Enter") byId("note").click();
  };
  byId("mark").onclick = () => send("m");
  byId<HTMLButtonElement>("stop").onclick = () => {
    setState("Cerrando la grabación y el audio…");
    for (const button of ["note", "mark", "stop"]) byId<HTMLButtonElement>(button).disabled = true;
    send("q");
  };
}

export async function runCapture(args: string[], title: string, people: LiveMemory): Promise<CaptureResult> {
  byId("capture-title").textContent = title;
  for (const list of ["captions", "asr-lines"]) {
    byId(list).replaceChildren();
    wireFollow(list);
  }
  byId("events").replaceChildren();
  for (const button of ["note", "mark", "stop"]) byId<HTMLButtonElement>(button).disabled = false;
  setState("Arrancando…");
  const started = Date.now();
  const tick = setInterval(() => (byId("elapsed").textContent = clock(Date.now() - started)), 500);
  const id = newJobId("capture");

  let sessionDir: string | null = null;
  let error: string | null = null;
  wireButtons(id, () => sessionDir, people);
  try {
    const code = await runJob(
      id,
      () => spawnMc(id, ["--json", "capture", ...args], true),
      (text, stream) => {
        const event = stream === "stdout" ? parse(text) : null;
        if (event) {
          sessionDir = handle(event, people) ?? sessionDir;
          return;
        }
        error = errorOf(text);
        addEvent(error, true);
      },
    );
    return { sessionDir, code, error };
  } catch (err) {
    return { sessionDir, code: null, error: String(err) };
  } finally {
    clearInterval(tick);
  }
}
