// Tisty, where the minutes are kept: whether it is here, and the recommendation when it is not.
import { type Doctor, findCheck, openPath } from "./api";

const TISTY_URL = "https://www.tisty.app/";

export const TISTY_PITCH =
  "Tisty guarda cada minuta como una página del documento de su serie de reuniones y convierte tus compromisos en tareas, todo en archivos en tu equipo.";

/** missing: not installed; closed: installed, but its server does not answer (`doctor --tisty` only). */
export type TistyState = "ready" | "closed" | "missing" | "unknown";

export function tistyState(doctor: Doctor | Error | undefined): TistyState {
  const app = findCheck(doctor, "tisty");
  if (!app) return "unknown";
  if (app.status !== "ok") return "missing";
  const server = findCheck(doctor, "tisty_mcp");
  if (!server) return "unknown";
  return server.status === "ok" ? "ready" : "closed";
}

export const openTistySite = () => void openPath(TISTY_URL).catch((err) => alert(err));
