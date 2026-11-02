// The memory while the meeting runs: who each speaker is and their role, a term Teams keeps
// getting wrong, and a search. Corrections go through the capture, so this meeting's analyst
// uses them at once, and into the memory, so the next meetings do too.
import { byId, el } from "./dom";
import { change, listEntries, resolve, roleOf, showEntry, whoIs } from "./memorydata";

type Send = (line: string) => void;

type Found =
  | { state: "known"; id: number; role: string | null }
  | { state: "unknown" | "ambiguous" | "failed" };

function failed(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export class LiveMemory {
  private seen = new Set<string>();
  /** The people already known in this meeting: they tell a shared name apart. */
  private present: number[] = [];
  private send: Send = () => {};
  private openMemory: (query: string) => void = () => {};

  /** `me` is the user's name as the setup wrote it. */
  constructor(private readonly me: string | null) {}

  /** Fills the card; `openMemory` shows the Memoria tab with a search. */
  mount(openMemory: (query: string) => void): void {
    this.openMemory = openMemory;
    byId("live-people").replaceChildren();
    this.seen.clear();
    this.present = [];
    const heard = el("input", { placeholder: "Teams escribe (p. ej. gora dex)" });
    const correct = el("input", { placeholder: "Debería decir (p. ej. Kóradex)" });
    const fix = el("button", { type: "button", textContent: "Corregir" });
    const state = el("small", { className: "muted" });
    fix.onclick = () => {
      const [h, c] = [heard.value.trim(), correct.value.trim()];
      if (!h || !c) return (h ? correct : heard).focus();
      this.send(`t ${h} => ${c}`);
      state.textContent = `«${h}» → «${c}»: lo usan el analista y las próximas reuniones.`;
      heard.value = correct.value = "";
    };
    const query = el("input", { placeholder: "Buscar en la memoria" });
    const found = el("ul", { className: "plain small" });
    let asked = 0;
    query.oninput = async () => {
      const ask = ++asked;
      const text = query.value.trim();
      if (!text) return found.replaceChildren();
      const entries = await listEntries({ query: text }).catch(() => []);
      if (ask !== asked) return;
      found.replaceChildren(
        ...entries.slice(0, 8).map((e) => {
          const role = roleOf(e);
          return el("li", {}, el("strong", { textContent: e.name }), role ? ` (${role})` : "", e.meaning ? ` — ${e.meaning}` : "");
        }),
      );
    };
    const more = el("button", { className: "link", type: "button", textContent: "abrir en Memoria" });
    more.onclick = () => openMemory(query.value);
    byId("live-named").replaceChildren(this.namedForm());
    byId("live-memory").replaceChildren(
      el("div", { className: "row vocabulary-form" }, heard, correct, fix),
      state,
      el("div", { className: "row" }, query, more),
      found,
    );
  }

  /** Someone who is not here but is named, often by a nickname: «Pau» is Paula, the PO. */
  private namedForm(): HTMLElement {
    const nick = el("input", { placeholder: "Cómo la nombran (p. ej. Pau)" });
    const name = el("input", { placeholder: "Quién es (p. ej. Paula Vera)" });
    const role = el("input", { placeholder: "Rol (opcional)" });
    const save = el("button", { type: "button", textContent: "Guardar" });
    const state = el("small", { className: "muted" });
    save.onclick = async () => {
      const [n, who, r] = [nick.value.trim(), name.value.trim(), role.value.trim()];
      if (!n || !who) return (n ? name : nick).focus();
      try {
        const id = await resolve("person", who);
        if (id === null) {
          const args = ["add", "person", who, "--forms", n];
          if (r) args.push("--role", r);
          await change(args);
        } else {
          await change(["form", "add", String(id), n]);
          if (r) await change(["edit", String(id), "--role", r]);
        }
        state.textContent = `«${n}» es ${who}: lo sabrán la minuta y las próximas reuniones.`;
        nick.value = name.value = role.value = "";
      } catch (err) {
        state.textContent = failed(err);
      }
    };
    return el("div", {}, el("div", { className: "row vocabulary-form" }, nick, name, role, save), state);
  }

  /** Where corrections go: the running capture's commands. */
  attach(send: Send): void {
    this.send = send;
  }

  /** A speaker of this meeting gets a row the first time; the user does not, their role is in the setup. */
  see(speaker: string): void {
    const who = speaker.trim();
    if (!who || this.seen.has(who)) return;
    this.seen.add(who);
    if (this.me && isMe(who, this.me)) return;
    const row = el("li", {}, nameCell(who), el("small", { className: "muted", textContent: "buscando…" }));
    byId("live-people").append(row);
    this.lookUp(who).then((found) => {
      if (found.state === "known" && found.role) this.showKnown(row, who, found.id, found.role);
      else if (found.state === "known") this.ask(row, who, found.id, "");
      else if (found.state === "unknown") this.ask(row, who, null, "");
      else this.showUnsure(row, who, found.state);
    });
  }

  private async lookUp(who: string): Promise<Found> {
    try {
      const found = await whoIs("person", who, this.present);
      if (found.resolution !== "known") return { state: found.resolution };
      this.present.push(found.id);
      return { state: "known", id: found.id, role: roleOf((await showEntry(found.id)).entry) };
    } catch {
      return { state: "failed" };
    }
  }

  /** Several people share the name, or the memory did not answer: a new entry would only add another. */
  private showUnsure(row: HTMLLIElement, who: string, state: "ambiguous" | "failed"): void {
    const open = el("button", { className: "link", type: "button", textContent: "Ver en Memoria" });
    open.onclick = () => this.openMemory(who);
    const why = state === "ambiguous" ? "varias personas se llaman así" : "no se pudo consultar la memoria";
    row.replaceChildren(nameCell(who), el("small", { className: "muted", textContent: why }), open);
  }

  /** Someone the memory knows with a role: nothing to ask, only something to correct. */
  private showKnown(row: HTMLLIElement, who: string, id: number, role: string): void {
    const fix = el("button", { className: "link", type: "button", textContent: "Corregir" });
    fix.onclick = () => this.ask(row, who, id, role);
    row.replaceChildren(
      nameCell(who),
      el("span", { textContent: role }),
      fix,
      el("small", { className: "muted", textContent: "de otras reuniones" }),
    );
  }

  /** Asks for the role and the real name: someone new, someone with no role yet, or a correction. */
  private ask(row: HTMLLIElement, who: string, known: number | null, current: string): void {
    let id = known;
    const role = el("input", { value: current, placeholder: "Rol o cargo" });
    const real = el("input", { placeholder: "Nombre real (si Teams lo escribe mal)" });
    const state = el("small", { className: "muted" });
    role.onchange = async () => {
      const value = role.value.trim();
      if (!value || value === current) return;
      try {
        const saved =
          id === null ? await change(["add", "person", who, "--role", value]) : await change(["edit", String(id), "--role", value]);
        id = saved?.id ?? id;
        if (id !== null && !this.present.includes(id)) this.present.push(id);
        state.textContent = "guardado";
      } catch (err) {
        state.textContent = failed(err);
      }
    };
    real.onchange = () => {
      const name = real.value.trim();
      if (!name || name === who) return;
      this.send(`s ${who} => ${name}`);
      state.textContent = `ahora es ${name}`;
    };
    row.replaceChildren(nameCell(who), role, real, state);
    if (current) role.focus();
  }
}

function nameCell(who: string): HTMLElement {
  return el("span", { className: "who", textContent: who, title: who });
}

/** «Daniel Ibarra» is «Daniel Ignacio Ibarra Lagos»; a one-word name would also hide every other Daniel. */
function isMe(who: string, me: string): boolean {
  const words = (text: string) =>
    text
      .normalize("NFD")
      .replace(/\p{M}/gu, "")
      .toLowerCase()
      .split(/[^\p{L}\p{N}]+/u)
      .filter(Boolean);
  const [mine, theirs] = [words(me), words(who)];
  if (mine.length < 2) return mine.join(" ") === theirs.join(" ");
  return mine.every((w) => theirs.includes(w));
}
