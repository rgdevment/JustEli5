use mc_memory::{Context, Kind, Memory, Resolution, Sighting, State};

fn seen(session: &str, series: &str, at_ms: i64) -> Sighting {
    Sighting {
        session: session.to_string(),
        series: Some(series.to_string()),
        at_ms,
        attended: true,
    }
}

fn known(resolution: Resolution) -> i64 {
    match resolution {
        Resolution::Known { id } => id,
        other => panic!("expected one concept, got {other:?}"),
    }
}

#[test]
fn many_forms_name_one_term_whatever_the_case_or_accents() {
    let mut memory = Memory::open_in_memory().unwrap();
    let veridex = memory
        .add(
            Kind::Term,
            "Verídex",
            Some("herramienta interna"),
            State::Confirmed,
            1,
        )
        .unwrap();
    memory
        .add_form(veridex, "Beri dex", State::Proposed)
        .unwrap();
    memory
        .add_form(veridex, "el validador", State::Confirmed)
        .unwrap();

    let none = Context::default();
    for heard in ["veridex", "VERÍDEX", "beri-dex", "El validador"] {
        assert_eq!(
            known(memory.resolve(Kind::Term, heard, &none).unwrap()),
            veridex
        );
    }
    let forms: Vec<_> = memory.get(veridex).unwrap().forms;
    assert_eq!(forms[0].text, "Verídex", "the name comes first");
    assert_eq!(forms.len(), 3);
}

#[test]
fn a_homonym_follows_the_series_and_the_people_it_came_up_with() {
    let mut memory = Memory::open_in_memory().unwrap();
    let team = memory
        .add(
            Kind::Term,
            "Order",
            Some("equipo de órdenes"),
            State::Confirmed,
            1,
        )
        .unwrap();
    let purchase = memory
        .add(
            Kind::Term,
            "Order",
            Some("orden de compra"),
            State::Confirmed,
            1,
        )
        .unwrap();
    let kiran = memory
        .add(Kind::Person, "Kiran Sharma", None, State::Confirmed, 1)
        .unwrap();
    memory
        .record(team, &seen("s1", "rediseno del panel", 10))
        .unwrap();
    memory
        .record(team, &seen("s2", "rediseno del panel", 20))
        .unwrap();
    memory
        .record(kiran, &seen("s2", "rediseno del panel", 20))
        .unwrap();
    memory.record(purchase, &seen("s3", "compras", 30)).unwrap();

    let identity = Context {
        series: Some("rediseno del panel".to_string()),
        present: Vec::new(),
    };
    assert_eq!(
        known(memory.resolve(Kind::Term, "order", &identity).unwrap()),
        team
    );

    let shopping = Context {
        series: Some("compras".to_string()),
        present: Vec::new(),
    };
    assert_eq!(
        known(memory.resolve(Kind::Term, "order", &shopping).unwrap()),
        purchase
    );

    // A new meeting with no series: Kiran being there is enough.
    let with_kiran = Context {
        series: None,
        present: vec![kiran],
    };
    assert_eq!(
        known(memory.resolve(Kind::Term, "Order", &with_kiran).unwrap()),
        team
    );

    let Resolution::Ambiguous { candidates } = memory
        .resolve(Kind::Term, "order", &Context::default())
        .unwrap()
    else {
        panic!("with no context the memory must not guess");
    };
    assert_eq!(candidates.len(), 2);
}

#[test]
fn two_people_with_one_name_and_one_person_with_many_forms() {
    let mut memory = Memory::open_in_memory().unwrap();
    let me = memory
        .add(Kind::Person, "Mario Hidalgo", None, State::Confirmed, 1)
        .unwrap();
    let other = memory
        .add(Kind::Person, "Mario Torres", None, State::Confirmed, 1)
        .unwrap();
    memory.add_form(me, "Mario", State::Confirmed).unwrap();
    memory.add_form(other, "Mario", State::Confirmed).unwrap();
    let raul = memory
        .add(Kind::Person, "Raul Medina", None, State::Confirmed, 1)
        .unwrap();
    memory
        .add_form(raul, "MEDINA Raul", State::Proposed)
        .unwrap();
    memory.add_form(raul, "Raul M.", State::Proposed).unwrap();
    memory.record(other, &seen("core", "core user", 5)).unwrap();
    memory.record(raul, &seen("core", "core user", 5)).unwrap();

    let none = Context::default();
    assert_eq!(
        known(memory.resolve(Kind::Person, "raul m", &none).unwrap()),
        raul
    );
    assert!(matches!(
        memory.resolve(Kind::Person, "Mario", &none).unwrap(),
        Resolution::Ambiguous { .. }
    ));
    let with_raul = Context {
        series: None,
        present: vec![raul],
    };
    assert_eq!(
        known(memory.resolve(Kind::Person, "Mario", &with_raul).unwrap()),
        other
    );
    // A person is never a term.
    assert_eq!(
        memory.resolve(Kind::Term, "Raul Medina", &none).unwrap(),
        Resolution::Unknown
    );
}

#[test]
fn moving_a_form_and_merging_two_entries() {
    let mut memory = Memory::open_in_memory().unwrap();
    let core_user = memory
        .add(Kind::Term, "Core User", None, State::Confirmed, 1)
        .unwrap();
    let banking = memory
        .add(Kind::Term, "Core bancario", None, State::Confirmed, 1)
        .unwrap();
    memory.add_form(banking, "Core", State::Proposed).unwrap();
    memory.move_form(banking, core_user, "core").unwrap();
    assert_eq!(memory.candidates(Kind::Term, "Core").unwrap(), [core_user]);

    let keycloak = memory
        .add(
            Kind::Term,
            "Keycloak",
            Some("autenticación"),
            State::Proposed,
            1,
        )
        .unwrap();
    let typo = memory
        .add(Kind::Term, "Kicloak", None, State::Proposed, 1)
        .unwrap();
    memory.record(typo, &seen("s1", "auth", 3)).unwrap();
    memory.merge(keycloak, typo, 9).unwrap();
    let merged = memory.get(keycloak).unwrap();
    assert_eq!(merged.state, State::Confirmed);
    assert_eq!(merged.meetings, 1);
    assert!(merged.forms.iter().any(|f| f.text == "Kicloak"));
    assert!(memory.get(typo).is_err());
    assert!(
        memory.remove_form(keycloak, "keycloak").is_err(),
        "the name stays"
    );
}

#[test]
fn roles_and_confirmation() {
    let mut memory = Memory::open_in_memory().unwrap();
    let raul = memory
        .add(Kind::Person, "Raul Medina", None, State::Proposed, 1)
        .unwrap();
    memory
        .set_role(raul, "Product Owner", 10, State::Proposed)
        .unwrap();
    memory.confirm(raul, 11).unwrap();
    memory
        .set_role(raul, "Product Manager", 20, State::Proposed)
        .unwrap();
    let person = memory.get(raul).unwrap();
    assert_eq!(person.state, State::Confirmed);
    assert_eq!(
        person.role(),
        Some("Product Owner"),
        "a guess does not replace what the user confirmed"
    );

    let term = memory
        .add(Kind::Term, "RUT", None, State::Confirmed, 1)
        .unwrap();
    assert!(memory.set_role(term, "x", 1, State::Proposed).is_err());
}

#[test]
fn search_lists_by_form_and_meaning_and_export_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let mut memory = Memory::open(&path).unwrap();
    let veridex = memory
        .add(
            Kind::Term,
            "Verídex",
            Some("Proveedor de validación"),
            State::Confirmed,
            1,
        )
        .unwrap();
    memory
        .add_form(veridex, "Beri dex", State::Proposed)
        .unwrap();
    memory.record(veridex, &seen("s1", "panel", 4)).unwrap();
    let raul = memory
        .add(
            Kind::Person,
            "Raul Medina",
            Some("convoca a legal"),
            State::Proposed,
            1,
        )
        .unwrap();
    memory
        .set_role(raul, "Product Owner", 4, State::Confirmed)
        .unwrap();

    assert_eq!(memory.list(None, Some("beri"), None).unwrap().len(), 1);
    assert_eq!(
        memory.list(None, Some("VALIDACION"), None).unwrap().len(),
        1
    );
    assert_eq!(
        memory.list(Some(Kind::Person), None, None).unwrap().len(),
        1
    );
    assert_eq!(
        memory.list(None, None, Some(State::Proposed)).unwrap()[0].name,
        "Raul Medina"
    );

    let backup = memory.export().unwrap();
    assert!(
        memory.import(&backup, 2).is_err(),
        "never into a memory that holds anything"
    );
    drop(memory);

    let mut restored = Memory::open(dir.path().join("restored.sqlite")).unwrap();
    assert_eq!(restored.import(&backup, 2).unwrap(), 2);
    assert_eq!(restored.export().unwrap(), backup);

    // Reopening keeps everything: the memory is never rebuilt.
    let reopened = Memory::open(&path).unwrap();
    assert_eq!(reopened.sightings(veridex).unwrap().len(), 1);
}

fn lesson(
    session: &str,
    series: &str,
    people: &[(&str, Option<&str>, bool)],
    terms: &[(&str, &str)],
) -> mc_memory::Lesson {
    mc_memory::Lesson {
        session: session.to_string(),
        series: Some(series.to_string()),
        at_ms: 100,
        people: people
            .iter()
            .map(|&(name, role, attended)| mc_memory::Met {
                name: name.to_string(),
                also: Vec::new(),
                role: role.map(str::to_string),
                attended,
            })
            .collect(),
        terms: terms
            .iter()
            .map(|&(t, m)| (t.to_string(), m.to_string()))
            .collect(),
    }
}

#[test]
fn minutes_teach_proposals_and_never_overwrite_what_the_user_confirmed() {
    let mut memory = Memory::open_in_memory().unwrap();
    let rut = memory
        .add(
            Kind::Term,
            "RUT",
            Some("Rol Único Tributario"),
            State::Confirmed,
            1,
        )
        .unwrap();
    let first = memory
        .learn(&lesson(
            "s1",
            "panel",
            &[
                ("Raul Medina", Some("Product Owner"), true),
                ("el Flaco", Some("negocio"), false),
            ],
            &[
                ("rut", "número de identificación"),
                ("Verídex", "proveedor de validación"),
            ],
        ))
        .unwrap();
    assert_eq!(first.added.len(), 3, "Raul, el Flaco and Verídex are new");
    assert_eq!(first.seen, [rut]);
    let kept = memory.get(rut).unwrap();
    assert_eq!(kept.meaning.as_deref(), Some("Rol Único Tributario"));
    assert_eq!(kept.meetings, 1);

    let raul = memory.candidates(Kind::Person, "raul medina").unwrap()[0];
    let flaco = memory.candidates(Kind::Person, "El Flaco").unwrap()[0];
    assert_eq!(memory.get(raul).unwrap().state, State::Proposed);
    assert!(!memory.sightings(flaco).unwrap()[0].attended);

    // The same meeting again, and a second one: weight grows, roles do not repeat.
    memory
        .learn(&lesson(
            "s1",
            "panel",
            &[("Raul Medina", Some("product owner"), true)],
            &[],
        ))
        .unwrap();
    memory
        .learn(&lesson(
            "s2",
            "panel",
            &[("Raul Medina", Some("Product Owner"), true)],
            &[],
        ))
        .unwrap();
    let raul = memory.get(raul).unwrap();
    assert_eq!(raul.meetings, 2);
    assert_eq!(raul.roles.len(), 1);
}

#[test]
fn a_homonym_the_meeting_cannot_settle_is_reported_not_guessed() {
    let mut memory = Memory::open_in_memory().unwrap();
    memory
        .add(Kind::Term, "Order", Some("equipo"), State::Confirmed, 1)
        .unwrap();
    memory
        .add(
            Kind::Term,
            "Order",
            Some("orden de compra"),
            State::Confirmed,
            1,
        )
        .unwrap();
    let learnt = memory
        .learn(&lesson("s9", "nueva", &[], &[("Order", "algo")]))
        .unwrap();
    assert_eq!(learnt.ambiguous, ["Order"]);
    assert!(learnt.added.is_empty() && learnt.seen.is_empty());
}

#[test]
fn many_commands_opening_a_new_memory_at_once_all_get_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let adopted: Vec<bool> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    let mut memory = Memory::open(&path).unwrap();
                    let entry = mc_memory::Adopted {
                        kind: Kind::Term,
                        name: "Verídex".into(),
                        meaning: None,
                        forms: vec!["beri dex".into()],
                        role: None,
                    };
                    memory.adopt("glossary.json", &[entry], 1).unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(adopted.iter().filter(|a| **a).count(), 1, "taken in once");
    let memory = Memory::open(&path).unwrap();
    assert_eq!(memory.list(None, None, None).unwrap().len(), 1);
}

#[test]
fn a_form_claimed_by_an_entry_takes_in_the_loose_one_unless_they_mean_different_things() {
    let mut memory = Memory::open_in_memory().unwrap();
    let keycloak = memory
        .add(
            Kind::Term,
            "Keycloak",
            Some("Autenticación"),
            State::Confirmed,
            1,
        )
        .unwrap();
    let typo = memory
        .add(Kind::Term, "Kicloak", None, State::Proposed, 1)
        .unwrap();
    memory.record(typo, &seen("s1", "auth", 2)).unwrap();
    assert_eq!(memory.claim_form(keycloak, "Kicloak", 3).unwrap(), [typo]);
    assert!(memory.get(typo).is_err(), "shown once, inside Keycloak");
    assert_eq!(memory.get(keycloak).unwrap().meetings, 1);

    let team = memory
        .add(
            Kind::Term,
            "Order",
            Some("equipo de órdenes"),
            State::Confirmed,
            1,
        )
        .unwrap();
    let purchase = memory
        .add(
            Kind::Term,
            "Orden de compra",
            Some("orden de compra"),
            State::Confirmed,
            1,
        )
        .unwrap();
    assert!(
        memory.claim_form(purchase, "Order", 4).unwrap().is_empty(),
        "homonyms stay"
    );
    assert!(memory.get(team).is_ok());

    let apart = memory.split_form(keycloak, "Kicloak", 5).unwrap();
    assert_eq!(memory.get(apart).unwrap().name, "Kicloak");
    assert_eq!(memory.candidates(Kind::Term, "kicloak").unwrap(), [apart]);
}

#[test]
fn tidying_joins_a_loose_entry_another_already_claims_and_leaves_homonyms() {
    let mut memory = Memory::open_in_memory().unwrap();
    let keycloak = memory
        .add(
            Kind::Term,
            "Keycloak",
            Some("Autenticación"),
            State::Confirmed,
            1,
        )
        .unwrap();
    let typo = memory
        .add(Kind::Term, "Kicloak", None, State::Proposed, 1)
        .unwrap();
    memory
        .add_form(keycloak, "Kicloak", State::Confirmed)
        .unwrap();
    memory
        .add(Kind::Term, "Order", Some("equipo"), State::Confirmed, 1)
        .unwrap();
    memory
        .add(
            Kind::Term,
            "Order",
            Some("orden de compra"),
            State::Confirmed,
            1,
        )
        .unwrap();
    assert_eq!(memory.tidy(2).unwrap(), [typo]);
    assert_eq!(memory.list(Some(Kind::Term), None, None).unwrap().len(), 3);
    assert!(memory.tidy(3).unwrap().is_empty());
}

#[test]
fn a_name_the_minutes_list_as_a_term_is_left_to_the_person_it_names() {
    let mut memory = Memory::open_in_memory().unwrap();
    let carolina = memory
        .add(Kind::Person, "Carolina Soto", None, State::Confirmed, 1)
        .unwrap();
    memory.add_form(carolina, "Caro", State::Confirmed).unwrap();
    let learnt = memory
        .learn(&lesson(
            "s1",
            "zeta",
            &[],
            &[("Caro", "Product Owner de Pagos"), ("UAT", "pruebas")],
        ))
        .unwrap();
    assert_eq!(learnt.added.len(), 1, "only UAT");
    assert!(memory.candidates(Kind::Term, "Caro").unwrap().is_empty());
}
