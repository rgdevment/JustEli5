//! The resources: `meeting://current/...` and `meeting://{session}/...`.

use serde_json::{Value, json};

use crate::meetings::Meetings;
use crate::rpc::{INVALID_PARAMS, RESOURCE_NOT_FOUND, RpcError};
use crate::tools::{UNTRUSTED_NOTICE, ledger_view, mark_untrusted, transcript_view};

const SCHEME: &str = "meeting://";
/// Sessions listed besides the current one.
const LISTED_MEETINGS: usize = 20;
const RESOURCE_LINES: usize = 200;

fn entry(uri: String, name: String, description: &str, mime: &str) -> Value {
    json!({"uri": uri, "name": name, "description": format!("{description} {UNTRUSTED_NOTICE}"), "mimeType": mime})
}

pub fn list(meetings: &Meetings) -> Value {
    let mut resources = vec![
        entry(
            format!("{SCHEME}current/transcript"),
            "Current meeting: transcript".into(),
            "The latest committed lines.",
            "application/json",
        ),
        entry(
            format!("{SCHEME}current/ledger"),
            "Current meeting: ledger".into(),
            "Topics, decisions and commitments so far.",
            "application/json",
        ),
    ];
    for meeting in meetings.list().iter().take(LISTED_MEETINGS) {
        let label = meeting
            .manifest
            .meeting_name
            .clone()
            .unwrap_or_else(|| meeting.dir_name());
        if meeting.has_minutes() {
            resources.push(entry(
                format!("{SCHEME}{}/minutes", meeting.id()),
                format!("{label}: minutes"),
                "The latest minutes (Markdown).",
                "text/markdown",
            ));
        }
        if meeting.has_commitments() {
            resources.push(entry(
                format!("{SCHEME}{}/commitments", meeting.id()),
                format!("{label}: commitments"),
                "The commitments of the latest minutes.",
                "application/json",
            ));
        }
    }
    json!({"resources": resources})
}

pub fn templates() -> Value {
    let template = |kind: &str, mime: &str| {
        json!({"uriTemplate": format!("{SCHEME}{{session}}/{kind}"), "name": format!("Meeting {kind}"),
               "description": format!("Use `current` as the session for the meeting in progress. {UNTRUSTED_NOTICE}"),
               "mimeType": mime})
    };
    json!({"resourceTemplates": [
        template("transcript", "application/json"),
        template("ledger", "application/json"),
        template("minutes", "text/markdown"),
        template("commitments", "application/json"),
    ]})
}

fn not_found(message: impl Into<String>) -> RpcError {
    RpcError::new(RESOURCE_NOT_FOUND, message)
}

pub fn read(meetings: &Meetings, uri: &str) -> Result<Value, RpcError> {
    let rest = uri
        .strip_prefix(SCHEME)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("unsupported uri {uri:?}")))?;
    let (who, kind) = rest
        .split_once('/')
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("malformed uri {uri:?}")))?;
    let meeting = meetings.resolve(Some(who)).map_err(not_found)?;
    let (mime, body) = match kind {
        "transcript" => json_body(
            transcript_view(meetings, &meeting, None, None, RESOURCE_LINES).map_err(not_found)?,
        ),
        "ledger" => json_body(ledger_view(&meeting).map_err(not_found)?),
        "minutes" => {
            let (_, _, markdown) = meeting
                .minutes()
                .ok_or_else(|| not_found("this meeting has no minutes yet"))?;
            ("text/markdown", markdown)
        }
        "commitments" => {
            let (minutes_id, source, commitments) = meeting
                .commitments()
                .ok_or_else(|| not_found("this meeting has no commitments yet"))?;
            json_body(json!({"session": meeting.id(), "minutes_id": minutes_id,
                             "transcript_source": source, "commitments": commitments}))
        }
        other => return Err(not_found(format!("unknown resource {other:?}"))),
    };
    // Markdown has no room for a flag in its body, so every content also says it in `_meta`.
    Ok(json!({"contents": [{
        "uri": uri, "mimeType": mime, "text": body,
        "_meta": {"untrusted": true, "notice": UNTRUSTED_NOTICE},
    }]}))
}

fn json_body(value: Value) -> (&'static str, String) {
    ("application/json", mark_untrusted(value).to_string())
}
