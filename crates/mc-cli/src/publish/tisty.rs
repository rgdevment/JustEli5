//! The Tisty adapter: `tisty mcp` over stdio (no port, no token). The person admits this
//! assistant once in Tisty (Settings > Assistants); until then every call is rejected.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mc_mcp_client::{McpClient, McpError, Timeouts};
use serde_json::{Value, json};

use super::{
    Abort, AbortKind, Item, Plan, PublishReport, Publisher, Status, TaskPlan, page_item,
    series_item, task_item,
};
use crate::cli::{Env, Os};

pub const DEFAULT_COMMAND: &str = "tisty";
const TIMEOUTS: Timeouts = Timeouts {
    init: Duration::from_secs(20),
    call: Duration::from_secs(30),
};
pub const NOT_ADMITTED_HINT: &str =
    "admit JustEli5 in Tisty (Settings > Assistants) and run the command again";

/// Where the installer puts Tisty, when the OS has a known place.
pub fn default_install_path(env: &Env) -> Option<PathBuf> {
    match env.os {
        Os::Windows => env
            .var("LOCALAPPDATA")
            .map(|base| Path::new(base).join("Programs/Tisty/tisty.exe")),
        Os::Mac => Some(PathBuf::from(
            "/Applications/Tisty.app/Contents/MacOS/tisty",
        )),
        Os::Linux => None,
    }
}

/// The configured command (a path, or a name on PATH); the default name also tries the
/// default install, since the installer does not put Tisty on PATH.
pub fn resolve(env: &Env, command: &str) -> Option<PathBuf> {
    let path = env.var("PATH").unwrap_or_default();
    let pathext =
        (env.os == Os::Windows).then(|| env.var("PATHEXT").unwrap_or(".COM;.EXE;.BAT;.CMD"));
    mc_llm::resolve_command(command, &env.cwd, path, pathext).or_else(|| {
        (command == DEFAULT_COMMAND)
            .then(|| default_install_path(env))
            .flatten()
            .filter(|p| p.is_file())
    })
}

/// Only a message that says the assistant is not admitted: other refusals are about the item.
fn is_not_admitted(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("assistant") && text.contains("admit")
}

fn brief(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 300 {
        format!("{}...", flat.chars().take(300).collect::<String>())
    } else {
        flat
    }
}

fn abort(kind: AbortKind, message: impl Into<String>) -> Abort {
    Abort {
        kind,
        message: message.into(),
    }
}

fn not_admitted(detail: &str) -> Abort {
    abort(
        AbortKind::NotAdmitted,
        format!(
            "Tisty has not admitted this assistant ({}); {NOT_ADMITTED_HINT}",
            brief(detail)
        ),
    )
}

fn transport(what: &str, error: &McpError) -> Abort {
    match error {
        McpError::Rpc { message, .. } if is_not_admitted(message) => not_admitted(message),
        _ => abort(AbortKind::Unavailable, format!("{what}: {error}")),
    }
}

/// A tool call that did not return text: Tisty refused it, or the conversation is over.
enum Failure {
    Refused(String),
    Abort(Abort),
}

/// A JSON-RPC error answers one call, so it fails that item; only a dead conversation aborts.
fn call(client: &mut McpClient, tool: &str, args: Value) -> Result<String, Failure> {
    let out = match client.call_tool(tool, args) {
        Ok(out) => out,
        Err(McpError::Rpc { message, .. }) if is_not_admitted(&message) => {
            return Err(Failure::Abort(not_admitted(&message)));
        }
        Err(McpError::Rpc { code, message }) => {
            return Err(Failure::Refused(format!(
                "{} (code {code})",
                brief(&message)
            )));
        }
        Err(e) => return Err(Failure::Abort(transport(tool, &e))),
    };
    match out {
        o if o.is_error && is_not_admitted(&o.text) => Err(Failure::Abort(not_admitted(&o.text))),
        o if o.is_error => Err(Failure::Refused(brief(&o.text))),
        // Tisty's text is written for a person; the data travels in `structuredContent`.
        o => Ok(o.structured.map_or(o.text, |data| data.to_string())),
    }
}

fn start(program: &Path, args: &[String]) -> Result<McpClient, Abort> {
    let mut client = McpClient::spawn(program, args, TIMEOUTS)
        .map_err(|e| abort(AbortKind::Unavailable, e.to_string()))?;
    client
        .initialize()
        .map_err(|e| transport("initialize", &e))?;
    Ok(client)
}

/// Whether Tisty is reachable and this assistant may use it (`mc doctor --tisty`).
#[derive(Debug, PartialEq, Eq)]
pub enum Health {
    Ready { tools: usize },
    NotAdmitted(String),
    MissingTools(Vec<String>),
    Broken(String),
}

pub fn check(program: &Path, args: &[String]) -> Health {
    let mut client = match start(program, args) {
        Ok(client) => client,
        Err(a) if a.kind == AbortKind::NotAdmitted => return Health::NotAdmitted(a.message),
        Err(a) => return Health::Broken(a.message),
    };
    let tools = match client.list_tools() {
        Ok(tools) => tools,
        Err(e) => return Health::Broken(format!("tools/list: {e}")),
    };
    let missing: Vec<String> = ["write_doc", "propose"]
        .iter()
        .filter(|name| !tools.iter().any(|t| t.name == **name))
        .map(|name| name.to_string())
        .collect();
    if !missing.is_empty() {
        return Health::MissingTools(missing);
    }
    // `rules` is harmless and is rejected until the assistant is admitted.
    let health = match call(&mut client, "rules", json!({})) {
        Ok(_) | Err(Failure::Refused(_)) => Health::Ready { tools: tools.len() },
        Err(Failure::Abort(a)) if a.kind == AbortKind::NotAdmitted => {
            Health::NotAdmitted(a.message)
        }
        Err(Failure::Abort(a)) => Health::Broken(a.message),
    };
    client.close();
    health
}

pub struct TistyPublisher {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl Publisher for TistyPublisher {
    fn publish(&self, plan: &Plan) -> PublishReport {
        let mut report = PublishReport::default();
        match self.run(plan, &mut report) {
            Ok(()) => report,
            Err(stop) => report.stopped_by(plan, stop),
        }
    }
}

impl TistyPublisher {
    fn run(&self, plan: &Plan, report: &mut PublishReport) -> Result<(), Abort> {
        let mut client = start(&self.program, &self.args)?;
        require(&mut client, "docs", &plan.minutes.folder, "folder")?;
        let lists = plan.parent.as_ref().and_then(|t| t.list.as_deref());
        if let Some(list) = lists {
            require(&mut client, "lists", list, "list")?;
        }
        let series = ensure_series(&mut client, plan, report)?;
        report.series_doc.clone_from(&series);
        let written = match series {
            Some(series) => ensure_page(&mut client, plan, &series, report)?,
            None => {
                report.items.push(page_item(
                    plan,
                    Status::Failed("the document of the series is not available".to_string()),
                ));
                false
            }
        };
        if !written {
            client.close();
            return Err(abort(
                AbortKind::MinutesNotWritten,
                "the minutes page was not written, so no task was proposed (each one points at it)",
            ));
        }
        propose_tasks(&mut client, plan, report)?;
        client.close();
        Ok(())
    }
}

/// A change to Tisty. If the conversation dies during it, `in_flight` is reported as
/// unknown: it may have been applied. A refusal comes back as the inner `Err`.
fn mutate(
    client: &mut McpClient,
    tool: &str,
    args: Value,
    in_flight: Vec<Item>,
    report: &mut PublishReport,
) -> Result<Result<String, String>, Abort> {
    match call(client, tool, args) {
        Ok(text) => Ok(Ok(text)),
        Err(Failure::Refused(text)) => Ok(Err(text)),
        Err(Failure::Abort(stop)) => {
            if stop.kind == AbortKind::Unavailable {
                for mut item in in_flight {
                    item.status = Status::Unknown(stop.message.clone());
                    report.items.push(item);
                }
            }
            Err(stop)
        }
    }
}

fn same_title(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// The id of the document called `title` among those `docs` lists for `args`, page by page.
fn lookup(client: &mut McpClient, args: Value, title: &str) -> Result<Option<String>, Failure> {
    let mut seen = 0;
    loop {
        let mut page = args.clone();
        page["after"] = json!(seen);
        let text = call(client, "docs", page)?;
        let answer: Value = serde_json::from_str(&text).map_err(|_| {
            Failure::Refused("Tisty's list of documents could not be read".to_string())
        })?;
        let docs = answer["docs"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let hit = docs
            .iter()
            .find(|d| d["title"].as_str().is_some_and(|t| same_title(t, title)))
            .and_then(|d| d["doc"].as_str().or_else(|| d["id"].as_str()));
        if let Some(id) = hit {
            return Ok(Some(id.to_string()));
        }
        seen += docs.len();
        let total = answer["total"].as_u64().unwrap_or(0) as usize;
        if docs.is_empty() || seen >= total {
            return Ok(None);
        }
    }
}

/// The id Tisty gives a document it has just written.
fn written_id(text: &str) -> Option<String> {
    let answer: Value = serde_json::from_str(text).ok()?;
    [&answer["id"], &answer["doc"], &answer["doc"]["id"]]
        .into_iter()
        .find_map(|v| v.as_str())
        .map(str::to_string)
}

/// The series document already in Tisty: by the id a previous publish kept, else by its short
/// title or the one series documents had before (`… — minutas`).
fn find_series(client: &mut McpClient, plan: &Plan) -> Result<Option<String>, Failure> {
    let m = &plan.minutes;
    if let Some(id) = &m.series_doc {
        match call(client, "read_doc", json!({"doc": id, "chars": 200})) {
            Ok(_) => return Ok(Some(id.clone())),
            Err(Failure::Abort(stop)) => return Err(Failure::Abort(stop)),
            // Deleted or moved away: found again by title, or written anew.
            Err(Failure::Refused(_)) => {}
        }
    }
    let in_folder = json!({"folder": m.folder, "limit": 200});
    for title in [&m.series_title, &m.legacy_series_title] {
        if let Some(id) = lookup(client, in_folder.clone(), title)? {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Writes the series document again with this publish's «Dónde va», keeping its title (the
/// user may have renamed it) and its pages, which Tisty puts back at the end.
fn refresh_series(
    client: &mut McpClient,
    plan: &Plan,
    id: &str,
    report: &mut PublishReport,
) -> Result<(), Abort> {
    let read = match call(client, "read_doc", json!({"doc": id})) {
        Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or_default(),
        Err(Failure::Abort(stop)) => return Err(stop),
        Err(Failure::Refused(_)) => Value::Null,
    };
    let (Some(body), Some(print)) = (read["body"].as_str(), read["print"].as_str()) else {
        return Ok(());
    };
    let title = body.lines().next().unwrap_or_default();
    let rest = plan
        .minutes
        .series_body
        .split_once('\n')
        .map_or("", |(_, r)| r);
    let args = json!({"doc": id, "print": print, "body": format!("{title}\n{rest}")});
    if let Err(why) = mutate(client, "write_doc", args, Vec::new(), report)? {
        let status = Status::Failed(format!("«Dónde va» was not updated: {why}"));
        report.items.push(series_item(plan, status));
    }
    Ok(())
}

/// Finds the document of the meeting series, or writes it. A refusal because the title is
/// taken (same subject elsewhere) is a reason to find and reuse, never a failure.
fn ensure_series(
    client: &mut McpClient,
    plan: &Plan,
    report: &mut PublishReport,
) -> Result<Option<String>, Abort> {
    let m = &plan.minutes;
    let in_folder = json!({"folder": m.folder, "limit": 200});
    let found = match find_series(client, plan) {
        Ok(found) => found,
        Err(Failure::Abort(stop)) => return Err(stop),
        Err(Failure::Refused(why)) => {
            report.items.push(series_item(
                plan,
                Status::Failed(format!("could not look for it: {why}")),
            ));
            return Ok(None);
        }
    };
    if let Some(id) = &found {
        report.items.push(series_item(plan, Status::Existed));
        refresh_series(client, plan, id, report)?;
        return Ok(found);
    }
    let args = json!({"body": m.series_body, "folder": m.folder});
    let flight = vec![series_item(plan, Status::Created)];
    match mutate(client, "write_doc", args, flight, report)? {
        Ok(text) => {
            report.items.push(series_item(plan, Status::Created));
            match written_id(&text) {
                Some(id) => Ok(Some(id)),
                None => Ok(lookup(client, in_folder, &m.series_title).unwrap_or(None)),
            }
        }
        Err(refusal) => match lookup(client, json!({"limit": 200}), &m.series_title) {
            Ok(Some(id)) => {
                report.items.push(series_item(plan, Status::Existed));
                Ok(Some(id))
            }
            Ok(None) | Err(Failure::Refused(_)) => {
                report
                    .items
                    .push(series_item(plan, Status::Failed(refusal)));
                Ok(None)
            }
            Err(Failure::Abort(stop)) => Err(stop),
        },
    }
}

/// This meeting's minutes: a page of the series, unless one with that title is there already.
fn ensure_page(
    client: &mut McpClient,
    plan: &Plan,
    series: &str,
    report: &mut PublishReport,
) -> Result<bool, Abort> {
    let m = &plan.minutes;
    let pages = json!({"page_of": series, "limit": 200});
    let found = match lookup(client, pages.clone(), &m.page_title) {
        Ok(found) => found,
        Err(Failure::Abort(stop)) => return Err(stop),
        Err(Failure::Refused(why)) => {
            let status = Status::Failed(format!("could not look for it: {why}"));
            report.items.push(page_item(plan, status));
            return Ok(false);
        }
    };
    if found.is_some() {
        report.items.push(page_item(plan, Status::Existed));
        return Ok(true);
    }
    let args = json!({"body": m.page_body, "page_of": series});
    let flight = vec![page_item(plan, Status::Created)];
    let status = match mutate(client, "write_doc", args, flight, report)? {
        Ok(text) => {
            if let Some(page) = written_id(&text) {
                attach_images(client, plan, &page, report)?;
            }
            Status::Created
        }
        Err(refusal) => match lookup(client, pages, &m.page_title) {
            Ok(Some(_)) => Status::Existed,
            Ok(None) | Err(Failure::Refused(_)) => Status::Failed(refusal),
            Err(Failure::Abort(stop)) => return Err(stop),
        },
    };
    let written = matches!(status, Status::Created | Status::Existed);
    report.items.push(page_item(plan, status));
    Ok(written)
}

/// The pictures of the notes, at the end of a page just written (a page that existed has them).
fn attach_images(
    client: &mut McpClient,
    plan: &Plan,
    page: &str,
    report: &mut PublishReport,
) -> Result<(), Abort> {
    for (label, path) in &plan.minutes.images {
        let args = json!({"doc": page, "path": path, "label": label});
        if let Err(why) = mutate(client, "attach", args, Vec::new(), report)? {
            let status = Status::Failed(format!("the picture {path} was not attached: {why}"));
            report.items.push(page_item(plan, status));
        }
    }
    Ok(())
}

/// The meeting's task with the user's commitments as its parts, in one call. Tisty answers a
/// known `source` with `proposed: false` and the task's id: the parts then go in under it one
/// by one (each with its own source), so publishing again only adds what is new.
fn propose_tasks(
    client: &mut McpClient,
    plan: &Plan,
    report: &mut PublishReport,
) -> Result<(), Abort> {
    let Some(parent) = &plan.parent else {
        return Ok(());
    };
    let mut args = propose_args(parent);
    args["parts"] = json!(plan.tasks.iter().map(part_args).collect::<Vec<_>>());
    let flight = std::iter::once(parent)
        .chain(&plan.tasks)
        .map(|t| task_item(t, Status::Created))
        .collect();
    let answer = match mutate(client, "propose", args, flight, report)? {
        Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
        Err(refusal) => {
            for task in std::iter::once(parent).chain(&plan.tasks) {
                report
                    .items
                    .push(task_item(task, Status::Failed(refusal.clone())));
            }
            return Ok(());
        }
    };
    let (status, parts) = parent_outcome(&answer, plan.tasks.len());
    report.items.push(task_item(parent, status));
    let statuses = match (parts, answer["id"].as_str()) {
        (Some(statuses), _) => statuses,
        (None, Some(id)) => propose_parts(client, plan, id, report)?,
        (None, None) => {
            let why = "not sent: the meeting's task is not in Tisty".to_string();
            vec![Status::Failed(why); plan.tasks.len()]
        }
    };
    for (task, status) in plan.tasks.iter().zip(statuses) {
        report.items.push(task_item(task, status));
    }
    Ok(())
}

/// The meeting's task as Tisty answered it, and its parts' statuses when that answer settles
/// them (`None`: the task was already there, so its parts still have to be offered).
fn parent_outcome(answer: &Value, parts: usize) -> (Status, Option<Vec<Status>>) {
    let unreadable = "Tisty's answer could not be read; sending it again is safe";
    match answer["proposed"].as_bool() {
        Some(true) => {
            let filed = answer["parts"].as_array().map_or(0, Vec::len);
            let status = if filed == parts {
                Status::Created
            } else {
                Status::Unknown(format!("Tisty filed {filed} of {parts} parts"))
            };
            (Status::Created, Some(vec![status; parts]))
        }
        Some(false) if answer["erased"] == json!(true) => {
            let why = "the person erased the meeting's task in Tisty".to_string();
            (
                Status::Failed(why.clone()),
                Some(vec![Status::Failed(why); parts]),
            )
        }
        Some(false) if answer["id"].is_string() => (Status::Existed, None),
        Some(false) => {
            let why = answer["said"]
                .as_str()
                .map_or("Tisty did not file the meeting's task".to_string(), brief);
            (
                Status::Failed(why.clone()),
                Some(vec![Status::Failed(why); parts]),
            )
        }
        None => (
            Status::Unknown(unreadable.to_string()),
            Some(vec![Status::Unknown(unreadable.to_string()); parts]),
        ),
    }
}

/// The parts under a meeting's task that was filed before: those already there say so.
fn propose_parts(
    client: &mut McpClient,
    plan: &Plan,
    parent: &str,
    report: &mut PublishReport,
) -> Result<Vec<Status>, Abort> {
    if plan.tasks.is_empty() {
        return Ok(Vec::new());
    }
    let tasks: Vec<Value> = plan
        .tasks
        .iter()
        .map(|t| {
            let mut args = part_args(t);
            args["part_of"] = json!(parent);
            args
        })
        .collect();
    let flight = plan
        .tasks
        .iter()
        .map(|t| task_item(t, Status::Created))
        .collect();
    Ok(
        match mutate(client, "propose", json!({"tasks": tasks}), flight, report)? {
            Ok(text) => task_statuses(&text, plan.tasks.len()),
            Err(refusal) => vec![Status::Failed(refusal); plan.tasks.len()],
        },
    )
}

/// One status per task, in order, from the answer to a `propose` with `tasks`.
fn task_statuses(text: &str, count: usize) -> Vec<Status> {
    let unreadable = || {
        let why = "Tisty's answer could not be matched to the tasks; sending them again is safe";
        vec![Status::Unknown(why.to_string()); count]
    };
    let Ok(answer) = serde_json::from_str::<Value>(text) else {
        return unreadable();
    };
    let entries: Vec<&Value> = match ["tasks", "results"]
        .iter()
        .find_map(|key| answer[*key].as_array())
        .or_else(|| answer.as_array())
    {
        Some(list) => list.iter().collect(),
        None if answer.get("proposed").is_some() => vec![&answer],
        None => return unreadable(),
    };
    if entries.len() != count {
        return unreadable();
    }
    entries
        .into_iter()
        .map(|entry| {
            if let Some(why) = ["error", "refused"].iter().find_map(|k| entry[*k].as_str()) {
                Status::Failed(brief(why))
            } else {
                match entry["proposed"].as_bool() {
                    Some(true) => Status::Created,
                    Some(false) => Status::Existed,
                    None => Status::Unknown("Tisty did not say if it was proposed".to_string()),
                }
            }
        })
        .collect()
}

fn propose_args(task: &TaskPlan) -> Value {
    let mut args = json!({
        "title": task.title,
        "description": task.description,
        "tags": task.tags,
        "source": task.source,
    });
    if let Some(list) = &task.list {
        args["list"] = json!(list);
    }
    if let Some(deadline) = &task.deadline {
        args["deadline"] = json!(deadline);
    }
    args
}

/// A part takes the list of the task around it.
fn part_args(task: &TaskPlan) -> Value {
    let mut args = propose_args(task);
    if let Some(fields) = args.as_object_mut() {
        fields.remove("list");
    }
    args
}

/// The destination names are checked before anything is written, so a typo creates nothing.
fn require(client: &mut McpClient, tool: &str, name: &str, what: &str) -> Result<(), Abort> {
    // `docs` names every folder only when asked for them.
    let args = if tool == "docs" {
        json!({"folders": true, "limit": 1})
    } else {
        json!({})
    };
    let listing = match call(client, tool, args) {
        Ok(text) => text,
        Err(Failure::Abort(a)) => return Err(a),
        // Tisty will refuse an unknown name itself when the write comes.
        Err(Failure::Refused(_)) => return Ok(()),
    };
    if let Some(paths) = shared_folder(&listing, name) {
        return Err(abort(
            AbortKind::AmbiguousTarget,
            format!(
                "{name:?} is the name of several Tisty folders; set the whole path: {}",
                paths.join(" or ")
            ),
        ));
    }
    if listing_has(&listing, name) {
        return Ok(());
    }
    Err(abort(
        AbortKind::MissingTarget,
        format!(
            "the {what} {name:?} does not exist in Tisty (it has: {}); create it there or choose another",
            brief(&listing)
        ),
    ))
}

/// Whether a `lists` or `docs` answer names `name` (a list, or a folder by name or whole path).
fn listing_has(listing: &str, name: &str) -> bool {
    let wanted = name.trim().to_lowercase();
    let Ok(value) = serde_json::from_str::<Value>(listing) else {
        return listing.to_lowercase().contains(&wanted);
    };
    let lists = value["lists"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    let folders = value["folders"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|f| [f["path"].as_str(), f["folder"].as_str()])
        .flatten();
    lists
        .chain(folders)
        .any(|n| n.trim().to_lowercase() == wanted)
}

/// The whole paths of the folders called `name`, when there are several and `name` is not
/// itself one of their paths.
fn shared_folder(listing: &str, name: &str) -> Option<Vec<String>> {
    let wanted = name.trim().to_lowercase();
    let value: Value = serde_json::from_str(listing).ok()?;
    let folders = value["folders"].as_array()?;
    let is = |v: &Value| {
        v.as_str()
            .is_some_and(|s| s.trim().to_lowercase() == wanted)
    };
    if folders.iter().any(|f| is(&f["path"])) {
        return None;
    }
    let paths: Vec<String> = folders
        .iter()
        .filter(|f| is(&f["folder"]))
        .filter_map(|f| f["path"].as_str().map(str::to_string))
        .collect();
    (paths.len() > 1).then_some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_name_shared_by_two_folders_needs_its_whole_path() {
        let docs = r#"{"docs":[],"folders":[{"folder":"Meets","path":"Personal / Proyectos / Meets"},{"folder":"Meets","path":"Trabajo / Meets"}],"total":0}"#;
        assert_eq!(
            shared_folder(docs, "meets"),
            Some(vec![
                "Personal / Proyectos / Meets".to_string(),
                "Trabajo / Meets".to_string()
            ])
        );
        assert_eq!(shared_folder(docs, "Trabajo / Meets"), None);
        assert_eq!(shared_folder(docs, "Otra"), None);
    }

    #[test]
    fn the_answer_to_a_batch_propose_is_read_task_by_task() {
        // Tisty's `structuredContent` for `propose` with `tasks` (tisty-cli mcp/chores.rs).
        let answer = r#"{"tasks":[
            {"id":"01A","title":"a","proposed":true,"for_the_person":"I added the task \"a\" to Trabajo."},
            {"id":"01B","title":"b","proposed":false},
            {"title":"c","proposed":false,"refused":"No list called \"X\"."}],
            "written":1,"refused":1,"said":"1 written, 1 already there, 1 turned away."}"#;
        assert_eq!(
            task_statuses(answer, 3),
            vec![
                Status::Created,
                Status::Existed,
                Status::Failed("No list called \"X\".".to_string())
            ]
        );
    }

    #[test]
    fn the_meeting_task_and_its_parts_are_read_from_tisty_s_answer() {
        // Shapes of `propose` with `parts` (tisty-cli mcp/chores.rs `proposed`).
        let filed = serde_json::json!({"id":"01P","title":"t","proposed":true,
            "parts":[{"id":"01Q","title":"a"},{"id":"01R","title":"b"}]});
        assert_eq!(
            parent_outcome(&filed, 2),
            (Status::Created, Some(vec![Status::Created; 2]))
        );
        let known = serde_json::json!({"id":"01P","title":"t","proposed":false});
        assert_eq!(parent_outcome(&known, 2), (Status::Existed, None));
        let erased = serde_json::json!({"id":"01P","proposed":false,"erased":true});
        assert!(matches!(parent_outcome(&erased, 1), (Status::Failed(_), Some(s)) if s.len() == 1));
        let collided = serde_json::json!({"id":null,"proposed":false,"collided":"01Z"});
        assert!(matches!(parent_outcome(&collided, 1).0, Status::Failed(_)));
    }

    #[test]
    fn real_tisty_listings_are_understood() {
        // Shapes copied from Tisty's own answers (21/10/2026).
        let lists = r#"{"lists":["Personal","Meet Project"],"said":"..."}"#;
        assert!(listing_has(lists, "meet project"));
        assert!(!listing_has(lists, "Meet"));
        let docs = r#"{"docs":[],"folders":[{"folder":"Meets","path":"Personal / Proyectos / Meets"}],"total":1}"#;
        assert!(listing_has(docs, "Personal / Proyectos / Meets"));
        assert!(listing_has(docs, "Meets"));
        assert!(!listing_has(docs, "Proyectos"));
    }

    #[test]
    fn batch_answers_are_mapped_task_by_task() {
        let answer = r#"{"tasks":[
            {"proposed":true,"title":"a"},
            {"proposed":false,"said":"Already proposed from that source","title":"b"},
            {"proposed":false,"error":"no such list","title":"c"}]}"#;
        assert_eq!(
            task_statuses(answer, 3),
            [
                Status::Created,
                Status::Existed,
                Status::Failed("no such list".to_string())
            ]
        );
        let single = r#"{"id":"01","proposed":true,"title":"a"}"#;
        assert_eq!(task_statuses(single, 1), [Status::Created]);
        // A count that does not match, or no JSON, is never taken as a success.
        assert!(matches!(task_statuses(answer, 2)[0], Status::Unknown(_)));
        assert!(matches!(task_statuses("ok", 1)[0], Status::Unknown(_)));
    }

    #[test]
    fn only_a_message_about_the_assistant_not_being_admitted_stops_everything() {
        assert!(is_not_admitted("This assistant is not admitted yet"));
        assert!(!is_not_admitted("Unknown folder"));
        assert!(!is_not_admitted("Editing this is not allowed"));
        assert!(!is_not_admitted("the page was not admitted to the folder"));
    }

    #[test]
    fn written_ids_are_read_from_the_answer() {
        assert_eq!(
            written_id(r#"{"id":"q7-0001"}"#).as_deref(),
            Some("q7-0001")
        );
        assert_eq!(
            written_id(r#"{"doc":"q7-0002"}"#).as_deref(),
            Some("q7-0002")
        );
        assert_eq!(written_id("Document written").as_deref(), None);
    }

    #[test]
    fn long_texts_are_cut_and_flattened() {
        let text = "a\n".repeat(400);
        let out = brief(&text);
        assert!(out.ends_with("...") && !out.contains('\n'));
    }

    #[test]
    fn the_default_install_depends_on_the_os() {
        let env = |os, vars: &[(&str, &str)]| Env {
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            os,
            cwd: PathBuf::new(),
            stdin_tty: false,
            stdout_tty: false,
        };
        let windows = default_install_path(&env(Os::Windows, &[("LOCALAPPDATA", "/l")])).unwrap();
        assert!(windows.ends_with("Programs/Tisty/tisty.exe"), "{windows:?}");
        assert!(default_install_path(&env(Os::Windows, &[])).is_none());
        assert_eq!(
            default_install_path(&env(Os::Mac, &[])).unwrap(),
            PathBuf::from("/Applications/Tisty.app/Contents/MacOS/tisty")
        );
        assert!(default_install_path(&env(Os::Linux, &[])).is_none());
    }
}
