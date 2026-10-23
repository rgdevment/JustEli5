//! `mc export --tisty` and `mc doctor --tisty` against a scripted MCP server (never the real Tisty).

mod common;

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use common::{Out, Sandbox, copy_tree, env, path_str};
use mc_cli::cli::{Io, Os, run};
use serde_json::{Value, json};

const FAKE: &str = env!("CARGO_BIN_EXE_mc-cli-fake-mcp");
const ME: &str = "Mario Hidalgo";

const MINUTES: &str = "---\ndate: 2023-11-14\ntitle: \"Daily de equipo\"\nsession: \"s1\"\nparticipants: [\"Ana\", \"Luis\"]\ntags: []\n---\n# Daily de equipo\n\n## Resumen\n\nHubo bloqueos [#0].\n\n## Compromisos\n\n- [ ] @Ana: cerrar el bug (el martes) [#3]\n- [ ] @Luis: revisar el PR (2024-03-05) [#4]\n";

struct Fixture {
    sb: Sandbox,
    session: String,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let sb = Sandbox::new();
        let dir = sb.path("sessions/basic");
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fusion/fusion_basic"),
            &dir,
        );
        let minutes = dir.join("minutes/m1");
        fs::create_dir_all(&minutes).unwrap();
        fs::write(minutes.join("minutes.md"), MINUTES).unwrap();
        let commitments = json!([
            {"id": "c1", "who": "Mario Hidalgo", "what": "cerrar el bug", "due": "el martes",
             "evidence": [3], "inferred": false, "source": "s1#c1"},
            {"id": "c2", "who": "Mario Hidalgo", "what": "revisar el PR", "due": "2024-03-05",
             "evidence": [4], "inferred": false, "source": "s1#c2"},
            {"id": "c3", "who": "Luis", "what": "mirar los logs", "due": null,
             "evidence": [5], "inferred": false, "source": "s1#c3"},
        ]);
        fs::write(minutes.join("commitments.json"), commitments.to_string()).unwrap();
        let state = sb.path("tisty-state.json");
        Self {
            session: path_str(&dir).to_string(),
            state,
            sb,
        }
    }

    /// A config whose Tisty is the scripted server with `extra` options.
    fn config(&self, extra: &[&str], more: Value) -> String {
        let mut args = vec!["--state".to_string(), path_str(&self.state).to_string()];
        args.extend(extra.iter().map(|a| a.to_string()));
        let mut tisty = json!({"command": FAKE, "args": args});
        for (k, v) in more.as_object().cloned().unwrap_or_default() {
            tisty[k] = v;
        }
        let path = self.sb.path("config.json");
        fs::write(
            &path,
            json!({"tisty": tisty, "fusion": {"me": ME}}).to_string(),
        )
        .unwrap();
        path_str(&path).to_string()
    }

    fn mc(&self, config: &str, args: &[&str]) -> Out {
        let mut full = vec!["--config", config];
        full.extend(args);
        self.sb.mc(&full)
    }

    fn state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.state).expect("state written")).unwrap()
    }
}

fn items(out: &Out) -> Vec<(String, String)> {
    out.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["label"].as_str().unwrap().to_string(),
                i["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn yes_publishes_the_minutes_and_one_task_with_the_user_s_commitments_as_parts() {
    let f = Fixture::new();
    let config = f.config(
        &["--lists", "Trabajo"],
        json!({"list": "Trabajo", "tags": ["equipo"]}),
    );
    let out = f.mc(
        &config,
        &["--json", "export", &f.session, "--tisty", "--yes"],
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(out.json()["ok"], true);
    assert_eq!(
        items(&out)
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>(),
        ["created", "created", "created", "created", "created"]
    );

    let state = f.state();
    let docs = state["docs"].as_array().unwrap();
    assert_eq!(docs.len(), 2, "the series document and this meeting's page");
    let (series, page) = (&docs[0], &docs[1]);
    assert_eq!(series["folder"], "Reuniones");
    assert_eq!(series["title"], "Daily de equipo");
    assert_eq!(page["page_of"], series["id"]);
    assert!(
        page.get("folder").is_none(),
        "a page takes its document's folder"
    );
    let body = page["body"].as_str().unwrap();
    assert!(body.starts_with("Minuta 2023-11-14\n\n"), "{body}");
    assert!(!body.contains("\n\n\n") && !body.contains("---"), "{body}");
    assert!(
        body.contains("- Ana: cerrar el bug (el martes)\n"),
        "{body}"
    );

    let tasks = state["tasks"].as_array().unwrap();
    assert_eq!(
        tasks.len(),
        3,
        "the meeting's task and the user's two commitments"
    );
    let parent = &tasks[0];
    assert_eq!(
        parent["title"],
        "Reunión Daily de equipo (2023-11-14): mis compromisos"
    );
    assert_eq!(parent["source"], "meet-copilot:s1");
    assert_eq!(parent["list"], "Trabajo");
    assert_eq!(parent["tags"], json!(["reunion", "daily", "equipo"]));
    assert!(
        parent["description"]
            .as_str()
            .unwrap()
            .contains("El compromiso de otra persona queda solo en la minuta")
    );
    assert_eq!(tasks[1]["title"], "cerrar el bug");
    assert_eq!(tasks[1]["source"], "s1#c1");
    assert_eq!(tasks[1]["part_of"], parent["id"]);
    assert!(
        tasks[1].get("list").is_none(),
        "a part takes its task's list"
    );
    assert!(tasks[1].get("deadline").is_none());
    assert!(
        tasks[1]["description"]
            .as_str()
            .unwrap()
            .contains("el martes")
    );
    assert_eq!(tasks[2]["deadline"], "2024-03-05");
    assert!(
        !tasks[2]["description"]
            .as_str()
            .unwrap()
            .contains("inferido")
    );
}

#[test]
fn publishing_again_reports_duplicates_and_does_not_rewrite_the_document() {
    let f = Fixture::new();
    let config = f.config(&["--lists", "Tareas"], json!({}));
    let args = ["--json", "export", &f.session, "--tisty", "--yes"];
    assert_eq!(f.mc(&config, &args).code, 0);
    let again = f.mc(&config, &args);
    assert_eq!(again.code, 0, "{}", again.stdout);
    assert_eq!(
        items(&again)
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>(),
        ["existed", "existed", "existed", "existed", "existed"]
    );
    let state = f.state();
    assert_eq!(state["docs"].as_array().unwrap().len(), 2);
    assert_eq!(state["tasks"].as_array().unwrap().len(), 3);
}

#[test]
fn a_refused_meeting_task_is_reported_and_a_retry_sends_it() {
    let f = Fixture::new();
    let failing = f.config(&["--fail-title", "revisar"], json!({}));
    let out = f.mc(
        &failing,
        &["--json", "export", &f.session, "--tisty", "--yes"],
    );
    assert_eq!(out.code, 3);
    let report = out.json();
    assert_eq!(report["ok"], false);
    assert_eq!(report["items"][2]["status"], "failed");
    assert_eq!(report["items"][4]["status"], "failed");
    assert!(
        report["items"][2]["reason"]
            .as_str()
            .unwrap()
            .contains("could not be saved")
    );
    assert_eq!(report["retry"], format!("mc export {} --tisty", f.session));
    assert!(
        Path::new(&f.session)
            .join("minutes/m1/minutes.md")
            .is_file()
    );

    let fixed = f.config(&[], json!({}));
    let retry = f.mc(
        &fixed,
        &["--json", "export", &f.session, "--tisty", "--yes"],
    );
    assert_eq!(retry.code, 0, "{}", retry.stdout);
    let statuses: Vec<String> = items(&retry).into_iter().map(|(_, s)| s).collect();
    assert_eq!(
        statuses,
        ["existed", "existed", "created", "created", "created"]
    );
    assert_eq!(f.state()["docs"].as_array().unwrap().len(), 2);
}

#[test]
fn an_assistant_that_is_not_admitted_stops_with_a_clear_message() {
    let f = Fixture::new();
    let config = f.config(&["--not-admitted"], json!({}));
    let out = f.mc(&config, &["export", &f.session, "--tisty", "--yes"]);
    assert_eq!(out.code, 3);
    assert!(
        out.stdout.contains("has not admitted this assistant"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Settings > Assistants"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("mc export"), "{}", out.stdout);
    assert!(out.stdout.contains("nothing was lost"), "{}", out.stdout);

    let json = f
        .mc(
            &config,
            &["--json", "export", &f.session, "--tisty", "--yes"],
        )
        .json();
    assert_eq!(json["aborted"]["kind"], "not_admitted");
    assert_eq!(json["items"].as_array().unwrap().len(), 5);
    assert!(
        json["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["status"] == "failed")
    );
    assert!(!f.state.exists());
}

#[test]
fn a_missing_folder_or_list_creates_nothing() {
    let f = Fixture::new();
    let config = f.config(&["--folders", "Reuniones"], json!({}));
    let folder = f.mc(
        &config,
        &[
            "--json",
            "export",
            &f.session,
            "--tisty",
            "--yes",
            "--tisty-folder",
            "Otra",
        ],
    );
    assert_eq!(folder.code, 3);
    assert_eq!(folder.json()["aborted"]["kind"], "missing_target");
    assert!(
        folder.json()["aborted"]["message"]
            .as_str()
            .unwrap()
            .contains("\"Otra\"")
    );

    let list = f.mc(
        &config,
        &[
            "--json",
            "export",
            &f.session,
            "--tisty",
            "--yes",
            "--tisty-list",
            "Inexistente",
        ],
    );
    assert_eq!(list.code, 3);
    assert_eq!(list.json()["aborted"]["kind"], "missing_target");
    assert!(
        !f.state.exists(),
        "nothing may be written when a name is wrong"
    );
}

#[test]
fn dry_run_prints_the_plan_and_never_starts_tisty() {
    let f = Fixture::new();
    let config = f.config(&[], json!({"command": "no-such-tisty"}));
    let out = f.mc(&config, &["export", &f.session, "--tisty", "--dry-run"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains("minutes: page «Minuta 2023-11-14» of «Daily de equipo» -> folder Reuniones"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(
            "task: Reunión Daily de equipo (2023-11-14): mis compromisos -> inbox\n  part: cerrar el bug\n  part: revisar el PR, due 2024-03-05"
        ),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("dry run"));
    let json = f
        .mc(
            &config,
            &["--json", "export", &f.session, "--tisty", "--dry-run"],
        )
        .json();
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["plan"]["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(json["plan"]["others"], 1);
    assert!(
        out.stdout
            .contains("1 commitment(s) of other people stay in the minutes only")
    );
}

#[test]
fn without_yes_the_plan_is_shown_and_confirmation_is_asked() {
    let f = Fixture::new();
    let config = f.config(&[], json!({}));
    let ask = |answer: &str| {
        let mut env = f.sb.env.clone();
        env.stdin_tty = true;
        let args: Vec<String> = ["--config", &config, "export", &f.session, "--tisty"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            &args,
            &env,
            &mut Io {
                out: &mut out,
                err: &mut err,
                input: Some(Box::new(Cursor::new(format!("{answer}\n").into_bytes()))),
            },
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    let (code, out, err) = ask("n");
    assert_eq!(code, 2, "{err}");
    assert!(
        out.contains("task: Reunión Daily de equipo (2023-11-14): mis compromisos -> inbox"),
        "{out}"
    );
    assert!(err.contains("Publish this to Tisty?"), "{err}");
    assert!(!f.state.exists(), "declined means nothing is sent");

    let (code, out, err) = ask("s");
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("created"), "{out}");
    assert_eq!(f.state()["tasks"].as_array().unwrap().len(), 3);
}

#[test]
fn non_interactive_use_needs_yes_unless_the_config_does_not_ask() {
    let f = Fixture::new();
    let config = f.config(&[], json!({}));
    let out = f.mc(&config, &["export", &f.session, "--tisty"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("--yes"), "{}", out.stderr);
    assert!(!f.state.exists());

    let quiet = f.config(&[], json!({"ask": false}));
    assert_eq!(f.mc(&quiet, &["export", &f.session, "--tisty"]).code, 0);
}

#[test]
fn a_missing_tisty_is_reported_and_keeps_the_files() {
    let f = Fixture::new();
    let config = f.config(&[], json!({"command": "no-such-tisty"}));
    let out = f.mc(
        &config,
        &["--json", "export", &f.session, "--tisty", "--yes"],
    );
    assert_eq!(out.code, 3);
    let report = out.json();
    assert_eq!(report["aborted"]["kind"], "unavailable");
    assert!(
        report["aborted"]["message"]
            .as_str()
            .unwrap()
            .contains("tisty.command")
    );
}

#[test]
fn the_default_install_is_used_when_tisty_is_not_on_path() {
    let f = Fixture::new();
    let install = f.sb.path("local/Programs/Tisty");
    fs::create_dir_all(&install).unwrap();
    fs::copy(FAKE, install.join("tisty.exe")).unwrap();
    let config = f.sb.path("config.json");
    fs::write(
        &config,
        json!({"tisty": {"args": ["--state", path_str(&f.state)]}, "fusion": {"me": ME}})
            .to_string(),
    )
    .unwrap();
    let local = path_str(&f.sb.path("local")).to_string();
    let mut vars: Vec<(String, String)> = f.sb.env.vars.clone().into_iter().collect();
    vars.push(("LOCALAPPDATA".into(), local));
    let refs: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let windows = env(Os::Windows, &refs);
    let out = common::mc(
        &windows,
        &[
            "--config",
            path_str(&config),
            "--json",
            "export",
            &f.session,
            "--tisty",
            "--yes",
        ],
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(f.state()["tasks"].as_array().unwrap().len(), 3);

    let doctor = common::mc(
        &windows,
        &["--config", path_str(&config), "--json", "doctor"],
    );
    let check = doctor.json()["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "tisty")
        .cloned()
        .unwrap();
    assert_eq!(check["status"], "ok");
}

#[test]
fn tisty_options_conflict_with_the_file_formats() {
    let f = Fixture::new();
    let config = f.config(&[], json!({}));
    let with_format = f.mc(
        &config,
        &["export", &f.session, "--tisty", "--format", "md"],
    );
    assert_eq!(with_format.code, 2);
    let without_tisty = f.mc(&config, &["export", &f.session, "--format", "md", "--yes"]);
    assert_eq!(without_tisty.code, 2);
    assert!(
        without_tisty.stderr.contains("need --tisty"),
        "{}",
        without_tisty.stderr
    );
}

#[test]
fn minutes_are_made_first_when_the_session_has_none() {
    let f = Fixture::new();
    fs::remove_dir_all(Path::new(&f.session).join("minutes")).unwrap();
    assert_eq!(f.sb.mc(&["--json", "run", &f.session]).code, 0);
    let analysis = json!({"add": {
        "commitments": [{"who": ME, "what": "cerrar el bug", "due": "el martes", "evidence": [3]}],
    }})
    .to_string();
    // A commitment the check does not confirm is inferred, and an inferred one is never a task.
    let id = mc_analyst::item_id('c', &[ME, "cerrar el bug"]);
    let verdicts =
        json!({"verdicts": [{"id": id, "verdict": "supported", "reason": "r"}]}).to_string();
    let answers = [analysis.as_str(), verdicts.as_str(), "## Resumen\nok"];
    let path = f.sb.path("llm.json");
    fs::write(
        &path,
        json!({
            "llm": {"provider": "fake", "args": answers},
            "tisty": {"command": FAKE, "args": ["--state", path_str(&f.state)]},
            "fusion": {"me": ME},
        })
        .to_string(),
    )
    .unwrap();
    let out = f.mc(
        path_str(&path),
        &["--json", "export", &f.session, "--tisty", "--yes"],
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(f.state()["tasks"].as_array().unwrap().len(), 2);
    let body = f.state()["docs"][1]["body"].as_str().unwrap().to_string();
    assert!(body.starts_with("Minuta 2023-11-"), "{body}");
}

fn statuses(out: &Out) -> Vec<String> {
    items(out).into_iter().map(|(_, s)| s).collect()
}

fn publish(f: &Fixture, config: &str) -> Out {
    f.mc(
        config,
        &["--json", "export", &f.session, "--tisty", "--yes"],
    )
}

/// A later meeting of the same series, as the newest minutes of the session.
fn add_next_meeting(f: &Fixture) {
    let dir = Path::new(&f.session).join("minutes/m2");
    fs::create_dir_all(&dir).unwrap();
    let next = MINUTES
        .replace("2023-11-14", "2023-11-21")
        .replace("session: \"s1\"", "session: \"s2\"");
    fs::write(dir.join("minutes.md"), next).unwrap();
    let commitments = json!([{"id": "c9", "who": ME, "what": "otra cosa", "due": null,
        "evidence": [5], "inferred": false, "source": "s2#c9"}]);
    fs::write(dir.join("commitments.json"), commitments.to_string()).unwrap();
}

#[test]
fn each_meeting_of_a_series_is_a_page_of_one_document() {
    let f = Fixture::new();
    let config = f.config(&[], json!({}));
    assert_eq!(publish(&f, &config).code, 0);
    add_next_meeting(&f);
    let second = publish(&f, &config);
    assert_eq!(second.code, 0, "{}", second.stdout);
    assert_eq!(
        statuses(&second),
        ["existed", "created", "created", "created"]
    );
    let docs = f.state()["docs"].as_array().unwrap().clone();
    assert_eq!(docs.len(), 3, "one series document, two pages");
    assert_eq!(docs[2]["page_of"], docs[0]["id"]);
    assert_eq!(docs[2]["title"], "Minuta 2023-11-21");
}

#[test]
fn a_title_taken_elsewhere_is_found_and_reused_not_failed() {
    let f = Fixture::new();
    let old = json!({"docs": [{"id": "d0099", "title": "Daily de equipo",
        "folder": "Otra", "body": "x"}], "tasks": []});
    fs::write(&f.state, old.to_string()).unwrap();
    let config = f.config(&["--folders", "Reuniones,Otra"], json!({}));
    let out = publish(&f, &config);
    assert_eq!(out.code, 0, "{}", out.stdout);
    assert_eq!(
        statuses(&out),
        ["existed", "created", "created", "created", "created"]
    );
    let docs = f.state()["docs"].as_array().unwrap().clone();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[1]["page_of"], "d0099");
}

#[test]
fn a_series_document_with_its_former_title_is_found_and_written_again_keeping_its_title() {
    let f = Fixture::new();
    let title = "Daily de equipo — minutas";
    let old = json!({"docs": [{"id": "d0007", "title": title, "folder": "Reuniones",
        "body": format!("{title}\nviejo")}], "tasks": []});
    fs::write(&f.state, old.to_string()).unwrap();
    let out = publish(&f, &f.config(&[], json!({})));
    assert_eq!(out.code, 0, "{}", out.stdout);
    assert_eq!(statuses(&out)[0], "existed");
    let docs = f.state()["docs"].as_array().unwrap().clone();
    assert_eq!(docs.len(), 2, "no second series document");
    let body = docs[0]["body"].as_str().unwrap();
    assert!(body.starts_with(&format!("{title}\n")), "{body}");
    assert!(
        body.contains("Minutas de «Daily de equipo»") && body.contains("## Sesiones"),
        "{body}"
    );
}

#[test]
fn a_json_rpc_error_fails_only_the_tasks_of_that_call() {
    let f = Fixture::new();
    let config = f.config(&["--rpc-error-title", "revisar"], json!({}));
    let out = publish(&f, &config);
    assert_eq!(out.code, 3);
    let report = out.json();
    assert_eq!(report["aborted"], Value::Null, "{report}");
    assert_eq!(
        statuses(&out),
        ["created", "created", "failed", "failed", "failed"]
    );
    assert!(
        report["items"][2]["reason"]
            .as_str()
            .unwrap()
            .contains("-32602")
    );
}

#[test]
fn the_item_in_flight_when_the_server_dies_is_unknown_and_a_retry_reconciles_it() {
    let f = Fixture::new();
    let dying = f.config(&["--die-on", "propose"], json!({}));
    let out = publish(&f, &dying);
    assert_eq!(out.code, 3);
    assert_eq!(out.json()["aborted"]["kind"], "unavailable");
    assert_eq!(
        statuses(&out),
        ["created", "created", "unknown", "unknown", "unknown"]
    );
    let human = f.mc(&dying, &["export", &f.session, "--tisty", "--yes"]);
    assert!(
        human.stdout.contains("may have been applied"),
        "{}",
        human.stdout
    );

    let fixed = f.config(&[], json!({}));
    let retry = publish(&f, &fixed);
    assert_eq!(retry.code, 0, "{}", retry.stdout);
    assert_eq!(
        statuses(&retry),
        ["existed", "existed", "created", "created", "created"]
    );
}

#[test]
fn a_folder_name_two_folders_share_stops_before_writing_and_names_their_paths() {
    let f = Fixture::new();
    let config = f.config(
        &["--folders", "Personal / Proyectos / Meets,Trabajo / Meets"],
        json!({}),
    );
    let export = |folder: &str| {
        f.mc(
            &config,
            &[
                "--json",
                "export",
                &f.session,
                "--tisty",
                "--yes",
                "--tisty-folder",
                folder,
            ],
        )
    };
    let out = export("Meets");
    assert_eq!(out.code, 3);
    let aborted = &out.json()["aborted"];
    assert_eq!(aborted["kind"], "ambiguous_target");
    let message = aborted["message"].as_str().unwrap();
    assert!(
        message.contains("Personal / Proyectos / Meets or Trabajo / Meets"),
        "{message}"
    );
    assert!(!f.state.exists(), "nothing may be written");

    let whole = export("Trabajo / Meets");
    assert_eq!(whole.code, 0, "{}", whole.stdout);
    assert_eq!(f.state()["docs"][0]["folder"], "Trabajo / Meets");
}

#[test]
fn without_the_user_s_name_only_the_minutes_are_published() {
    let f = Fixture::new();
    let config = f.sb.path("nameless.json");
    fs::write(
        &config,
        json!({"tisty": {"command": FAKE, "args": ["--state", path_str(&f.state)]}}).to_string(),
    )
    .unwrap();
    let out = f.mc(
        path_str(&config),
        &["--json", "export", &f.session, "--tisty", "--yes"],
    );
    assert_eq!(out.code, 0, "{}", out.stdout);
    assert_eq!(statuses(&out), ["created", "created"]);
    assert!(f.state()["tasks"].as_array().unwrap().is_empty());
}

mod doctor {
    use super::*;

    fn tisty_mcp(f: &Fixture, extra: &[&str]) -> Value {
        let config = f.config(extra, json!({}));
        let out = f.mc(&config, &["--json", "doctor", "--tisty"]);
        assert_eq!(out.code, 0, "{}", out.stdout);
        out.json()["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "tisty_mcp")
            .cloned()
            .expect("a tisty_mcp check")
    }

    #[test]
    fn a_ready_tisty_is_ok() {
        let f = Fixture::new();
        let check = tisty_mcp(&f, &[]);
        assert_eq!(check["status"], "ok", "{check}");
    }

    #[test]
    fn not_admitted_is_told_apart() {
        let f = Fixture::new();
        let check = tisty_mcp(&f, &["--not-admitted"]);
        assert_eq!(check["status"], "warn");
        assert!(
            check["detail"].as_str().unwrap().contains("not admitted"),
            "{check}"
        );
    }

    #[test]
    fn missing_tools_are_named() {
        let f = Fixture::new();
        let check = tisty_mcp(&f, &["--no-propose"]);
        assert_eq!(check["status"], "warn");
        assert!(
            check["detail"].as_str().unwrap().contains("propose"),
            "{check}"
        );
    }

    #[test]
    fn a_server_that_dies_is_a_warning_not_a_failure() {
        let f = Fixture::new();
        let check = tisty_mcp(&f, &["--exit-on-init"]);
        assert_eq!(check["status"], "warn");
        assert!(
            check["detail"].as_str().unwrap().contains("fatal start-up"),
            "{check}"
        );
    }

    #[test]
    fn without_the_flag_tisty_is_not_started() {
        let f = Fixture::new();
        let config = f.config(&["--exit-on-init"], json!({}));
        let out = f.mc(&config, &["--json", "doctor"]);
        assert!(
            out.json()["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["name"] != "tisty_mcp")
        );
    }
}
