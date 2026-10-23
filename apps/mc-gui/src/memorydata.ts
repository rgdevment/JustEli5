// The memory between meetings as `eli5 memory --json` hands it over, and the calls that change it.
import { mcJson } from "./api";

export type Kind = "term" | "person";
export type State = "proposed" | "confirmed";

export interface Form {
  text: string;
  state: State;
}

export interface Role {
  role: string;
  since_ms: number;
  state: State;
}

export interface Entry {
  id: number;
  kind: Kind;
  name: string;
  meaning: string | null;
  state: State;
  forms: Form[];
  roles: Role[];
  meetings: number;
}

export interface Sighting {
  session: string;
  series: string | null;
  at_ms: number;
  attended: boolean;
}

export interface Filter {
  query?: string;
  kind?: Kind | "";
  state?: State | "";
}

/** The role the user confirmed last, else the newest one proposed (as `Concept::role`). */
export function roleOf(entry: Entry): string | null {
  return (entry.roles.find((r) => r.state === "confirmed") ?? entry.roles[0])?.role ?? null;
}

export async function listEntries(filter: Filter = {}): Promise<Entry[]> {
  const args = ["memory", "list"];
  if (filter.query?.trim()) args.push("--query", filter.query.trim());
  if (filter.kind) args.push("--kind", filter.kind);
  if (filter.state) args.push("--state", filter.state);
  return (await mcJson<{ entries: Entry[] }>(args)).entries;
}

export async function showEntry(id: number): Promise<{ entry: Entry; meetings: Sighting[] }> {
  return mcJson(["memory", "show", String(id)]);
}

/** Runs `eli5 memory <args>` and hands back the entry it left, when it names one. */
export async function change(args: string[]): Promise<Entry | null> {
  return (await mcJson<{ entry?: Entry }>(["memory", ...args])).entry ?? null;
}

/** The entry of `kind` that `text` names here, when the memory can tell. */
export async function resolve(kind: Kind, text: string): Promise<number | null> {
  const found = await mcJson<{ resolution: string; id?: number }>(["memory", "resolve", kind, text]);
  return found.resolution === "known" && typeof found.id === "number" ? found.id : null;
}
