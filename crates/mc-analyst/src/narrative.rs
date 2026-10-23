//! Cleaning of the model's narrative (TL;DR, Próximas sesiones, Conclusiones and Temas) before it enters the minutes.

use std::collections::BTreeSet;

/// The narrative sections only, with citations to segments that exist.
pub fn clean_narrative(answer: &str, known: &BTreeSet<u64>) -> Narrative {
    narrative_only(&strip_unknown_citations(strip_fence(answer), known))
}

/// The model sometimes wraps Markdown in a code fence.
fn strip_fence(text: &str) -> &str {
    let text = text.trim();
    let Some(inner) = text.strip_prefix("```") else {
        return text;
    };
    let inner = inner.split_once('\n').map_or("", |(_, rest)| rest);
    inner.strip_suffix("```").unwrap_or(inner).trim()
}

/// The ids of a bracket made only of `#id` entries (`[#1]`, `[#1, #2]`, `[ #1 ]`).
fn citation_ids(inner: &str) -> Option<Vec<u64>> {
    let ids: Option<Vec<u64>> = inner
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(|t| t.strip_prefix('#')?.parse().ok())
        .collect();
    ids.filter(|ids| !ids.is_empty())
}

/// Keeps in every citation bracket only the ids that exist; a bracket left empty goes away.
fn strip_unknown_citations(text: &str, known: &BTreeSet<u64>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let cited = tail
            .find(']')
            .and_then(|end| citation_ids(&tail[1..end]).map(|ids| (end, ids)));
        let Some((end, ids)) = cited else {
            out.push('[');
            rest = &tail[1..];
            continue;
        };
        rest = &tail[end + 1..];
        let kept: Vec<String> = ids
            .iter()
            .filter(|id| known.contains(id))
            .map(|id| format!("#{id}"))
            .collect();
        if !kept.is_empty() {
            out.push_str(&format!("[{}]", kept.join(", ")));
            continue;
        }
        if out.ends_with(' ') {
            rest = rest.trim_start_matches(' ');
        }
        if rest.is_empty() || rest.starts_with(['.', ',', ';', ':', ')']) {
            out.truncate(out.trim_end().len());
        }
    }
    out.push_str(rest);
    out
}

/// The level and text of a Markdown heading.
fn heading(line: &str) -> Option<(usize, &str)> {
    let line = line.trim_start();
    let level = line.chars().take_while(|c| *c == '#').count();
    let title = line[level..].strip_prefix(' ')?;
    (1..=6).contains(&level).then_some((level, title.trim()))
}

fn lowered(title: &str) -> String {
    let lower = title.to_lowercase();
    lower
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

fn section_name(title: &str) -> Option<&'static str> {
    let lower = lowered(title).replace([';', '.', ' '], "");
    if lower.starts_with("tldr") || lower.starts_with("resumen") {
        Some(TLDR)
    } else if lower.starts_with("temas") {
        Some(TOPICS)
    } else if lower.starts_with("conclusi") {
        Some(CONCLUSIONS)
    } else if lower.starts_with("particip") {
        Some(PEOPLE)
    } else if lower.starts_with("glosario") {
        Some(TERMS)
    } else if (lower.starts_with("pendiente") && !lower.contains("anterior"))
        || lower.starts_with("próxim")
        || lower.starts_with("sesion")
    {
        Some(NEXT)
    } else {
        None
    }
}

/// A sub-heading under Temas is one topic, unless it names a section the system writes.
fn is_topic(level: usize, title: &str) -> bool {
    const SYSTEM: [&str; 7] = [
        "decisi",
        "compromis",
        "pendiente",
        "pregunta",
        "riesgo",
        "destacad",
        "nota",
    ];
    let lower = lowered(title);
    level >= 3 && !lower.is_empty() && !SYSTEM.iter().any(|s| lower.starts_with(s))
}

const TLDR: &str = "TL;DR";
const SERIES_CHARS: usize = 40;
const NEXT: &str = "Próximas sesiones";
const CONCLUSIONS: &str = "Conclusiones";
const TOPICS: &str = "Temas";
const PEOPLE: &str = "Participantes";
const TERMS: &str = "Glosario";

/// The four sections the model writes, each without its heading; the minutes interleave them
/// with the ones the system writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Narrative {
    /// A short title for the document that gathers every session of the meeting.
    pub series: String,
    pub tldr: String,
    pub next: String,
    pub conclusions: String,
    pub topics: String,
    /// `- Name: role` lines and maybe «Mencionados sin asistir: …»; the system builds the table.
    pub people: String,
    /// `- Term: what it is` lines; the system builds the table.
    pub terms: String,
}

impl Narrative {
    pub fn is_empty(&self) -> bool {
        [&self.tldr, &self.next, &self.conclusions, &self.topics]
            .iter()
            .all(|s| s.trim().is_empty())
    }

    /// The topics (`### title`) whose bullets lack the closing «**Resultado:**».
    pub fn topics_without_result(&self) -> Vec<String> {
        let mut missing = Vec::new();
        let mut current: Option<(&str, bool)> = None;
        for line in self.topics.lines().chain(std::iter::once("### ")) {
            if let Some(title) = line.strip_prefix("### ") {
                if let Some((done, false)) = current {
                    missing.push(done.to_string());
                }
                current = Some((title.trim(), false));
            } else if let Some((_, found)) = current.as_mut() {
                let bullet = line
                    .trim_start()
                    .trim_start_matches(['-', '*'])
                    .trim_start();
                *found |=
                    bullet.starts_with("**Resultado:**") || bullet.starts_with("**Resultado**:");
            }
        }
        missing
    }
}

enum Place {
    Preamble,
    Keep(usize),
    Drop,
}

/// Keeps the TL;DR, Próximas sesiones, Conclusiones and Temas sections (any heading level or
/// wording like "Resumen de la reunión"), with each topic of Temas as a `### ` sub-heading; the
/// system writes the rest. Text before the first heading becomes the TL;DR when the model wrote none.
fn narrative_only(text: &str) -> Narrative {
    let mut preamble: Vec<String> = Vec::new();
    let mut sections: Vec<(&str, Vec<String>)> = Vec::new();
    let mut place = Place::Preamble;
    // The section a sub-heading belongs to, even after a dropped sub-heading inside it.
    let mut top: Option<usize> = None;
    let mut series = String::new();
    for line in text.lines() {
        if let Some(title) = line.trim().strip_prefix("Serie:")
            && series.is_empty()
        {
            series = title.trim().trim_matches(['«', '»', '"']).to_string();
            continue;
        }
        if let Some((level, title)) = heading(line) {
            let in_temas = top.is_some_and(|i| sections[i].0 == TOPICS);
            place = match section_name(title) {
                Some(name) => {
                    sections.push((name, Vec::new()));
                    top = Some(sections.len() - 1);
                    Place::Keep(sections.len() - 1)
                }
                None if in_temas && is_topic(level, title) => {
                    let i = sections.len() - 1;
                    sections[i].1.push(format!("### {title}"));
                    Place::Keep(i)
                }
                None => {
                    if level <= 2 {
                        top = None;
                    }
                    Place::Drop
                }
            };
            continue;
        }
        match place {
            Place::Preamble => preamble.push(line.to_string()),
            Place::Keep(i) => sections[i].1.push(line.to_string()),
            Place::Drop => {}
        }
    }
    if !sections.iter().any(|(name, _)| *name == TLDR) && !preamble.join("").trim().is_empty() {
        sections.insert(0, (TLDR, preamble));
    }
    let body = |name: &str| {
        sections
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, lines)| lines.join("\n").trim().to_string())
            .filter(|b| !b.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    Narrative {
        series: series
            .chars()
            .take(SERIES_CHARS)
            .collect::<String>()
            .trim()
            .to_string(),
        tldr: body(TLDR),
        next: body(NEXT),
        conclusions: body(CONCLUSIONS),
        topics: body(TOPICS),
        people: body(PEOPLE),
        terms: body(TERMS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citations_keep_only_known_ids_in_every_form() {
        let known: BTreeSet<u64> = [1, 2].into();
        let clean = |t: &str| strip_unknown_citations(t, &known);
        assert_eq!(clean("a [#1] b [#9] c [#x] [#"), "a [#1] b c [#x] [#");
        assert_eq!(clean("x [#1, #9, #2] y"), "x [#1, #2] y");
        assert_eq!(clean("x [ #9 , #8 ] y."), "x y.");
        assert_eq!(clean("fin [#9]."), "fin.");
        assert_eq!(clean("[#2 #7][#1,#5]"), "[#2][#1]");
        assert_eq!(clean("lista [a, b] y [1]"), "lista [a, b] y [1]");
    }

    #[test]
    fn strips_a_code_fence() {
        assert_eq!(
            strip_fence("```markdown\n## Resumen\nx\n```"),
            "## Resumen\nx"
        );
    }

    fn n(tldr: &str, next: &str, conclusions: &str, topics: &str) -> Narrative {
        Narrative {
            series: String::new(),
            tldr: tldr.into(),
            next: next.into(),
            conclusions: conclusions.into(),
            topics: topics.into(),
            people: String::new(),
            terms: String::new(),
        }
    }

    #[test]
    fn keeps_only_the_model_s_sections_whatever_the_wording_or_level() {
        let text = "## Resumen de la reunión:\nuno\n### Decisiones\nfalso\n# Temas tratados\n- t\n## Riesgos\nr\n**Resumen**\nnada";
        assert_eq!(narrative_only(text), n("uno", "", "", "- t"));
        let tldr = "## TL;DR\n- **Decidido:** x\n## Próximas sesiones\n- legal";
        assert_eq!(
            narrative_only(tldr),
            n("- **Decidido:** x", "- legal", "", "")
        );
    }

    #[test]
    fn text_before_the_first_heading_becomes_the_tldr_unless_there_is_one() {
        assert_eq!(
            narrative_only("solo prosa\n## Temas\n- t"),
            n("solo prosa", "", "", "- t")
        );
        assert_eq!(narrative_only("intro\n## TL;DR\nr"), n("r", "", "", ""));
        assert!(narrative_only("## Decisiones\nx").is_empty());
    }

    #[test]
    fn topics_under_temas_and_the_conclusions_are_kept() {
        let text = "## TL;DR\nr\n## Temas\n#### Deploy\n- staging [#1]\n### Decisiones\nfalso\n### Logs\n- MMS\n## Riesgos\nx\n### Fuera\ny\n## Conclusiones de la reunión\n- listo";
        assert_eq!(
            narrative_only(text),
            n(
                "r",
                "",
                "- listo",
                "### Deploy\n- staging [#1]\n### Logs\n- MMS"
            )
        );
    }

    #[test]
    fn what_was_left_open_is_kept_but_the_previous_meeting_is_the_system_s() {
        let text = "## TL;DR\nr\n## Pendientes de la reunión anterior\nx\n## Pendientes y próximos pasos\n- mapping [#2]";
        assert_eq!(narrative_only(text), n("r", "- mapping [#2]", "", ""));
    }

    #[test]
    fn the_series_line_is_read_apart_and_cut_to_forty_characters() {
        let text = "Serie: «Rediseño del panel y reportes»\n## TL;DR\n- x";
        let got = narrative_only(text);
        assert_eq!(got.series, "Rediseño del panel y reportes");
        assert_eq!(got.tldr, "- x");
        let long = narrative_only(&format!("Serie: {}\n## TL;DR\nx", "a".repeat(60)));
        assert_eq!(long.series.chars().count(), 40);
    }

    #[test]
    fn a_topic_without_its_result_is_reported() {
        let topics =
            "### A\n- x\n- **Resultado:** resuelto\n### B\n- y\n### C\n* **Resultado**: pendiente";
        assert_eq!(n("", "", "", topics).topics_without_result(), ["B"]);
        assert!(n("t", "", "", "").topics_without_result().is_empty());
    }
}
