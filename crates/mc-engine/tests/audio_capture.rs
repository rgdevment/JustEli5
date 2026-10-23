//! Audio recorded next to the captions through the capture controller.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mc_audio::wav::{WavWriter, read_wav};
use mc_audio::{ChannelConfig, WavFileBackend};
use mc_captions::FakeSource;
use mc_engine::controller::{Capture, CaptureSpec, RecordSpec, start};
use mc_engine::corpus::repair_session_audio;
use mc_engine::live::{AudioSpec, EndReason, LiveConfig, LiveEvent, once};
use mc_store::manifest::Manifest;

fn long_source(frames: usize) -> FakeSource {
    let texts: Vec<String> = (0..frames).map(|i| format!("frase numero {i}")).collect();
    FakeSource::frames(
        texts
            .iter()
            .map(|t| vec![(Some("Ana"), Some(t.as_str()))])
            .collect(),
    )
}

fn wav(path: &Path, rate: u32, seconds: f64, level: i16) {
    let mut w = WavWriter::create(path, rate).unwrap();
    w.write_samples(&vec![level; (f64::from(rate) * seconds) as usize])
        .unwrap();
    w.finish().unwrap();
}

fn spec(audio: Option<AudioSpec>, frames: usize) -> CaptureSpec {
    let source = long_source(frames);
    CaptureSpec {
        source: once(source),
        cfg: LiveConfig {
            poll: Duration::from_millis(20),
            rediscover: Duration::from_millis(5),
            ..LiveConfig::default()
        },
        record: Some(RecordSpec {
            platform: "fake".into(),
            capture_source: "FakeSource".into(),
            meeting_name: None,
            consent_method: "flag",
            audio,
        }),
        stop_file: false,
        until: None,
        analyst: None,
    }
}

/// Collects events until one satisfies `done`, or fails after `limit`.
fn collect_until(
    capture: &Capture,
    limit: Duration,
    done: impl Fn(&LiveEvent) -> bool,
) -> Vec<LiveEvent> {
    let started = Instant::now();
    let mut all = Vec::new();
    while started.elapsed() < limit {
        let events = capture.poll(Duration::from_millis(20));
        let hit = events.iter().any(&done);
        all.extend(events);
        if hit {
            return all;
        }
    }
    panic!("timed out; saw {all:?}");
}

#[test]
fn records_audio_with_consent_and_stops_captions_first() {
    let root = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let (mic, system) = (files.path().join("m.wav"), files.path().join("s.wav"));
    wav(&mic, 16_000, 1.0, 8000);
    wav(&system, 48_000, 1.0, -4000);
    let backend = WavFileBackend {
        mic: Some(mic),
        system: Some(system),
    };
    let mut audio = AudioSpec::new(Some(Arc::new(backend)));
    audio.channels = ChannelConfig {
        sync_interval_ms: 200.0,
        ..ChannelConfig::default()
    };
    let capture = start(spec(Some(audio), 400), root.path()).unwrap();
    // The session (and its consent) exists once the meeting is found.
    let mut events = collect_until(&capture, Duration::from_secs(10), |e| {
        matches!(e, LiveEvent::Session { .. })
    });
    let dir = capture.session_dir().unwrap().to_path_buf();

    // The manifest already carries the consent and the placeholder.
    let early = Manifest::load(dir.join("manifest.json")).unwrap();
    assert_eq!(early.extra["consent"]["audio"], true);
    assert_eq!(early.audio["mic"]["status"], "starting");

    events.extend(collect_until(&capture, Duration::from_secs(10), |e| {
        matches!(e, LiveEvent::Rec(true))
    }));
    // Let both files play to the end, then stop.
    std::thread::sleep(Duration::from_millis(1300));
    capture.request_stop();
    let stopping = Instant::now();
    events.extend(collect_until(&capture, Duration::from_secs(10), |e| {
        matches!(e, LiveEvent::Ended { .. })
    }));
    let summary = capture.finish().unwrap();
    assert!(stopping.elapsed() < Duration::from_secs(4));
    assert_eq!(summary.reason, EndReason::Stop);
    assert!(events.iter().any(|e| matches!(e, LiveEvent::Rec(false))));

    let manifest = Manifest::load(dir.join("manifest.json")).unwrap();
    assert!(manifest.ended_cleanly());
    assert_eq!(manifest.extra["consent"]["audio"], true);
    for ch in ["mic", "system"] {
        assert_eq!(manifest.audio[ch]["status"], "ok", "{ch}");
        assert_eq!(manifest.audio[ch]["samplerate"], 16_000);
    }
    assert_eq!(manifest.audio["system"]["device_samplerate"], 48_000);
    assert_eq!(manifest.libs["audio_backend"], "wav-files");
    assert_eq!(summary.audio.unwrap()["mic"]["status"], "ok");

    // About a second of audio on top of whatever silence the start-up delay
    // put in front (the WAV is aligned to the session clock, not to the file).
    for ch in ["mic", "system"] {
        let (rate, samples) = read_wav(&dir.join(format!("{ch}.wav"))).unwrap();
        assert_eq!(rate, 16_000);
        let padded = manifest.audio[ch]["padded_frames"].as_u64().unwrap() as usize;
        let played = samples.len() - padded;
        assert!(
            (15_000..=17_500).contains(&played),
            "{ch}: {played} of {}",
            samples.len()
        );
    }
    let sync = std::fs::read_to_string(dir.join("audio_sync.jsonl")).unwrap();
    assert!(sync.lines().any(|l| l.contains("\"event\":\"start\"")));
    assert!(sync.lines().any(|l| l.contains("\"event\":\"stop\"")));
    // The captions were written too.
    assert!(std::fs::metadata(dir.join("captions.jsonl")).unwrap().len() > 0);
}

#[test]
fn audio_that_cannot_start_never_harms_the_captions() {
    let root = tempfile::tempdir().unwrap();
    let capture = start(spec(Some(AudioSpec::new(None)), 6), root.path()).unwrap();
    let events = collect_until(&capture, Duration::from_secs(10), |e| {
        matches!(e, LiveEvent::Ended { .. })
    });
    let dir = capture.session_dir().unwrap().to_path_buf();
    let summary = capture.finish().unwrap();
    assert_eq!(summary.reason, EndReason::SourceEnded);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LiveEvent::Audio(m) if m.contains("no audio backend")))
    );
    assert!(!events.iter().any(|e| matches!(e, LiveEvent::Rec(true))));
    let manifest = Manifest::load(dir.join("manifest.json")).unwrap();
    assert!(manifest.ended_cleanly());
    assert_eq!(manifest.audio["mic"]["status"], "unavailable");
    assert_eq!(manifest.audio["system"]["status"], "unavailable");
}

#[test]
fn without_audio_the_consent_says_so_and_no_wav_appears() {
    let root = tempfile::tempdir().unwrap();
    let capture = start(spec(None, 4), root.path()).unwrap();
    collect_until(&capture, Duration::from_secs(10), |e| {
        matches!(e, LiveEvent::Ended { .. })
    });
    let dir = capture.session_dir().unwrap().to_path_buf();
    capture.finish().unwrap();
    let manifest = Manifest::load(dir.join("manifest.json")).unwrap();
    assert_eq!(manifest.extra["consent"]["audio"], false);
    assert_eq!(manifest.audio["mic"]["status"], "disabled");
    assert_eq!(manifest.audio["system"]["enabled"], false);
    assert!(!dir.join("mic.wav").exists());
    assert!(!dir.join("audio_sync.jsonl").exists());
}

#[test]
fn a_session_killed_mid_recording_gets_its_wav_headers_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mic.wav");
    let mut w = WavWriter::create(&path, 16_000).unwrap();
    w.write_samples(&[5; 100]).unwrap();
    w.flush_header().unwrap();
    w.write_samples(&[6; 100]).unwrap();
    std::mem::forget(w);
    // BufWriter never flushed the second batch; add bytes the way a dying process might.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map(|mut f| std::io::Write::write_all(&mut f, &[6, 0].repeat(100)))
        .unwrap()
        .unwrap();
    assert_eq!(repair_session_audio(dir.path()), ["mic.wav"]);
    assert_eq!(read_wav(&path).unwrap().1.len(), 200);
    assert!(repair_session_audio(dir.path()).is_empty());
}

#[test]
fn a_source_that_fails_to_start_leaves_no_recording_behind() {
    let root = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let mic = files.path().join("m.wav");
    wav(&mic, 16_000, 0.5, 100);
    let backend = WavFileBackend {
        mic: Some(mic),
        system: None,
    };
    let mut capture_spec = spec(Some(AudioSpec::new(Some(Arc::new(backend)))), 4);
    capture_spec.source = Box::new(|| Err(mc_captions::SourceError::Fatal("no window".into())));
    let capture = start(capture_spec, root.path()).unwrap();
    collect_until(&capture, Duration::from_secs(10), |e| {
        matches!(e, LiveEvent::Ended { .. })
    });
    let summary = capture.finish().unwrap();
    assert_eq!(summary.reason, EndReason::SourceFatal);
    // No source, no session: neither a folder nor a recording.
    assert!(summary.session_dir.is_none());
    assert!(summary.audio.is_none());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn a_session_with_fresh_audio_files_is_not_reported_as_unclean() {
    use mc_engine::corpus::{CorpusSession, SessionSpec, scan_unclean};
    let root = tempfile::tempdir().unwrap();
    let session = CorpusSession::create(
        root.path(),
        &SessionSpec {
            platform: "fake",
            capture_source: "FakeSource",
            meeting_name: None,
            consent_method: "flag",
            t0_wall_ms: 1_700_000_000_000,
            debounce_clock: "mono",
        },
    )
    .unwrap();
    let dir = session.dir().to_path_buf();
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    let age = |name: &str| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join(name))
            .unwrap();
        file.set_modified(old).unwrap();
    };
    age("manifest.json");
    age("captions.jsonl");
    // A hung caption thread, audio threads still writing.
    std::fs::write(dir.join("mic.wav"), b"fresh").unwrap();
    assert!(scan_unclean(root.path()).is_empty());
    std::fs::remove_file(dir.join("mic.wav")).unwrap();
    assert_eq!(scan_unclean(root.path()).len(), 1);
}

/// A mic whose read never returns until released: a driver that hangs on stop.
struct BlockedMic(Arc<std::sync::atomic::AtomicBool>);

impl mc_audio::AudioSource for BlockedMic {
    fn info(&self) -> &mc_audio::StreamInfo {
        static INFO: std::sync::OnceLock<mc_audio::StreamInfo> = std::sync::OnceLock::new();
        INFO.get_or_init(|| mc_audio::StreamInfo {
            name: "blocked".into(),
            device_id: None,
            samplerate: 16_000,
            channels: 1,
            latency_s: None,
        })
    }

    fn read(&mut self, _frames: usize) -> Result<mc_audio::Read, mc_audio::AudioError> {
        let limit = Instant::now() + Duration::from_secs(20);
        while !self.0.load(std::sync::atomic::Ordering::Relaxed) && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(mc_audio::Read::Idle)
    }
}

struct BlockedBackend(Arc<std::sync::atomic::AtomicBool>);

impl mc_audio::AudioBackend for BlockedBackend {
    fn open_mic(
        &self,
        _device: Option<&str>,
    ) -> Result<Box<dyn mc_audio::AudioSource>, mc_audio::AudioError> {
        Ok(Box::new(BlockedMic(Arc::clone(&self.0))))
    }

    fn open_system(
        &self,
        _device: Option<&str>,
    ) -> Result<Box<dyn mc_audio::AudioSource>, mc_audio::AudioError> {
        Err(mc_audio::AudioError::Unavailable("none".into()))
    }
}

#[test]
fn a_stuck_source_and_stuck_audio_still_finish_within_the_drop_wait() {
    let root = tempfile::tempdir().unwrap();
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut audio = AudioSpec::new(Some(Arc::new(BlockedBackend(Arc::clone(&release)))));
    audio.stop_timeout = Duration::from_secs(10);
    let mut capture_spec = spec(Some(audio), 4);
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    capture_spec.source = once(StuckSource(Arc::clone(&entered)));
    let capture = start(capture_spec, root.path()).unwrap();
    collect_until(&capture, Duration::from_secs(10), |e| {
        matches!(e, LiveEvent::Rec(true))
    });
    while !entered.load(std::sync::atomic::Ordering::Relaxed) {
        capture.poll(Duration::from_millis(10));
    }
    let dir = capture.session_dir().unwrap().to_path_buf();
    capture.request_stop();
    let started = Instant::now();
    let summary = capture
        .finish_timeout(Duration::from_secs(3))
        .expect("finish must fit in the drop wait");
    assert!(started.elapsed() < Duration::from_millis(2900));
    assert_eq!(summary.reason, EndReason::Stop);
    assert!(
        Manifest::load(dir.join("manifest.json"))
            .unwrap()
            .ended_cleanly()
    );
    release.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// A caption source blocked inside `poll` for good.
struct StuckSource(Arc<std::sync::atomic::AtomicBool>);

impl mc_captions::CaptureSource for StuckSource {
    fn name(&self) -> &str {
        "StuckSource"
    }
    fn discover(&mut self) -> Result<mc_captions::Discovery, mc_captions::SourceError> {
        Ok(mc_captions::Discovery::Attached { window: "w".into() })
    }
    fn poll(&mut self) -> Result<Vec<mc_core::RawObservation>, mc_captions::SourceError> {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(20));
        Ok(Vec::new())
    }
}

/// Remembers the pid the system channel was asked to listen to.
type Seen = Arc<std::sync::Mutex<Vec<Option<u32>>>>;

struct PidSpy(Seen);

impl mc_audio::AudioBackend for PidSpy {
    fn open_mic(
        &self,
        _device: Option<&str>,
    ) -> Result<Box<dyn mc_audio::AudioSource>, mc_audio::AudioError> {
        Err(mc_audio::AudioError::Unavailable("none".into()))
    }

    fn open_system(
        &self,
        device: Option<&str>,
    ) -> Result<Box<dyn mc_audio::AudioSource>, mc_audio::AudioError> {
        self.open_system_for(device, None)
    }

    fn open_system_for(
        &self,
        _device: Option<&str>,
        meeting_pid: Option<u32>,
    ) -> Result<Box<dyn mc_audio::AudioSource>, mc_audio::AudioError> {
        self.0.lock().unwrap().push(meeting_pid);
        Err(mc_audio::AudioError::Unavailable("spy".into()))
    }
}

#[test]
fn the_system_channel_is_aimed_at_the_process_of_the_attached_window() {
    let root = tempfile::tempdir().unwrap();
    let seen = Seen::default();
    let audio = AudioSpec::new(Some(Arc::new(PidSpy(Arc::clone(&seen)))));
    let mut capture_spec = spec(Some(audio), 4);
    capture_spec.source = once(long_source(4).with_pid(4242));
    let capture = start(capture_spec, root.path()).unwrap();
    collect_until(
        &capture,
        Duration::from_secs(10),
        |e| matches!(e, LiveEvent::Audio(m) if m.contains("system")),
    );
    capture.request_stop();
    capture.finish().unwrap();
    assert_eq!(*seen.lock().unwrap(), [Some(4242)]);
}
