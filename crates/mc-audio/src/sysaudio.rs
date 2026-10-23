//! macOS system-audio decisions: which mechanism to use for an OS version,
//! how a `--system-device` value is read, and what an error code means for the
//! user. Pure, so it is tested on every platform; the FFI lives in `mac_*`.

use std::fmt;

use crate::silence::{PlayingCheck, SilenceAlert};
use crate::timeline::AudioError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OsVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl OsVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// `"14.4.1"`, `"14.4"` or `"15"`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
        let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
        parts
            .next()
            .is_none()
            .then(|| Self::new(major, minor, patch))
    }
}

impl fmt::Display for OsVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// Core Audio process tap, macOS 14.4+.
    ProcessTap,
    /// ScreenCaptureKit audio-only stream, macOS 13.0+.
    ScreenCaptureKit,
    /// No native capture: a virtual input (BlackHole) opened by name.
    NamedDevice,
}

const TAP_SINCE: OsVersion = OsVersion::new(14, 4, 0);
const SCK_SINCE: OsVersion = OsVersion::new(13, 0, 0);

impl Mechanism {
    pub fn label(self) -> &'static str {
        match self {
            Self::ProcessTap => "Core Audio process tap",
            Self::ScreenCaptureKit => "ScreenCaptureKit",
            Self::NamedDevice => "virtual device",
        }
    }
}

/// Mechanisms to try for the automatic choice, best first.
pub fn mechanisms_for(version: OsVersion) -> Vec<Mechanism> {
    if version >= TAP_SINCE {
        vec![Mechanism::ProcessTap, Mechanism::ScreenCaptureKit]
    } else if version >= SCK_SINCE {
        vec![Mechanism::ScreenCaptureKit]
    } else {
        vec![Mechanism::NamedDevice]
    }
}

/// The line `mc doctor --audio` prints about the system channel.
pub fn doctor_line(version: OsVersion) -> String {
    match mechanisms_for(version)[0] {
        Mechanism::ProcessTap => format!(
            "system: macOS {version} captures the meeting app with a Core Audio process tap (all apps if none is found); needs System Audio Recording permission"
        ),
        Mechanism::ScreenCaptureKit => format!(
            "system: macOS {version} has no process taps (14.4+); using ScreenCaptureKit audio of all apps; needs Screen Recording permission"
        ),
        Mechanism::NamedDevice => format!(
            "system: macOS {version} has no native system-audio capture (13.0+); install a virtual device (BlackHole + Multi-Output) and pass --system-device BlackHole"
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemSpec {
    /// Pick by OS version and running apps.
    Auto,
    /// `process:<name|pid|meeting>`: tap that app.
    Process(String),
    /// `tap`: tap every app but this one.
    GlobalTap,
    /// `sck`: ScreenCaptureKit.
    ScreenCaptureKit,
    /// Anything else: an output or virtual input device by name.
    Device(String),
}

pub fn parse_system_spec(device: Option<&str>) -> SystemSpec {
    let spec = device.map_or("", str::trim);
    if spec.is_empty() || spec.eq_ignore_ascii_case("auto") {
        return SystemSpec::Auto;
    }
    if let Some(process) = spec.strip_prefix("process:") {
        return SystemSpec::Process(process.to_string());
    }
    match spec.to_ascii_lowercase().as_str() {
        "tap" => SystemSpec::GlobalTap,
        "sck" | "screencapturekit" => SystemSpec::ScreenCaptureKit,
        _ => SystemSpec::Device(spec.to_string()),
    }
}

const TAP_SETTINGS: &str = "System Settings > Privacy & Security > Screen & System Audio Recording > System Audio Recording Only (enable your terminal or the mc app, then restart it)";
const SCK_SETTINGS: &str = "System Settings > Privacy & Security > Screen & System Audio Recording (called Screen Recording before macOS 15; enable your terminal or the mc app, then restart it)";
const MIC_SETTINGS: &str = "System Settings > Privacy & Security > Microphone";

pub fn settings_path(mechanism: Mechanism) -> &'static str {
    match mechanism {
        Mechanism::ProcessTap => TAP_SETTINGS,
        Mechanism::ScreenCaptureKit => SCK_SETTINGS,
        Mechanism::NamedDevice => MIC_SETTINGS,
    }
}

/// Where to allow the microphone, when the OS says it is denied.
pub fn mic_settings() -> &'static str {
    MIC_SETTINGS
}

/// Zeros on the mic: macOS does not refuse a denied microphone, it feeds silence.
pub fn mic_silence_alert(after_s: f64) -> SilenceAlert {
    SilenceAlert {
        after_s,
        message: format!(
            "the microphone delivers only digital silence: if it is not muted, allow Microphone access in {MIC_SETTINGS}"
        ),
        playing: None,
    }
}

/// Zeros on the system channel only mean a denied permission while `playing` says
/// something is playing; a quiet meeting gives the same zeros.
pub fn system_silence_alert(
    mechanism: Mechanism,
    after_s: f64,
    playing: Option<PlayingCheck>,
) -> SilenceAlert {
    SilenceAlert {
        after_s,
        message: format!(
            "the system audio is exact silence while other apps are playing sound: allow access in {}",
            settings_path(mechanism)
        ),
        playing,
    }
}

/// Whether the tap now delivers another format than the one it was opened with
/// (`(rate, channels)` each). Only a real change counts, not the device clock.
pub fn format_changed(opened: (u32, u32), now: (u32, u32)) -> bool {
    opened != now
}

/// Whether the app's Core Audio processes now include one the tap was not built with
/// (a helper started after the call began). An empty list never rebuilds.
pub fn needs_rebuild(built: &[u32], now: &[u32]) -> bool {
    now.iter().any(|object| !built.contains(object))
}

/// Seconds of leading zeros on the mic before warning: long enough for the stream to settle.
pub const MIC_SILENCE_S: f64 = 5.0;
/// Longer for the system channel, where a meeting may simply not have started.
pub const SYSTEM_SILENCE_S: f64 = 20.0;

/// The mechanism a `--system-device` value ends up using, when it is a native one.
fn native_mechanism(version: OsVersion, spec: &SystemSpec) -> Option<Mechanism> {
    match spec {
        SystemSpec::Auto => {
            Some(mechanisms_for(version)[0]).filter(|m| *m != Mechanism::NamedDevice)
        }
        SystemSpec::Process(_) | SystemSpec::GlobalTap => Some(Mechanism::ProcessTap),
        SystemSpec::ScreenCaptureKit => Some(Mechanism::ScreenCaptureKit),
        SystemSpec::Device(_) => None,
    }
}

/// The `mc doctor --audio` line about how the system channel is captured.
pub fn system_note(version: OsVersion, spec: &SystemSpec) -> String {
    match spec {
        SystemSpec::Auto => doctor_line(version),
        SystemSpec::Device(name) => format!("system: capturing the device named {name:?}"),
        _ => format!(
            "system: forced {} (--system-device)",
            native_mechanism(version, spec).map_or("", Mechanism::label)
        ),
    }
}

/// The silence warning for `channel`, if its zeros could mean a denied permission.
pub fn silence_alert_for(
    channel: &str,
    version: OsVersion,
    spec: &SystemSpec,
    playing: PlayingCheck,
) -> Option<SilenceAlert> {
    if channel == "mic" {
        return Some(mic_silence_alert(MIC_SILENCE_S));
    }
    native_mechanism(version, spec)
        .map(|m| system_silence_alert(m, SYSTEM_SILENCE_S, Some(playing)))
}

/// Why opening a mechanism failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFailure {
    pub denied: bool,
    pub message: String,
}

impl OpenFailure {
    fn denied(mechanism: Mechanism, what: &str) -> Self {
        Self {
            denied: true,
            message: format!(
                "{what}; macOS denied access, allow it in {}",
                settings_path(mechanism)
            ),
        }
    }

    pub fn other(message: String) -> Self {
        Self {
            denied: false,
            message,
        }
    }
}

impl From<OpenFailure> for AudioError {
    fn from(failure: OpenFailure) -> Self {
        Self::Unavailable(failure.message)
    }
}

/// Four printable characters of an `OSStatus`, as Core Audio's codes are.
fn fourcc(status: i32) -> Option<String> {
    let bytes = status.to_be_bytes();
    bytes
        .iter()
        .all(|b| (0x20..0x7f).contains(b))
        .then(|| bytes.iter().map(|b| char::from(*b)).collect())
}

/// An `OSStatus` from the tap, aggregate device or IOProc calls.
pub fn core_audio_failure(status: i32, stage: &str) -> OpenFailure {
    let code = fourcc(status).map_or_else(|| status.to_string(), |c| format!("{c:?}"));
    let what = format!("Core Audio {stage} failed ({code})");
    match fourcc(status).as_deref() {
        // The HAL's "illegal operation": what a tap request without permission gets.
        Some("nope") => OpenFailure::denied(Mechanism::ProcessTap, &what),
        Some("unop") => OpenFailure::other(format!(
            "{what}: this macOS does not support process taps (14.4+ needed)"
        )),
        Some("!obj") => OpenFailure::other(format!("{what}: the process is gone")),
        _ => OpenFailure::other(what),
    }
}

/// `SCStreamErrorUserDeclined`.
const SCK_USER_DECLINED: i64 = -3801;
/// `SCStreamErrorMissingEntitlements`.
const SCK_MISSING_ENTITLEMENTS: i64 = -3803;

/// An `NSError` code from ScreenCaptureKit.
pub fn sck_failure(code: i64, description: &str) -> OpenFailure {
    let what = format!("ScreenCaptureKit failed ({code}: {description})");
    match code {
        SCK_USER_DECLINED => OpenFailure::denied(Mechanism::ScreenCaptureKit, &what),
        SCK_MISSING_ENTITLEMENTS => OpenFailure::other(format!(
            "{what}: the app is missing the entitlement to capture the screen"
        )),
        _ => OpenFailure::other(what),
    }
}

/// Tries each mechanism in turn; the first that opens wins. When all fail the
/// error lists every reason, so a denied permission is not hidden by a fallback.
pub fn first_that_opens<T>(
    chain: &[Mechanism],
    mut open: impl FnMut(Mechanism) -> Result<T, OpenFailure>,
) -> Result<T, AudioError> {
    let mut reasons = Vec::new();
    for mechanism in chain {
        match open(*mechanism) {
            Ok(source) => return Ok(source),
            Err(failure) => reasons.push(format!("{}: {}", mechanism.label(), failure.message)),
        }
    }
    Err(AudioError::Unavailable(reasons.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yes() -> PlayingCheck {
        std::sync::Arc::new(|| true)
    }

    const TAP_AND_SCK: [Mechanism; 2] = [Mechanism::ProcessTap, Mechanism::ScreenCaptureKit];

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(OsVersion::parse("14.4.1"), Some(OsVersion::new(14, 4, 1)));
        assert_eq!(OsVersion::parse(" 15 "), Some(OsVersion::new(15, 0, 0)));
        assert_eq!(OsVersion::parse("13.6"), Some(OsVersion::new(13, 6, 0)));
        assert_eq!(OsVersion::parse("14.x"), None);
        assert_eq!(OsVersion::parse("1.2.3.4"), None);
        assert!(OsVersion::new(14, 10, 0) > OsVersion::new(14, 4, 0));
    }

    #[test]
    fn the_mechanism_follows_the_os_version() {
        use Mechanism::*;
        assert_eq!(mechanisms_for(OsVersion::new(15, 0, 0)), TAP_AND_SCK);
        assert_eq!(mechanisms_for(OsVersion::new(14, 4, 0)), TAP_AND_SCK);
        assert_eq!(mechanisms_for(OsVersion::new(14, 3, 1)), [ScreenCaptureKit]);
        assert_eq!(mechanisms_for(OsVersion::new(13, 0, 0)), [ScreenCaptureKit]);
        assert_eq!(mechanisms_for(OsVersion::new(12, 7, 6)), [NamedDevice]);
    }

    #[test]
    fn doctor_names_the_mechanism_and_its_permission() {
        assert!(doctor_line(OsVersion::new(15, 1, 0)).contains("process tap"));
        assert!(doctor_line(OsVersion::new(13, 5, 0)).contains("Screen Recording"));
        assert!(doctor_line(OsVersion::new(12, 0, 0)).contains("BlackHole"));
    }

    #[test]
    fn system_device_values_are_read() {
        assert_eq!(parse_system_spec(None), SystemSpec::Auto);
        assert_eq!(parse_system_spec(Some("  ")), SystemSpec::Auto);
        assert_eq!(parse_system_spec(Some("AUTO")), SystemSpec::Auto);
        assert_eq!(
            parse_system_spec(Some("process:zoom.us")),
            SystemSpec::Process("zoom.us".to_string())
        );
        assert_eq!(parse_system_spec(Some("tap")), SystemSpec::GlobalTap);
        assert_eq!(parse_system_spec(Some("SCK")), SystemSpec::ScreenCaptureKit);
        assert_eq!(
            parse_system_spec(Some("BlackHole 2ch")),
            SystemSpec::Device("BlackHole 2ch".to_string())
        );
    }

    #[test]
    fn a_refused_tap_points_at_the_privacy_setting() {
        let failure = core_audio_failure(0x6E6F_7065, "tap creation");
        assert!(failure.denied);
        assert!(failure.message.contains("\"nope\""), "{}", failure.message);
        assert!(failure.message.contains("System Audio Recording Only"));
    }

    #[test]
    fn other_core_audio_codes_are_not_blamed_on_permissions() {
        let unsupported = core_audio_failure(0x756E_6F70, "tap creation");
        assert!(!unsupported.denied);
        assert!(unsupported.message.contains("14.4"));
        let numeric = core_audio_failure(-50, "aggregate device");
        assert!(!numeric.denied);
        assert!(numeric.message.contains("-50"));
    }

    #[test]
    fn a_declined_screen_capture_points_at_screen_recording() {
        let failure = sck_failure(-3801, "user declined");
        assert!(failure.denied);
        assert!(failure.message.contains("Screen & System Audio Recording"));
        assert!(!sck_failure(-3802, "failed to start").denied);
        assert!(sck_failure(-3803, "x").message.contains("entitlement"));
    }

    #[test]
    fn the_chain_falls_through_and_keeps_every_reason() {
        let mut tried = Vec::new();
        let opened = first_that_opens(&TAP_AND_SCK, |m| {
            tried.push(m);
            match m {
                Mechanism::ProcessTap => Err(core_audio_failure(0x6E6F_7065, "tap creation")),
                _ => Ok("sck"),
            }
        });
        assert_eq!(opened.unwrap(), "sck");
        assert_eq!(tried, TAP_AND_SCK);

        let failed: Result<(), _> = first_that_opens(&TAP_AND_SCK, |m| match m {
            Mechanism::ProcessTap => Err(core_audio_failure(0x6E6F_7065, "tap creation")),
            _ => Err(sck_failure(-3801, "user declined")),
        });
        let message = failed.unwrap_err().to_string();
        assert!(message.contains("Core Audio process tap:"), "{message}");
        assert!(message.contains("ScreenCaptureKit:"), "{message}");
    }

    #[test]
    fn only_native_system_capture_gets_a_permission_hint() {
        let (new, old) = (OsVersion::new(15, 0, 0), OsVersion::new(12, 0, 0));
        let tap = silence_alert_for("system", new, &SystemSpec::Auto, yes()).unwrap();
        assert!(tap.message.contains("System Audio Recording"));
        let sck = silence_alert_for("system", new, &SystemSpec::ScreenCaptureKit, yes()).unwrap();
        assert!(sck.message.contains("Screen"));
        assert!(silence_alert_for("system", old, &SystemSpec::Auto, yes()).is_none());
        let named = SystemSpec::Device("BlackHole".to_string());
        assert!(silence_alert_for("system", new, &named, yes()).is_none());
        let mic = silence_alert_for("mic", old, &named, yes()).unwrap();
        assert_eq!(mic.after_s, MIC_SILENCE_S);
    }

    #[test]
    fn the_doctor_note_says_what_was_forced() {
        let version = OsVersion::new(15, 0, 0);
        assert!(system_note(version, &SystemSpec::Auto).contains("process tap"));
        assert!(
            system_note(version, &SystemSpec::ScreenCaptureKit).contains("forced ScreenCaptureKit")
        );
        let named = SystemSpec::Device("BlackHole".to_string());
        assert!(system_note(version, &named).contains("BlackHole"));
    }

    #[test]
    fn a_new_helper_process_rebuilds_the_tap_but_a_departed_one_does_not() {
        assert!(needs_rebuild(&[1, 2], &[1, 2, 3]));
        assert!(!needs_rebuild(&[1, 2], &[1]));
        assert!(!needs_rebuild(&[1, 2], &[]));
    }

    #[test]
    fn only_a_real_change_of_the_tap_format_counts() {
        assert!(!format_changed((48_000, 2), (48_000, 2)));
        assert!(format_changed((48_000, 2), (44_100, 2)));
        assert!(format_changed((48_000, 2), (48_000, 1)));
    }

    #[test]
    fn silence_alerts_carry_the_fix() {
        let mic = mic_silence_alert(5.0);
        assert!(mic.message.contains("Microphone"));
        let system = system_silence_alert(Mechanism::ProcessTap, 20.0, None);
        assert!(system.message.contains("System Audio Recording"));
        assert_eq!(system.after_s, 20.0);
    }
}
