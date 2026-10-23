//! Test-only MCP server that behaves like a scripted Tisty. Options:
//! `--state FILE` keeps documents and tasks between runs, `--folders A,B`, `--lists A,B`,
//! `--not-admitted` rejects every tool call, `--hang` never answers a tool call,
//! `--no-propose` leaves `propose` out of the tool list, `--fail-title TEXT` makes `propose`
//! fail for titles containing TEXT, `--rpc-error-title TEXT` answers a JSON-RPC error to a `propose`
//! with such a title, `--garbage` prints a non-UTF-8 line before each answer, `--deaf` stops reading
//! stdin after `initialize`, `--die-on TOOL` exits when that tool is called, `--exit-on-init` dies right after reading `initialize`.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

struct Script {
    state: Option<String>,
    folders: Vec<String>,
    lists: Vec<String>,
    not_admitted: bool,
    hang: bool,
    no_propose: bool,
    fail_title: Option<String>,
    exit_on_init: bool,
    rpc_error_title: Option<String>,
    garbage: bool,
    deaf: bool,
    die_on: Option<String>,
}

fn parse() -> Script {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let list = |name: &str, default: &str| -> Vec<String> {
        value(name)
            .unwrap_or_else(|| default.to_string())
            .split(',')
            .map(str::to_string)
            .collect()
    };
    Script {
        state: value("--state"),
        folders: list("--folders", "Reuniones"),
        lists: list("--lists", "Tareas"),
        not_admitted: args.iter().any(|a| a == "--not-admitted"),
        hang: args.iter().any(|a| a == "--hang"),
        no_propose: args.iter().any(|a| a == "--no-propose"),
        fail_title: value("--fail-title"),
        exit_on_init: args.iter().any(|a| a == "--exit-on-init"),
        rpc_error_title: value("--rpc-error-title"),
        garbage: args.iter().any(|a| a == "--garbage"),
        deaf: args.iter().any(|a| a == "--deaf"),
        die_on: value("--die-on"),
    }
}

fn load(script: &Script) -> Value {
    script
        .state
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({"docs": [], "tasks": []}))
}

fn save(script: &Script, state: &Value) {
    if let Some(path) = &script.state {
        std::fs::write(path, state.to_string()).expect("write the state");
    }
}

fn text(message: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": is_error})
}

/// Tisty's answer: a line for a person in the text, the data in `structuredContent`.
fn told(message: &str, data: Value) -> Value {
    json!({"content": [{"type": "text", "text": message}], "structuredContent": data})
}

/// The folder a name or a whole path points at; a name shared by several is refused, as in Tisty.
fn folder_of<'a>(script: &'a Script, wanted: &str) -> Result<&'a str, String> {
    if let Some(path) = script.folders.iter().find(|f| *f == wanted) {
        return Ok(path);
    }
    let named: Vec<&String> = script
        .folders
        .iter()
        .filter(|f| f.rsplit(" / ").next() == Some(wanted))
        .collect();
    match named.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("Unknown folder {wanted:?}.")),
        many => Err(format!(
            "{wanted:?} is the name of {} folders. Send the whole path, or the id.",
            many.len()
        )),
    }
}

fn tool_names(script: &Script) -> Vec<&'static str> {
    let mut names = vec![
        "rules",
        "docs",
        "find",
        "write_doc",
        "read_doc",
        "attach",
        "lists",
        "language",
    ];
    if !script.no_propose {
        names.push("propose");
    }
    names
}

/// One task of a `propose`, judged on its own; the answer has Tisty's shape.
fn propose_one(script: &Script, tasks: &mut Vec<Value>, task: &Value) -> Value {
    let title = task["title"].as_str().unwrap_or_default();
    let source = task["source"].as_str().unwrap_or_default();
    if let Some(list) = task["list"].as_str()
        && !script.lists.iter().any(|l| l == list)
    {
        return json!({"proposed": false, "refused": format!("Unknown list {list:?}."), "title": title});
    }
    if script
        .fail_title
        .as_deref()
        .is_some_and(|t| title.contains(t))
    {
        return json!({"proposed": false, "refused": "The task could not be saved.", "title": title});
    }
    // A repeated source is an answer, not an error.
    if let Some(known) = tasks.iter().find(|t| t["source"] == source) {
        return json!({"id": known["id"], "proposed": false, "title": title});
    }
    let id = format!("t{:04}", tasks.len() + 1);
    let mut kept = task.clone();
    kept["id"] = json!(id);
    tasks.push(kept);
    json!({"id": id, "proposed": true, "title": title, "for_the_person": format!("I added the task {title:?}.")})
}

/// A single `propose`, maybe with `parts`, answered as Tisty does: a known source gives the
/// task's id back; a part that cannot be filed refuses the whole call.
fn propose_whole(script: &Script, tasks: &mut Vec<Value>, task: &Value, parts: &[Value]) -> Value {
    let title = task["title"].as_str().unwrap_or_default();
    if let Some(known) = tasks.iter().find(|t| t["source"] == task["source"]) {
        let said = format!("Already proposed from that source: {title:?}. Nothing was written.");
        return told(
            &said,
            json!({"id": known["id"], "title": title, "proposed": false}),
        );
    }
    let failing = script.fail_title.as_deref().is_some_and(|bad| {
        std::iter::once(task)
            .chain(parts)
            .any(|t| t["title"].as_str().is_some_and(|x| x.contains(bad)))
    });
    if failing {
        return text("The task could not be saved. Nothing was written.", true);
    }
    let id = format!("t{:04}", tasks.len() + 1);
    let mut kept = task.clone();
    kept["id"] = json!(id);
    if let Some(fields) = kept.as_object_mut() {
        fields.remove("parts");
    }
    tasks.push(kept);
    let mut filed = Vec::new();
    for part in parts {
        let part_id = format!("t{:04}", tasks.len() + 1);
        let mut one = part.clone();
        one["id"] = json!(part_id);
        one["part_of"] = json!(id);
        tasks.push(one);
        filed.push(json!({"id": part_id, "title": part["title"]}));
    }
    told(
        &format!("I added the task {title:?}."),
        json!({"id": id, "title": title, "proposed": true, "parts": filed}),
    )
}

fn docs_answer(script: &Script, state: &Value, args: &Value) -> Value {
    let wanted = |key: &str| args[key].as_str();
    let docs: Vec<Value> = state["docs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| match wanted("page_of") {
            Some(parent) => d["page_of"] == parent,
            // Pages are left out of a folder listing, as in Tisty.
            None => {
                d["page_of"].is_null()
                    && wanted("folder")
                        .is_none_or(|f| folder_of(script, f).ok() == d["folder"].as_str())
            }
        })
        .map(|d| json!({"doc": d["id"], "title": d["title"], "folder": d["folder"]}))
        .collect();
    let total = docs.len();
    let after = args["after"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(50) as usize;
    let docs: Vec<Value> = docs.into_iter().skip(after).take(limit).collect();
    let mut answer = json!({"docs": docs, "total": total});
    if args["folders"] == json!(true) {
        answer["folders"] = script
            .folders
            .iter()
            .map(|path| json!({"folder": path.rsplit(" / ").next(), "path": path}))
            .collect();
    }
    answer
}

/// What Tisty calls a print: anything that changes with the body.
fn print_of(body: &str) -> String {
    format!("p{}-{}", body.len(), body.lines().count())
}

fn read_doc(state: &Value, args: &Value) -> Value {
    let found = state["docs"]
        .as_array()
        .and_then(|docs| docs.iter().find(|d| d["id"] == args["doc"]));
    match found {
        Some(doc) => {
            let body = doc["body"].as_str().unwrap_or_default();
            told(
                "The document.",
                json!({"doc": doc["id"], "body": body, "print": print_of(body)}),
            )
        }
        None => text("No such document.", true),
    }
}

fn write_doc(script: &Script, state: &mut Value, args: &Value) -> Value {
    let body = args["body"].as_str().unwrap_or_default();
    let title = body.lines().next().unwrap_or_default().to_string();
    let docs = state["docs"].as_array_mut().expect("docs");
    if let Some(id) = args["doc"].as_str() {
        let Some(doc) = docs.iter_mut().find(|d| d["id"] == id) else {
            return text("No such document.", true);
        };
        if args["print"].as_str() != Some(&print_of(doc["body"].as_str().unwrap_or_default())) {
            return text("The document moved on: read it again.", true);
        }
        doc["body"] = json!(body);
        doc["title"] = json!(title);
        save(script, state);
        return told("I wrote it again.", json!({"doc": id, "title": title}));
    }
    let mut doc = args.clone();
    match args["page_of"].as_str() {
        Some(parent) => {
            if !docs.iter().any(|d| d["id"] == parent) {
                return text(&format!("No document {parent:?}."), true);
            }
            if docs
                .iter()
                .any(|d| d["page_of"] == parent && d["title"] == title.as_str())
            {
                return text(
                    &format!("A page titled {title:?} is already taken here."),
                    true,
                );
            }
        }
        None => {
            let folder = match folder_of(script, args["folder"].as_str().unwrap_or_default()) {
                Ok(path) => path,
                Err(why) => return text(&why, true),
            };
            doc["folder"] = json!(folder);
            if let Some(same) = docs
                .iter()
                .find(|d| d["page_of"].is_null() && d["title"] == title.as_str())
            {
                let id = &same["id"];
                return text(
                    &format!("A title already taken: {title:?} is the document {id}."),
                    true,
                );
            }
        }
    }
    let id = format!("d{:04}", docs.len() + 1);
    doc["id"] = json!(id);
    doc["title"] = json!(title);
    docs.push(doc);
    save(script, state);
    told(
        &format!("I wrote the document {title:?}."),
        json!({"doc": id, "title": title}),
    )
}

fn call(script: &Script, name: &str, args: &Value) -> Result<Value, (i64, &'static str)> {
    if script.not_admitted {
        return Ok(text(
            "This assistant is not admitted yet. Ask the person to admit it in Settings > Assistants.",
            true,
        ));
    }
    let mut state = load(script);
    Ok(match name {
        "rules" => text("Write in the person's language.", false),
        "language" => text("Tisty Markdown: the first line is the title.", false),
        "docs" => told("Documents and folders.", docs_answer(script, &state, args)),
        "lists" => told(&script.lists.join(", "), json!({"lists": script.lists})),
        "find" => {
            let source = args["source"].as_str().unwrap_or_default();
            let found = state["tasks"]
                .as_array()
                .is_some_and(|t| t.iter().any(|x| x["source"] == source));
            text(if found { "1 task" } else { "nothing found" }, false)
        }
        "write_doc" => write_doc(script, &mut state, args),
        "read_doc" => read_doc(&state, args),
        "attach" => {
            let attached = state["attached"].as_array().cloned().unwrap_or_default();
            let mut attached = attached;
            attached.push(args.clone());
            state["attached"] = json!(attached);
            save(script, &state);
            told("Attached.", json!({"attached": true}))
        }
        "propose" => {
            let parts: Vec<Value> = args["parts"].as_array().cloned().unwrap_or_default();
            let batch: Vec<Value> = match args["tasks"].as_array() {
                Some(tasks) => tasks.clone(),
                None => vec![args.clone()],
            };
            let titled = |bad: &str| {
                batch
                    .iter()
                    .chain(&parts)
                    .any(|t| t["title"].as_str().is_some_and(|x| x.contains(bad)))
            };
            if script.rpc_error_title.as_deref().is_some_and(titled) {
                return Err((-32602, "invalid params"));
            }
            let tasks = state["tasks"].as_array_mut().expect("tasks");
            if args["tasks"].is_null() {
                let answer = propose_whole(script, tasks, args, &parts);
                save(script, &state);
                return Ok(answer);
            }
            let results: Vec<Value> = batch
                .iter()
                .map(|t| propose_one(script, tasks, t))
                .collect();
            save(script, &state);
            let written = results.iter().filter(|r| r["proposed"] == true).count();
            let refused = results.iter().filter(|r| r["refused"].is_string()).count();
            told(
                &format!(
                    "{written} written, {} already there, {refused} turned away.",
                    results.len() - written - refused
                ),
                json!({"tasks": results, "written": written, "refused": refused}),
            )
        }
        other => text(&format!("Unknown tool {other}."), true),
    })
}

fn reply(id: &Value, result: Result<Value, (i64, &str)>) -> Value {
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    }
}

fn main() {
    let script = parse();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id") else {
            continue; // a notification
        };
        let params = &request["params"];
        let answer = match request["method"].as_str().unwrap_or_default() {
            "initialize" => {
                if script.exit_on_init {
                    eprintln!("fake server: fatal start-up error");
                    std::process::exit(7);
                }
                reply(
                    id,
                    Ok(json!({
                        "protocolVersion": params["protocolVersion"],
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "fake-tisty", "version": "0"},
                    })),
                )
            }
            "tools/list" => {
                let tools: Vec<Value> = tool_names(&script)
                    .into_iter()
                    .map(|n| json!({"name": n, "description": format!("the {n} tool")}))
                    .collect();
                reply(id, Ok(json!({"tools": tools})))
            }
            "tools/call" => {
                if script.hang {
                    std::thread::sleep(std::time::Duration::from_secs(120));
                }
                let name = params["name"].as_str().unwrap_or_default();
                if script.die_on.as_deref() == Some(name) {
                    std::process::exit(9);
                }
                reply(id, call(&script, name, &params["arguments"]))
            }
            _ => reply(id, Err((-32601, "unknown method"))),
        };
        if script.garbage {
            let _ = stdout.write_all(&[0xFF, 0xFE, b' ', b'x', 0x0A]);
        }
        if writeln!(stdout, "{answer}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
        if script.deaf && request["method"] == "initialize" {
            std::thread::sleep(std::time::Duration::from_secs(120));
        }
    }
}
