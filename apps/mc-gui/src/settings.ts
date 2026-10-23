// What the window remembers between runs.
import { settingsLoad, settingsSave } from "./api";

export interface Settings {
  name: string;
  platform: string;
  micDevice: string;
  systemDevice: string;
  recordAudio: boolean;
  liveAsr: boolean;
  /** Whisper over the whole recording after the meeting: hours on a CPU, so off unless asked. */
  finalPass: boolean;
  analyst: boolean;
  tisty: boolean;
  context: string;
  recentNames: string[];
  liveModel: string;
  finalModel: string;
  /** The first-run wizard was completed (it can be opened again from Configuración). */
  setupDone: boolean;
}

export const DEFAULTS: Settings = {
  name: "",
  platform: "teams",
  micDevice: "",
  systemDevice: "",
  recordAudio: true,
  liveAsr: false,
  finalPass: false,
  analyst: true,
  tisty: false,
  context: "",
  recentNames: [],
  liveModel: "whisper-base",
  finalModel: "whisper-large-v3-turbo-q5_0",
  setupDone: false,
};

export async function loadSettings(): Promise<Settings> {
  const stored = await settingsLoad().catch(() => null);
  return { ...DEFAULTS, ...(stored && typeof stored === "object" ? stored : {}) };
}

export function saveSettings(settings: Settings): Promise<void> {
  return settingsSave(settings);
}

export function withRecentName(settings: Settings, name: string): string[] {
  const others = settings.recentNames.filter((n) => n !== name);
  return name ? [name, ...others].slice(0, 15) : others;
}
