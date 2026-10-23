//! Unit tests for the Python-semantics helpers, decisions and clocks.

use std::panic::{self, AssertUnwindSafe};
use std::thread;
use std::time::Duration;

use mc_core::clock::{Clock, FakeClock, ScriptedClock, SessionClock, Stamp};
use mc_core::pytext::{clean_text, common_prefix_len, fmt2, has_alnum, normalize_text, py_isspace};
use mc_core::speakers::normalize_speaker;
use mc_core::types::{Decision, UnknownDecision};

#[test]
fn py_isspace_matches_python_whitespace() {
    for c in '\u{1c}'..='\u{1f}' {
        assert!(
            py_isspace(c),
            "{c:?} (U+{:04X}) should be whitespace",
            c as u32
        );
    }
    for c in ['\u{85}', '\u{2028}', ' ', '\t'] {
        assert!(
            py_isspace(c),
            "{c:?} (U+{:04X}) should be whitespace",
            c as u32
        );
    }
    for c in ['\u{200b}', '\u{feff}', '\u{180e}', 'a'] {
        assert!(
            !py_isspace(c),
            "{c:?} (U+{:04X}) should not be whitespace",
            c as u32
        );
    }
}

#[test]
fn clean_text_collapses_python_whitespace() {
    assert_eq!(clean_text("  a \u{1c} b  "), "a b");
}

#[test]
fn has_alnum_is_ascii_only() {
    assert!(!has_alnum("é"));
    assert!(!has_alnum("٣"));
    assert!(!has_alnum("１２３"));
    assert!(has_alnum("a"));
    assert!(has_alnum("7"));
}

#[test]
fn normalize_text_keeps_spanish_inverted_punctuation() {
    assert_eq!(normalize_text("¿Qué? ¡Hola!"), "¿qué ¡hola");
}

#[test]
fn common_prefix_len_counts_characters_not_bytes() {
    assert_eq!(common_prefix_len("😀a", "😀b"), 1);
    assert_eq!(common_prefix_len("😀😃", "😀😄"), 1);
    assert_eq!(common_prefix_len("a😀", "a😃"), 1);
    assert_eq!(common_prefix_len("😀😃x", "😀😃x"), 3);
    assert_eq!(common_prefix_len("é", "è"), 0);
}

#[test]
fn decision_display_matches_python_labels() {
    let cases = [
        (Decision::EmptyText, "empty_text"),
        (Decision::NoAlnum, "no_alnum"),
        (Decision::NewOpen, "new_open"),
        (Decision::DupExact, "dup_exact"),
        (Decision::DupNorm, "dup_norm"),
        (Decision::DupNormLonger, "dup_norm_longer"),
        (Decision::Grow, "grow"),
        (
            Decision::Merge {
                prefix_ratio: 0.5,
                ratio: 0.5,
            },
            "merge_p0.50_r0.50",
        ),
        (
            Decision::Merge {
                prefix_ratio: 0.5,
                ratio: 0.8,
            },
            "merge_p0.50_r0.80",
        ),
        (Decision::Newline, "newline"),
        (Decision::Committed, "committed"),
        (Decision::DupCommittedSkip, "dup_committed_skip"),
    ];
    for (decision, label) in cases {
        assert_eq!(decision.to_string(), label, "{decision:?}");
        let parsed: Decision = label.parse().expect("label must parse");
        assert_eq!(parsed, decision, "round-trip of {label}");
    }
}

#[test]
fn decision_from_str_rejects_unknown_labels() {
    let err = "nope".parse::<Decision>().unwrap_err();
    assert_eq!(err, UnknownDecision("nope".to_string()));
    assert!("merge_pX_r0.50".parse::<Decision>().is_err());
    assert!("".parse::<Decision>().is_err());
}

#[test]
fn fake_clock_follows_mono_and_wall_offset() {
    let clock = FakeClock::new(1_700_000_000_000);
    let s = clock.now();
    assert_eq!(s.mono_ms, 0.0);
    assert_eq!(s.wall_ms, 1_700_000_000_000);

    clock.set_mono_ms(1500.0);
    let s = clock.now();
    assert_eq!(s.mono_ms, 1500.0);
    assert_eq!(s.wall_ms, 1_700_000_000_000 + 1500);

    clock.advance_ms(250.0);
    let s = clock.now();
    assert_eq!(s.mono_ms, 1750.0);
    assert_eq!(s.wall_ms, 1_700_000_000_000 + 1750);
}

#[test]
fn scripted_clock_replays_stamps_in_order() {
    let a = Stamp {
        wall_ms: 10,
        mono_ms: 0.0,
    };
    let b = Stamp {
        wall_ms: 20,
        mono_ms: 10.0,
    };
    let c = Stamp {
        wall_ms: 35,
        mono_ms: 25.5,
    };
    let clock = ScriptedClock::new([a, b, c]);
    assert_eq!(clock.calls(), 0);
    assert_eq!(clock.remaining(), 3);

    assert_eq!(clock.now(), a);
    assert_eq!(clock.calls(), 1);
    assert_eq!(clock.remaining(), 2);

    assert_eq!(clock.now(), b);
    assert_eq!(clock.now(), c);
    assert_eq!(clock.calls(), 3);
    assert_eq!(clock.remaining(), 0);
}

#[test]
#[should_panic(expected = "scripted clock exhausted")]
fn scripted_clock_panics_when_exhausted() {
    let clock = ScriptedClock::new([Stamp {
        wall_ms: 1,
        mono_ms: 0.0,
    }]);
    let _ = clock.now();
    let _ = clock.now();
}

#[test]
fn session_clock_is_monotonic_and_sane() {
    let clock = SessionClock::new();
    assert!(
        clock.t0_wall_ms() > 1_700_000_000_000,
        "t0 {}",
        clock.t0_wall_ms()
    );

    let mut prev = clock.now();
    assert!(prev.mono_ms >= 0.0, "mono {}", prev.mono_ms);
    assert!(
        prev.wall_ms >= clock.t0_wall_ms() - 2000,
        "wall {} far before t0 {}",
        prev.wall_ms,
        clock.t0_wall_ms()
    );
    for _ in 0..1000 {
        let next = clock.now();
        assert!(
            next.mono_ms >= prev.mono_ms,
            "mono went back: {} -> {}",
            prev.mono_ms,
            next.mono_ms
        );
        assert!(next.mono_ms >= 0.0);
        prev = next;
    }
}

#[test]
fn session_clock_mono_grows_across_sleep() {
    let clock = SessionClock::new();
    let before = clock.now();
    thread::sleep(Duration::from_millis(20));
    let after = clock.now();
    let grown = after.mono_ms - before.mono_ms;
    assert!(
        grown >= 15.0,
        "mono grew only {grown} ms after a 20 ms sleep"
    );
    assert!(after.wall_ms >= before.wall_ms);
}

#[test]
fn clock_is_object_safe_and_send_sync() {
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    fn read(clock: &dyn Clock) -> Stamp {
        clock.now()
    }
    fn assert_clock<C: Clock>(clock: &C) -> Stamp {
        clock.now()
    }

    assert_send_sync::<dyn Clock>();
    assert_send_sync::<Box<dyn Clock>>();
    assert_send_sync::<SessionClock>();
    assert_send_sync::<FakeClock>();
    assert_send_sync::<ScriptedClock>();

    let fake = FakeClock::new(5);
    assert_eq!(read(&fake).wall_ms, 5);
    assert_eq!(assert_clock(&fake).wall_ms, 5);
    let boxed: Box<dyn Clock> = Box::new(SessionClock::new());
    let _ = read(boxed.as_ref());
}

#[test]
fn fake_clock_wall_rounds_mono_ties_to_even() {
    let base = 1_700_000_000_000;
    for (mono, offset) in [
        (1500.6, 1501),
        (1500.5, 1500),
        (1501.5, 1502),
        (0.4, 0),
        (2.5, 2),
    ] {
        let clock = FakeClock::new(base);
        clock.set_mono_ms(mono);
        let s = clock.now();
        assert_eq!(s.mono_ms, mono);
        assert_eq!(s.wall_ms, base + offset, "mono {mono}");
    }
}

#[test]
fn decision_from_str_rejects_non_canonical_merge_labels() {
    for label in [
        "merge_p0.5_r0.5",
        "merge_pnan_r1",
        "merge_p+1e0_r-0",
        "merge_pinf_rinfinity",
    ] {
        assert!(
            label.parse::<Decision>().is_err(),
            "{label} must be rejected"
        );
    }
}

#[test]
fn decision_merge_equality_is_by_label() {
    let parsed: Decision = "merge_p0.67_r0.67".parse().expect("canonical label");
    let built = Decision::Merge {
        prefix_ratio: 2.0 / 3.0,
        ratio: 2.0 / 3.0,
    };
    assert_eq!(parsed, built);
    assert_eq!(parsed.to_string(), built.to_string());
    assert_eq!(parsed.to_string(), "merge_p0.67_r0.67");
}

#[test]
fn decision_merge_label_grid_round_trips() {
    for p in 0..=100 {
        for r in 0..=100 {
            let label = format!("merge_p{:.2}_r{:.2}", p as f64 / 100.0, r as f64 / 100.0);
            let decision: Decision = label
                .parse()
                .unwrap_or_else(|e| panic!("{label} must parse: {e}"));
            assert_eq!(decision.to_string(), label);
        }
    }
}

/// Python's `str.isspace()` code points as inclusive ranges (29 in total).
const PY_SPACE_RANGES: [(u32, u32); 11] = [
    (0x9, 0xD),
    (0x1C, 0x1F),
    (0x20, 0x20),
    (0x85, 0x85),
    (0xA0, 0xA0),
    (0x1680, 0x1680),
    (0x2000, 0x200A),
    (0x2028, 0x2029),
    (0x202F, 0x202F),
    (0x205F, 0x205F),
    (0x3000, 0x3000),
];

#[test]
fn py_isspace_matches_python_list_for_every_char() {
    let listed: u32 = PY_SPACE_RANGES.iter().map(|(lo, hi)| hi - lo + 1).sum();
    assert_eq!(listed, 29, "Python isspace list must have 29 points");

    for cp in 0..=0x10FFFF_u32 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let expected = PY_SPACE_RANGES
            .iter()
            .any(|&(lo, hi)| (lo..=hi).contains(&cp));
        assert_eq!(py_isspace(c), expected, "U+{cp:04X}");
    }
}

#[test]
fn normalize_speaker_folds_anonymous_aliases_like_python() {
    let anonymous = "Invitado sin nombre";
    for raw in [
        "UNKNOWN USER",
        "Unknown\u{1c}User",
        " unknown\u{85}user ",
        // U+212A KELVIN SIGN folds to 'k', so this spells "unknown user".
        "un\u{212a}nown user",
        "uſuario desconocido",
        "USUARIO DESCONOCIDO",
    ] {
        assert_eq!(
            normalize_speaker(Some(raw)),
            (anonymous.to_string(), true),
            "{raw:?}"
        );
    }

    // "Known user" with a Kelvin sign is not an alias.
    assert_eq!(
        normalize_speaker(Some("\u{212a}nown user")),
        ("\u{212a}nown user".to_string(), false)
    );
    assert_eq!(
        normalize_speaker(Some("unknown\u{200b}user")),
        ("unknown\u{200b}user".to_string(), false)
    );
    assert_eq!(
        normalize_speaker(Some("Ünknown user")),
        ("Ünknown user".to_string(), false)
    );
}

#[test]
fn normalize_text_handles_final_sigma_and_special_lowercase() {
    assert_eq!(normalize_text("ΑΣ."), "ας");
    assert_eq!(normalize_text("ΑΣ1"), "ας1");
    assert_eq!(normalize_text("ΑΣ'Α"), "ασα");
    let dotted = normalize_text("İ");
    assert_eq!(dotted, "i\u{307}");
    assert_eq!(dotted.chars().count(), 2);
    assert_eq!(normalize_text("Straße"), "straße");
}

#[test]
fn fmt2_prints_non_finite_and_signed_zero_like_python() {
    assert_eq!(fmt2(f64::NAN), "nan");
    assert_eq!(fmt2(f64::INFINITY), "inf");
    assert_eq!(fmt2(f64::NEG_INFINITY), "-inf");
    assert_eq!(fmt2(-0.0), "-0.00");
    assert_eq!(fmt2(1e-7), "0.00");
}

#[test]
fn scripted_clock_stays_usable_after_exhaustion_panic() {
    let clock = ScriptedClock::new(std::iter::empty::<Stamp>());
    let result = panic::catch_unwind(AssertUnwindSafe(|| clock.now()));
    assert!(result.is_err(), "exhausted clock must panic");
    assert_eq!(clock.remaining(), 0);
    assert_eq!(clock.calls(), 1);
}
