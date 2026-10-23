// After the meeting: the same steps as scripts/meet.ps1, each one shown with its progress.
import { errorText, newJobId, runJob, spawnMc } from "./api";
import { byId, el, joinPath, plainPath } from "./dom";

export interface PostOptions {
  sessionDir: string;
  /** Whisper over the whole recording, then the fusion; skipped unless asked. */
  finalPass: boolean;
  finalModel: string;
  contextFile: string | null;
  tisty: boolean;
}

export interface PostResult {
  minutes: string | null;
  failed: string[];
  /** Forms the minutes used that name more than one entry of the memory here. */
  ambiguous: string[];
}

interface Step {
  label: string;
  args: string[];
  /** Why the step does not run, decided before it would start. */
  skip?: (done: Set<string>) => string | null;
}

const TRANSCRIBE = "transcribe";

function steps(o: PostOptions): Step[] {
  const dir = o.sessionDir;
  const context = o.contextFile ? ["--context", o.contextFile] : [];
  const notAsked = () => (o.finalPass ? null : "omitido: sin pasada final de Whisper");
  const all: Step[] = [
    { label: "Transcripción de los subtítulos", args: ["run", dir] },
    { label: "Reconocimiento del audio (Whisper)", args: [TRANSCRIBE, dir, "--model", o.finalModel], skip: notAsked },
    {
      label: "Fusión de subtítulos y audio",
      args: ["fuse", dir],
      skip: (done) => notAsked() ?? (done.has(TRANSCRIBE) ? null : "omitido: no hay reconocimiento del audio"),
    },
    { label: "Minuta", args: ["minutes", dir, ...context] },
    { label: "Exportar la transcripción (.md)", args: ["export", dir, "--format", "md", "--save"] },
    {
      label: "Guardar en Tisty",
      args: ["export", dir, "--tisty"],
      skip: (done) => (done.has("minutes") ? null : "omitido: no hay minuta"),
    },
  ];
  return o.tisty ? all : all.filter((s) => !s.args.includes("--tisty"));
}

function stepNode(label: string): { node: HTMLElement; state: HTMLElement; out: HTMLElement } {
  const state = el("span", { className: "state", textContent: "pendiente" });
  const out = el("span", { className: "out" });
  return { node: el("li", {}, label, state, out), state, out };
}

/** Runs one step; resolves with its JSON result or throws its error. */
async function runStep(step: Step, out: HTMLElement): Promise<Record<string, unknown>> {
  let stdout = "";
  let stderr = "";
  const id = newJobId("post");
  const code = await runJob(
    id,
    () => spawnMc(id, ["--json", ...step.args]),
    (line, stream) => {
      if (stream === "stdout") stdout = line;
      else {
        stderr += line + "\n";
        out.textContent = line;
      }
    },
  );
  if (code !== 0) throw new Error(errorText({ code, stdout, stderr }));
  try {
    return JSON.parse(stdout);
  } catch {
    return {};
  }
}

function ambiguousOf(memory: unknown): string[] {
  const list = (memory as { ambiguous?: unknown } | null)?.ambiguous;
  return Array.isArray(list) ? list.map(String) : [];
}

export async function runPost(o: PostOptions): Promise<PostResult> {
  const list = byId("steps");
  list.replaceChildren();
  const result: PostResult = { minutes: null, failed: [], ambiguous: [] };
  const done = new Set<string>();
  for (const step of steps({ ...o, sessionDir: plainPath(o.sessionDir) })) {
    const view = stepNode(step.label);
    list.append(view.node);
    const skipped = step.skip?.(done);
    if (skipped) {
      view.state.textContent = skipped;
      view.state.className = "state skipped";
      continue;
    }
    view.state.textContent = "en curso…";
    view.state.className = "state running";
    try {
      const value = await runStep(step, view.out);
      done.add(step.args[0]);
      view.state.textContent = "hecho";
      view.state.className = "state ok";
      const path = typeof value.path === "string" ? plainPath(value.path) : "";
      view.out.textContent = path;
      if (step.args[0] === "minutes" && path) result.minutes = joinPath(path, "minutes.md");
      if (step.args[0] === "minutes") result.ambiguous = ambiguousOf(value.memory);
    } catch (err) {
      view.state.textContent = "falló";
      view.state.className = "state bad";
      view.out.textContent = err instanceof Error ? err.message : String(err);
      result.failed.push(step.label);
    }
  }
  return result;
}
