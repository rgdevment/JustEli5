// Ties the views together: the checks behind the checklist, the fixes, and a meeting from start to minutes.
import {
  type AudioReport,
  type Detect,
  type Doctor,
  type Model,
  contextWrite,
  locateMc,
  mcInstall,
  mcJson,
  mcSource,
  openPath,
  peopleFile,
  runMc,
  openSettings,
  configCreate,
  configLoad,
  prereqInstall,
  prereqs,
  spawnMc,
  type Prereq,
} from "./api";
import { renderProbe } from "./audio";
import { runCapture } from "./capture";
import { type Fixes, type Snapshot, renderPrereqs, renderRows, rows } from "./checks";
import { type ConfigView, configView } from "./config";
import { byId, show } from "./dom";
import { confirmAndRun } from "./jobdialog";
import { LiveMemory } from "./livememory";
import { MemoryView } from "./memory";
import { platformLabel } from "./platforms";
import { runPost } from "./post";
import * as form from "./prepare";
import { type Settings, saveSettings, withRecentName } from "./settings";
import { runSetup } from "./setup";
import { mountVocabulary } from "./vocabulary";

type MeetingView = "prepare" | "capture" | "post";

const AUDIO_TEST_SECONDS = 10;

function failure(err: unknown): Error {
  return err instanceof Error ? err : new Error(String(err));
}

export class App {
  private snapshot: Snapshot = { mcPath: null, mcSource: null };
  private models: Model[] = [];
  private meetingView: MeetingView = "prepare";
  private readonly config: ConfigView;
  private readonly memory = new MemoryView();

  constructor(private settings: Settings) {
    this.config = configView(
      () => this.settings,
      () => this.models,
      (saved) => {
        this.settings = saved;
        this.refresh();
      },
    );
  }

  init(): void {
    form.writeForm(this.settings);
    this.bindForm();
    this.bindTabs();
    byId("recheck").onclick = () => this.refresh();
    byId("detect").onclick = () => this.detect();
    byId("audio-test").onclick = () => this.testAudio();
    byId("start").onclick = () => this.startMeeting();
    mountVocabulary([byId("vocab-prepare")]);
    this.memory.mount();
    this.adoptPeople();
    byId("open-setup").onclick = () => this.openSetup();
    byId("new-meeting").onclick = () => {
      this.showMeeting("prepare");
      this.refresh();
    };
    if (this.settings.setupDone) this.refresh();
    else this.openSetup();
  }

  /** The participant notes of older versions go into the memory, once; the memory keeps the mark. */
  private async adoptPeople(): Promise<void> {
    const file = await peopleFile().catch(() => null);
    if (file) await runMc(["memory", "adopt-people", file]).catch(() => {});
  }

  /** The Memoria tab, showing what `query` finds. */
  private openMemory(query: string): void {
    this.showTab("memory");
    this.memory.open(query);
  }

  /** The first-run wizard, also reachable from Configuración; the tabs come back when it closes. */
  async openSetup(): Promise<void> {
    show(byId("tabs"), false);
    for (const view of ["prepare", "capture", "post", "config", "memory"]) show(byId(`view-${view}`), false);
    show(byId("view-setup"), true);
    // The wizard calls mc, so it needs to know where mc is first.
    await locateMc();
    this.settings = await runSetup(this.settings);
    await saveSettings(this.settings);
    form.writeForm(this.settings);
    show(byId("view-setup"), false);
    show(byId("tabs"), true);
    this.showMeeting("prepare");
    this.refresh();
  }

  private bindForm(): void {
    const changed = () => {
      const before = this.settings;
      this.settings = form.readForm(before);
      form.syncAudioControls(this.settings.recordAudio);
      saveSettings(this.settings);
      if (this.settings.tisty && !before.tisty) this.loadDoctor();
      this.render();
    };
    for (const id of ["record-audio", "live-asr", "final-pass", "analyst", "tisty", "name", "context"]) {
      byId(id).addEventListener("change", changed);
    }
    for (const id of ["mic-device", "system-device"]) {
      byId(id).addEventListener("change", () => {
        changed();
        this.probeAudio(1);
      });
    }
  }

  private bindTabs(): void {
    for (const tab of document.querySelectorAll<HTMLButtonElement>(".tab")) {
      tab.onclick = () => this.showTab(tab.dataset.tab as string);
    }
  }

  private showTab(tab: string): void {
    for (const t of document.querySelectorAll<HTMLButtonElement>(".tab")) {
      t.classList.toggle("active", t.dataset.tab === tab);
    }
    show(byId("view-config"), tab === "config");
    show(byId("view-memory"), tab === "memory");
    for (const view of ["prepare", "capture", "post"] as MeetingView[]) {
      show(byId(`view-${view}`), tab === "meeting" && view === this.meetingView);
    }
    if (tab === "config") this.config.reload();
    if (tab === "memory") this.memory.open();
  }

  private showMeeting(view: MeetingView): void {
    this.meetingView = view;
    this.showTab("meeting");
  }

  private fixes(): Fixes {
    return {
      installMc: () => this.installMc(),
      createConfig: () => configCreate().then(() => this.refresh()),
      openConfig: () => this.showTab("config"),
      pullModel: (name) => this.pullModel(name),
      testAudio: () => this.testAudio(),
      openSettings: (page) => openSettings(page).catch((err) => alert(err)),
      redetect: () => this.detect(),
    };
  }

  private render(): void {
    const s = this.snapshot;
    renderRows(byId("checks"), rows(s, this.settings, this.fixes()));
    form.renderPlatforms(this.settings.platform, s.detect instanceof Error ? undefined : s.detect, (id) => {
      this.settings = { ...this.settings, platform: id };
      saveSettings(this.settings);
      this.render();
    });
    byId("detected").textContent = form.detectedText(s.detect);
    const configBroken =
      s.doctor instanceof Error || Boolean(s.doctor?.checks.some((c) => c.name === "config" && c.status === "FAIL"));
    byId<HTMLButtonElement>("start").disabled = !s.mcPath || configBroken;
  }

  /** Re-reads everything the checklist shows; each part re-renders as it arrives. */
  async refresh(): Promise<void> {
    this.snapshot = { mcPath: await locateMc(), mcSource: await mcSource() };
    this.render();
    show(byId("prereqs"), !this.snapshot.mcPath);
    if (!this.snapshot.mcPath) {
      this.loadPrereqs();
      return;
    }
    this.loadDoctor();
    this.loadModels();
    this.loadNames();
    this.detect();
    this.probeAudio(1);
    this.loadConfig();
  }

  private async loadConfig(): Promise<void> {
    this.snapshot.config = await configLoad();
    this.render();
  }

  private async loadPrereqs(): Promise<void> {
    renderPrereqs(byId("prereq-list"), await prereqs(), (p) => this.installPrereq(p));
  }

  private async loadDoctor(): Promise<void> {
    const args = this.settings.tisty ? ["doctor", "--tisty"] : ["doctor"];
    this.snapshot.doctor = await mcJson<Doctor>(args).catch(failure);
    if (!(this.snapshot.doctor instanceof Error)) form.setTistyTarget(this.snapshot.doctor.tisty_target);
    this.render();
  }

  private async loadModels(): Promise<void> {
    const listed = await mcJson<{ models: Model[] }>(["models", "list"]).catch(failure);
    this.snapshot.models = listed instanceof Error ? listed : listed.models;
    this.models = listed instanceof Error ? [] : listed.models;
    this.render();
  }

  private async loadNames(): Promise<void> {
    const status = await mcJson<{ sessions: { meeting_name: string | null }[] }>(["status"]).catch(() => null);
    const fromSessions = (status?.sessions ?? []).map((s) => s.meeting_name ?? "").filter(Boolean);
    form.fillNames([...new Set([...this.settings.recentNames, ...fromSessions])]);
  }

  async detect(): Promise<void> {
    this.snapshot.detect = undefined;
    this.render();
    const detect = await mcJson<Detect>(["detect"]).catch(failure);
    this.snapshot.detect = detect;
    if (!(detect instanceof Error)) {
      this.settings = { ...this.settings, platform: form.preferredPlatform(this.settings.platform, detect) };
    }
    this.render();
  }

  private audioArgs(seconds: number): string[] {
    const args = ["doctor", "--audio", "--audio-seconds", String(seconds)];
    if (this.settings.micDevice) args.push("--mic-device", this.settings.micDevice);
    if (this.settings.systemDevice) args.push("--system-device", this.settings.systemDevice);
    return args;
  }

  private async probeAudio(seconds: number): Promise<AudioReport | Error> {
    const doctor = await mcJson<Doctor>(this.audioArgs(seconds)).catch(failure);
    const audio = doctor instanceof Error ? doctor : (doctor.audio ?? new Error("eli5 no devolvió el audio"));
    this.snapshot.audio = audio;
    if (!(audio instanceof Error)) form.fillDevices(audio.devices ?? [], this.settings);
    this.render();
    return audio;
  }

  private async testAudio(): Promise<void> {
    const button = byId<HTMLButtonElement>("audio-test");
    const state = byId("audio-test-state");
    const result = byId("audio-test-result");
    button.disabled = true;
    let left = AUDIO_TEST_SECONDS;
    state.textContent = `Escuchando… habla y pon a sonar la reunión (${left} s)`;
    const tick = setInterval(() => {
      left = Math.max(0, left - 1);
      state.textContent = left ? `Escuchando… habla y pon a sonar la reunión (${left} s)` : "Analizando…";
    }, 1000);
    const audio = await this.probeAudio(AUDIO_TEST_SECONDS);
    clearInterval(tick);
    button.disabled = !this.settings.recordAudio;
    state.textContent = audio instanceof Error ? audio.message : "";
    show(result, !(audio instanceof Error));
    if (!(audio instanceof Error)) renderProbe(result, audio);
  }

  private async pullModel(name: string): Promise<void> {
    const done = await confirmAndRun({
      title: `Descargar ${name}`,
      text: "Se descarga una vez y se comprueba su sha256. Puede tardar unos minutos.",
      command: `eli5 models pull ${name}`,
      start: (id) => spawnMc(id, ["models", "pull", name]),
    });
    if (done) this.loadModels();
  }

  private async installMc(): Promise<void> {
    const source = this.snapshot.mcSource ?? "";
    const done = await confirmAndRun({
      title: "Compilar e instalar eli5",
      text: `Compila eli5 con Whisper desde ${source} y lo deja en ~/.cargo/bin. Tarda varios minutos.`,
      command: "cargo install --locked --path crates/mc-cli --features whisper",
      start: (id) => mcInstall(id),
    });
    if (done) this.refresh();
  }

  private async installPrereq(p: Prereq): Promise<void> {
    const elevation = p.elevates
      ? " El instalador pedirá permisos de administrador: el sistema te lo preguntará."
      : "";
    const done = await confirmAndRun({
      title: `Instalar ${p.label}`,
      text: `Se instala con el gestor de paquetes del sistema.${elevation}`,
      command: p.install ?? p.manual,
      start: (id) => prereqInstall(id, p.id),
    });
    if (done) this.loadPrereqs();
  }

  private captureArgs(contextFile: string | null): string[] {
    const s = this.settings;
    const args = ["--source", s.platform, "--consent", "--stdin-commands"];
    if (s.name) args.push("--name", s.name);
    if (s.recordAudio) {
      args.push("--audio", "--consent-audio");
      if (s.micDevice) args.push("--mic-device", s.micDevice);
      if (s.systemDevice) args.push("--system-device", s.systemDevice);
      if (s.liveAsr) args.push("--live-asr", "--asr-model", s.liveModel);
    }
    if (s.analyst) {
      args.push("--analyst");
      if (contextFile) args.push("--context", contextFile);
    }
    return args;
  }

  private async startMeeting(): Promise<void> {
    const error = byId("start-error");
    show(error, false);
    this.settings = form.readForm(this.settings);
    this.settings.recentNames = withRecentName(this.settings, this.settings.name);
    await saveSettings(this.settings);
    const contextFile = await contextWrite(this.settings.context);
    const title = `${this.settings.name || "Reunión"} · ${platformLabel(this.settings.platform)}`;

    this.showMeeting("capture");
    const people = new LiveMemory();
    people.mount((query) => this.openMemory(query));
    const capture = await runCapture(this.captureArgs(contextFile), title, people);
    if (!capture.sessionDir) {
      this.showMeeting("prepare");
      error.textContent = capture.error ?? "No se grabó ninguna reunión.";
      show(error, true);
      return;
    }

    this.showMeeting("post");
    show(byId("post-done"), false);
    // Who each participant is now lives in the memory, which the minutes read on their own.
    const post = await runPost({
      sessionDir: capture.sessionDir,
      finalPass: this.settings.recordAudio && this.settings.finalPass,
      finalModel: this.settings.finalModel,
      contextFile,
      tisty: this.settings.tisty,
    });
    this.showResult(capture.sessionDir, post.minutes, post.ambiguous);
  }

  private showResult(sessionDir: string, minutes: string | null, ambiguous: string[]): void {
    show(byId("ambiguous"), ambiguous.length > 0);
    byId("ambiguous-list").textContent = ambiguous.join(", ");
    byId("ambiguous-open").onclick = () => this.openMemory(ambiguous[0] ?? "");
    show(byId("post-done"), true);
    byId("minutes-path").textContent = minutes ?? "no se pudo escribir la minuta (mira el paso que falló)";
    const open = byId<HTMLButtonElement>("open-minutes");
    open.disabled = !minutes;
    open.onclick = () => minutes && openPath(minutes);
    byId("open-session").onclick = () => openPath(sessionDir);
  }
}
