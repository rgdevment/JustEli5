// Typed calls to the Rust side and to `mc --json`.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface RunOutput {
  code: number | null;
  stdout: string;
  stderr: string;
}

export interface Check {
  name: string;
  status: "ok" | "warn" | "FAIL";
  detail: string;
}

export interface Device {
  name: string;
  direction: "input" | "output";
  default: boolean;
}

export interface ChannelProbe {
  status: string;
  device: string | null;
  peak_db: number | null;
  rms_db: number | null;
  error: string | null;
}

export interface AudioReport {
  available: boolean;
  devices?: Device[];
  probe?: { mic: ChannelProbe; system: ChannelProbe; echo: number | null; notes: string[] };
}

export interface Doctor {
  checks: Check[];
  ok: boolean;
  tisty_target?: { configured: boolean; folder: string; list: string | null };
  audio?: AudioReport;
}

export interface Model {
  name: string;
  installed: boolean;
  description: string;
}

export interface DetectedPlatform {
  platform: string;
  app: string;
  captions_on: boolean;
  captions: number;
  error: string | null;
}

export interface Detect {
  platforms: DetectedPlatform[];
  advice: string[];
}

export interface Prereq {
  id: string;
  label: string;
  found: boolean;
  install: string | null;
  manual: string;
  elevates: boolean;
}

export interface Agents {
  claude: string | null;
  copilot: string | null;
  anthropic_key: boolean;
  openai_key: boolean;
  ollama: boolean;
  lmstudio: boolean;
}

export interface ConfigFile {
  path: string | null;
  exists: boolean;
  value: Record<string, unknown>;
  error: string | null;
}

/** The message of a failed `mc` call: its JSON error (with the hint), else its stderr. */
export function errorText(out: RunOutput): string {
  for (const line of out.stderr.split("\n").reverse()) {
    try {
      const err = JSON.parse(line).error;
      if (err) return err.hint ? `${err.message} (${err.hint})` : err.message;
    } catch {
      // Not JSON: plain text below.
    }
  }
  return out.stderr.trim() || `eli5 terminó con el código ${out.code}`;
}

export const locateMc = () => invoke<string | null>("mc_locate");
export const runMc = (args: string[]) => invoke<RunOutput>("mc_run", { args });

/** One JSON document from `mc --json …`; negative results (exit 1) still carry one. */
export async function mcJson<T>(args: string[]): Promise<T> {
  const out = await runMc(["--json", ...args]);
  const text = out.stdout.trim();
  if (!text || (out.code !== 0 && out.code !== 1)) throw new Error(errorText(out));
  return JSON.parse(text.split("\n").pop() as string) as T;
}

interface JobLine {
  id: string;
  stream: "stdout" | "stderr";
  line: string;
}

interface JobExit {
  id: string;
  code: number | null;
}

export function newJobId(kind: string): string {
  return `${kind}-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
}

/** Runs a job started by `start` and resolves with its exit code; `onLine` sees its output. */
export async function runJob(
  id: string,
  start: () => Promise<void>,
  onLine: (line: string, stream: JobLine["stream"]) => void,
): Promise<number | null> {
  const stopLines = await listen<JobLine>("job-line", (e) => {
    if (e.payload.id === id) onLine(e.payload.line, e.payload.stream);
  });
  let resolveExit: (code: number | null) => void = () => {};
  const exited = new Promise<number | null>((resolve) => (resolveExit = resolve));
  const stopExit = await listen<JobExit>("job-exit", (e) => {
    if (e.payload.id === id) resolveExit(e.payload.code);
  });
  try {
    await start();
    return await exited;
  } finally {
    stopLines();
    stopExit();
  }
}

export const spawnMc = (id: string, args: string[], stdin = false) =>
  invoke<void>("mc_spawn", { id, args, stdin });
export const jobWrite = (id: string, line: string) => invoke<void>("job_write", { id, line });
export const prereqs = () => invoke<Prereq[]>("prereqs");
export const prereqInstall = (id: string, tool: string) =>
  invoke<void>("prereq_install", { id, tool });
export const agentsDetect = () => invoke<Agents>("agents_detect");
export const mcSource = () => invoke<string | null>("mc_source");
export const mcInstall = (id: string) => invoke<void>("mc_install", { id });
export const openPath = (path: string) => invoke<void>("open_path", { path });
export const openSettings = (page: string) => invoke<void>("open_settings", { page });
export const settingsLoad = () => invoke<unknown>("settings_load");
export const settingsSave = (value: unknown) => invoke<void>("settings_save", { value });
export const contextWrite = (text: string) => invoke<string | null>("context_write", { text });
/** The participant notes the window kept before the memory existed, when there is a file. */
export const peopleFile = () => invoke<string | null>("people_file");
export const noteImageSave = (sessionDir: string, ext: string, bytes: number[]) =>
  invoke<string>("note_image_save", { sessionDir, ext, bytes });
export const configLoad = () => invoke<ConfigFile>("config_load");
export const configCreate = () => invoke<string>("config_create");
export const configSave = (value: unknown) => invoke<string>("config_save", { value });
