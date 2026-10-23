// The first-run wizard: who you are, the AI agent, the meeting app, Tisty, audio and models,
// written to mc's config.json (round-tripped) and to the window's settings.
import {
  type Agents,
  type AudioReport,
  type Doctor,
  type Model,
  agentsDetect,
  configLoad,
  configSave,
  mcJson,
  spawnMc,
} from "./api";
import { renderProbe } from "./audio";
import { getAt, setAt } from "./config";
import { byId, el, show } from "./dom";
import { confirmAndRun } from "./jobdialog";
import { PLATFORMS, platformLabel } from "./platforms";
import type { Settings } from "./settings";

interface Agent {
  id: string;
  label: string;
  provider: string;
  baseUrl?: string;
  model: string;
  found: (a: Agents) => string | null;
}

const AGENTS: Agent[] = [
  {
    id: "claude-cli",
    label: "Claude Code (claude)",
    provider: "claude-cli",
    model: "sonnet",
    found: (a) => a.claude,
  },
  {
    id: "copilot-cli",
    label: "GitHub Copilot CLI (copilot)",
    provider: "copilot-cli",
    model: "",
    found: (a) => a.copilot,
  },
  {
    id: "anthropic",
    label: "API de Anthropic",
    provider: "anthropic",
    model: "claude-sonnet-5-5",
    found: (a) => (a.anthropic_key ? "ANTHROPIC_API_KEY definida" : null),
  },
  {
    id: "ollama",
    label: "Ollama (local)",
    provider: "openai-compatible",
    baseUrl: "http://localhost:11434/v1",
    model: "",
    found: (a) => (a.ollama ? "responde en localhost:11434" : null),
  },
  {
    id: "lmstudio",
    label: "LM Studio (local)",
    provider: "openai-compatible",
    baseUrl: "http://localhost:1234/v1",
    model: "",
    found: (a) => (a.lmstudio ? "responde en localhost:1234" : null),
  },
];

const LIVE_MODEL = "whisper-base";
const FINAL_MODEL = "whisper-large-v3-turbo-q5_0";
const AUDIO_TEST_SECONDS = 10;

interface Draft {
  me: string;
  meRole: string;
  agent: string;
  model: string;
  platform: string;
  tisty: boolean;
  folder: string;
  list: string;
  micDevice: string;
  systemDevice: string;
  liveAsr: boolean;
  finalPass: boolean;
}

interface Step {
  title: string;
  hint: string;
  body: (d: Draft) => HTMLElement | Promise<HTMLElement>;
  skip?: (d: Draft) => boolean;
}

const text = (value: unknown): string => (typeof value === "string" ? value : "");

function agentOf(config: Record<string, unknown>): Agent {
  const provider = text(getAt(config, "llm.provider")) || "claude-cli";
  if (provider === "openai-compatible") {
    const local = text(getAt(config, "llm.base_url")).includes(":1234") ? "lmstudio" : "ollama";
    return AGENTS.find((a) => a.id === local) as Agent;
  }
  return AGENTS.find((a) => a.provider === provider) ?? AGENTS[0];
}

function draftFrom(config: Record<string, unknown>, s: Settings): Draft {
  const agent = agentOf(config);
  return {
    me: text(getAt(config, "fusion.me")),
    meRole: text(getAt(config, "fusion.me_role")),
    agent: agent.id,
    model: text(getAt(config, "llm.model")) || agent.model,
    platform: s.platform,
    tisty: s.tisty,
    folder: text(getAt(config, "tisty.folder")) || "Trabajo / Meets",
    list: text(getAt(config, "tisty.list")) || "Trabajo",
    micDevice: s.micDevice,
    systemDevice: s.systemDevice,
    liveAsr: s.liveAsr,
    finalPass: s.finalPass,
  };
}

function field(label: string, value: string, set: (v: string) => void, placeholder = "", list?: string): HTMLElement {
  const input = el("input", { value, placeholder });
  if (list) input.setAttribute("list", list);
  input.oninput = () => set(input.value);
  return el("label", { className: "field" }, el("span", { textContent: label }), input);
}

function checkbox(label: string, checked: boolean, set: (v: boolean) => void, hint = ""): HTMLElement {
  const input = el("input", { type: "checkbox", checked });
  input.onchange = () => set(input.checked);
  return el(
    "label",
    { className: "toggle" },
    input,
    el("span", {}, label, hint ? el("small", { className: "muted", textContent: ` ${hint}` }) : null),
  );
}

function choice(label: string, detail: string, selected: boolean, missing: boolean, pick: () => void): HTMLElement {
  return el(
    "button",
    { className: `choice${selected ? " selected" : ""}`, onclick: pick, type: "button" },
    el("strong", { textContent: label }),
    el("small", { className: missing ? "bad" : "ok", textContent: detail }),
  );
}

/** Re-renders the current step after a change that alters its own layout. */
let rerender: () => void = () => {};

const steps: Step[] = [
  {
    title: "¿Quién eres?",
    hint: "Tu nombre tal como aparece en Teams (o en la plataforma que uses) y tu rol. Con él la minuta sabe qué compromisos son tuyos.",
    body: (d) =>
      el(
        "div",
        {},
        field("Tu nombre en la reunión", d.me, (v) => (d.me = v), "Ej.: Mario Hidalgo"),
        field("Tu rol", d.meRole, (v) => (d.meRole = v), "Ej.: Lead - Pagos"),
      ),
  },
  {
    title: "Agente de IA",
    hint: "El que escribe el análisis en vivo y la minuta. En rojo, lo que no encontré en este equipo.",
    body: async (d) => {
      const found = await agentsDetect().catch(() => null);
      const agent = AGENTS.find((a) => a.id === d.agent) ?? AGENTS[0];
      const result = el("p", { className: "muted" });
      const test = el("button", { textContent: "Probar", type: "button" });
      test.onclick = async () => {
        result.textContent = "Probando…";
        const args = ["doctor", "--llm-provider", agent.provider];
        if (d.model) args.push("--llm-model", d.model);
        const doctor = await mcJson<Doctor>(args).catch((err: Error) => err);
        const llm = doctor instanceof Error ? null : doctor.checks.find((c) => c.name === "llm");
        result.textContent = doctor instanceof Error ? doctor.message : `${llm?.status ?? "?"}: ${llm?.detail ?? ""}`;
        result.className = llm?.status === "ok" ? "ok" : "bad";
      };
      const models = el("datalist", { id: "setup-models" });
      const ask = ["llm", "models", "--llm-provider", agent.provider, ...(agent.baseUrl ? ["--base-url", agent.baseUrl] : [])];
      mcJson<{ models: string[] }>(ask)
        .then((found) => models.replaceChildren(...found.models.map((m) => el("option", { value: m }))))
        .catch(() => {});
      return el(
        "div",
        {},
        el(
          "div",
          { className: "choices" },
          ...AGENTS.map((a) => {
            const where = found ? a.found(found) : null;
            return choice(a.label, where ? `✓ ${where}` : "no encontrado", a.id === d.agent, !where, () => {
              d.agent = a.id;
              d.model = a.model;
              rerender();
            });
          }),
        ),
        field("Modelo", d.model, (v) => (d.model = v), agent.model || "el que tengas cargado", "setup-models"),
        models,
        agent.baseUrl ? el("p", { className: "muted", textContent: `Servidor: ${agent.baseUrl}` }) : null,
        el("div", { className: "row" }, test, result),
      );
    },
  },
  {
    title: "Tu aplicación de reuniones",
    hint: "La que más usas. Igual se detecta sola la que tengas abierta al empezar.",
    body: (d) =>
      el(
        "div",
        { className: "choices" },
        ...PLATFORMS.map((p) =>
          choice(p.label, p.id === d.platform ? "predeterminada" : "", p.id === d.platform, false, () => {
            d.platform = p.id;
            rerender();
          }),
        ),
      ),
  },
  {
    title: "Tisty",
    hint: "Dónde guardar la minuta y tus compromisos al terminar cada reunión.",
    body: async (d) => {
      const doctor = await mcJson<Doctor>(["doctor", "--tisty"]).catch(() => null);
      const mcp = doctor?.checks.find((c) => c.name === "tisty_mcp");
      const ok = mcp?.status === "ok";
      return el(
        "div",
        {},
        el("p", {
          className: ok ? "ok" : "bad",
          textContent: ok ? "✓ Tisty encontrado" : `Tisty no responde${mcp ? `: ${mcp.detail}` : ""}`,
        }),
        checkbox("Guardar en Tisty al final de cada reunión", d.tisty, (v) => (d.tisty = v)),
        field("Carpeta de las minutas", d.folder, (v) => (d.folder = v), "Trabajo / Meets"),
        field("Lista de tus compromisos", d.list, (v) => (d.list = v), "Trabajo"),
      );
    },
  },
  {
    title: "Audio",
    hint: "Se graba como respaldo de los subtítulos. Whisper viene apagado: los subtítulos de la plataforma son la fuente principal.",
    body: async (d) => {
      const doctor = await mcJson<Doctor>(["doctor", "--audio", "--audio-seconds", "1"]).catch(() => null);
      const devices = doctor?.audio?.devices ?? [];
      const select = (current: string, first: string, kind: "input" | "output", set: (v: string) => void) => {
        const node = el(
          "select",
          {},
          el("option", { value: "", textContent: first }),
          ...devices
            .filter((dev) => dev.direction === kind)
            .map((dev) => el("option", { value: dev.name, textContent: dev.default ? `${dev.name} (predeterminado)` : dev.name })),
        );
        node.value = current;
        node.onchange = () => set(node.value);
        return node;
      };
      const probe = el("div", { className: "probe" });
      const test = el("button", { textContent: `Probar audio (${AUDIO_TEST_SECONDS} s)`, type: "button" });
      test.onclick = async () => {
        test.disabled = true;
        probe.textContent = "Escuchando… habla y pon algo a sonar en la reunión.";
        const args = ["doctor", "--audio", "--audio-seconds", String(AUDIO_TEST_SECONDS)];
        if (d.micDevice) args.push("--mic-device", d.micDevice);
        if (d.systemDevice) args.push("--system-device", d.systemDevice);
        const report = await mcJson<Doctor>(args).catch((err: Error) => err);
        test.disabled = false;
        const audio: AudioReport | undefined = report instanceof Error ? undefined : report.audio;
        if (audio) renderProbe(probe, audio);
        else probe.textContent = report instanceof Error ? report.message : "eli5 no devolvió el audio";
      };
      return el(
        "div",
        {},
        el(
          "label",
          { className: "field" },
          el("span", { textContent: "Micrófono" }),
          select(d.micDevice, "Predeterminado del sistema", "input", (v) => (d.micDevice = v)),
        ),
        el(
          "label",
          { className: "field" },
          el("span", { textContent: "Audio de la reunión" }),
          select(d.systemDevice, "Automático (la app de la reunión)", "output", (v) => (d.systemDevice = v)),
        ),
        el("div", { className: "row" }, test),
        probe,
        checkbox("Whisper en vivo", d.liveAsr, (v) => (d.liveAsr = v), "transcribe el audio mientras grabas; aporta poco si hay subtítulos"),
        checkbox(
          "Pasada final de Whisper",
          d.finalPass,
          (v) => (d.finalPass = v),
          "corrige términos con el audio al terminar; sin tarjeta gráfica tarda horas",
        ),
      );
    },
  },
  {
    title: "Modelos de Whisper",
    hint: "Se descargan una sola vez.",
    skip: (d) => !d.liveAsr && !d.finalPass,
    body: async (d) => {
      const listed = await mcJson<{ models: Model[] }>(["models", "list"]).catch(() => null);
      const needed = [d.liveAsr ? LIVE_MODEL : "", d.finalPass ? FINAL_MODEL : ""].filter(Boolean);
      return el(
        "ul",
        { className: "plain" },
        ...needed.map((name) => {
          const installed = listed?.models.find((m) => m.name === name)?.installed ?? false;
          const pull = el("button", { textContent: "Descargar", type: "button" });
          pull.onclick = async () => {
            const done = await confirmAndRun({
              title: `Descargar ${name}`,
              text: "Se descarga una vez y se comprueba su sha256. Puede tardar unos minutos.",
              command: `eli5 models pull ${name}`,
              start: (id) => spawnMc(id, ["models", "pull", name]),
            });
            if (done) rerender();
          };
          return el(
            "li",
            {},
            el("strong", { textContent: name }),
            " ",
            installed ? el("span", { className: "ok", textContent: "✓ descargado" }) : pull,
          );
        }),
      );
    },
  },
  {
    title: "Resumen",
    hint: "Revisa y pulsa «Listo». Todo se puede cambiar después en Configuración.",
    body: (d) => {
      const agent = AGENTS.find((a) => a.id === d.agent) ?? AGENTS[0];
      const line = (k: string, v: string) => el("li", {}, el("strong", { textContent: `${k}: ` }), v || "—");
      return el(
        "ul",
        { className: "plain" },
        line("Tú", d.me ? `${d.me}${d.meRole ? ` (${d.meRole})` : ""}` : ""),
        line("Agente de IA", `${agent.label}${d.model ? ` · ${d.model}` : ""}`),
        line("Reuniones en", platformLabel(d.platform)),
        line("Tisty", d.tisty ? `sí · «${d.folder}», lista «${d.list}»` : "no"),
        line("Whisper", [d.liveAsr ? "en vivo" : "", d.finalPass ? "pasada final" : ""].filter(Boolean).join(" y ") || "apagado"),
      );
    },
  },
];

const empty = (v: string): string | undefined => v.trim() || undefined;

async function save(d: Draft, s: Settings): Promise<Settings> {
  const file = await configLoad();
  if (file.error) throw new Error(`tu config.json no es JSON válido: ${file.error}`);
  const config = file.value;
  const agent = AGENTS.find((a) => a.id === d.agent) ?? AGENTS[0];
  setAt(config, "fusion.me", empty(d.me));
  setAt(config, "fusion.me_role", empty(d.meRole));
  setAt(config, "llm.provider", agent.provider);
  setAt(config, "llm.model", empty(d.model));
  setAt(config, "llm.base_url", agent.baseUrl);
  setAt(config, "tisty.folder", empty(d.folder));
  setAt(config, "tisty.list", empty(d.list));
  await configSave(config);
  return {
    ...s,
    platform: d.platform,
    tisty: d.tisty,
    micDevice: d.micDevice,
    systemDevice: d.systemDevice,
    liveAsr: d.liveAsr,
    finalPass: d.finalPass,
    setupDone: true,
  };
}

/** Shows the wizard and resolves with the saved settings, or the unchanged ones if it is closed. */
export async function runSetup(settings: Settings): Promise<Settings> {
  const file = await configLoad();
  const draft = draftFrom(file.value, settings);
  const body = byId("setup-body");
  const back = byId<HTMLButtonElement>("setup-back");
  const next = byId<HTMLButtonElement>("setup-next");
  const skip = byId<HTMLButtonElement>("setup-skip");
  const error = byId("setup-error");
  const visible = () => steps.filter((step) => !step.skip?.(draft));
  let index = 0;

  const render = async () => {
    const shown = visible();
    index = Math.min(index, shown.length - 1);
    const step = shown[index];
    byId("setup-count").textContent = `Paso ${index + 1} de ${shown.length}`;
    byId("setup-title").textContent = step.title;
    byId("setup-hint").textContent = step.hint;
    show(error, false);
    back.disabled = index === 0;
    next.textContent = index === shown.length - 1 ? "Listo" : "Siguiente";
    body.replaceChildren(el("p", { className: "muted", textContent: "Revisando…" }));
    body.replaceChildren(await step.body(draft));
  };
  rerender = () => void render();

  return new Promise((resolve) => {
    back.onclick = () => {
      index = Math.max(0, index - 1);
      render();
    };
    skip.onclick = () => resolve({ ...settings, setupDone: true });
    next.onclick = async () => {
      if (index < visible().length - 1) {
        index += 1;
        render();
        return;
      }
      next.disabled = true;
      try {
        resolve(await save(draft, settings));
      } catch (err) {
        error.textContent = `No se pudo guardar: ${err instanceof Error ? err.message : err}`;
        show(error, true);
      } finally {
        next.disabled = false;
      }
    };
    render();
  });
}
