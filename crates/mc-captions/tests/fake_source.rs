//! Script semantics of `FakeSource`, which the live driver tests rely on.

use std::time::{Duration, Instant};

use mc_captions::{CaptureSource, Discovery, FakeSource, FakeStep, SourceError};
use mc_core::RawObservation;

fn obs(speaker: Option<&str>, text: Option<&str>) -> RawObservation {
    RawObservation {
        speaker: speaker.map(str::to_string),
        text: text.map(str::to_string),
        node_id: None,
    }
}

fn frame(read_ms: Option<f64>) -> FakeStep {
    FakeStep::Frame {
        nodes: vec![obs(Some("Ana"), Some("hola"))],
        read_ms,
    }
}

#[test]
fn empty_script_discovers_ended() {
    let mut source = FakeSource::new([]);
    assert_eq!(source.discover(), Ok(Discovery::Ended));
    assert_eq!(source.discover(), Ok(Discovery::Ended));
}

#[test]
fn non_empty_script_discovers_attached_under_the_source_name() {
    let mut source = FakeSource::new([frame(None)]).with_name("Teams window");
    assert_eq!(
        source.discover(),
        Ok(Discovery::Attached {
            window: "Teams window".to_string()
        })
    );
    assert_eq!(source.name(), "Teams window");
    assert_eq!(source.remaining(), 1, "discover does not consume frames");
}

#[test]
fn default_name_is_fake_source() {
    assert_eq!(FakeSource::new([]).name(), "FakeSource");
}

#[test]
fn scripted_discovery_answers_come_before_frames() {
    let mut source = FakeSource::new([
        FakeStep::Discovery(Discovery::Searching),
        FakeStep::Discovery(Discovery::Attached {
            window: "W".to_string(),
        }),
        frame(None),
    ]);
    assert_eq!(source.discover(), Ok(Discovery::Searching));
    assert_eq!(
        source.discover(),
        Ok(Discovery::Attached {
            window: "W".to_string()
        })
    );
    // The scripted answers were consumed; the frame is still there.
    assert_eq!(source.remaining(), 1);
    assert!(source.poll().is_ok());
}

#[test]
fn scripted_ended_discovery_answers_ended() {
    let mut source = FakeSource::new([FakeStep::Discovery(Discovery::Ended), frame(None)]);
    assert_eq!(source.discover(), Ok(Discovery::Ended));
}

#[test]
fn discovery_step_mid_run_is_lost_and_is_answered_by_the_next_discover() {
    let mut source = FakeSource::new([frame(None), FakeStep::Discovery(Discovery::Searching)]);
    assert!(source.discover().is_ok());
    assert!(source.poll().is_ok());
    // The window "closed": poll reports it, and the step is kept.
    assert_eq!(
        source.poll(),
        Err(SourceError::Lost("window closed".to_string()))
    );
    assert_eq!(source.remaining(), 1);
    assert_eq!(source.discover(), Ok(Discovery::Searching));
    assert_eq!(source.remaining(), 0);
}

#[test]
fn frames_helper_scripts_one_poll_per_frame_then_lost() {
    let mut source = FakeSource::frames(vec![
        vec![(Some("Ana"), Some("uno")), (Some("Luis"), None)],
        vec![],
        vec![(None, Some("tres"))],
    ]);
    assert_eq!(source.remaining(), 3);

    let first = source.poll().expect("first frame");
    assert_eq!(
        first,
        vec![obs(Some("Ana"), Some("uno")), obs(Some("Luis"), None)]
    );

    assert_eq!(source.poll(), Ok(Vec::new()), "an empty frame is a frame");

    assert_eq!(source.poll(), Ok(vec![obs(None, Some("tres"))]));

    assert_eq!(
        source.poll(),
        Err(SourceError::Lost("script exhausted".to_string())),
        "poll after exhaustion is Lost"
    );
    assert_eq!(
        source.poll(),
        Err(SourceError::Lost("script exhausted".to_string())),
        "and stays Lost"
    );
}

#[test]
fn frames_helper_reads_no_read_ms() {
    let mut source = FakeSource::frames(vec![vec![(Some("Ana"), Some("x"))]]);
    assert!(source.discover().is_ok());
    assert!(source.poll().is_ok());
    assert_eq!(source.read_ms_override(), None);
    assert_eq!(source.last_read_ms(), None);
}

#[test]
fn delay_really_sleeps_for_at_least_its_duration() {
    let wait = Duration::from_millis(60);
    let mut source = FakeSource::new([FakeStep::Delay(wait), frame(None)]);
    let started = Instant::now();
    let polled = source.poll();
    let elapsed = started.elapsed();
    assert!(polled.is_ok(), "the delay is followed by the frame");
    assert!(
        elapsed >= wait,
        "slept {elapsed:?}, wanted at least {wait:?}"
    );
}

#[test]
fn consecutive_delays_add_up() {
    let mut source = FakeSource::new([
        FakeStep::Delay(Duration::from_millis(30)),
        FakeStep::Delay(Duration::from_millis(30)),
        frame(None),
    ]);
    let started = Instant::now();
    assert!(source.poll().is_ok());
    assert!(started.elapsed() >= Duration::from_millis(60));
}

#[test]
fn error_steps_are_returned_by_poll_in_order() {
    let mut source = FakeSource::new([
        FakeStep::Error(SourceError::Transient("blip".to_string())),
        FakeStep::Error(SourceError::Lost("gone".to_string())),
        FakeStep::Error(SourceError::Fatal("broken".to_string())),
    ]);
    assert_eq!(
        source.poll(),
        Err(SourceError::Transient("blip".to_string()))
    );
    assert_eq!(source.poll(), Err(SourceError::Lost("gone".to_string())));
    assert_eq!(source.poll(), Err(SourceError::Fatal("broken".to_string())));
    assert_eq!(source.remaining(), 0);
}

#[test]
fn error_messages_name_their_kind() {
    assert_eq!(
        SourceError::Transient("x".into()).to_string(),
        "transient source error: x"
    );
    assert_eq!(SourceError::Lost("x".into()).to_string(), "source lost: x");
    assert_eq!(
        SourceError::Fatal("x".into()).to_string(),
        "fatal source error: x"
    );
}

#[test]
fn read_ms_override_reflects_the_last_frame() {
    let mut source = FakeSource::new([frame(Some(12.5)), frame(None), frame(Some(3.0))]);
    assert_eq!(source.read_ms_override(), None, "nothing polled yet");

    assert!(source.poll().is_ok());
    assert_eq!(source.read_ms_override(), Some(12.5));

    assert!(source.poll().is_ok());
    assert_eq!(
        source.read_ms_override(),
        None,
        "a frame without read_ms clears it"
    );

    assert!(source.poll().is_ok());
    assert_eq!(source.read_ms_override(), Some(3.0));
    assert_eq!(source.last_read_ms(), Some(3.0));
}

#[test]
fn read_ms_override_survives_an_error_step() {
    // Documents the current behaviour: an error does not reset the override,
    // so the driver reuses the previous frame's read_ms for the next frame.
    let mut source = FakeSource::new([
        frame(Some(7.0)),
        FakeStep::Error(SourceError::Transient("skip".into())),
    ]);
    assert!(source.poll().is_ok());
    assert!(source.poll().is_err());
    assert_eq!(source.read_ms_override(), Some(7.0));
}

#[test]
fn meeting_name_is_reported_when_set() {
    assert_eq!(FakeSource::new([]).meeting_name(), None);
    let source = FakeSource::new([]).with_meeting_name("Daily");
    assert_eq!(source.meeting_name().as_deref(), Some("Daily"));
}

#[test]
fn discovery_step_behind_a_frame_is_not_answered_first() {
    // The source comment says scripted answers come first "wherever they sit";
    // the code only takes one from the front, so a frame ahead of it wins.
    let mut source = FakeSource::new([frame(None), FakeStep::Discovery(Discovery::Ended)]);
    assert!(matches!(source.discover(), Ok(Discovery::Attached { .. })));
    assert!(source.poll().is_ok());
    assert_eq!(
        source.poll(),
        Err(SourceError::Lost("window closed".to_string()))
    );
    assert_eq!(source.discover(), Ok(Discovery::Ended));
}
