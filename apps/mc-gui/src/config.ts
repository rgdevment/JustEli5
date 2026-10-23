// The Configuración tab: the real `mc` config.json (round-tripped, so unknown keys survive)
// and the window's own choices (Whisper models, default platform).
import { configCreate, configLoad, configSave, mcJson, type Model } from "./api";
import { byId, el, show } from "./dom";
import { PLATFORMS } from "./platforms";
import { type Settings, saveSettings } from "./settings";

type Kind = "text" | "number" | "list" | "select";

interface Field {
  path: string;
  label: string;
  kind: Kind;
  options?: [string, string][];
  placeholder?: string;
  hint?: string;
  /** Shown only for these LLM providers. */
  providers?: string[];
  /** The file keeps the number this many times larger (minutes shown, seconds kept). */
  scale?: number;
}

interface Group {
  title: string;
  hint?: string;
  fields: Field[];
}

const PROVIDERS: [string, string][] = [
  ["", "claude-cli (predeterminado)"],
  ["claude-cli", "claude-cli: Claude Code instalado"],
  ["copilot-cli", "copilot-cli: GitHub Copilot CLI"],
  ["anthropic", "anthropic: API de Anthropic"],
  ["openai-compatible", "openai-compatible: OpenAI, Ollama, LM Studio…"],
  ["command", "command: un programa propio"],
];

const GROUPS: Group[] = [
  {
    title: "IA (analista y minuta)",
    fields: [
      { path: "llm.provider", label: "Proveedor", kind: "select", options: PROVIDERS },
      {
        path: "llm.model",
        label: "Modelo",
        kind: "text",
        placeholder: "el del proveedor (p. ej. sonnet)",
        hint: "Elige uno de la lista o escribe el nombre exacto: se le pasa tal cual al proveedor.",
      },
      {
        path: "llm.base_url",
        label: "URL del servidor",
        kind: "text",
        placeholder: "http://localhost:11434/v1",
        providers: ["openai-compatible"],
      },
      {
        path: "llm.api_key_env",
        label: "Variable de entorno con la clave",
        kind: "text",
        placeholder: "ANTHROPIC_API_KEY / OPENAI_API_KEY",
        providers: ["anthropic", "openai-compatible"],
      },
      {
        path: "llm.command",
        label: "Programa",
        kind: "text",
        placeholder: "claude / copilot",
        providers: ["", "claude-cli", "copilot-cli", "command"],
      },
      { path: "llm.args", label: "Argumentos (separados por comas)", kind: "list", providers: ["command"] },
      {
        path: "llm.timeout_s",
        label: "Tiempo máximo por llamada (min, opcional)",
        kind: "number",
        placeholder: "3",
        scale: 60,
        hint: "Cuánto esperar a la IA en cada llamada. Vacío: 3 minutos. 0: sin límite.",
      },
    ],
  },
  {
    title: "Tú",
    fields: [
      {
        path: "fusion.me",
        label: "Tu nombre",
        kind: "text",
        hint: "Tal como aparece en los subtítulos (basta con algunas palabras). Sin él, Tisty recibe la minuta pero ninguna tarea tuya.",
      },
    ],
  },
  {
    title: "Tisty",
    hint: "Escribe la carpeta y la lista tal como se llaman en Tisty (por ejemplo «Trabajo / Meets»).",
    fields: [
      { path: "tisty.folder", label: "Carpeta de las minutas", kind: "text", placeholder: "Reuniones" },
      { path: "tisty.list", label: "Lista de los compromisos", kind: "text", placeholder: "vacío: la bandeja de entrada" },
      { path: "tisty.tags", label: "Etiquetas (separadas por comas)", kind: "list" },
    ],
  },
  {
    title: "Carpetas",
    fields: [
      { path: "data_dir", label: "Datos (sesiones, modelos, índice)", kind: "text", placeholder: "la predeterminada" },
      { path: "output_dir", label: "Exportaciones (.md)", kind: "text", placeholder: "Documentos/justeli5" },
    ],
  },
  {
    title: "Captura",
    fields: [
      {
        path: "capture.source",
        label: "Plataforma por defecto de eli5 en la terminal",
        kind: "select",
        options: [["", "ninguna"], ...PLATFORMS.map((p): [string, string] => [p.id, p.label])],
      },
      { path: "capture.poll_ms", label: "Intervalo de lectura (ms)", kind: "number", placeholder: "100" },
    ],
  },
];

export function getAt(obj: Record<string, unknown>, path: string): unknown {
  return path.split(".").reduce<unknown>((node, key) => (node as Record<string, unknown> | undefined)?.[key], obj);
}

/** `undefined` removes the key, so an emptied field falls back to mc's default. */
export function setAt(obj: Record<string, unknown>, path: string, value: unknown): void {
  const keys = path.split(".");
  const last = keys.pop() as string;
  let node = obj;
  for (const key of keys) {
    if (typeof node[key] !== "object" || node[key] === null) {
      if (value === undefined) return;
      node[key] = {};
    }
    node = node[key] as Record<string, unknown>;
  }
  if (value === undefined) delete node[last];
  else node[last] = value;
}

function shown(value: unknown, kind: Kind, scale = 1): string {
  if (value === undefined || value === null) return "";
  if (kind === "number" && typeof value === "number") return String(value / scale);
  return kind === "list" && Array.isArray(value) ? value.join(", ") : String(value);
}

function parsed(text: string, kind: Kind, scale = 1): unknown {
  const trimmed = text.trim();
  if (!trimmed) return undefined;
  if (kind === "number") return Math.round(Number(trimmed) * scale);
  if (kind === "list") return trimmed.split(",").map((s) => s.trim()).filter(Boolean);
  return trimmed;
}

function input(field: Field, value: unknown): HTMLInputElement | HTMLSelectElement {
  const text = shown(value, field.kind, field.scale);
  if (field.kind === "select") {
    const options = field.options ?? [];
    const known = options.some(([v]) => v === text) ? options : [...options, [text, text] as [string, string]];
    const select = el("select", {}, ...known.map(([v, label]) => el("option", { value: v, textContent: label })));
    select.value = text;
    return select;
  }
  return el("input", {
    type: field.kind === "number" ? "number" : "text",
    value: text,
    placeholder: field.placeholder ?? "",
  });
}

function fieldNode(field: Field, value: unknown): HTMLElement {
  const control = input(field, value);
  control.dataset.path = field.path;
  control.dataset.kind = field.kind;
  if (field.scale) control.dataset.scale = String(field.scale);
  const node = el(
    "label",
    { className: "field" },
    el("span", { textContent: field.label }),
    control,
    field.hint ? el("small", { className: "hint", textContent: field.hint }) : null,
  );
  if (field.providers) node.dataset.providers = field.providers.join(",");
  return node;
}

function appSelect(setting: keyof Settings, options: [string, string][], current: string): HTMLSelectElement {
  const select = el("select", {}, ...options.map(([v, label]) => el("option", { value: v, textContent: label })));
  select.value = current;
  select.dataset.setting = setting;
  return select;
}

function appGroup(settings: Settings, models: Model[]): HTMLElement {
  const modelOptions = (current: string): [string, string][] => {
    const list = models.map((m): [string, string] => [m.name, `${m.name}${m.installed ? " (descargado)" : ""}`]);
    return list.some(([v]) => v === current) ? list : [[current, current], ...list];
  };
  const labelled = (text: string, control: HTMLElement) =>
    el("label", { className: "field" }, el("span", { textContent: text }), control);
  return el(
    "fieldset",
    {},
    el("legend", { textContent: "Esta app" }),
    labelled("Plataforma al abrir", appSelect("platform", PLATFORMS.map((p) => [p.id, p.label]), settings.platform)),
    labelled("Modelo de Whisper en vivo", appSelect("liveModel", modelOptions(settings.liveModel), settings.liveModel)),
    labelled("Modelo de Whisper para la pasada final", appSelect("finalModel", modelOptions(settings.finalModel), settings.finalModel)),
  );
}

function applyProviderVisibility(form: HTMLFormElement): void {
  const provider = form.querySelector<HTMLSelectElement>('[data-path="llm.provider"]')?.value ?? "";
  for (const node of form.querySelectorAll<HTMLElement>("[data-providers]")) {
    show(node, (node.dataset.providers ?? "").split(",").includes(provider));
  }
}

const MODEL_LIST = "llm-models";

/** Fills the model suggestions with what the chosen provider accepts; quiet when it cannot say. */
async function suggestModels(form: HTMLFormElement): Promise<void> {
  const provider = form.querySelector<HTMLSelectElement>('[data-path="llm.provider"]')?.value ?? "";
  const list = form.querySelector<HTMLDataListElement>(`#${MODEL_LIST}`);
  if (!list || list.dataset.provider === provider) return;
  list.dataset.provider = provider;
  const args = ["llm", "models", ...(provider ? ["--llm-provider", provider] : [])];
  const found = await mcJson<{ models: string[] }>(args).catch(() => ({ models: [] }));
  // A slower answer for a provider no longer chosen is dropped.
  if (list.dataset.provider !== provider) return;
  list.replaceChildren(...found.models.map((m) => el("option", { value: m })));
}

/** The model box offers the list without closing the door to a name typed by hand. */
function attachModelList(form: HTMLFormElement): void {
  const input = form.querySelector<HTMLInputElement>('[data-path="llm.model"]');
  if (!input) return;
  input.setAttribute("list", MODEL_LIST);
  input.after(el("datalist", { id: MODEL_LIST }));
}

export interface ConfigView {
  reload: () => Promise<void>;
}

/** Builds the tab; `onSaved` lets the checklist re-run after a change. */
export function configView(
  settings: () => Settings,
  models: () => Model[],
  onSaved: (settings: Settings) => void,
): ConfigView {
  const form = byId<HTMLFormElement>("config-form");
  const state = byId("config-state");
  let base: Record<string, unknown> = {};

  const reload = async () => {
    const file = await configLoad();
    base = file.value;
    byId("config-path").textContent = file.path ?? "(sin ubicación conocida)";
    show(byId("config-missing"), !file.exists);
    const error = byId("config-error");
    error.textContent = file.error ? `El archivo no es JSON válido: ${file.error}` : "";
    show(error, Boolean(file.error));
    // Saving over a file that does not parse would lose what the user wrote in it.
    byId<HTMLButtonElement>("config-save").disabled = Boolean(file.error);
    form.replaceChildren(
      appGroup(settings(), models()),
      ...GROUPS.map((group) =>
        el(
          "fieldset",
          {},
          el("legend", { textContent: group.title }),
          group.hint ? el("p", { className: "hint", textContent: group.hint }) : null,
          ...group.fields.map((f) => fieldNode(f, getAt(base, f.path))),
        ),
      ),
    );
    applyProviderVisibility(form);
    attachModelList(form);
    suggestModels(form);
    state.textContent = "";
  };

  form.onchange = () => {
    applyProviderVisibility(form);
    suggestModels(form);
  };
  form.onsubmit = (event) => event.preventDefault();
  byId("config-reload").onclick = () => reload();
  byId("config-create").onclick = async () => {
    await configCreate().catch((err) => (state.textContent = String(err)));
    await reload();
  };
  byId("config-save").onclick = async () => {
    const value = structuredClone(base);
    for (const control of form.querySelectorAll<HTMLInputElement | HTMLSelectElement>("[data-path]")) {
      setAt(value, control.dataset.path as string, parsed(control.value, control.dataset.kind as Kind, Number(control.dataset.scale ?? 1)));
    }
    const next = { ...settings() };
    for (const control of form.querySelectorAll<HTMLSelectElement>("[data-setting]")) {
      (next as unknown as Record<string, string>)[control.dataset.setting as string] = control.value;
    }
    state.className = "muted";
    state.textContent = "Comprobando con eli5…";
    try {
      await configSave(value);
      await saveSettings(next);
      base = value;
      state.className = "";
      state.textContent = "Guardado.";
      onSaved(next);
    } catch (err) {
      state.className = "bad";
      state.textContent = `eli5 no acepta esta configuración: ${err}`;
    }
  };
  return { reload };
}
