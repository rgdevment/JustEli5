//! A profile card hovered during a Teams meeting is also a group of
//! two texts: its labels and bios must not become speakers.

use mc_captions::ui::node::{Role, UiNode};
use mc_captions::ui::selectors::TeamsSel;
use mc_captions::ui::teams;

fn group(speaker: &str, text: &str) -> UiNode {
    UiNode::new(Role::Group, "").with_children(vec![
        UiNode::new(Role::Text, speaker),
        UiNode::new(Role::Text, text),
    ])
}

#[test]
fn profile_card_labels_and_bios_are_not_captions() {
    let root = UiNode::new(Role::Document, "").with_children(vec![
        group("Raul Ignacio Medina Carrasco", "Correcto, lo vemos el jueves."),
        group("Organización", "Administrador"),
        group(
            "Me gusta el ciclismo de montaña, la fotografía de paisajes y cocinar para mis amigos los fines de semana, y",
            "también leer novelas de ciencia ficción, aprender idiomas nuevos y salir a caminar con mi perro por el c…",
        ),
        group(
            "Loves hiking, coffee and long weekend bike rides with friends, and reading about history and space.At home, I",
            "grow tomatoes, bake bread, and play board games with my kids most evenings (chess, Go, car…",
        ),
        group("Hector E Cortes V", "Primero revisamos la agenda."),
        group("J. Pérez", "Una inicial con punto es un nombre."),
    ]);
    let speakers: Vec<String> = teams::captions(&root, &TeamsSel::default())
        .into_iter()
        .filter_map(|o| o.speaker)
        .collect();
    assert_eq!(
        speakers,
        [
            "Raul Ignacio Medina Carrasco",
            "Hector E Cortes V",
            "J. Pérez"
        ]
    );
}
