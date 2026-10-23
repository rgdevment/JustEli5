// The memory while the meeting runs: who each speaker is and their role, a term Teams keeps
// getting wrong, and a search. Corrections go through the capture, so this meeting's analyst
// uses them at once, and into the memory, so the next meetings do too.
import { byId, el } from "./dom";
import { change, listEntries, resolve, roleOf, showEntry } from "./memorydata";

type Send = (line: string) => void;

function failed(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export class LiveMemory {
  private seen = new Set<string>();
  private send: Send = () => {};

  /** Fills the card; `openMemory` shows the Memoria tab with a search. */
  mount(openMemory: (query: string) => void): void {
    byId("live-people").replaceChildren();
    this.seen.clear();
    const heard = el("input", { placeholder: "Teams escribe (p. ej. beri dex)" });
    const correct = el("input", { placeholder: "Debería decir (p. ej. Verídex)" });
    const fix = el("button", { type: "button", textContent: "Corregir" });
    const state = el("small", { className: "muted" });
    fix.onclick = () => {
      const [h, c] = [heard.value.trim(), correct.value.trim()];
      if (!h || !c) return (h ? correct : heard).focus();
      this.send(`t ${h} => ${c}`);
      state.textContent = `«${h}» → «${c}»: lo usan el analista y las próximas reuniones.`;
      heard.value = correct.value = "";
    };
    const query = el("input", { placeholder: "Buscar en la memoria" });
    const found = el("ul", { className: "plain small" });
    let asked = 0;
    query.oninput = async () => {
      const ask = ++asked;
      const text = query.value.trim();
      if (!text) return found.replaceChildren();
      const entries = await listEntries({ query: text }).catch(() => []);
      if (ask !== asked) return;
      found.replaceChildren(
        ...entries.slice(0, 8).map((e) => {
          const role = roleOf(e);
          return el("li", {}, el("strong", { textContent: e.name }), role ? ` (${role})` : "", e.meaning ? ` — ${e.meaning}` : "");
        }),
      );
    };
    const more = el("button", { className: "link", type: "button", textContent: "abrir en Memoria" });
    more.onclick = () => openMemory(query.value);
    byId("live-named").replaceChildren(this.namedForm());
    byId("live-memory").replaceChildren(
      el("div", { className: "row vocabulary-form" }, heard, correct, fix),
      state,
      el("div", { className: "row" }, query, more),
      found,
    );
  }

  /** Someone who is not here but is named, often by a nickname: «Caro» is Carolina, the PO. */
  private namedForm(): HTMLElement {
    const nick = el("input", { placeholder: "Cómo la nombran (p. ej. Caro)" });
    const name = el("input", { placeholder: "Quién es (p. ej. Carolina Soto)" });
    const role = el("input", { placeholder: "Rol (opcional)" });
    const save = el("button", { type: "button", textContent: "Guardar" });
    const state = el("small", { className: "muted" });
    save.onclick = async () => {
      const [n, who, r] = [nick.value.trim(), name.value.trim(), role.value.trim()];
      if (!n || !who) return (n ? name : nick).focus();
      try {
        const id = await resolve("person", who);
        if (id === null) {
          const args = ["add", "person", who, "--forms", n];
          if (r) args.push("--role", r);
          await change(args);
        } else {
          await change(["form", "add", String(id), n]);
          if (r) await change(["edit", String(id), "--role", r]);
        }
        state.textContent = `«${n}» es ${who}: lo sabrán la minuta y las próximas reuniones.`;
        nick.value = name.value = role.value = "";
      } catch (err) {
        state.textContent = failed(err);
      }
    };
    return el("div", {}, el("div", { className: "row vocabulary-form" }, nick, name, role, save), state);
  }

  /** Where corrections go: the running capture's commands. */
  attach(send: Send): void {
    this.send = send;
  }

  /** A speaker of this meeting: gets a row, with the role the memory knows, the first time. */
  see(speaker: string): void {
    const who = speaker.trim();
    if (!who || this.seen.has(who)) return;
    this.seen.add(who);
    const role = el("input", { placeholder: "Rol o cargo" });
    const real = el("input", { placeholder: "Nombre real (si Teams lo escribe mal)" });
    const state = el("small", { className: "muted" });
    let id: number | null = null;
    resolve("person", who)
      .then(async (known) => {
        id = known;
        if (known !== null && !role.value) role.value = roleOf((await showEntry(known)).entry) ?? "";
      })
      .catch(() => {});
    role.onchange = async () => {
      const value = role.value.trim();
      if (!value) return;
      try {
        const saved =
          id === null ? await change(["add", "person", who, "--role", value]) : await change(["edit", String(id), "--role", value]);
        id = saved?.id ?? id;
        state.textContent = "guardado";
      } catch (err) {
        state.textContent = failed(err);
      }
    };
    real.onchange = () => {
      const name = real.value.trim();
      if (!name || name === who) return;
      this.send(`s ${who} => ${name}`);
      state.textContent = `ahora es ${name}`;
    };
    byId("live-people").append(
      el("li", {}, el("span", { className: "who", textContent: who, title: who }), role, real, state),
    );
  }
}
