//! Our minutes are Markdown with a YAML-ish header; Tisty wants the title on the first
//! line, one paragraph per line with a blank line between blocks, and folds `:::details`.

/// The header of `minutes.md`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Properties {
    pub title: String,
    /// The short title the minutes propose for the series document.
    pub series_title: String,
    pub date: String,
    pub session: String,
    pub participants: Vec<String>,
}

fn text_value(raw: &str) -> String {
    serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.trim().to_string())
}

fn list_value(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Splits the `---` header from the body; a file without one is all body.
pub fn parse_properties(markdown: &str) -> (Properties, String) {
    let markdown = lf(markdown);
    let mut props = Properties {
        title: "Reunión".to_string(),
        ..Properties::default()
    };
    let Some(rest) = markdown.strip_prefix("---\n") else {
        return (props, markdown);
    };
    let Some((header, body)) = rest.split_once("\n---\n") else {
        return (props, markdown);
    };
    for line in header.lines() {
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        let raw = raw.trim();
        match key.trim() {
            "title" => props.title = text_value(raw),
            "series_title" => props.series_title = text_value(raw),
            "date" => props.date = text_value(raw),
            "session" => props.session = text_value(raw),
            "participants" => props.participants = list_value(raw),
            _ => {}
        }
    }
    (props, body.to_string())
}

/// A task checkbox line becomes a plain item: the commitments are proposed as real tasks.
fn plain_item(line: &str) -> String {
    for prefix in ["- [ ] ", "- [x] ", "- [X] "] {
        if let Some(item) = line.strip_prefix(prefix) {
            return format!("- {}", item.strip_prefix('@').unwrap_or(item));
        }
    }
    line.to_string()
}

fn is_fence(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("```") || line.starts_with("~~~")
}

/// Whether a line opens a block of its own (so it never continues the line before it).
fn starts_block(line: &str) -> bool {
    let line = line.trim_start();
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let numbered = digits > 0 && line[digits..].starts_with(". ");
    ["#", "- ", "* ", "+ ", ">", "|", "---", ":::"]
        .iter()
        .any(|start| line.starts_with(start))
        || numbered
        || is_fence(line)
}

/// Joins soft-wrapped lines (a paragraph or list item broken at some column) into one line
/// each, keeps one blank line between blocks, and leaves fenced code exactly as written.
pub fn unwrap_paragraphs(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;
    // Whether the last line can take a continuation: a paragraph or an item, not a heading.
    let mut open = false;
    for line in lf(body).lines() {
        let line = line.trim_end();
        if is_fence(line) {
            in_fence = !in_fence;
            out.push(line.to_string());
            open = false;
        } else if in_fence {
            out.push(line.to_string());
        } else if line.trim().is_empty() {
            open = false;
            // In Tisty the blank line is what separates paragraphs.
            if out.last().is_some_and(|l| !l.is_empty()) {
                out.push(String::new());
            }
        } else if open && !starts_block(line) {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(line.trim());
            }
        } else {
            out.push(line.to_string());
            open =
                !line.trim_start().starts_with(['#', '>', '|', '-', ':']) || line.starts_with("- ");
        }
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// Drops the `[#12, #13]` citations: they point at transcript lines a Tisty reader cannot open.
pub(crate) fn without_citations(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("[#") {
        let tail = &rest[start..];
        let cited = tail.find(']').filter(|&end| {
            tail[1..end].split(',').all(|t| {
                t.trim()
                    .strip_prefix('#')
                    .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
            })
        });
        match cited {
            Some(end) => {
                out.push_str(rest[..start].trim_end());
                rest = &tail[end + 1..];
            }
            None => {
                out.push_str(&rest[..start + 2]);
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The pictures a body shows (`![alt](path)`), in order: Tisty gets them as attachments.
pub fn image_refs(body: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("![") {
        let tail = &rest[start + 2..];
        let parsed = tail.split_once("](").and_then(|(alt, after)| {
            let (path, _) = after.split_once(')')?;
            (!alt.contains(['\n', ']']) && !path.contains(['\n', ' ']))
                .then(|| (alt.to_string(), path.to_string()))
        });
        match parsed {
            Some(image) => {
                rest = &tail[image.0.len() + image.1.len() + 3..];
                found.push(image);
            }
            None => rest = tail,
        }
    }
    found
}

/// A picture's path is local to the minutes folder: on the page it is attached, so the line says so.
fn without_images(line: &str) -> String {
    let mut out = line.to_string();
    for (alt, path) in image_refs(line) {
        out = out.replace(
            &format!("![{alt}]({path})"),
            "(imagen adjunta al final de la página)",
        );
    }
    out
}

/// Sections a reader opens only when needed: Tisty folds them.
const FOLDED: [&str; 3] = ["Notas", "Destacados", "Participantes"];

/// Tisty blocks: the TL;DR becomes a callout, every `### ` sub-section and the notes fold as
/// `:::details`, so the first screen of a page is what matters.
fn tisty_blocks(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let (mut folded, mut callout) = (false, false);
    let close = |out: &mut Vec<String>, folded: &mut bool| {
        if std::mem::take(folded) {
            out.extend([String::new(), ":::".to_string(), String::new()]);
        }
    };
    for line in lines {
        if let Some(name) = line.strip_prefix("## ") {
            close(&mut out, &mut folded);
            callout = name.trim() == "TL;DR";
            if callout {
                out.extend([String::new(), "> [!IMPORTANT] TL;DR".to_string()]);
            } else if FOLDED.iter().any(|f| name.trim().starts_with(f)) {
                out.extend([
                    String::new(),
                    format!(":::details {}", name.trim()),
                    String::new(),
                ]);
                folded = true;
            } else {
                out.extend([String::new(), line.clone(), String::new()]);
            }
        } else if let Some(name) = line.strip_prefix("### ") {
            close(&mut out, &mut folded);
            out.extend([
                String::new(),
                format!(":::details {}", name.trim()),
                String::new(),
            ]);
            folded = true;
        } else if callout {
            if !line.trim().is_empty() {
                out.push(format!("> {line}"));
            }
        } else {
            out.push(line);
        }
    }
    close(&mut out, &mut folded);
    let mut tidy: Vec<String> = Vec::new();
    for line in out {
        if line.is_empty() && tidy.last().is_none_or(String::is_empty) {
            continue;
        }
        tidy.push(line);
    }
    while tidy.last().is_some_and(String::is_empty) {
        tidy.pop();
    }
    tidy
}

/// `title`, then the body without soft wraps, citations or its own `# title`, in Tisty blocks.
pub fn to_tisty_markdown(title: &str, _props: &Properties, body: &str) -> String {
    let mut lines = Vec::new();
    let mut dropped_heading = false;
    for line in unwrap_paragraphs(body) {
        if !dropped_heading && line.starts_with("# ") {
            dropped_heading = true;
            continue;
        }
        lines.push(without_images(&without_citations(&plain_item(&line))));
    }
    // Tisty takes the first line as the title and glues a following line to it: a blank line ends it.
    let mut page = vec![title.to_string(), String::new()];
    page.extend(tisty_blocks(lines));
    page.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citations_leave_the_page_and_other_brackets_stay() {
        assert_eq!(
            without_citations("No se sale el lunes [#13, #152]. Ver [nota] y [#x] [#7]"),
            "No se sale el lunes. Ver [nota] y [#x]"
        );
    }

    const MD: &str = "---\ndate: 2023-11-14\ntitle: \"Daily \\\"de\\\" equipo\"\nsession: \"s1\"\nparticipants: [\"Ana\", \"Luis\"]\ntags: []\n---\n# Daily\n\n## Resumen\n\nUn párrafo.\n\nOtro.\n\n## Compromisos\n\n- [ ] @Ana: cerrar (el martes) [#3]\n- [x] Luis: listo\n";

    #[test]
    fn the_header_is_parsed_with_quotes_and_lists() {
        let (props, body) = parse_properties(MD);
        assert_eq!(props.title, "Daily \"de\" equipo");
        assert_eq!(props.date, "2023-11-14");
        assert_eq!(props.session, "s1");
        assert_eq!(props.participants, ["Ana", "Luis"]);
        assert!(body.starts_with("# Daily"));
    }

    #[test]
    fn a_file_without_header_is_all_body() {
        let (props, body) = parse_properties("# Solo\ntexto\n");
        assert_eq!(props.title, "Reunión");
        assert_eq!(body, "# Solo\ntexto\n");
    }

    #[test]
    fn crlf_files_are_read_like_lf_ones() {
        let (props, body) = parse_properties(&MD.replace('\n', "\r\n"));
        assert_eq!(props.title, "Daily \"de\" equipo");
        assert!(!body.contains('\r'));
        let out = to_tisty_markdown("T", &props, &body);
        assert!(
            !out.contains('\r') && out.contains("- Ana: cerrar"),
            "{out}"
        );
    }

    #[test]
    fn conversion_gives_the_title_first_and_blank_lines_between_blocks() {
        let (props, body) = parse_properties(MD);
        let out = to_tisty_markdown("Minuta: Daily", &props, &body);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "Minuta: Daily");
        assert_eq!(lines[1], "", "a blank line ends the title");
        assert!(!out.contains("---"));
        assert!(
            !out.contains("# Daily"),
            "the body title is the first line already"
        );
        assert!(
            out.contains("## Resumen\n\nUn párrafo.\n\nOtro.\n\n## Compromisos"),
            "{out}"
        );
        assert!(lines.contains(&"- Ana: cerrar (el martes)"), "{out}");
        assert!(lines.contains(&"- Luis: listo"));
        assert!(!out.contains("\n\n\n"), "{out}");
    }

    #[test]
    fn pictures_are_listed_and_the_page_says_they_are_attached() {
        let body = "## Notas\n\n- [00:02] matriz ![matriz](../../notes/7.png) y ![x](a b)\n";
        assert_eq!(
            image_refs(body),
            [("matriz".to_string(), "../../notes/7.png".to_string())]
        );
        let out = to_tisty_markdown("M", &Properties::default(), body);
        assert!(
            out.contains("- [00:02] matriz (imagen adjunta al final de la página) y ![x](a b)"),
            "{out}"
        );
    }

    #[test]
    fn the_tldr_is_a_callout_and_topics_and_notes_fold() {
        let body = "# T\n\n## TL;DR\n\n- **Decidido:** x [#1]\n- **Para ti:** nada\n\n## Decisiones\n\n1. uno\n\n### Otras decisiones (1)\n\n2. dos\n\n## Temas\n\n### Checkout\n- a\n- **Resultado:** ok\n### Pruebas\n- b\n\n## Notas\n\n- mía\n";
        let out = to_tisty_markdown("Minuta", &Properties::default(), body);
        assert_eq!(
            out,
            "Minuta\n\n> [!IMPORTANT] TL;DR\n> - **Decidido:** x\n> - **Para ti:** nada\n\n## Decisiones\n\n1. uno\n\n:::details Otras decisiones (1)\n\n2. dos\n\n:::\n\n## Temas\n\n:::details Checkout\n\n- a\n- **Resultado:** ok\n\n:::\n\n:::details Pruebas\n\n- b\n\n:::\n\n:::details Notas\n\n- mía\n\n:::\n"
        );
    }

    #[test]
    fn soft_wrapped_paragraphs_and_items_become_one_line() {
        let body = "## Resumen\nHubo bloqueos en\nel pipeline y se habló\n\nOtro párrafo\n\n- primer punto que\n  sigue aquí\n- segundo\n1. uno\n   dos\n";
        assert_eq!(
            unwrap_paragraphs(body),
            [
                "## Resumen",
                "Hubo bloqueos en el pipeline y se habló",
                "",
                "Otro párrafo",
                "",
                "- primer punto que sigue aquí",
                "- segundo",
                "1. uno dos",
            ]
        );
    }

    #[test]
    fn text_after_a_heading_is_a_new_line_not_a_continuation() {
        assert_eq!(
            unwrap_paragraphs("## Temas\ntexto\nmás texto"),
            ["## Temas", "texto más texto"]
        );
    }

    #[test]
    fn fenced_code_is_kept_as_written() {
        let body = "Antes\n```sh\nls -la\n\n  indentado\n```\nDespués\nsigue\n";
        assert_eq!(
            unwrap_paragraphs(body),
            [
                "Antes",
                "```sh",
                "ls -la",
                "",
                "  indentado",
                "```",
                "Después sigue"
            ]
        );
    }
}
