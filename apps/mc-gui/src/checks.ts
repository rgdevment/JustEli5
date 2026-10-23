// The status checklist: what is ready (green), what needs a look (amber) and what is missing (red).
import type { AudioReport, Check, ConfigFile, Detect, Doctor, Model, Prereq } from "./api";
import { type State, verdict } from "./audio";
import { el } from "./dom";
import { PLATFORMS, platformLabel } from "./platforms";
import type { Settings } from "./settings";

export interface Action {
  label: string;
  run: () => void;
}

export interface Row {
  label: string;
  state: State;
  detail: string;
  actions: Action[];
}

/** Everything the checklist is computed from; `undefined` while still being read. */
export interface Snapshot {
  mcPath: string | null;
  mcSource: string | null;
  config?: ConfigFile;
  doctor?: Doctor | Error;
  models?: Model[] | Error;
  detect?: Detect | Error;
  audio?: AudioReport | Error;
}

/** What the fix buttons do; main wires them. */
export interface Fixes {
  installMc: () => void;
  createConfig: () => void;
  openConfig: () => void;
  pullModel: (name: string) => void;
  testAudio: () => void;
  openSettings: (page: string) => void;
  redetect: () => void;
}

const isMac = navigator.userAgent.includes("Mac");

const pending = (label: string): Row => ({ label, state: "warn", detail: "Comprobando…", actions: [] });

function check(doctor: Doctor, name: string): Check | undefined {
  return doctor.checks.find((c) => c.name === name);
}

function mcRow(s: Snapshot, fix: Fixes): Row {
  if (s.mcPath) return { label: "eli5", state: "ok", detail: s.mcPath, actions: [] };
  return {
    label: "eli5",
    state: "bad",
    detail: s.mcSource
      ? "No está instalado; puedes compilarlo desde la copia del repositorio (abajo, lo que necesita)."
      : "No está instalado. Desde una copia del repositorio: cargo install --path crates/mc-cli --features whisper",
    actions: s.mcSource ? [{ label: "Compilar e instalar", run: fix.installMc }] : [],
  };
}

function configRow(s: Snapshot, fix: Fixes): Row {
  const label = "Configuración";
  if (!s.config || !s.doctor) return pending(label);
  const failed = s.doctor instanceof Error ? undefined : check(s.doctor, "config");
  if (s.doctor instanceof Error || failed?.status === "FAIL") {
    const detail = s.doctor instanceof Error ? s.doctor.message : (failed?.detail ?? "");
    return { label, state: "bad", detail, actions: [{ label: "Abrir configuración", run: fix.openConfig }] };
  }
  if (!s.config.exists) {
    return {
      label,
      state: "warn",
      detail: "No hay archivo de configuración: se usan los valores por defecto.",
      actions: [{ label: "Crear", run: fix.createConfig }],
    };
  }
  return { label, state: "ok", detail: s.config.path ?? "", actions: [] };
}

function whisperRow(s: Snapshot, fix: Fixes): Row {
  const label = "Whisper en eli5";
  if (!s.doctor) return pending(label);
  const found = s.doctor instanceof Error ? undefined : check(s.doctor, "whisper");
  if (found?.status === "ok") return { label, state: "ok", detail: found.detail, actions: [] };
  return {
    label,
    state: "bad",
    detail: "Este eli5 no reconoce voz: compílalo con --features whisper.",
    actions: s.mcSource ? [{ label: "Recompilar con Whisper", run: fix.installMc }] : [],
  };
}

function modelRow(s: Snapshot, fix: Fixes, name: string, use: string): Row {
  const label = `Modelo ${use}: ${name}`;
  if (!s.models) return pending(label);
  if (s.models instanceof Error) return { label, state: "bad", detail: s.models.message, actions: [] };
  const model = s.models.find((m) => m.name === name);
  if (model?.installed) return { label, state: "ok", detail: "Descargado", actions: [] };
  return {
    label,
    state: "bad",
    detail: model ? "No está descargado." : "eli5 no conoce este modelo; elige otro en Configuración.",
    actions: model ? [{ label: "Descargar", run: () => fix.pullModel(name) }] : [],
  };
}

const LLM_HELP: Record<string, string> = {
  "claude-cli": "Instala Claude Code (npm install -g @anthropic-ai/claude-code) y entra una vez con «claude».",
  "copilot-cli": "Instala Copilot CLI (npm install -g @github/copilot) y entra una vez con «copilot».",
  anthropic: "Define la variable de entorno ANTHROPIC_API_KEY con tu clave y reinicia la app.",
  "openai-compatible": "Revisa base_url y la variable de entorno de la clave en Configuración.",
};

function llmRow(s: Snapshot, fix: Fixes): Row {
  const label = "Proveedor de IA";
  if (!s.doctor) return pending(label);
  if (s.doctor instanceof Error) return { label, state: "bad", detail: s.doctor.message, actions: [] };
  const found = check(s.doctor, "llm");
  if (found?.status === "ok") return { label, state: "ok", detail: found.detail, actions: [] };
  const provider = found?.detail.split(/[ :(]/)[0] ?? "";
  return {
    label,
    state: "bad",
    detail: `${found?.detail ?? "sin comprobar"}. ${LLM_HELP[provider] ?? ""}`.trim(),
    actions: [{ label: "Cambiar proveedor", run: fix.openConfig }],
  };
}

function tistyRow(s: Snapshot): Row {
  const label = "Tisty";
  if (!s.doctor) return pending(label);
  if (s.doctor instanceof Error) return { label, state: "bad", detail: s.doctor.message, actions: [] };
  const app = check(s.doctor, "tisty");
  const server = check(s.doctor, "tisty_mcp");
  if (app?.status === "ok" && server?.status === "ok") {
    return { label, state: "ok", detail: server.detail, actions: [] };
  }
  const why = app?.status !== "ok" ? app?.detail : server?.detail;
  return {
    label,
    state: "bad",
    detail: `${why ?? "sin comprobar"}. Instala Tisty y ábrelo una vez; la minuta se guarda igual en archivos.`,
    actions: [],
  };
}

function audioRow(s: Snapshot, fix: Fixes, kind: "mic" | "system"): Row {
  const label = kind === "mic" ? "Micrófono" : "Audio de la reunión";
  if (!s.audio) return pending(label);
  if (s.audio instanceof Error) return { label, state: "bad", detail: s.audio.message, actions: [] };
  if (!s.audio.probe) return { label, state: "bad", detail: "No hay audio en este sistema.", actions: [] };
  const v = verdict(s.audio.probe[kind], kind);
  const settingsPage = kind === "mic" ? "microphone" : isMac ? "screen" : "sound";
  const actions: Action[] = [{ label: "Probar audio", run: fix.testAudio }];
  if (v.state !== "ok") actions.push({ label: "Abrir ajustes", run: () => fix.openSettings(settingsPage) });
  return { label, state: v.state, detail: v.text, actions };
}

function captionsRow(s: Snapshot, fix: Fixes, platform: string): Row {
  const label = `Subtítulos de ${platformLabel(platform)}`;
  if (!s.detect) return pending(label);
  const again = { label: "Detectar", run: fix.redetect };
  if (s.detect instanceof Error) {
    const actions = isMac ? [again, { label: "Permiso de accesibilidad", run: () => fix.openSettings("accessibility") }] : [again];
    return { label, state: "bad", detail: s.detect.message, actions };
  }
  const found = s.detect.platforms.find((p) => p.platform === platform);
  if (!found) {
    return {
      label,
      state: "bad",
      detail: `No veo una ventana de ${platformLabel(platform)} abierta; ábrela (la grabación también la espera).`,
      actions: [again],
    };
  }
  if (found.error) return { label, state: "bad", detail: found.error, actions: [again] };
  if (found.captions_on) {
    return { label, state: "ok", detail: `Activos (${found.captions} líneas a la vista)`, actions: [] };
  }
  const how = PLATFORMS.find((p) => p.id === platform)?.captions ?? "";
  return { label, state: "bad", detail: `No están activados. ${how}`, actions: [again] };
}

export function rows(s: Snapshot, settings: Settings, fix: Fixes): Row[] {
  const list = [mcRow(s, fix)];
  if (!s.mcPath) return list;
  list.push(configRow(s, fix));
  if (settings.recordAudio) {
    // Whisper is checked only when it will run: recording audio alone does not need it.
    if (settings.liveAsr || settings.finalPass) list.push(whisperRow(s, fix));
    if (settings.liveAsr) list.push(modelRow(s, fix, settings.liveModel, "en vivo"));
    if (settings.finalPass) list.push(modelRow(s, fix, settings.finalModel, "final"));
  }
  list.push(llmRow(s, fix));
  if (settings.tisty) list.push(tistyRow(s));
  if (settings.recordAudio) list.push(audioRow(s, fix, "mic"), audioRow(s, fix, "system"));
  list.push(captionsRow(s, fix, settings.platform));
  return list;
}

function rowNode(row: Row): HTMLElement {
  return el(
    "li",
    { className: `check ${row.state}` },
    el("span", { className: `dot ${row.state}` }),
    el(
      "div",
      {},
      el("div", { className: "label", textContent: row.label }),
      el("div", { className: "detail", textContent: row.detail }),
    ),
    el(
      "div",
      { className: "actions" },
      ...row.actions.map((a) => el("button", { className: "small", textContent: a.label, onclick: a.run })),
    ),
  );
}

export function renderRows(list: HTMLElement, items: Row[]): void {
  list.replaceChildren(...items.map(rowNode));
}

export function renderPrereqs(list: HTMLElement, items: Prereq[], install: (p: Prereq) => void): void {
  list.replaceChildren(
    ...items.map((p) =>
      rowNode({
        label: p.label,
        state: p.found ? "ok" : "bad",
        detail: p.found ? "Instalado" : p.install ? `Se instala con: ${p.install}` : p.manual,
        actions: !p.found && p.install ? [{ label: "Instalar", run: () => install(p) }] : [],
      }),
    ),
  );
}
