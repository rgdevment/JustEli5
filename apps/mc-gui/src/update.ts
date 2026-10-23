// Updates: a look at the release feed when the window opens, and an install only when asked,
// so a recording in progress is never cut by a restart.
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { byId, el, show } from "./dom";

export async function offerUpdate(): Promise<void> {
  // Offline, or no release published yet: nothing to offer.
  const update = await check().catch(() => null);
  if (!update) return;
  const bar = byId("update");
  const state = el("small", { className: "muted" });
  const install = el("button", { textContent: "Actualizar y reiniciar", type: "button" });
  install.onclick = async () => {
    install.disabled = true;
    let total = 0;
    let done = 0;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") total = event.data.contentLength ?? 0;
        if (event.event === "Progress") done += event.data.chunkLength;
        state.textContent = total ? `Descargando… ${Math.round((done / total) * 100)} %` : "Descargando…";
      });
      await relaunch();
    } catch (err) {
      state.textContent = `No se pudo actualizar: ${err instanceof Error ? err.message : err}`;
      state.className = "bad";
      install.disabled = false;
    }
  };
  bar.replaceChildren(el("span", { textContent: `JustEli5 ${update.version} está disponible.` }), install, state);
  show(bar, true);
}
