//! The driver: feeds the transcript to the model in windows and keeps the ledger.

use std::ops::Range;

use mc_llm::{Llm, LlmRequest};
use serde::{Deserialize, Serialize};

use crate::carry::CarriedCommitment;
use crate::ledger::{ChangedItem, Ledger};
use crate::patch::{Known, Rejection, apply_patch_carried, parse_patch};
use crate::prompt::{ANALYST_SYSTEM, analysis_user, shown_carried};
use crate::transcript::TranscriptSegment;

#[derive(Debug, Clone, PartialEq)]
pub struct AnalystConfig {
    /// A window closes at this many segments...
    pub window_segments: usize,
    /// ...or when it spans this many seconds of meeting time.
    pub window_seconds: i64,
    pub max_tokens: u32,
}

impl AnalystConfig {
    /// A recorded meeting has no one waiting for the panel: fewer, larger windows cost far fewer
    /// tokens, since every call sends the instructions and the ledger again.
    pub fn offline() -> Self {
        Self {
            window_segments: 60,
            window_seconds: 900,
            ..Self::default()
        }
    }
}

impl Default for AnalystConfig {
    fn default() -> Self {
        Self {
            window_segments: 20,
            window_seconds: 75,
            max_tokens: 4096,
        }
    }
}

/// What the model is told besides the transcript.
#[derive(Debug, Clone, Default)]
pub struct Background {
    pub glossary: Vec<String>,
    /// How the recognisers tend to mishear a glossary term: (term, misspellings).
    pub aliases: Vec<(String, Vec<String>)>,
    /// A form that names more than one entry, and what each of them is: the context decides.
    pub homonyms: Vec<(String, Vec<String>)>,
    pub project_context: String,
    /// (heard, correct) the user accepted while the meeting ran.
    pub term_fixes: Vec<(String, String)>,
    /// Commitments still open from the previous meeting of the series.
    pub carried: Vec<CarriedCommitment>,
    /// The person recording, as the captions name them (`fusion.me`).
    pub me: Option<String>,
    /// The meeting's name and date, so "mañana" and the topics have an anchor.
    pub meeting: String,
    /// Who speaks in the transcript, to tell the user's lines from a namesake's.
    pub participants: Vec<String>,
}

/// Segments of the previous window shown again, so an answer can be read with its question.
const RECENT_SEGMENTS: usize = 5;

/// One model call, as `ledger_events.jsonl` records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub first_id: u64,
    pub last_id: u64,
    pub segments: usize,
    pub model: String,
    pub attempts: u32,
    pub accepted: usize,
    pub rejected: usize,
    pub rejections: Vec<Rejection>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub ledger: Ledger,
    pub events: Vec<LedgerEvent>,
}

impl Analysis {
    pub fn failed_windows(&self) -> usize {
        self.events.iter().filter(|e| e.error.is_some()).count()
    }
}

pub fn render_events(events: &[LedgerEvent]) -> String {
    events
        .iter()
        .map(|e| serde_json::to_string(e).expect("an event serialises") + "\n")
        .collect()
}

/// Whether the segment at `i` closes the window that started at `start`.
fn closes(segments: &[TranscriptSegment], start: usize, i: usize, cfg: &AnalystConfig) -> bool {
    let span = segments[i].start_ms - segments[start].start_ms;
    i + 1 - start >= cfg.window_segments.max(1) || span >= cfg.window_seconds.max(1) * 1000
}

/// Index ranges of the windows: closed by segment count or by meeting time, the tail included.
pub fn windows(segments: &[TranscriptSegment], cfg: &AnalystConfig) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..segments.len() {
        if closes(segments, start, i, cfg) {
            out.push(start..i + 1);
            start = i + 1;
        }
    }
    if start < segments.len() {
        out.push(start..segments.len());
    }
    out
}

/// Two tries: a transient failure or a garbled answer is not worth losing a window over.
pub(crate) fn ask<T>(
    llm: &dyn Llm,
    request: &LlmRequest,
    parse: impl Fn(&str) -> Result<T, String>,
) -> (Result<T, String>, u32) {
    let mut last = String::new();
    for attempt in 1..=2 {
        match llm
            .complete(request)
            .map_err(|e| e.to_string())
            .and_then(|answer| parse(&answer))
        {
            Ok(value) => return (Ok(value), attempt),
            Err(e) => last = e,
        }
    }
    (Err(last), 2)
}

/// Asks the model about one window and applies its patch. A window that fails twice
/// leaves the ledger as it was and says why in the event.
fn analyze_window(
    llm: &dyn Llm,
    ledger: &mut Ledger,
    known: &mut Known,
    background: &Background,
    previous: &[TranscriptSegment],
    window: &[TranscriptSegment],
    cfg: &AnalystConfig,
) -> LedgerEvent {
    let request = LlmRequest {
        system: ANALYST_SYSTEM.to_string(),
        user: analysis_user(ledger, background, previous, window),
        max_tokens: cfg.max_tokens,
        temperature: Some(0.0),
        json: true,
    };
    let mut event = LedgerEvent {
        first_id: window[0].id,
        last_id: window[window.len() - 1].id,
        segments: window.len(),
        model: llm.name(),
        attempts: 0,
        accepted: 0,
        rejected: 0,
        rejections: Vec::new(),
        error: None,
    };
    let (patch, attempts) = ask(llm, &request, |a| parse_patch(a).map_err(|e| e.to_string()));
    event.attempts = attempts;
    match patch {
        Ok(patch) => {
            // Only what the model has been shown can be cited.
            known.extend(window.iter().map(|s| (s.id, s.start_ms)));
            let shown: Vec<CarriedCommitment> = shown_carried(&background.carried)
                .into_iter()
                .cloned()
                .collect();
            let applied = apply_patch_carried(ledger, &patch, known, &shown);
            event.accepted = applied.accepted;
            event.rejected = applied.rejected.len();
            event.rejections = applied.rejected;
            *ledger = applied.ledger;
        }
        Err(error) => event.error = Some(error),
    }
    event
}

/// Feeds every window in order. A window that fails twice is skipped and logged;
/// the ledger built so far is never lost.
pub fn analyze(
    llm: &dyn Llm,
    transcript: &[TranscriptSegment],
    background: &Background,
    cfg: &AnalystConfig,
) -> Analysis {
    let mut ledger = Ledger::default();
    let mut events = Vec::new();
    let mut known = Known::new();
    for range in windows(transcript, cfg) {
        let previous = &transcript[range.start.saturating_sub(RECENT_SEGMENTS)..range.start];
        let window = &transcript[range];
        events.push(analyze_window(
            llm,
            &mut ledger,
            &mut known,
            background,
            previous,
            window,
            cfg,
        ));
    }
    Analysis { ledger, events }
}

/// The analyst of a meeting still going on: segments arrive one by one and a window is
/// analysed as soon as it closes. No threads and no I/O; the caller decides when to step.
pub struct Analyst {
    llm: Box<dyn Llm>,
    cfg: AnalystConfig,
    background: Background,
    ledger: Ledger,
    known: Known,
    pending: Vec<TranscriptSegment>,
    /// The tail of the last window analysed.
    recent: Vec<TranscriptSegment>,
    events: Vec<LedgerEvent>,
}

/// What one window changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub event: LedgerEvent,
    pub changed: Vec<ChangedItem>,
}

impl Analyst {
    pub fn new(llm: Box<dyn Llm>, cfg: AnalystConfig, background: Background) -> Self {
        Self {
            llm,
            cfg,
            background,
            ledger: Ledger::default(),
            known: Known::new(),
            pending: Vec::new(),
            recent: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn push(&mut self, segment: TranscriptSegment) {
        self.pending.push(segment);
    }

    /// Glossary and context for the windows still to come (the user may add term fixes meanwhile).
    pub fn set_background(&mut self, background: Background) {
        self.background = background;
    }

    pub fn pending(&mut self) -> &mut [TranscriptSegment] {
        &mut self.pending
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn events(&self) -> &[LedgerEvent] {
        &self.events
    }

    /// Analyses the first closed window; with `flush`, an open one too (end of the meeting
    /// or a long pause). `None`: nothing to do yet.
    pub fn step(&mut self, flush: bool) -> Option<Step> {
        let closed = (0..self.pending.len()).find(|&i| closes(&self.pending, 0, i, &self.cfg));
        let len = match closed {
            Some(i) => i + 1,
            None if flush && !self.pending.is_empty() => self.pending.len(),
            None => return None,
        };
        let window: Vec<TranscriptSegment> = self.pending.drain(..len).collect();
        let before = self.ledger.clone();
        let event = analyze_window(
            self.llm.as_ref(),
            &mut self.ledger,
            &mut self.known,
            &self.background,
            &self.recent,
            &window,
            &self.cfg,
        );
        self.recent = window[window.len().saturating_sub(RECENT_SEGMENTS)..].to_vec();
        self.events.push(event.clone());
        Some(Step {
            event,
            changed: self.ledger.changed_since(&before),
        })
    }
}

#[cfg(test)]
mod tests {
    use mc_llm::{FakeLlm, LlmError};

    use super::*;

    fn segments(n: u64, step_ms: i64) -> Vec<TranscriptSegment> {
        (0..n)
            .map(|id| TranscriptSegment {
                id,
                speaker: "Ana".into(),
                text: format!("texto {id}"),
                start_ms: id as i64 * step_ms,
                end_ms: id as i64 * step_ms + 1,
            })
            .collect()
    }

    fn cfg(count: usize, seconds: i64) -> AnalystConfig {
        AnalystConfig {
            window_segments: count,
            window_seconds: seconds,
            ..AnalystConfig::default()
        }
    }

    #[test]
    fn windows_close_by_count_or_by_meeting_time() {
        let by_count = windows(&segments(7, 1000), &cfg(3, 600));
        assert_eq!(by_count, [0..3, 3..6, 6..7]);
        // 40 s apart: the span reaches 60 s on the third segment.
        let by_time = windows(&segments(5, 40_000), &cfg(100, 60));
        assert_eq!(by_time, [0..3, 3..5]);
        assert!(windows(&[], &cfg(3, 60)).is_empty());
    }

    #[test]
    fn the_ledger_grows_window_by_window_and_events_are_recorded() {
        let llm = FakeLlm::new([
            r#"{"add":{"decisions":[{"text":"Usar Rust","evidence":[0,1]}]}}"#,
            r#"{"add":{"risks":[{"text":"Plazo corto","evidence":[3]},{"text":"Inventado","evidence":[40]}]}}"#,
        ]);
        let analysis = analyze(
            &llm,
            &segments(4, 1000),
            &Background::default(),
            &cfg(2, 600),
        );
        assert_eq!(analysis.ledger.decisions.len(), 1);
        assert_eq!(analysis.ledger.risks.len(), 1);
        let [first, second] = analysis.events.as_slice() else {
            panic!("two events");
        };
        assert_eq!((first.first_id, first.last_id, first.accepted), (0, 1, 1));
        assert_eq!((second.accepted, second.rejected), (1, 1));
        assert!(llm.requests()[1].user.contains("Usar Rust"));
        assert_eq!(llm.requests()[0].system, llm.requests()[1].system);
    }

    #[test]
    fn a_window_cannot_cite_segments_the_model_has_not_seen_yet() {
        let llm = FakeLlm::new([r#"{"add":{"risks":[{"text":"Futuro","evidence":[3]}]}}"#]);
        let analysis = analyze(
            &llm,
            &segments(4, 1000),
            &Background::default(),
            &cfg(2, 600),
        );
        assert!(analysis.ledger.risks.is_empty());
        assert_eq!(analysis.events[0].rejected, 1);
    }

    #[test]
    fn a_garbled_answer_is_retried_once_then_the_window_is_skipped() {
        let llm = FakeLlm::scripted([
            Ok(r#"{"add":{"decisions":[{"text":"Primera","evidence":[0]}]}}"#.to_string()),
            Ok("no es json".to_string()),
            Err(LlmError::Timeout),
        ]);
        let analysis = analyze(
            &llm,
            &segments(4, 1000),
            &Background::default(),
            &cfg(2, 600),
        );
        assert_eq!(analysis.ledger.decisions.len(), 1);
        assert_eq!(analysis.failed_windows(), 1);
        assert_eq!(analysis.events[1].attempts, 2);
        assert!(
            analysis.events[1]
                .error
                .as_deref()
                .unwrap()
                .contains("in time")
        );
    }

    #[test]
    fn a_failure_then_success_counts_two_attempts() {
        let llm = FakeLlm::scripted([
            Err(LlmError::Timeout),
            Ok(r#"{"add":{"topics":[{"title":"T","evidence":[0]}]}}"#.to_string()),
        ]);
        let analysis = analyze(
            &llm,
            &segments(1, 1000),
            &Background::default(),
            &cfg(5, 60),
        );
        assert_eq!(analysis.events[0].attempts, 2);
        assert_eq!(analysis.ledger.topics.len(), 1);
        assert_eq!(analysis.failed_windows(), 0);
    }

    #[test]
    fn the_live_analyst_steps_window_by_window_and_matches_the_batch_result() {
        let answers = [
            r#"{"add":{"decisions":[{"text":"Usar Rust","evidence":[0,1]}]}}"#,
            r#"{"add":{"risks":[{"text":"Plazo corto","evidence":[3]}]}}"#,
        ];
        let batch = analyze(
            &FakeLlm::new(answers),
            &segments(4, 1000),
            &Background::default(),
            &cfg(2, 600),
        );
        let mut live = Analyst::new(
            Box::new(FakeLlm::new(answers)),
            cfg(2, 600),
            Background::default(),
        );
        let mut steps = Vec::new();
        for segment in segments(4, 1000) {
            live.push(segment);
            steps.extend(live.step(false));
        }
        assert_eq!(live.ledger(), &batch.ledger);
        assert_eq!(live.events(), batch.events.as_slice());
        let kinds: Vec<_> = steps.iter().map(|s| s.changed[0].kind).collect();
        assert_eq!(kinds, ["decision", "risk"]);
    }

    #[test]
    fn an_open_window_waits_until_it_is_flushed() {
        let llm = FakeLlm::new([r#"{"add":{"topics":[{"title":"T","evidence":[0]}]}}"#]);
        let mut live = Analyst::new(Box::new(llm), cfg(5, 600), Background::default());
        live.push(segments(1, 1000).remove(0));
        assert!(live.step(false).is_none());
        let step = live.step(true).expect("flushed");
        assert_eq!(step.changed.len(), 1);
        assert!(live.step(true).is_none());
    }

    #[test]
    fn a_failed_window_changes_nothing_and_says_why() {
        let llm = FakeLlm::scripted([Err(LlmError::Timeout), Err(LlmError::Timeout)]);
        let mut live = Analyst::new(Box::new(llm), cfg(1, 600), Background::default());
        live.push(segments(1, 1000).remove(0));
        let step = live.step(false).expect("window");
        assert!(step.event.error.is_some() && step.changed.is_empty());
        assert_eq!(live.ledger().item_count(), 0);
    }
}
