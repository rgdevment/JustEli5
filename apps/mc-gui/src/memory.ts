// The Memoria tab: every term and person known between meetings, to search, correct, add, merge
// and confirm. What the user saves here is confirmed: the model never changes it.
import { byId, el, show } from "./dom";
import {
  type Entry,
  type Kind,
  type State,
  change,
  listEntries,
  roleOf,
  showEntry,
} from "./memorydata";

const KIND_LABEL: Record<Kind, string> = { term: "Término", person: "Persona" };

function failed(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export class MemoryView {
  private kind: Kind = "term";
  private entries: Entry[] = [];
  private selected = new Set<number>();
  private searching = 0;

  mount(): void {
    const reload = () => this.reload();
    byId("memory-query").oninput = reload;
    byId("memory-state").onchange = reload;
    byId("memory-terms").onclick = () => this.section("term");
    byId("memory-people").onclick = () => this.section("person");
    byId("memory-new").onclick = () => this.edit(null, this.kind);
    byId("memory-confirm").onclick = () => this.confirmSelected();
    byId("memory-merge").onclick = () => this.mergeSelected();
  }

  /** Terms and people live apart: one section at a time. */
  private section(kind: Kind): void {
    this.kind = kind;
    this.selected.clear();
    byId("memory-terms").classList.toggle("selected", kind === "term");
    byId("memory-people").classList.toggle("selected", kind === "person");
    byId("memory-new").textContent = kind === "term" ? "Nuevo término" : "Nueva persona";
    show(byId("memory-editor"), false);
    this.reload();
  }

  /** Shows the entries a search finds; `query` fills the search box first. */
  async open(query?: string): Promise<void> {
    if (query !== undefined) byId<HTMLInputElement>("memory-query").value = query;
    await this.reload();
  }

  private say(text: string, bad = false): void {
    const state = byId("memory-state-text");
    state.textContent = text;
    state.className = bad ? "bad" : "muted";
  }

  private async reload(): Promise<void> {
    const ask = ++this.searching;
    try {
      const entries = await listEntries({
        query: byId<HTMLInputElement>("memory-query").value,
        kind: this.kind,
        state: byId<HTMLSelectElement>("memory-state").value as State | "",
      });
      // A slower answer to an older search must not replace the newer one.
      if (ask !== this.searching) return;
      this.entries = entries;
      for (const id of [...this.selected]) if (!entries.some((e) => e.id === id)) this.selected.delete(id);
      this.render();
    } catch (err) {
      this.say(failed(err), true);
    }
  }

  private render(): void {
    const list = byId("memory-list");
    list.replaceChildren(...this.entries.map((entry) => this.row(entry)));
    if (!this.entries.length) list.append(el("li", { className: "muted", textContent: "Nada coincide." }));
    const picked = this.entries.filter((e) => this.selected.has(e.id));
    byId<HTMLButtonElement>("memory-confirm").disabled = !picked.some((e) => e.state === "proposed");
    byId<HTMLButtonElement>("memory-merge").disabled = picked.length !== 2 || picked[0].kind !== picked[1].kind;
  }

  private row(entry: Entry): HTMLLIElement {
    const pick = el("input", { type: "checkbox", checked: this.selected.has(entry.id) });
    pick.onchange = () => {
      if (pick.checked) this.selected.add(entry.id);
      else this.selected.delete(entry.id);
      this.render();
    };
    const role = roleOf(entry);
    const others = entry.forms.map((f) => f.text).filter((f) => f !== entry.name);
    const open = el("button", { className: "link entry-name", textContent: entry.name, type: "button" });
    open.onclick = () => this.edit(entry, entry.kind);
    return el(
      "li",
      { className: `memory-entry ${entry.state}` },
      pick,
      el(
        "div",
        {},
        open,
        role ? el("span", { className: "muted", textContent: ` (${role})` }) : null,
        entry.state === "proposed" ? el("span", { className: "memory-badge", textContent: "por revisar" }) : null,
        entry.meaning ? el("div", { className: "small", textContent: entry.meaning }) : null,
        others.length ? el("div", { className: "muted small", textContent: `también: ${others.join(", ")}` }) : null,
      ),
      el("small", {
        className: "muted",
        textContent: `${entry.meetings} ${entry.meetings === 1 ? "reunión" : "reuniones"}`,
      }),
    );
  }

  private async confirmSelected(): Promise<void> {
    const ids = this.entries.filter((e) => this.selected.has(e.id) && e.state === "proposed").map((e) => String(e.id));
    try {
      await change(["confirm", ...ids]);
      this.say(`${ids.length} confirmados.`);
      this.selected.clear();
      await this.reload();
    } catch (err) {
      this.say(failed(err), true);
    }
  }

  /** Two entries that are one: the user picks which name stays. */
  private mergeSelected(): void {
    const [a, b] = this.entries.filter((e) => this.selected.has(e.id));
    const state = byId("memory-state-text");
    const keep = (kept: Entry, gone: Entry) =>
      el("button", {
        type: "button",
        textContent: `Quedarse con «${kept.name}»`,
        onclick: async () => {
          try {
            await change(["merge", String(kept.id), String(gone.id)]);
            this.say(`«${gone.name}» ahora es parte de «${kept.name}».`);
            this.selected.clear();
            await this.reload();
          } catch (err) {
            this.say(failed(err), true);
          }
        },
      });
    state.className = "";
    state.replaceChildren("Unir: ", keep(a, b), " ", keep(b, a));
  }

  /** The editor of one entry, or of a new one of `kind`. */
  private async edit(entry: Entry | null, kind: Kind): Promise<void> {
    const box = byId("memory-editor");
    show(box, true);
    const name = el("input", { value: entry?.name ?? "", placeholder: kind === "term" ? "Término" : "Nombre" });
    const meaning = el("input", {
      value: entry?.meaning ?? "",
      placeholder: kind === "term" ? "Qué es" : "Nota (opcional)",
    });
    const role = el("input", { value: entry ? (roleOf(entry) ?? "") : "", placeholder: "Rol o cargo" });
    const forms = el("input", {
      placeholder: kind === "term" ? "Cómo se oye o escribe mal, separado por comas" : "Cómo aparece en Teams, separado por comas",
    });
    const state = el("small", { className: "muted" });
    const fields = [
      el("label", { className: "field" }, el("span", { textContent: kind === "term" ? "Término" : "Nombre" }), name),
      el("label", { className: "field" }, el("span", { textContent: kind === "term" ? "Qué es" : "Nota" }), meaning),
      kind === "person" ? el("label", { className: "field" }, el("span", { textContent: "Rol" }), role) : null,
      el("label", { className: "field" }, el("span", { textContent: "Otras formas" }), forms),
    ];
    const save = el("button", { className: "primary", type: "button", textContent: entry ? "Guardar y confirmar" : "Agregar" });
    save.onclick = async () => {
      if (!name.value.trim()) return name.focus();
      try {
        const saved = await this.save(entry, kind, name.value, meaning.value, role.value, forms.value);
        state.textContent = "Guardado.";
        await this.reload();
        if (saved) await this.edit(saved, saved.kind);
      } catch (err) {
        state.textContent = failed(err);
        state.className = "bad";
      }
    };
    const close = el("button", { type: "button", textContent: "Cerrar", onclick: () => show(box, false) });
    const actions = el("div", { className: "row" }, save, close, state);
    const parts: (HTMLElement | null)[] = [el("h2", { textContent: entry ? entry.name : `Nuevo ${KIND_LABEL[kind].toLowerCase()}` }), ...fields, actions];
    if (entry) {
      parts.push(this.formsOf(entry), this.removal(entry), el("h3", { textContent: "Reuniones" }), await this.meetingsOf(entry));
    }
    box.replaceChildren(...parts.filter((p): p is HTMLElement => p !== null));
    name.focus();
  }

  private async save(
    entry: Entry | null,
    kind: Kind,
    name: string,
    meaning: string,
    role: string,
    forms: string,
  ): Promise<Entry | null> {
    const extra = forms.split(",").map((f) => f.trim()).filter(Boolean);
    if (!entry) {
      const args = ["add", kind, name.trim()];
      if (meaning.trim()) args.push("--meaning", meaning.trim());
      if (extra.length) args.push("--forms", extra.join(","));
      if (kind === "person" && role.trim()) args.push("--role", role.trim());
      return change(args);
    }
    const args = ["edit", String(entry.id)];
    if (name.trim() !== entry.name) args.push("--name", name.trim());
    if (meaning.trim() && meaning.trim() !== entry.meaning) args.push("--meaning", meaning.trim());
    if (kind === "person" && role.trim() && role.trim() !== roleOf(entry)) args.push("--role", role.trim());
    let saved = await change(args);
    for (const form of extra) saved = await change(["form", "add", String(entry.id), form]);
    return saved;
  }

  /** Each form with a way to drop it or hand it to the entry it really belongs to. */
  private formsOf(entry: Entry): HTMLElement {
    const others = this.entries.filter((e) => e.kind === entry.kind && e.id !== entry.id);
    const items = entry.forms
      .filter((f) => f.text !== entry.name)
      .map((form) => {
        const drop = el("button", { className: "link", type: "button", textContent: "quitar" });
        drop.onclick = () => this.formChange(entry, ["form", "remove", String(entry.id), form.text]);
        const apart = el("button", { className: "link", type: "button", textContent: "separar" });
        apart.title = "Dejarla como una entrada aparte";
        apart.onclick = () => this.formChange(entry, ["form", "split", String(entry.id), form.text]);
        const move = el("select", {}, el("option", { value: "", textContent: "es de otro…" }));
        for (const other of others) move.append(el("option", { value: String(other.id), textContent: other.name }));
        move.onchange = () =>
          move.value && this.formChange(entry, ["form", "move", String(entry.id), move.value, form.text]);
        return el("li", { className: "row" }, el("span", { textContent: form.text }), drop, apart, others.length ? move : null);
      });
    return el(
      "div",
      {},
      el("h3", { textContent: "Formas" }),
      items.length ? el("ul", { className: "plain" }, ...items) : el("p", { className: "muted", textContent: "Solo su nombre." }),
    );
  }

  private async formChange(entry: Entry, args: string[]): Promise<void> {
    try {
      await change(args);
      await this.reload();
      await this.edit(this.entries.find((e) => e.id === entry.id) ?? entry, entry.kind);
    } catch (err) {
      this.say(failed(err), true);
    }
  }

  /** Two clicks, with no browser dialog: the first asks, the second removes. */
  private removal(entry: Entry): HTMLElement {
    const button = el("button", { className: "danger", type: "button", textContent: "Borrar" });
    let armed = false;
    button.onclick = async () => {
      if (!armed) {
        armed = true;
        button.textContent = "¿Borrar de verdad? Pulsa otra vez";
        return;
      }
      try {
        await change(["remove", String(entry.id)]);
        show(byId("memory-editor"), false);
        this.say(`«${entry.name}» borrado.`);
        await this.reload();
      } catch (err) {
        this.say(failed(err), true);
      }
    };
    return el("div", { className: "row" }, button);
  }

  private async meetingsOf(entry: Entry): Promise<HTMLElement> {
    try {
      const { meetings } = await showEntry(entry.id);
      if (!meetings.length) return el("p", { className: "muted", textContent: "Todavía en ninguna reunión." });
      return el(
        "ul",
        { className: "plain" },
        ...meetings.map((m) =>
          el("li", {
            textContent: [
              new Date(m.at_ms).toLocaleDateString(),
              m.series ?? m.session,
              m.attended ? "" : "mencionado",
            ]
              .filter(Boolean)
              .join(" · "),
          }),
        ),
      );
    } catch (err) {
      return el("p", { className: "bad", textContent: failed(err) });
    }
  }
}
