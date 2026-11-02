// macOS privacy permissions, read from `eli5 doctor`, for the checklist and the wizard.
import type { Check, Doctor } from "./api";
import type { Action, Row } from "./checks";

export const isMac = navigator.userAgent.includes("Mac");

interface Permission {
  name: string;
  label: string;
  /** The settings page `open_settings` knows. */
  page: string;
  /** Needed only when audio is recorded. */
  audio: boolean;
  missing: string;
}

const PERMISSIONS: Permission[] = [
  {
    name: "accessibility",
    label: "Permiso de Accesibilidad",
    page: "accessibility",
    audio: false,
    missing: "Sin él no se leen los subtítulos. Actívalo para JustEli5 en Ajustes del Sistema > Privacidad y seguridad > Accesibilidad.",
  },
  {
    name: "microphone",
    label: "Permiso de Micrófono",
    page: "microphone",
    audio: true,
    missing: "Sin él macOS graba silencio sin avisar. Actívalo para JustEli5 en Ajustes del Sistema > Privacidad y seguridad > Micrófono.",
  },
  {
    name: "screen_recording",
    label: "Permiso de Grabación de pantalla",
    page: "screen",
    audio: true,
    missing:
      "En esta versión de macOS el audio de la reunión se graba con él. Actívalo para JustEli5 en Ajustes del Sistema > Privacidad y seguridad > Grabación de pantalla y audio del sistema.",
  },
];

/** Shows macOS's own prompt for one permission; for the microphone eli5 waits for the answer. */
export function askArgs(name: string): string[] {
  return ["doctor", "--ask-permission", name];
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

/** Whether doctor reported a permission still to grant, so coming back to the window is worth a re-read. */
export function permissionMissing(doctor: Doctor | Error | undefined): boolean {
  if (!doctor || doctor instanceof Error) return false;
  return doctor.checks.some((c) => PERMISSIONS.some((p) => p.name === c.name) && c.status !== "ok");
}

/** Whether doctor reported Accessibility at all (an older eli5 does not). */
export function reportsAccessibility(doctor: Doctor | Error | undefined): boolean {
  return Boolean(doctor && !(doctor instanceof Error) && doctor.checks.some((c) => c.name === "accessibility"));
}

/** One row per permission doctor reported (none off macOS); the audio ones only when audio is recorded. */
export function permissionRows(
  doctor: Doctor,
  recordAudio: boolean,
  ask: (name: string) => void,
  open: (page: string) => void,
): Row[] {
  return PERMISSIONS.flatMap((permission) => {
    const check = doctor.checks.find((c) => c.name === permission.name);
    if (!check || (permission.audio && !recordAudio)) return [];
    const ok = check.status === "ok";
    // An ok check not granted is Screen Recording kept for when the Core Audio tap fails: nothing to do.
    if (ok && check.state !== "granted") return [];
    const actions: Action[] = ok
      ? []
      : [
          { label: "Pedir permiso", run: () => ask(permission.name) },
          { label: "Abrir ajustes", run: () => open(permission.page) },
        ];
    return [{ label: permission.label, state: ok ? "ok" : "bad", detail: detail(permission, check), actions }];
  });
}
