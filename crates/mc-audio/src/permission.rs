//! macOS audio permissions as `eli5 doctor` reports them. Pure, so it is tested
//! on every platform; the queries live in `mac_permission`.

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

/// The audio permissions of this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioAccess {
    pub microphone: Access,
    /// Screen Recording, only where ScreenCaptureKit captures the system audio (macOS 13.0 to 14.3).
    pub screen_recording: Option<bool>,
}

/// One doctor line: its name, whether it is fine and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionLine {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

fn microphone_line(access: Access) -> PermissionLine {
    let (ok, detail) = match access {
        Access::Granted => (true, "allowed".to_string()),
        Access::NotAsked => (
            false,
            "not asked yet: macOS asks the first time audio is recorded (`eli5 doctor --audio` asks now)"
                .to_string(),
        ),
        Access::Denied => (
            false,
            format!(
                "denied: the recording would be silence; allow it in {}",
                mic_settings()
            ),
        ),
        Access::Restricted => (
            false,
            "restricted by a profile or parental controls: ask whoever manages this Mac"
                .to_string(),
        ),
    };
    PermissionLine {
        name: "microphone",
        ok,
        detail,
    }
}

fn screen_line(granted: bool) -> PermissionLine {
    PermissionLine {
        name: "screen_recording",
        ok: granted,
        detail: if granted {
            "allowed (the system audio is captured with ScreenCaptureKit)".to_string()
        } else {
            format!(
                "not allowed: this macOS captures the system audio with ScreenCaptureKit; allow it in {}",
                settings_path(Mechanism::ScreenCaptureKit)
            )
        },
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

    fn access(microphone: Access, screen_recording: Option<bool>) -> AudioAccess {
        AudioAccess {
            microphone,
            screen_recording,
        }
    }

    #[test]
    fn a_granted_microphone_is_fine_and_alone_without_screen_capture() {
        let lines = lines(access(Access::Granted, None));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ok);
        assert_eq!(lines[0].name, "microphone");
    }

    #[test]
    fn a_denied_microphone_points_at_its_setting() {
        let line = &lines(access(Access::Denied, None))[0];
        assert!(!line.ok);
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
        assert!(line.detail.contains("doctor --audio"), "{}", line.detail);
        assert!(!lines(access(Access::Restricted, None))[0].ok);
    }

    #[test]
    fn screen_recording_is_reported_only_where_it_captures_the_audio() {
        let lines = lines(access(Access::Granted, Some(false)));
        assert_eq!(lines[1].name, "screen_recording");
        assert!(!lines[1].ok);
        assert!(lines[1].detail.contains("Screen & System Audio Recording"));
        assert!(super::lines(access(Access::Granted, Some(true)))[1].ok);
    }
}
