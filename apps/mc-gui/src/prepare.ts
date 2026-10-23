// The preparation form: reading and writing its fields, device lists, platforms and names.
import type { Detect, Device } from "./api";
import { byId, el } from "./dom";
import { PLATFORMS, platformLabel } from "./platforms";
import type { Settings } from "./settings";

const checkbox = (id: string) => byId<HTMLInputElement>(id);

export function writeForm(s: Settings): void {
  byId<HTMLInputElement>("name").value = s.name;
  checkbox("record-audio").checked = s.recordAudio;
  checkbox("live-asr").checked = s.liveAsr;
  checkbox("final-pass").checked = s.finalPass;
  checkbox("analyst").checked = s.analyst;
  checkbox("tisty").checked = s.tisty;
  byId<HTMLTextAreaElement>("context").value = s.context;
  // The saved devices stay selected until the real list arrives.
  fillDevices([], s);
  syncAudioControls(s.recordAudio);
}

export function readForm(s: Settings): Settings {
  return {
    ...s,
    name: byId<HTMLInputElement>("name").value.trim(),
    recordAudio: checkbox("record-audio").checked,
    liveAsr: checkbox("live-asr").checked,
    finalPass: checkbox("final-pass").checked,
    analyst: checkbox("analyst").checked,
    tisty: checkbox("tisty").checked,
    context: byId<HTMLTextAreaElement>("context").value,
    micDevice: byId<HTMLSelectElement>("mic-device").value,
    systemDevice: byId<HTMLSelectElement>("system-device").value,
  };
}

/** Live recognition and the device pickers only mean something while audio is recorded. */
export function syncAudioControls(recordAudio: boolean): void {
  for (const id of ["live-asr", "final-pass", "mic-device", "system-device", "audio-test"]) {
    byId<HTMLInputElement>(id).disabled = !recordAudio;
  }
}

export function renderPlatforms(selected: string, detect: Detect | undefined, onPick: (id: string) => void): void {
  const found = detect?.platforms ?? [];
  byId("platforms").replaceChildren(
    ...PLATFORMS.map((p) => {
      const seen = found.find((d) => d.platform === p.id);
      const badge = seen ? el("span", { className: "badge", textContent: seen.captions_on ? "● abierta, CC" : "● abierta" }) : null;
      return el(
        "button",
        { className: `pill${p.id === selected ? " selected" : ""}`, onclick: () => onPick(p.id) },
        p.label,
        badge,
      );
    }),
  );
}

export function detectedText(detect: Detect | Error | undefined): string {
  if (!detect) return "Buscando reuniones abiertas…";
  if (detect instanceof Error) return `No se pudo detectar: ${detect.message}`;
  if (!detect.platforms.length) return "No veo ninguna reunión abierta.";
  const names = detect.platforms.map(
    (p) => `${platformLabel(p.platform)}${p.captions_on ? " (con subtítulos)" : " (sin subtítulos)"}`,
  );
  return `Detectado: ${names.join(", ")}.`;
}

/** The platform to preselect: the current one if it is open, else one with captions, else any open one. */
export function preferredPlatform(current: string, detect: Detect): string {
  const open = detect.platforms;
  if (open.some((p) => p.platform === current)) return current;
  return (open.find((p) => p.captions_on) ?? open[0])?.platform ?? current;
}

function options(select: HTMLSelectElement, items: [string, string][], current: string): void {
  const known = items.some(([v]) => v === current) || !current ? items : [...items, [current, `${current} (no conectado)`] as [string, string]];
  select.replaceChildren(...known.map(([value, label]) => el("option", { value, textContent: label })));
  select.value = current;
}

export function fillDevices(devices: Device[], s: Settings): void {
  const named = (d: Device): [string, string] => [d.name, d.default ? `${d.name} (predeterminado)` : d.name];
  const inputs = devices.filter((d) => d.direction === "input").map(named);
  const outputs = devices.filter((d) => d.direction === "output").map(named);
  options(byId("mic-device"), [["", "Predeterminado del sistema"], ...inputs], s.micDevice);
  options(byId("system-device"), [["", "Automático (la app de la reunión)"], ...outputs], s.systemDevice);
}

export function fillNames(names: string[]): void {
  byId("recent-names").replaceChildren(...names.map((value) => el("option", { value })));
}

export function setTistyTarget(target: { folder: string; list: string | null } | undefined): void {
  byId("tisty-target").textContent = target
    ? `Tisty: carpeta «${target.folder}»${target.list ? `, lista «${target.list}»` : ", compromisos en la bandeja de entrada"} (se cambia en Configuración).`
    : "";
}
