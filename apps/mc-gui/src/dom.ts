// Small DOM helpers shared by the views.

export function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (!found) throw new Error(`#${id} is missing from index.html`);
  return found as T;
}

type Child = Node | string | null | undefined | false;

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> & { className?: string } = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const node = Object.assign(document.createElement(tag), props);
  for (const child of children) {
    if (child) node.append(child);
  }
  return node;
}

export function show(node: HTMLElement, visible: boolean): void {
  node.classList.toggle("hidden", !visible);
}

/** `mm:ss` or `h:mm:ss`. */
export function clock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const [h, m, s] = [Math.floor(total / 3600), Math.floor((total % 3600) / 60), total % 60];
  const pad = (n: number) => String(n).padStart(2, "0");
  return h ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

const VERBATIM = "\\\\?\\";
const VERBATIM_UNC = "\\\\?\\UNC\\";

/** A Windows path without the `\\?\` prefix canonical paths carry, which other tools refuse. */
export function plainPath(path: string): string {
  if (path.startsWith(VERBATIM_UNC)) return "\\\\" + path.slice(VERBATIM_UNC.length);
  return path.startsWith(VERBATIM) ? path.slice(VERBATIM.length) : path;
}

/** Joins a file name to a folder path written by `mc` (backslashes on Windows). */
export function joinPath(dir: string, name: string): string {
  const sep = dir.includes("\\") ? "\\" : "/";
  return dir.endsWith(sep) ? dir + name : dir + sep + name;
}
