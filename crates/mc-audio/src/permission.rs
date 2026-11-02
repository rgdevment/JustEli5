//! macOS audio permissions as `eli5 doctor` reports them; pure, the queries live in `mac_permission`.

use crate::sysaudio::{Mechanism, mic_settings, settings_path};

/// What macOS answers about a permission it keeps per app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    NotAsked,
    /// Blocked by a profile or parental controls; the user cannot change it.
    Restricted,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::NotAsked => "not_asked",
            Self::Restricted => "restricted",
        }
    }
}

/// Screen Recording, which ScreenCaptureKit needs to capture the system audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    pub granted: bool,
    /// Whether ScreenCaptureKit only takes over when the Core Audio tap fails (macOS 14.4+).
    pub fallback: bool,
}

/// The audio permissions of this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioAccess {
    pub microphone: Access,
    /// `None` before macOS 13, where no native capture exists.
    pub screen_recording: Option<Screen>,
}

/// One doctor line: its name, whether it is fine, a machine-readable state and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionLine {
    pub name: &'static str,
    pub ok: bool,
    pub state: &'static str,
    pub detail: String,
}

fn microphone_line(access: Access) -> PermissionLine {
    let detail = match access {
        Access::Granted => "allowed".to_string(),
        Access::NotAsked => {
            "not asked yet: macOS asks the first time audio is recorded (`eli5 doctor --ask-permission microphone` asks now)"
                .to_string()
        }
        Access::Denied => format!(
            "denied: the recording would be silence; allow it in {}",
            mic_settings()
        ),
        Access::Restricted => {
            "restricted by a profile or parental controls: ask whoever manages this Mac".to_string()
        }
    };
    PermissionLine {
        name: "microphone",
        ok: access == Access::Granted,
        state: access.as_str(),
        detail,
    }
}

fn screen_line(screen: Screen) -> PermissionLine {
    let settings = settings_path(Mechanism::ScreenCaptureKit);
    let detail = match (screen.granted, screen.fallback) {
        (true, false) => "allowed (the system audio is captured with ScreenCaptureKit)".to_string(),
        (true, true) => "allowed (used only if the Core Audio tap fails)".to_string(),
        (false, true) => format!(
            "not allowed; only needed if the Core Audio tap fails and ScreenCaptureKit takes over ({settings})"
        ),
        (false, false) => format!(
            "not allowed: this macOS captures the system audio with ScreenCaptureKit; allow it in {settings}"
        ),
    };
    PermissionLine {
        name: "screen_recording",
        ok: screen.granted || screen.fallback,
        state: if screen.granted { "granted" } else { "missing" },
        detail,
    }
}

pub fn lines(access: AudioAccess) -> Vec<PermissionLine> {
    let mut lines = vec![microphone_line(access.microphone)];
    lines.extend(access.screen_recording.map(screen_line));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(microphone: Access, screen_recording: Option<Screen>) -> AudioAccess {
        AudioAccess {
            microphone,
            screen_recording,
        }
    }

    fn screen(granted: bool, fallback: bool) -> Option<Screen> {
        Some(Screen { granted, fallback })
    }

    #[test]
    fn a_granted_microphone_is_fine_and_alone_without_native_capture() {
        let lines = lines(access(Access::Granted, None));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ok);
        assert_eq!((lines[0].name, lines[0].state), ("microphone", "granted"));
    }

    #[test]
    fn a_denied_microphone_points_at_its_setting() {
        let line = &lines(access(Access::Denied, None))[0];
        assert!(!line.ok);
        assert_eq!(line.state, "denied");
        assert!(
            line.detail.contains("Privacy & Security > Microphone"),
            "{}",
            line.detail
        );
    }

    #[test]
    fn a_microphone_never_asked_says_how_to_ask() {
        let line = &lines(access(Access::NotAsked, None))[0];
        assert!(!line.ok);
        assert_eq!(line.state, "not_asked");
        assert!(line.detail.contains("--ask-permission"), "{}", line.detail);
        assert_eq!(
            lines(access(Access::Restricted, None))[0].state,
            "restricted"
        );
    }

    #[test]
    fn screen_recording_warns_only_where_it_captures_the_audio() {
        let first = &lines(access(Access::Granted, screen(false, false)))[1];
        assert_eq!(
            (first.name, first.ok, first.state),
            ("screen_recording", false, "missing")
        );
        assert!(first.detail.contains("Screen & System Audio Recording"));
        let fallback = &lines(access(Access::Granted, screen(false, true)))[1];
        assert!(fallback.ok);
        assert_eq!(fallback.state, "missing");
        assert!(fallback.detail.contains("tap fails"));
        assert!(lines(access(Access::Granted, screen(true, false)))[1].ok);
    }
}
