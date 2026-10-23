use std::path::Path;

use mc_analyst::{
    CarriedRecord, CarryStatus, CommitmentRecord, MINUTES_VERSION, MinutesContext, MinutesMeta,
    VerifyReport, flat, minutes as write_minutes,
};
use mc_engine::runs::fmt_rel;
use mc_engine::sidecar::{read_marks, read_notes};
use mc_store::manifest::Manifest;
use serde_json::{Value, json};

use crate::cli::{CliError, Ctx, Io, emit};

use super::carry::{identity as carry_identity, load as load_memory};
use super::memory::{known_roles, learn_minutes, named_people};

use super::analysis::{
    fnv_hex, llm_identity, make_llm, pretty, read_json, read_optional, resolve_analysis,
    resolve_transcript, speakers, write_folder,
};

/// `--notes FILE`, else what was noted during the capture (`<session>/notes.jsonl`).
fn meeting_notes(ctx: &Ctx<'_>, dir: &str) -> Result<String, CliError> {
    if ctx.args.value("notes").is_some() {
        return read_optional(ctx, "notes");
    }
    let lines: Vec<String> = read_notes(Path::new(dir))
        .iter()
        .map(|n| format!("- [{}] {}", fmt_rel(Some(n.mono_ms)), flat(&n.text)))
        .collect();
    Ok(lines.join("\n"))
}

fn meta(dir: &str, duration_ms: Option<i64>) -> Result<MinutesMeta, CliError> {
    let manifest = Manifest::load(Path::new(dir).join("manifest.json"))?;
    let date: String = manifest.started_utc_iso.chars().take(10).collect();
    Ok(MinutesMeta {
        title: manifest
            .meeting_name
            .clone()
            .unwrap_or_else(|| manifest.session_id.clone()),
        session_id: manifest.session_id,
        date,
        duration_ms,
    })
}

/// The minutes' terms and people into the memory; a memory that cannot be opened costs the
/// minutes nothing, so its failure is reported, not raised.
fn learn(ctx: &Ctx<'_>, dir: &str, series: Option<&str>, learned: &mc_analyst::Learned) -> Value {
    let taught = Manifest::load(Path::new(dir).join("manifest.json"))
        .map_err(CliError::from)
        .and_then(|m| learn_minutes(&ctx.cfg, &m.session_id, series, m.started_wall_ms, learned));
    match taught {
        Ok(learnt) => json!(learnt),
        Err(e) => json!({"error": e.message()}),
    }
}

/// The user's context plus what the memory knows of the people: the roles of who spoke, so the
/// participants table has them even when nobody says them, and who the names said in the meeting
/// are («Caro» is Carolina). A memory that cannot be read adds nothing.
fn with_people(context: String, roles: &[(String, String)], named: &[String]) -> String {
    let mut blocks = vec![context.trim().to_string()];
    if !roles.is_empty() {
        let lines: Vec<String> = roles.iter().map(|(n, r)| format!("- {n}: {r}")).collect();
        blocks.push(format!("## Roles que ya se conocen\n{}", lines.join("\n")));
    }
    if !named.is_empty() {
        blocks.push(format!(
            "## Personas que se nombran (no asistieron; así se las llama)\n{}",
            named.join("\n")
        ));
    }
    blocks.retain(|b| !b.is_empty());
    blocks.join("\n\n")
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let [dir] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage(
            "usage: eli5 minutes <session> [--run ID | --fuse ID] [--analysis ID] [--notes F] [--llm-provider P] [--llm-model M] [--glossary F] [--context F] [--force]",
        ));
    };
    let force = ctx.args.flag("force");
    let transcript = resolve_transcript(ctx, dir)?;
    let memory = load_memory(ctx, Path::new(dir))?;
    let (carry, glossary) = (&memory.carry, &memory.glossary);
    let analysis = resolve_analysis(ctx, dir, &transcript, &memory)?;
    let notes = meeting_notes(ctx, dir)?;
    let marks = read_marks(Path::new(dir));
    let attendees = speakers(&transcript.segments);
    let roles = known_roles(&ctx.cfg, &attendees, carry.series.as_deref()).unwrap_or_default();
    let said: Vec<&str> = transcript
        .segments
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    let named = named_people(&ctx.cfg, &attendees, &said).unwrap_or_default();
    let user_context = with_people(read_optional(ctx, "context")?, &roles, &named);
    let identity = format!(
        "{MINUTES_VERSION}|{}|{}|{}|{}|{}|{:?}|{}|{}|{}|{}|{}",
        transcript.label(),
        transcript.hash,
        analysis.id,
        llm_identity(ctx),
        fnv_hex(&notes),
        marks,
        fnv_hex(&glossary.fingerprint()),
        fnv_hex(&carry_identity(&carry.carried)),
        ctx.cfg.fusion.me.as_deref().unwrap_or_default(),
        fnv_hex(&user_context),
        ctx.cfg.fusion.me_role.as_deref().unwrap_or_default(),
    );
    let id = format!("m{}", fnv_hex(&identity));
    let out = Path::new(dir).join("minutes").join(&id);
    let reused = !force && out.join("minutes.md").is_file();
    let mut learnt: Option<Value> = None;
    let (commitments, carried, verify): (Vec<CommitmentRecord>, Vec<CarriedRecord>, VerifyReport) =
        if reused {
            (
                read_json(&out.join("commitments.json"))?,
                read_json(&out.join("carried.json")).unwrap_or_default(),
                read_json(&out.join("verify.json"))?,
            )
        } else {
            let context = MinutesContext {
                carried: carry.carried.clone(),
                glossary: glossary
                    .terms()
                    .into_iter()
                    .map(
                        |term| match glossary.meanings().into_iter().find(|(t, _)| *t == term) {
                            Some((_, meaning)) => format!("{term}: {meaning}"),
                            None => term,
                        },
                    )
                    .collect(),
                marks,
                me: ctx.cfg.fusion.me.clone(),
                context: user_context,
                me_role: ctx.cfg.fusion.me_role.clone(),
                known_roles: roles,
            };
            let llm = make_llm(ctx)?;
            let made = write_minutes(
                llm.as_ref(),
                &transcript.segments,
                &analysis.ledger,
                &notes,
                &meta(dir, transcript.segments.iter().map(|s| s.end_ms).max())?,
                &context,
            )
            .map_err(|e| CliError::Runtime {
                code: "llm",
                message: e.to_string(),
                hint: Some("`mc doctor` checks the llm settings".to_string()),
            })?;
            write_folder(
                &out,
                &[
                    ("commitments.json", pretty(&made.commitments)),
                    ("carried.json", pretty(&made.carried)),
                    ("verify.json", pretty(&made.verify)),
                    // The ids its commitments cite belong to this transcript.
                    (
                        "meta.json",
                        pretty(&json!({"minutes_id": id, "source": transcript.label(),
                                       "analysis_id": analysis.id,
                                       "usage": mc_llm::usage_json(&llm.usage())})),
                    ),
                    ("learned.json", pretty(&made.learned)),
                    ("minutes.md", made.markdown),
                ],
                "minutes.md",
            )?;
            learnt = Some(learn(ctx, dir, carry.series.as_deref(), &made.learned));
            (made.commitments, made.carried, made.verify)
        };
    let out = crate::cli::canonical(out);
    let inferred = commitments.iter().filter(|c| c.inferred).count();
    let count = |status: Option<CarryStatus>| carried.iter().filter(|r| r.status == status).count();
    let (done, open, dropped, unknown) = (
        count(Some(CarryStatus::Done)),
        count(Some(CarryStatus::StillOpen)),
        count(Some(CarryStatus::Dropped)),
        count(None),
    );
    let usage = |folder: &Path| -> Value {
        read_json::<Value>(&folder.join("meta.json")).map_or(Value::Null, |m| m["usage"].clone())
    };
    let value = json!({
        "usage": {"minutes": usage(&out), "analysis": usage(&analysis.dir)},
        "minutes_id": id,
        "path": out.display().to_string(),
        "reused": reused,
        "source": {"kind": transcript.kind.name(), "id": transcript.id},
        "analysis_id": analysis.id,
        "analysis_complete": analysis.complete,
        "commitments": commitments.len(),
        "inferred_commitments": inferred,
        "removed": verify.removed,
        "dropped": verify.dropped,
        "verify_error": verify.error,
        "warnings": carry.warnings,
        "memory": learnt,
        "carried": {
            "series": carry.series,
            "from": carry.previous,
            "total": carried.len(),
            "done": done,
            "still_open": open,
            "dropped": dropped,
            "unknown": unknown,
        },
    });
    let mut human = format!(
        "{id}  {}  {}\n  from {} and analysis {}; {} commitments ({inferred} inferred), {} unsupported removed\n",
        if reused { "unchanged" } else { "created" },
        out.join("minutes.md").display(),
        transcript.label(),
        analysis.id,
        commitments.len(),
        verify.removed.len(),
    );
    if !carried.is_empty() {
        human.push_str(&format!(
            "  carried from {}: {done} done, {open} still open, {dropped} dropped, {unknown} unresolved
",
            carry.previous.as_deref().unwrap_or("the previous meeting"),
        ));
    }
    if !analysis.complete {
        human.push_str(
            "  warning: the analysis is incomplete (a window failed); run `mc analyze --force`\n",
        );
    }
    if !verify.dropped.is_empty() {
        human.push_str(&format!(
            "  note: {} items dropped for evidence outside the transcript\n",
            verify.dropped.len()
        ));
    }
    for warning in &carry.warnings {
        human.push_str(&format!(
            "  warning: {warning}
"
        ));
    }
    if let Some(error) = &verify.error {
        human.push_str(&format!(
            "  warning: verification failed, items kept as inferred: {error}\n"
        ));
    }
    emit(io, ctx.json, human, value)?;
    Ok(0)
}
