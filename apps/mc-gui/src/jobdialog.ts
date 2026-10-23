// The confirmation-and-output dialog for anything that installs or downloads.
import { newJobId, runJob } from "./api";
import { byId, show } from "./dom";

export interface JobRequest {
  title: string;
  text: string;
  /** What will run, shown before asking. */
  command: string;
  start: (id: string) => Promise<void>;
}

/** Asks first, then runs and shows the output; resolves true when the job ended well. */
export function confirmAndRun(request: JobRequest): Promise<boolean> {
  const dialog = byId<HTMLDialogElement>("job-dialog");
  const output = byId<HTMLPreElement>("job-output");
  const run = byId<HTMLButtonElement>("job-run");
  const close = byId<HTMLButtonElement>("job-close");
  byId("job-title").textContent = request.title;
  byId("job-text").textContent = request.text;
  byId("job-command").textContent = request.command;
  output.textContent = "";
  show(output, false);
  show(run, true);
  run.disabled = false;
  close.disabled = false;
  close.textContent = "Cancelar";
  dialog.showModal();

  return new Promise((resolve) => {
    let succeeded = false;
    close.onclick = () => {
      dialog.close();
      resolve(succeeded);
    };
    // Escape behaves like the close button, and never while the job runs.
    dialog.oncancel = (event) => {
      event.preventDefault();
      if (!close.disabled) close.click();
    };
    run.onclick = async () => {
      run.disabled = true;
      close.disabled = true;
      show(output, true);
      const append = (line: string) => {
        output.textContent += line + "\n";
        output.scrollTop = output.scrollHeight;
      };
      const id = newJobId("job");
      try {
        const code = await runJob(id, () => request.start(id), append);
        succeeded = code === 0;
        append(succeeded ? "Terminado." : `Terminó con el código ${code ?? "desconocido"}.`);
      } catch (err) {
        append(`No se pudo ejecutar: ${err}`);
      }
      show(run, false);
      close.disabled = false;
      close.textContent = "Cerrar";
    };
  });
}
