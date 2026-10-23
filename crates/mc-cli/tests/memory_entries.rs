//! `eli5 memory`: what the window calls to look up and correct the terms and people known
//! between meetings.

mod common;

use std::fs;

use common::{Sandbox, path_str};
use serde_json::{Value, json};

fn ok(sb: &Sandbox, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let out = sb.mc(&all);
    assert_eq!(out.code, 0, "{args:?}: {}", out.stderr);
    out.json()
}

fn id_of(entry: &Value) -> String {
    entry["entry"]["id"].as_i64().unwrap().to_string()
}

#[test]
fn the_first_use_takes_in_the_users_glossary() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.data()).unwrap();
    fs::write(
        sb.data().join("glossary.json"),
        r#"{"Verídex": {"aliases": ["beri dex"], "meaning": "herramienta interna"}}"#,
    )
    .unwrap();
    let list = ok(&sb, &["memory", "list"]);
    let entry = &list["entries"][0];
    assert_eq!(entry["name"], "Verídex");
    assert_eq!(entry["meaning"], "herramienta interna");
    assert_eq!(entry["state"], "confirmed");
    assert_eq!(entry["forms"][1]["text"], "beri dex");
}

#[test]
fn add_correct_merge_and_resolve_from_the_command_line() {
    let sb = Sandbox::new();
    let team = id_of(&ok(
        &sb,
        &[
            "memory",
            "add",
            "term",
            "Order",
            "--meaning",
            "equipo de órdenes",
        ],
    ));
    let purchase = id_of(&ok(
        &sb,
        &[
            "memory",
            "add",
            "term",
            "Order",
            "--meaning",
            "orden de compra",
        ],
    ));
    let raul = ok(
        &sb,
        &[
            "memory",
            "add",
            "person",
            "Raul Medina",
            "--forms",
            "MEDINA Raul, Raul M.",
            "--role",
            "Product Owner",
        ],
    );
    assert_eq!(raul["entry"]["roles"][0]["role"], "Product Owner");
    assert_eq!(raul["entry"]["forms"].as_array().unwrap().len(), 3);

    let both = ok(&sb, &["memory", "resolve", "term", "order"]);
    assert_eq!(both["resolution"], "ambiguous");

    let typo = id_of(&ok(&sb, &["memory", "add", "term", "Kicloak"]));
    let keycloak = id_of(&ok(&sb, &["memory", "add", "term", "Keycloak"]));
    let merged = ok(&sb, &["memory", "merge", &keycloak, &typo]);
    assert_eq!(merged["entry"]["forms"][1]["text"], "Kicloak");
    assert_eq!(
        ok(&sb, &["memory", "resolve", "term", "kicloak"]),
        json!({"resolution": "known", "id": keycloak.parse::<i64>().unwrap()})
    );

    let edited = ok(
        &sb,
        &["memory", "edit", &purchase, "--name", "Orden de compra"],
    );
    assert_eq!(edited["entry"]["name"], "Orden de compra");
    let found = ok(&sb, &["memory", "list", "--query", "ordenes"]);
    assert_eq!(found["entries"][0]["id"].to_string(), team);

    let backup = sb.path("memory.json");
    ok(&sb, &["memory", "export", "--out", path_str(&backup)]);
    assert!(
        fs::read_to_string(&backup)
            .unwrap()
            .contains("justeli5.memory/1")
    );

    let missing = sb.mc(&["--json", "memory", "show", "999"]);
    assert_eq!(missing.code, 3);
    assert_eq!(sb.mc(&["memory", "form", "remove", &team, "order"]).code, 2);
}

#[test]
fn the_windows_people_are_taken_in_once() {
    let sb = Sandbox::new();
    let file = sb.path("people.json");
    fs::write(
        &file,
        r#"{"Raul Medina": "Product Owner", "Kiran Sharma": "Tech Manager"}"#,
    )
    .unwrap();
    let first = ok(&sb, &["memory", "adopt-people", path_str(&file)]);
    assert_eq!(first["adopted"], 2);
    let again = ok(&sb, &["memory", "adopt-people", path_str(&file)]);
    assert_eq!(again["adopted"], 0);
    let people = ok(&sb, &["memory", "list", "--kind", "person"]);
    let raul = people["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Raul Medina")
        .unwrap();
    assert_eq!(raul["roles"][0]["role"], "Product Owner");
    assert_eq!(raul["state"], "confirmed");
}

#[test]
fn people_named_by_a_nickname_reach_the_minutes_but_not_a_first_name_someone_present_has() {
    let sb = Sandbox::new();
    ok(
        &sb,
        &[
            "memory",
            "add",
            "person",
            "Carolina Soto",
            "--forms",
            "Caro",
            "--role",
            "Product Owner",
        ],
    );
    ok(
        &sb,
        &["memory", "add", "person", "Luis Pérez", "--forms", "Luis"],
    );
    ok(
        &sb,
        &["memory", "add", "person", "Luis Gómez", "--forms", "Luis"],
    );
    let cfg =
        mc_cli::config::Config::load(&sb.env, &mc_cli::args::Parsed::default(), true).unwrap();
    let named = mc_cli::cmd::memory::named_people(
        &cfg,
        &["Luis Pérez".to_string()],
        &["Luis, pregúntale a Caro por el Figma", "y avísale a Luis"],
    )
    .unwrap();
    assert_eq!(named, ["- Caro: Carolina Soto (Product Owner)"]);
}
