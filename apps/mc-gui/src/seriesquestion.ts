// The meeting name, checked against the meetings already held: written a little differently it
// would start a new series, so the user is asked whether they meant one of them.
import { mcJson } from "./api";
import { byId, el, show } from "./dom";

interface Series {
  key: string;
  title: string;
  sessions: number;
  last_wall_ms: number | null;
  in_tisty: boolean;
}

interface Match {
  exact: Series | null;
  alike: Series[];
}

function detail(s: Series): string {
  const parts: string[] = [];
  if (s.sessions) parts.push(`${s.sessions} ${s.sessions === 1 ? "reunión" : "reuniones"}`);
  if (s.last_wall_ms) parts.push(`la última el ${new Date(s.last_wall_ms).toLocaleDateString("es")}`);
  if (s.in_tisty) parts.push("en Tisty");
  return parts.join(" · ");
}

export class SeriesQuestion {
  private asked = 0;
  /** Names the user already answered for, so they are not asked twice. */
  private settled = new Set<string>();

  /** `pick` writes the chosen series' name into the form; `tisty` asks Tisty too. */
  constructor(private readonly pick: (title: string) => void) {}

  async ask(name: string, tisty: boolean): Promise<void> {
    const box = byId("name-match");
    const ask = ++this.asked;
    show(box, false);
    if (!name || this.settled.has(name)) return;
    const args = ["series", "match", name];
    if (tisty) args.push("--tisty");
    const found = await mcJson<Match>(args).catch(() => null);
    if (ask !== this.asked || !found || found.exact || !found.alike.length) return;
    const answered = (title: string) => {
      this.settled.add(title);
      show(box, false);
    };
    const options = found.alike.map((s) =>
      el(
        "li",
        {},
        el("button", {
          className: "pill",
          type: "button",
          textContent: s.title,
          onclick: () => {
            answered(s.title);
            this.pick(s.title);
          },
        }),
        el("small", { className: "muted", textContent: ` ${detail(s)}` }),
      ),
    );
    const fresh = el("button", { className: "link", type: "button", textContent: "Es una nueva" });
    fresh.onclick = () => answered(name);
    box.replaceChildren(
      el("span", { textContent: options.length === 1 ? "¿Te refieres a esta o es una nueva?" : "¿Te refieres a una de estas o es una nueva?" }),
      el("ul", { className: "plain" }, ...options),
      fresh,
    );
    show(box, true);
  }
}
