//! Publishing a meeting's minutes and commitments to a task app: the plan (what will be
//! created, built from files so it can be previewed), the report, and the adapters.

mod markdown;
pub mod tisty;

use mc_analyst::{CommitmentRecord, is_me};
use serde_json::{Value, json};

pub use markdown::{
    Properties, image_refs, parse_properties, to_tisty_markdown, unwrap_paragraphs,
};

/// Where things go in the destination app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub folder: String,
    pub list: Option<String>,
    pub tags: Vec<String>,
    /// The person publishing, as the captions name them: only their commitments become tasks.
    pub me: Option<String>,
}

/// A recurring meeting is one living document; each meeting's minutes are a page of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinutesPlan {
    pub folder: String,
    pub series_title: String,
    /// The title series documents had before short titles; still found by it.
    pub legacy_series_title: String,
    /// The series document already known for this meeting (from a previous publish).
    pub series_doc: Option<String>,
    pub series_body: String,
    pub page_title: String,
    pub page_body: String,
    /// Pictures the notes carry (caption, path): attached to the page once it is written.
    pub images: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPlan {
    pub source: String,
    pub title: String,
    pub description: String,
    pub list: Option<String>,
    pub tags: Vec<String>,
    pub deadline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub minutes: MinutesPlan,
    /// One task for the meeting, holding the user's commitments as its parts; none without them.
    pub parent: Option<TaskPlan>,
    /// The user's commitments: the parts of `parent`.
    pub tasks: Vec<TaskPlan>,
    /// Commitments of other people, or of anyone when no name is configured: minutes only.
    pub others: usize,
    pub me: Option<String>,
    /// The configured user never speaks in the meeting: a wrong `fusion.me` looks the same.
    pub absent: bool,
}

/// Publishes a plan; never panics or errors out: whatever happened is in the report.
/// Publishing the same plan again is safe: what the destination already has is recognised.
pub trait Publisher {
    fn publish(&self, plan: &Plan) -> PublishReport;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Created,
    /// The destination already had it (a known `source`, or a document found by its title).
    Existed,
    Failed(String),
    /// The conversation ended while this was being sent: it may or may not have been applied.
    Unknown(String),
}

impl Status {
    fn name(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Existed => "existed",
            Self::Failed(_) => "failed",
            Self::Unknown(_) => "unknown",
        }
    }

    pub fn is_bad(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::Unknown(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// The document of the meeting series.
    Series,
    /// This meeting's minutes, a page of the series.
    Page,
    Task,
}

impl ItemKind {
    fn name(self) -> &'static str {
        match self {
            Self::Series => "series",
            Self::Page => "page",
            Self::Task => "task",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: ItemKind,
    pub label: String,
    pub source: Option<String>,
    pub status: Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortKind {
    /// The app is not installed, could not be started, or stopped answering.
    Unavailable,
    /// The user has not admitted this assistant in the app yet.
    NotAdmitted,
    /// The folder or the list does not exist.
    MissingTarget,
    /// The folder's name is shared by several folders: only its whole path says which.
    AmbiguousTarget,
    /// The minutes could not be written, so no task points at them.
    MinutesNotWritten,
}

impl AbortKind {
    fn name(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::NotAdmitted => "not_admitted",
            Self::MissingTarget => "missing_target",
            Self::AmbiguousTarget => "ambiguous_target",
            Self::MinutesNotWritten => "minutes_not_written",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Abort {
    pub kind: AbortKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublishReport {
    pub items: Vec<Item>,
    /// Why publishing stopped early; the items not reached are listed as failed.
    pub aborted: Option<Abort>,
    /// The series document written or found, to find it again by id next time.
    pub series_doc: Option<String>,
}

pub(crate) fn series_item(plan: &Plan, status: Status) -> Item {
    Item {
        kind: ItemKind::Series,
        label: plan.minutes.series_title.clone(),
        source: None,
        status,
    }
}

pub(crate) fn page_item(plan: &Plan, status: Status) -> Item {
    Item {
        kind: ItemKind::Page,
        label: plan.minutes.page_title.clone(),
        source: None,
        status,
    }
}

pub(crate) fn task_item(task: &TaskPlan, status: Status) -> Item {
    Item {
        kind: ItemKind::Task,
        label: task.title.clone(),
        source: Some(task.source.clone()),
        status,
    }
}

impl PublishReport {
    /// Ends the report at `stop`: whatever was not reached is listed as failed.
    pub fn stopped_by(mut self, plan: &Plan, stop: Abort) -> Self {
        let reason = format!("not attempted: {}", stop.message);
        let missing = |items: &[Item], kind: ItemKind, source: Option<&str>| {
            !items
                .iter()
                .any(|i| i.kind == kind && i.source.as_deref() == source)
        };
        if missing(&self.items, ItemKind::Series, None) {
            self.items
                .push(series_item(plan, Status::Failed(reason.clone())));
        }
        if missing(&self.items, ItemKind::Page, None) {
            self.items
                .push(page_item(plan, Status::Failed(reason.clone())));
        }
        for task in plan.parent.iter().chain(&plan.tasks) {
            if missing(&self.items, ItemKind::Task, Some(&task.source)) {
                self.items
                    .push(task_item(task, Status::Failed(reason.clone())));
            }
        }
        self.aborted = Some(stop);
        self
    }

    pub fn count(&self, kind: ItemKind, what: fn(&Status) -> bool) -> usize {
        self.items
            .iter()
            .filter(|i| i.kind == kind && what(&i.status))
            .count()
    }

    pub fn failed(&self) -> bool {
        self.aborted.is_some() || self.items.iter().any(|i| i.status.is_bad())
    }

    pub fn to_json(&self) -> Value {
        let items: Vec<Value> = self
            .items
            .iter()
            .map(|i| {
                let mut item = json!({
                    "kind": i.kind.name(),
                    "label": i.label,
                    "source": i.source,
                    "status": i.status.name(),
                });
                if let Status::Failed(reason) | Status::Unknown(reason) = &i.status {
                    item["reason"] = json!(reason);
                }
                item
            })
            .collect();
        json!({
            "ok": !self.failed(),
            "aborted": self.aborted.as_ref().map(|a| json!({"kind": a.kind.name(), "message": a.message})),
            "items": items,
        })
    }

    pub fn human(&self) -> String {
        let mut text = String::new();
        if let Some(abort) = &self.aborted {
            text.push_str(&format!("stopped: {}\n", abort.message));
        }
        for item in &self.items {
            let status = match &item.status {
                Status::Created => "created".to_string(),
                Status::Existed => "already existed".to_string(),
                Status::Failed(reason) => format!("FAILED: {reason}"),
                Status::Unknown(reason) => format!("UNKNOWN, may have been applied: {reason}"),
            };
            text.push_str(&format!("  {status:<16} {}\n", item.label));
        }
        text
    }
}

/// `YYYY-MM-DD` out of a spoken due date, when it is one.
fn iso_date(due: &str) -> Option<String> {
    let head: String = due.trim().chars().take(10).collect();
    let rest = due.trim().chars().nth(10);
    let parts: Vec<&str> = head.split('-').collect();
    let [year, month, day] = parts.as_slice() else {
        return None;
    };
    let digits = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_digit());
    let in_range = |s: &str, max: u32| s.parse::<u32>().is_ok_and(|n| (1..=max).contains(&n));
    let valid = digits(year, 4)
        && digits(month, 2)
        && digits(day, 2)
        && in_range(month, 12)
        && in_range(day, 31)
        && rest.is_none_or(|c| c == 'T' || c == ' ');
    valid.then_some(head)
}

fn unaccented(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' | 'â' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        other => other,
    }
}

/// One lowercase word without accents or punctuation (a tag is one word).
fn word_tag(word: &str) -> String {
    word.to_lowercase()
        .chars()
        .map(unaccented)
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// The tag that groups a recurring meeting: the first meaningful word of its name.
fn series_tag(title: &str) -> Option<String> {
    const FILLER: [&str; 12] = [
        "de", "del", "la", "el", "los", "las", "y", "con", "en", "para", "un", "una",
    ];
    title
        .split_whitespace()
        .map(word_tag)
        .find(|w| w.chars().count() >= 3 && !FILLER.contains(&w.as_str()))
}

fn task_tags(series: &str, configured: &[String]) -> Vec<String> {
    let mut tags = vec!["reunion".to_string()];
    tags.extend(series_tag(series));
    tags.extend(
        configured
            .iter()
            .flat_map(|t| t.split_whitespace().map(word_tag)),
    );
    let mut seen: Vec<String> = Vec::new();
    for tag in tags {
        if !tag.is_empty() && !seen.contains(&tag) {
            seen.push(tag);
        }
    }
    seen
}

fn task_plan(
    c: &CommitmentRecord,
    props: &Properties,
    minutes: &MinutesPlan,
    target: &Target,
    tags: &[String],
) -> TaskPlan {
    let mut lines = vec![format!(
        "Compromiso de la reunión «{}»{}.",
        props.title,
        if props.date.is_empty() {
            String::new()
        } else {
            format!(" ({})", props.date)
        }
    )];
    match (&c.due, iso_date(c.due.as_deref().unwrap_or_default())) {
        (Some(due), None) if !due.trim().is_empty() => {
            let day = if props.date.is_empty() {
                String::new()
            } else {
                format!(" del {}", props.date)
            };
            lines.push(format!("Plazo dicho en la reunión{day}: {}.", due.trim()));
        }
        _ => {}
    }
    if !c.said.is_empty() {
        lines.push("Lo que se dijo:".to_string());
        lines.extend(c.said.iter().map(|line| format!("> {line}")));
    }
    lines.push(format!(
        "Minuta: «{}», página de «{}» (carpeta {}).",
        minutes.page_title, minutes.series_title, minutes.folder
    ));
    TaskPlan {
        source: c.source.clone(),
        title: c.what.trim().to_string(),
        description: lines.join("\n"),
        list: target.list.clone(),
        tags: tags.to_vec(),
        deadline: c.due.as_deref().and_then(iso_date),
    }
}

/// The meeting's task: it holds the user's commitments as parts and points at the minutes.
fn parent_plan(
    props: &Properties,
    minutes: &MinutesPlan,
    target: &Target,
    tags: &[String],
    others: usize,
) -> TaskPlan {
    let when = if props.date.is_empty() {
        String::new()
    } else {
        format!(" ({})", props.date)
    };
    let mut lines = vec![format!(
        "Mis compromisos de la reunión «{}»{when}; cada uno es una parte de esta tarea.",
        props.title
    )];
    match others {
        0 => {}
        1 => lines.push("El compromiso de otra persona queda solo en la minuta.".to_string()),
        n => lines.push(format!(
            "Los {n} compromisos de otras personas quedan solo en la minuta."
        )),
    }
    lines.push(format!(
        "Minuta: «{}», página de «{}» (carpeta {}).",
        minutes.page_title, minutes.series_title, minutes.folder
    ));
    TaskPlan {
        // The pre-JustEli5 prefix stays: Tisty matches a republished meeting by this source.
        source: format!("meet-copilot:{}", props.session),
        title: format!("Reunión {}{when}: mis compromisos", props.title),
        description: lines.join("\n"),
        list: target.list.clone(),
        tags: tags.to_vec(),
        deadline: None,
    }
}

/// The meeting name cut at a word to at most 40 characters.
fn short_title(title: &str) -> String {
    const MAX: usize = 40;
    let title = title.trim();
    if title.chars().count() <= MAX {
        return title.to_string();
    }
    let mut short = String::new();
    for word in title.split_whitespace() {
        if short.chars().count() + word.chars().count() + 1 > MAX {
            break;
        }
        if !short.is_empty() {
            short.push(' ');
        }
        short.push_str(word);
    }
    short
}

/// The text after a TL;DR label (`- **Decidido:** …`), without citations, with its sub-items.
fn tldr_field(body: &str, label: &str) -> Option<String> {
    let mut lines = body
        .lines()
        .skip_while(|l| l.trim() != "## TL;DR")
        .skip(1)
        .take_while(|l| !l.starts_with("## "));
    let head = format!("- **{label}:**");
    let first = lines.find(|l| l.trim_start().starts_with(&head))?;
    let mut text = first.trim_start()[head.len()..].trim().to_string();
    for more in lines.take_while(|l| l.starts_with("  ")) {
        text.push_str("; ");
        text.push_str(more.trim().trim_start_matches(['-', '*']).trim());
    }
    let text = text.replace("**", "");
    let text = markdown::without_citations(&text);
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

/// The series document: its title, one line, where the meeting stands after the latest
/// session (from its TL;DR) and the sessions below.
fn series_body(title: &str, props: &Properties, body: &str) -> String {
    let fields = [
        ("Decidido hasta ahora", "Decidido"),
        ("Abierto", "Bloqueado"),
        ("Lo que sigue", "Siguiente"),
    ];
    let mut note: Vec<String> = fields
        .iter()
        .filter_map(|(shown, label)| {
            tldr_field(body, label).map(|text| format!("> - **{shown}:** {text}"))
        })
        .collect();
    let mut out = format!(
        "{title}\n\nMinutas de «{}»: una página por sesión.\n",
        props.title
    );
    if !note.is_empty() {
        let date = if props.date.is_empty() {
            "sin fecha"
        } else {
            &props.date
        };
        note.push(format!("> - Actualizado con la minuta del {date}."));
        out.push_str(&format!("\n> [!NOTE] Dónde va\n{}\n", note.join("\n")));
    }
    out.push_str("\n## Sesiones\n");
    out
}

/// What publishing `minutes_md` and its commitments to `target` will create.
pub fn build_plan(minutes_md: &str, commitments: &[CommitmentRecord], target: &Target) -> Plan {
    let (props, body) = parse_properties(minutes_md);
    let series_title = if props.series_title.trim().is_empty() {
        short_title(&props.title)
    } else {
        props.series_title.trim().to_string()
    };
    let when = [props.date.as_str(), props.session.as_str()]
        .into_iter()
        .find(|s| !s.is_empty())
        .map(|s| s.chars().take(10).collect::<String>())
        .unwrap_or_else(|| "sin fecha".to_string());
    let page_title = format!("Minuta {when}");
    let minutes = MinutesPlan {
        folder: target.folder.clone(),
        series_body: series_body(&series_title, &props, &body),
        legacy_series_title: format!("{} — minutas", props.title),
        series_doc: None,
        page_body: to_tisty_markdown(&page_title, &props, &body),
        images: image_refs(&body),
        series_title,
        page_title,
    };
    let tags = task_tags(&props.title, &target.tags);
    let me = target
        .me
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let (mine, theirs): (Vec<&CommitmentRecord>, Vec<&CommitmentRecord>) = commitments
        .iter()
        .partition(|c| me.is_some_and(|me| is_me(&c.who, me)));
    // A task is only what the user explicitly took on.
    let tasks: Vec<TaskPlan> = mine
        .into_iter()
        .filter(|c| !c.inferred)
        .map(|c| task_plan(c, &props, &minutes, target, &tags))
        .collect();
    let others = theirs.len();
    let parent = (!tasks.is_empty()).then(|| parent_plan(&props, &minutes, target, &tags, others));
    let absent = me.is_some_and(|me| !props.participants.iter().any(|p| is_me(p, me)));
    Plan {
        minutes,
        parent,
        tasks,
        others,
        me: me.map(str::to_string),
        absent,
    }
}

fn task_json(t: &TaskPlan) -> Value {
    json!({
        "source": t.source, "title": t.title, "list": t.list, "tags": t.tags,
        "deadline": t.deadline, "description": t.description,
    })
}

impl Plan {
    /// What the person is asked to confirm.
    pub fn preview(&self) -> String {
        let m = &self.minutes;
        let mut text = format!(
            "minutes: page «{}» of «{}» -> folder {} (the document is created if missing)\n",
            m.page_title, m.series_title, m.folder
        );
        match (&self.parent, &self.me) {
            (Some(parent), _) => {
                let list = parent.list.as_deref().unwrap_or("inbox");
                text.push_str(&format!("task: {} -> {list}\n", parent.title));
            }
            (None, None) => text.push_str(
                "tasks: none; set `fusion.me` to your name as the captions show it to get your commitments as tasks\n",
            ),
            (None, Some(me)) => {
                text.push_str(&format!("tasks: none; no commitment is {me}'s\n"));
            }
        }
        for task in &self.tasks {
            let due = task
                .deadline
                .as_deref()
                .map_or(String::new(), |d| format!(", due {d}"));
            text.push_str(&format!("  part: {}{due}\n", task.title));
        }
        if self.others > 0 {
            text.push_str(&format!(
                "{} commitment(s) of other people stay in the minutes only\n",
                self.others
            ));
        }
        if let (true, Some(me)) = (self.absent, &self.me) {
            text.push_str(&format!(
                "note: {me} does not speak in this meeting; if that is wrong, set `fusion.me` to your name as the captions show it\n"
            ));
        }
        text
    }

    pub fn to_json(&self) -> Value {
        let m = &self.minutes;
        json!({
            "minutes": {
                "folder": m.folder, "series_title": m.series_title, "series_body": m.series_body,
                "page_title": m.page_title, "page_body": m.page_body,
            },
            "parent": self.parent.as_ref().map(task_json),
            "tasks": self.tasks.iter().map(task_json).collect::<Vec<_>>(),
            "others": self.others,
            "me": self.me,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commitment(id: &str, who: &str, what: &str, due: Option<&str>) -> CommitmentRecord {
        CommitmentRecord {
            id: id.into(),
            who: who.into(),
            what: what.into(),
            due: due.map(str::to_string),
            evidence: vec![3, 4],
            said: vec!["Ana: yo lo cierro el martes".into()],
            inferred: false,
            source: format!("s1#{id}"),
        }
    }

    const MINUTES: &str = "---\ndate: 2023-11-14\ntitle: \"Daily de equipo\"\nsession: \"s1\"\nparticipants: [\"Ana\", \"Luis\"]\ntags: [\"docker\"]\n---\n# Daily de equipo\n\n## Resumen\n\nHubo bloqueos [#0].\n\n## Compromisos\n\n- [ ] @Ana: cerrar el bug (el martes) [#3]\n";

    fn target() -> Target {
        Target {
            folder: "Reuniones".into(),
            list: Some("Trabajo".into()),
            tags: vec!["equipo".into(), "Reunión".into(), "dos palabras".into()],
            me: Some("Mario Hidalgo".into()),
        }
    }

    #[test]
    fn iso_dates_are_recognised_only_when_they_are_dates() {
        assert_eq!(iso_date("2024-03-05"), Some("2024-03-05".into()));
        assert_eq!(iso_date(" 2024-03-05T10:00 "), Some("2024-03-05".into()));
        assert_eq!(iso_date("el martes"), None);
        assert_eq!(iso_date("2024-13-05"), None);
        assert_eq!(iso_date("2024-03-05x"), None);
        assert_eq!(iso_date("2024-3-5"), None);
    }

    #[test]
    fn tags_are_single_words() {
        assert_eq!(series_tag("Daily de equipo").as_deref(), Some("daily"));
        assert_eq!(
            series_tag("La revisión semanal").as_deref(),
            Some("revision")
        );
        assert_eq!(series_tag("1:1").as_deref(), None);
        assert_eq!(
            task_tags(
                "Daily de equipo",
                &["equipo".into(), "Reunión".into(), "dos palabras".into()]
            ),
            ["reunion", "daily", "equipo", "dos", "palabras"]
        );
    }

    #[test]
    fn the_minutes_are_a_page_of_the_series_document() {
        let plan = build_plan(MINUTES, &[], &target());
        let m = &plan.minutes;
        assert_eq!(m.series_title, "Daily de equipo");
        assert_eq!(m.legacy_series_title, "Daily de equipo — minutas");
        assert_eq!(m.page_title, "Minuta 2023-11-14");
        assert_eq!(m.folder, "Reuniones");
        assert!(m.series_body.starts_with("Daily de equipo\n\n"));
        let lines: Vec<&str> = m.series_body.lines().collect();
        assert_eq!(lines[1], "", "a blank line after the title");
        assert!(lines[2].starts_with("Minutas de «"), "{}", lines[2]);
        assert_eq!(lines.last(), Some(&"## Sesiones"), "{}", m.series_body);
        assert!(
            m.page_body
                .starts_with("Minuta 2023-11-14\n\n## Resumen\n\nHubo bloqueos."),
            "{}",
            m.page_body
        );
    }

    #[test]
    fn only_the_user_s_commitments_become_parts_of_one_meeting_task() {
        let commitments = vec![
            commitment(
                "c1",
                "Mario Hidalgo Rojas",
                "cerrar el bug",
                Some("el martes"),
            ),
            commitment("c2", "Luis", "revisar el PR", Some("2024-03-05")),
            commitment(
                "c3",
                "mario hidalgo",
                "mandar el informe",
                Some("2024-03-05"),
            ),
        ];
        let plan = build_plan(MINUTES, &commitments, &target());
        let parent = plan.parent.as_ref().expect("the user has commitments");
        assert_eq!(
            parent.title,
            "Reunión Daily de equipo (2023-11-14): mis compromisos"
        );
        assert_eq!(parent.source, "meet-copilot:s1");
        assert_eq!(parent.list.as_deref(), Some("Trabajo"));
        assert!(
            parent
                .description
                .contains("El compromiso de otra persona queda solo en la minuta.")
        );
        assert!(
            parent
                .description
                .contains("«Minuta 2023-11-14», página de «Daily de equipo»")
        );
        assert_eq!(plan.others, 1);
        let titles: Vec<&str> = plan.tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["cerrar el bug", "mandar el informe"]);
        let first = &plan.tasks[0];
        assert_eq!(first.source, "s1#c1");
        assert_eq!(
            first.tags,
            ["reunion", "daily", "equipo", "dos", "palabras"],
            "one word each, deduplicated, in order"
        );
        assert_eq!(first.deadline, None);
        assert!(
            first
                .description
                .contains("Plazo dicho en la reunión del 2023-11-14: el martes.")
        );
        assert!(first.description.contains("> Ana: yo lo cierro el martes"));
        assert_eq!(plan.tasks[1].deadline.as_deref(), Some("2024-03-05"));
        assert!(!plan.tasks[1].description.contains("Plazo dicho"));
    }

    #[test]
    fn without_the_user_s_name_or_commitments_nothing_is_proposed() {
        let others = [commitment("c1", "Ana", "cerrar el bug", None)];
        let nameless = build_plan(
            MINUTES,
            &others,
            &Target {
                me: None,
                ..target()
            },
        );
        assert!(nameless.parent.is_none() && nameless.tasks.is_empty());
        assert!(
            nameless.preview().contains("set `fusion.me`"),
            "{}",
            nameless.preview()
        );
        let theirs = build_plan(MINUTES, &others, &target());
        assert!(theirs.parent.is_none());
        let text = theirs.preview();
        assert!(
            text.contains("tasks: none; no commitment is Mario Hidalgo's"),
            "{text}"
        );
        assert!(
            text.contains("1 commitment(s) of other people stay in the minutes only"),
            "{text}"
        );
    }

    #[test]
    fn inferred_commitments_never_become_tasks() {
        let mut c = commitment("c1", "Mario Hidalgo", "x", None);
        c.inferred = true;
        let plan = build_plan(MINUTES, &[c], &target());
        assert!(plan.parent.is_none() && plan.tasks.is_empty());
    }

    #[test]
    fn the_preview_lists_the_minutes_the_meeting_task_and_its_parts() {
        let plan = build_plan(
            MINUTES,
            &[commitment(
                "c1",
                "Mario Hidalgo",
                "cerrar el bug",
                Some("2024-03-05"),
            )],
            &Target {
                list: None,
                ..target()
            },
        );
        let text = plan.preview();
        assert!(
            text.contains(
                "minutes: page «Minuta 2023-11-14» of «Daily de equipo» -> folder Reuniones"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "task: Reunión Daily de equipo (2023-11-14): mis compromisos -> inbox
  part: cerrar el bug, due 2024-03-05"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_report_fails_on_any_failed_or_unknown_item_or_abort() {
        let ok = PublishReport {
            items: vec![Item {
                kind: ItemKind::Task,
                label: "t".into(),
                source: Some("s".into()),
                status: Status::Existed,
            }],
            aborted: None,
            series_doc: None,
        };
        assert!(!ok.failed());
        assert_eq!(ok.to_json()["items"][0]["status"], "existed");
        let mut bad = ok.clone();
        bad.items[0].status = Status::Failed("boom".into());
        assert!(bad.failed());
        assert_eq!(bad.to_json()["items"][0]["reason"], "boom");
        bad.items[0].status = Status::Unknown("cut".into());
        assert!(bad.failed());
        assert_eq!(bad.to_json()["items"][0]["status"], "unknown");
        assert!(bad.human().contains("may have been applied"));
    }
}
