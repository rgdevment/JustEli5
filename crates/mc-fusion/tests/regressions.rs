//! Cases found by auditing the fusion: each one built by hand from a few
//! segments so the intended behaviour is readable.

use mc_asr::AsrSegment;
use mc_core::Segment;
use mc_fusion::{FusionConfig, Source, fuse};

fn cap(speaker: &str, text: &str, first: f64, last: f64) -> Segment {
    Segment {
        speaker: speaker.into(),
        speaker_raw: None,
        text: text.into(),
        first_mono_ms: Some(first),
        last_mono_ms: Some(last),
        committed_mono_ms: Some(last + 1200.0),
        committed_wall_ms: None,
    }
}

fn asr(ch: &str, start: f64, end: f64, text: &str) -> AsrSegment {
    AsrSegment {
        v: 1,
        ch: ch.into(),
        start_ms: start,
        end_ms: end,
        text: text.into(),
        lang: Some("es".into()),
        conf: Some(-0.2),
        no_speech: Some(0.02),
        words: vec![],
    }
}

fn cfg(glossary: &[&str]) -> FusionConfig {
    FusionConfig {
        glossary: glossary.iter().map(|s| s.to_string()).collect(),
        ..FusionConfig::default()
    }
}

#[test]
fn one_asr_segment_covering_two_captions_does_not_duplicate_words() {
    let captions = [
        cap("Luis", "Hice el dipley ayer", 2200.0, 3500.0),
        cap("Luis", "vale lo reviso hoy", 3800.0, 5000.0),
    ];
    let heard = [asr(
        "system",
        1000.0,
        3900.0,
        "Hice el deploy ayer vale lo reviso hoy",
    )];
    let result = fuse(&captions, &heard, &cfg(&["deploy"]));
    let text: Vec<&str> = result.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(text.len(), 2, "{:#?}", result.segments);
    assert_eq!(text.join(" ").matches("vale").count(), 1, "{text:?}");
    assert_eq!(text.join(" ").matches("ayer").count(), 1, "{text:?}");
    assert!(
        text[0].contains("deploy"),
        "the shared segment may still fix a term: {text:?}"
    );
}

#[test]
fn an_asr_segment_goes_to_the_caption_that_says_the_same_not_the_one_that_shares_two_words() {
    let captions = [
        cap(
            "Luis",
            "el deploy de ayer fue un desastre total",
            2200.0,
            4800.0,
        ),
        cap("Marta", "el dipley de hoy sale bien", 2300.0, 4400.0),
    ];
    let heard = [
        asr("system", 1000.0, 3600.0, "el deploy de hoy sale bien"),
        asr(
            "system",
            1100.0,
            3700.0,
            "el deploy de ayer fue un desastre total",
        ),
    ];
    let result = fuse(&captions, &heard, &cfg(&["deploy"]));
    let by = |name: &str| result.segments.iter().find(|s| s.speaker == name).unwrap();
    assert_eq!(by("Luis").text, "el deploy de ayer fue un desastre total");
    assert_eq!(
        by("Marta").text,
        "el deploy de hoy sale bien",
        "{:#?}",
        result.segments
    );
}

#[test]
fn only_the_term_is_replaced_and_its_punctuation_survives() {
    let captions = [cap(
        "Luis",
        "Creo que ¿estaging? no, a estaging va",
        2200.0,
        4200.0,
    )];
    let heard = [asr(
        "system",
        1000.0,
        3800.0,
        "Creo que staging no al staging va",
    )];
    let result = fuse(&captions, &heard, &cfg(&["staging"]));
    let text = &result.segments[0].text;
    assert!(text.contains("¿staging?"), "{text}");
    assert!(
        text.contains(" a staging "),
        "Teams' own word stays: {text}"
    );
    assert!(!text.contains("estaging"), "{text}");
}

#[test]
fn a_name_that_differs_only_by_accent_is_still_the_user() {
    let captions = [
        cap("Ana Pérez", "Yo hice push ayer y ya está", 2000.0, 4000.0),
        cap("Luis", "Perfecto seguimos con el sprint", 6000.0, 8000.0),
    ];
    let heard = [
        asr("mic", 800.0, 2900.0, "Yo hice push ayer y ya está"),
        asr("system", 4800.0, 7000.0, "Perfecto seguimos con el sprint"),
    ];
    let config = FusionConfig {
        me: Some("Ana Perez".into()),
        glossary: vec!["push".into(), "sprint".into()],
        ..FusionConfig::default()
    };
    let result = fuse(&captions, &heard, &config);
    assert_eq!(result.report.me.as_deref(), Some("Ana Pérez"));
    assert_eq!(result.report.me_source, "configured");
    assert_eq!(
        result.segments.len(),
        2,
        "no duplicate of the user's line: {:#?}",
        result.segments
    );
    assert!(result.segments.iter().all(|s| s.source == Source::Merged));
}

#[test]
fn a_configured_name_nobody_has_falls_back_to_the_audio() {
    let captions: Vec<Segment> = (0..4)
        .map(|n| {
            cap(
                "Ana",
                &format!("esta es mi frase numero {n} sobre el proyecto"),
                2000.0 + n as f64 * 6000.0,
                4000.0 + n as f64 * 6000.0,
            )
        })
        .collect();
    let heard: Vec<AsrSegment> = (0..4)
        .map(|n| {
            asr(
                "mic",
                800.0 + n as f64 * 6000.0,
                3000.0 + n as f64 * 6000.0,
                &format!("esta es mi frase numero {n} sobre el proyecto"),
            )
        })
        .collect();
    let config = FusionConfig {
        me: Some("Nadie Asi".into()),
        ..FusionConfig::default()
    };
    let result = fuse(&captions, &heard, &config);
    assert_eq!(result.report.me.as_deref(), Some("Ana"));
    assert_eq!(result.report.me_source, "inferred");
    assert_eq!(result.segments.len(), 4);
}

#[test]
fn non_finite_times_never_steal_speech() {
    let mut broken = cap("Luis", "primero el deploy de hoy", 2000.0, 3000.0);
    broken.first_mono_ms = Some(f64::NAN);
    broken.last_mono_ms = Some(f64::NAN);
    let captions = [
        broken,
        cap("Marta", "segundo el deploy de ayer", 9000.0, 10_500.0),
    ];
    let heard = [
        asr("system", 8000.0, 9800.0, "segundo el deploy de ayer"),
        asr("system", f64::NAN, 5000.0, "texto con tiempo roto"),
    ];
    let result = fuse(&captions, &heard, &cfg(&["deploy"]));
    let marta = result
        .segments
        .iter()
        .find(|s| s.speaker == "Marta")
        .unwrap();
    assert_eq!(marta.source, Source::Merged);
    let luis = result
        .segments
        .iter()
        .find(|s| s.speaker == "Luis")
        .unwrap();
    assert_eq!(luis.source, Source::Captions);
    assert_eq!(luis.text, "primero el deploy de hoy");
    assert!(
        result
            .segments
            .iter()
            .all(|s| s.start_ms.is_finite() && s.end_ms.is_finite())
    );
}

#[test]
fn a_lone_yes_cannot_set_the_lag() {
    let captions = [
        cap("Luis", "sí", 12_000.0, 12_400.0),
        cap(
            "Marta",
            "esto es una frase normal de prueba",
            20_000.0,
            22_000.0,
        ),
    ];
    let heard = [
        asr("system", 6_100.0, 6_500.0, "sí"),
        asr(
            "system",
            18_800.0,
            20_700.0,
            "esto es una frase normal de prueba",
        ),
    ];
    let result = fuse(&captions, &heard, &FusionConfig::default());
    let lag = result.report.lag.median_ms.unwrap();
    assert!((lag - 1200.0).abs() <= 100.0, "{lag}");
    assert_eq!(result.report.lag.samples, 1);
}

#[test]
fn a_short_reply_from_the_user_is_not_taken_for_a_leak() {
    let captions = [cap(
        "Luis",
        "estamos de acuerdo entonces sí",
        2000.0,
        4000.0,
    )];
    let heard = [
        asr("system", 800.0, 3000.0, "estamos de acuerdo entonces sí"),
        asr("mic", 2500.0, 2900.0, "Sí."),
    ];
    let config = FusionConfig {
        me: Some("Ana".into()),
        ..FusionConfig::default()
    };
    let result = fuse(&captions, &heard, &config);
    assert!(
        result
            .segments
            .iter()
            .any(|s| s.ch == "mic" && s.text == "Sí."),
        "{:#?}",
        result.segments
    );
    assert_eq!(result.report.counts.dropped_leak, 0);
}

#[test]
fn a_reply_during_a_long_caption_is_kept_when_it_says_something_else() {
    let captions = [cap(
        "Luis",
        "este es un discurso largo sobre la arquitectura del sistema nuevo",
        2000.0,
        12_000.0,
    )];
    let heard = [
        asr(
            "system",
            800.0,
            10_800.0,
            "este es un discurso largo sobre la arquitectura del sistema nuevo",
        ),
        asr("mic", 5000.0, 6000.0, "perdona una pregunta rápida"),
    ];
    let config = FusionConfig {
        me: Some("Ana".into()),
        ..FusionConfig::default()
    };
    let result = fuse(&captions, &heard, &config);
    assert!(
        result
            .segments
            .iter()
            .any(|s| s.source == Source::Asr && s.text.contains("pregunta")),
        "{:#?}",
        result.segments
    );
}

#[test]
fn a_huge_pair_is_left_to_the_captions_instead_of_exhausting_memory() {
    let long = (0..2500)
        .map(|n| format!("palabra{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let captions = [cap("Luis", &long, 2000.0, 900_000.0)];
    let heard = [asr("system", 800.0, 898_000.0, &long)];
    let result = fuse(&captions, &heard, &FusionConfig::default());
    assert_eq!(result.segments[0].text, long);
}

#[test]
fn an_acronym_the_captions_wrote_in_lowercase_gets_its_capitals_when_the_audio_says_it() {
    let captions = [cap(
        "Ana",
        "empezamos la daily del equipo saga.",
        1200.0,
        4000.0,
    )];
    let heard = [asr(
        "system",
        1000.0,
        4200.0,
        "empezamos la daily del equipo Saga",
    )];
    let result = fuse(&captions, &heard, &cfg(&["SAGA", "Daily"]));
    assert_eq!(
        result.segments[0].text, "empezamos la daily del equipo SAGA.",
        "the acronym is restored and a plain word keeps the caption's casing"
    );
}

#[test]
fn a_form_the_user_taught_becomes_its_term_even_when_the_audio_says_nothing_else() {
    let captions = [cap("Raul", "Lo valida Beri dex, siempre.", 2200.0, 4200.0)];
    let heard = [asr("system", 1000.0, 3800.0, "Lo valida beri dex siempre")];
    let config = FusionConfig {
        aliases: vec![("beri dex".into(), "Verídex".into())],
        ..cfg(&["Verídex"])
    };
    let result = fuse(&captions, &heard, &config);
    assert_eq!(result.segments[0].text, "Lo valida Verídex, siempre.");
    let without = fuse(&captions, &heard, &cfg(&["Verídex"]));
    assert!(
        without.segments[0].text.contains("Beri dex"),
        "taught forms only"
    );
}
