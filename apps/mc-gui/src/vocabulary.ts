// The user's own vocabulary: ambiguous terms, what they mean and how they get misheard.
// Saved with `mc glossary add`, so the analyst, the fusion and the minutes all use it.
import { errorText, mcJson, runMc } from "./api";
import { el } from "./dom";

interface Term {
  term: string;
  aliases: string[];
  meaning: string | null;
  user: boolean;
}

/** Fills `box` with the form and the list; every box reloads the list after a change. */
export function mountVocabulary(boxes: HTMLElement[]): void {
  const lists = boxes.map(() => el("ul", { className: "vocabulary" }));
  const reload = async () => {
    const found = await mcJson<{ glossary: Term[] }>(["glossary", "list"]).catch(() => null);
    const own = (found?.glossary ?? []).filter((t) => t.user);
    for (const list of lists) {
      list.replaceChildren(
        ...own.map((t) => {
          const heard = t.aliases.length ? ` (se oye: ${t.aliases.join(", ")})` : "";
          return el("li", {}, el("strong", { textContent: t.term }), t.meaning ? ` — ${t.meaning}` : "", heard);
        }),
      );
    }
  };
  boxes.forEach((box, i) => {
    const term = el("input", { placeholder: "Término (p. ej. SAGA)" });
    const meaning = el("input", { placeholder: "Qué es (opcional)" });
    const heard = el("input", { placeholder: "Cómo se oye mal, separado por comas (opcional)" });
    const state = el("small", { className: "muted" });
    const add = el("button", { textContent: "Agregar" });
    add.onclick = async () => {
      const name = term.value.trim();
      if (!name) return term.focus();
      const aliases = heard.value.split(",").map((a) => a.trim()).filter(Boolean);
      const first = ["glossary", "add", name];
      if (meaning.value.trim()) first.push("--meaning", meaning.value.trim());
      // One alias per call: `mc glossary add` takes one at a time.
      const calls = [aliases.length ? [...first, "--alias", aliases[0]] : first];
      for (const alias of aliases.slice(1)) calls.push(["glossary", "add", name, "--alias", alias]);
      for (const args of calls) {
        const out = await runMc(args);
        if (out.code !== 0) {
          state.textContent = errorText(out);
          return;
        }
      }
      state.textContent = `«${name}» guardado.`;
      term.value = meaning.value = heard.value = "";
      await reload();
    };
    box.replaceChildren(el("div", { className: "row vocabulary-form" }, term, meaning, heard, add), state, lists[i]);
  });
  void reload();
}
