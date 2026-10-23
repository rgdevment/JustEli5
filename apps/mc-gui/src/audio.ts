// Reading the audio probe of `mc doctor --audio`: a verdict per channel and level meters.
import type { AudioReport, ChannelProbe } from "./api";
import { el } from "./dom";

export type State = "ok" | "warn" | "bad";

export interface Verdict {
  state: State;
  text: string;
}

/** Exact zeros: macOS hands them over for a denied permission, Windows for a muted device. */
const DIGITAL_SILENCE_DB = -99;
const QUIET_DB = -50;

export function verdict(channel: ChannelProbe, kind: "mic" | "system"): Verdict {
  if (channel.status !== "ok") {
    return { state: "bad", text: `No se pudo abrir: ${channel.error ?? channel.status}` };
  }
  const peak = channel.peak_db ?? -100;
  if (peak <= DIGITAL_SILENCE_DB) {
    return kind === "mic"
      ? { state: "warn", text: "Silenciado: llega silencio absoluto (¿micrófono silenciado o sin permiso?)" }
      : { state: "warn", text: "Sin señal: no sonaba nada (pon a sonar la reunión mientras pruebas)" };
  }
  if (peak < QUIET_DB) {
    return { state: "warn", text: `Señal muy baja (${peak.toFixed(0)} dB)` };
  }
  return { state: "ok", text: `Con señal (pico ${peak.toFixed(0)} dB)` };
}

/** -60 dB and below is an empty bar, 0 dB a full one. */
function percent(db: number | null): number {
  return Math.round(Math.min(100, Math.max(0, ((db ?? -100) + 60) / 60) * 100));
}

function meter(title: string, channel: ChannelProbe, kind: "mic" | "system"): HTMLElement {
  const v = verdict(channel, kind);
  const fill = el("div");
  fill.style.width = `${percent(channel.peak_db)}%`;
  return el(
    "div",
    {},
    el(
      "div",
      { className: "meter-head" },
      el("strong", { textContent: title }),
      el("span", { className: v.state === "ok" ? "" : v.state, textContent: v.text }),
    ),
    el("div", { className: `bar ${v.state}` }, fill),
    el("small", { className: "muted", textContent: channel.device ?? "" }),
  );
}

export function renderProbe(target: HTMLElement, report: AudioReport): void {
  target.replaceChildren();
  const probe = report.probe;
  if (!probe) {
    target.append(el("p", { className: "bad", textContent: "No hay audio en este sistema." }));
    return;
  }
  target.append(meter("Micrófono", probe.mic, "mic"), meter("Audio de la reunión", probe.system, "system"));
  if (probe.echo !== null && probe.echo > 0.75) {
    target.append(
      el("p", { className: "warn", textContent: "El micrófono oye la reunión (eco): usa audífonos." }),
    );
  }
  for (const note of probe.notes) {
    target.append(el("small", { className: "muted", textContent: note }));
  }
}
