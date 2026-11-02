// macOS privacy permissions, read from `eli5 doctor`, for the checklist and the wizard.
import { type Check, type Doctor, findCheck, mcJson } from "./api";
import type { Action, Row } from "./checks";

export const isMac = navigator.userAgent.includes("Mac");

interface Permission {
  name: string;
  label: string;
  /** The settings page `open_settings` knows. */
  page: string;
  /** Needed only when audio is recorded. */
  audio: boolean;
  /** When macOS shows its prompt besides never-asked: on every request, on the first only, or never. */
  asks: "always" | "once" | "never";
  missing: string;
}

const PERMISSIONS: Permission[] = [
  {
    name: "accessibility",
    label: "Permiso de Accesibilidad",
    page: "accessibility",
    audio: false,
    asks: "always",
    missing: "Sin él no se leen los subtítulos. Actívalo para JustEli5 en Ajustes del Sistema > Privacidad y seguridad > Accesibilidad.",
  },
  {
    name: "microphone",
    label: "Permiso de Micrófono",
    page: "microphone",
    audio: true,
    asks: "never",
    missing: "Sin él macOS graba silencio sin avisar. Actívalo para JustEli5 en Ajustes del Sistema > Privacidad y seguridad > Micrófono.",
  },
  {
    name: "screen_recording",
    label: "Permiso de Grabación de pantalla",
    page: "screen",
    audio: true,
    // macOS only tells granted from missing here, so asking is offered once: it prompts only if it never did.
    asks: "once",
    missing:
      "En esta versión de macOS el audio de la reunión se graba con él. Actívalo para JustEli5 en Ajustes del Sistema > Privacidad y seguridad > Grabación de pantalla y audio del sistema.",
  },
];

/** The permissions being asked for now, and those already asked for in this window. */
export interface Asks {
  asking: Set<string>;
  asked: Set<string>;
}

export const newAsks = (): Asks => ({ asking: new Set(), asked: new Set() });

/** Shows macOS's own prompt (eli5 waits for the microphone answer) and returns doctor's report after it. */
export async function askFor(asks: Asks, name: string, redraw: () => void): Promise<Doctor | null> {
  if (asks.asking.has(name)) return null;
  asks.asking.add(name);
  redraw();
  const doctor = await mcJson<Doctor>(["doctor", "--ask-permission", name]).catch(() => null);
  asks.asking.delete(name);
  asks.asked.add(name);
  return doctor;
}

const isPermission = (name: string) => PERMISSIONS.some((p) => p.name === name);

/** `base` with the permission lines of `fresh`, which is newer. */
export function withPermissions(base: Doctor | Error | undefined, fresh: Doctor): Doctor {
  if (!base || base instanceof Error) return fresh;
  const others = base.checks.filter((c) => !isPermission(c.name));
  return { ...base, checks: [...others, ...fresh.checks.filter((c) => isPermission(c.name))] };
}

function detail(permission: Permission, check: Check): string {
  switch (check.state) {
    case "granted":
      return "Concedido";
    case "not_asked":
      return `macOS aún no lo ha pedido. ${permission.missing}`;
    case "restricted":
      return "Bloqueado por un perfil o el control parental: pídeselo a quien administra este Mac.";
    default:
      return permission.missing;
  }
}

/** The permissions doctor reported that matter now; an ok one not granted is Screen Recording kept for a failed tap. */
function relevant(doctor: Doctor | Error | undefined, recordAudio: boolean): [Permission, Check][] {
  return PERMISSIONS.flatMap((permission): [Permission, Check][] => {
    const check = findCheck(doctor, permission.name);
    if (!check || (permission.audio && !recordAudio)) return [];
    if (check.status === "ok" && check.state !== "granted") return [];
    return [[permission, check]];
  });
}

/** Whether a permission that matters is still missing, so coming back to the window is worth a re-read. */
export function permissionMissing(doctor: Doctor | Error | undefined, recordAudio: boolean): boolean {
  return relevant(doctor, recordAudio).some(([, check]) => check.status !== "ok");
}

function actions(
  permission: Permission,
  check: Check,
  asked: ReadonlySet<string>,
  ask: (name: string) => void,
  open: (page: string) => void,
): Action[] {
  if (check.status === "ok" || check.state === "restricted") return [];
  const settings = { label: "Abrir ajustes", run: () => open(permission.page) };
  const askable =
    check.state === "not_asked" || permission.asks === "always" || (permission.asks === "once" && !asked.has(permission.name));
  return askable ? [{ label: "Pedir permiso", run: () => ask(permission.name) }, settings] : [settings];
}

/** One row per permission that matters (none off macOS); one being asked for waits for the answer. */
export function permissionRows(
  doctor: Doctor,
  recordAudio: boolean,
  asks: Asks,
  ask: (name: string) => void,
  open: (page: string) => void,
): Row[] {
  return relevant(doctor, recordAudio).map(([permission, check]) => {
    if (asks.asking.has(permission.name)) {
      return { label: permission.label, state: "warn", detail: "Esperando tu respuesta en el aviso de macOS…", actions: [] };
    }
    const ok = check.status === "ok";
    return { label: permission.label, state: ok ? "ok" : "bad", detail: detail(permission, check), actions: actions(permission, check, asks.asked, ask, open) };
  });
}
