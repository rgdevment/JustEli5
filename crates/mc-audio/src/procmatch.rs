//! Finding the meeting app among the running processes. Pure: the platform
//! backends snapshot their processes into [`Proc`] and ask here.

use crate::timeline::AudioError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
}

/// The executable names one app goes by, most specific first.
pub type AppNames = &'static [&'static str];

/// Apps that count as "the meeting" together; a later tier is only looked at
/// when no app of the earlier ones is running (a browser is always open).
pub type Tier = &'static [AppNames];

pub const WINDOWS_APPS: &[Tier] = &[&[&["ms-teams.exe", "Teams.exe"], &["Zoom.exe"]]];

pub const MACOS_APPS: &[Tier] = &[
    &[&["MSTeams", "Microsoft Teams"], &["zoom.us"]],
    &[&["Google Chrome"], &["Microsoft Edge"], &["Brave Browser"]],
];

/// Roots of the process trees called `name` (case-insensitive, exact name).
/// Capturing a root with its children includes where the audio is rendered.
pub fn roots_named(procs: &[Proc], name: &str) -> Vec<u32> {
    let wanted = name.to_lowercase();
    let matching: Vec<&Proc> = procs
        .iter()
        .filter(|p| p.name.to_lowercase() == wanted)
        .collect();
    let mut roots: Vec<u32> = matching
        .iter()
        .filter(|p| {
            p.parent
                .is_none_or(|parent| !matching.iter().any(|m| m.pid == parent))
        })
        .map(|p| p.pid)
        .collect();
    roots.sort_unstable();
    roots
}

/// `root` and every process below it.
pub fn tree_of(procs: &[Proc], root: u32) -> Vec<u32> {
    let mut found = vec![root];
    let mut next = 0;
    while next < found.len() {
        let parent = found[next];
        next += 1;
        for p in procs {
            if p.parent == Some(parent) && !found.contains(&p.pid) {
                found.push(p.pid);
            }
        }
    }
    found
}

fn running_in(procs: &[Proc], tier: Tier) -> Vec<(String, u32)> {
    tier.iter()
        .filter_map(|names| {
            names.iter().find_map(|name| {
                roots_named(procs, name)
                    .first()
                    .map(|pid| ((*name).to_string(), *pid))
            })
        })
        .collect()
}

/// The one running meeting app. Several of the same tier at once (Teams idle
/// in the tray during a Zoom call) is ambiguous, so the caller should fall
/// back to capturing everything.
pub fn find_meeting_app(procs: &[Proc], tiers: &[Tier]) -> Result<(String, u32), AudioError> {
    for tier in tiers {
        let mut running = running_in(procs, tier);
        match running.len() {
            0 => {}
            1 => return Ok(running.remove(0)),
            _ => {
                let names: Vec<&str> = running.iter().map(|(n, _)| n.as_str()).collect();
                return Err(AudioError::Unavailable(format!(
                    "several meeting apps are running ({}); name one with process:<name>",
                    names.join(", ")
                )));
            }
        }
    }
    Err(AudioError::Unavailable(
        "no meeting app (Teams, Zoom or a browser) is running".to_string(),
    ))
}

/// Parses `process:<name|pid>`; `meeting` (or empty) looks for a meeting app.
pub fn resolve(procs: &[Proc], spec: &str, tiers: &[Tier]) -> Result<(String, u32), AudioError> {
    let spec = spec.trim();
    if let Ok(pid) = spec.parse::<u32>() {
        return Ok((format!("pid {pid}"), pid));
    }
    if spec.is_empty() || spec.eq_ignore_ascii_case("meeting") {
        return find_meeting_app(procs, tiers);
    }
    roots_named(procs, spec)
        .first()
        .map(|pid| (spec.to_string(), *pid))
        .ok_or_else(|| AudioError::Unavailable(format!("no running process matches {spec:?}")))
}

/// Whom the system channel listens to, and why (shown by `mc doctor --audio`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Process {
        label: String,
        pid: u32,
        why: Why,
    },
    /// No app to single out: capture the whole output.
    Everything {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// It owns the window the captions are read from.
    CaptionWindow,
    /// It is the one meeting app playing audio right now.
    ProducingAudio,
}

impl Choice {
    pub fn describe(&self) -> String {
        match self {
            Self::Process { label, pid, why } => format!(
                "system: {label} (pid {pid}), {}",
                match why {
                    Why::CaptionWindow => "the app that owns the caption window",
                    Why::ProducingAudio => "the meeting app that is producing audio",
                }
            ),
            Self::Everything { reason } => format!("system: all apps, {reason}"),
        }
    }
}

fn name_of(procs: &[Proc], pid: u32) -> String {
    procs
        .iter()
        .find(|p| p.pid == pid)
        .map_or_else(|| format!("pid {pid}"), |p| p.name.clone())
}

/// Meeting apps running with a process playing audio now; `is_playing` is asked
/// only about the pids of those apps' trees.
fn producing_audio(
    procs: &[Proc],
    tiers: &[Tier],
    is_playing: &impl Fn(u32) -> bool,
) -> Vec<(usize, String, u32)> {
    let mut found = Vec::new();
    for (rank, tier) in tiers.iter().enumerate() {
        for (label, root) in running_in(procs, tier) {
            if tree_of(procs, root).into_iter().any(is_playing) {
                found.push((rank, label, root));
            }
        }
    }
    found
}

/// The target of the system channel. With a caption window, its process; else
/// the meeting app that is playing audio (a native app before a browser, and
/// only when that is unambiguous); else everything, never an idle app.
pub fn choose(
    procs: &[Proc],
    tiers: &[Tier],
    caption_pid: Option<u32>,
    is_playing: impl Fn(u32) -> bool,
) -> Choice {
    if let Some(pid) = caption_pid {
        return Choice::Process {
            label: name_of(procs, pid),
            pid,
            why: Why::CaptionWindow,
        };
    }
    let playing = producing_audio(procs, tiers, &is_playing);
    let best = playing.iter().map(|(rank, ..)| *rank).min();
    let mut best_apps = playing.iter().filter(|(rank, ..)| Some(*rank) == best);
    match (best_apps.next(), best_apps.next()) {
        (Some((_, label, pid)), None) => Choice::Process {
            label: label.clone(),
            pid: *pid,
            why: Why::ProducingAudio,
        },
        (Some(_), Some(_)) => Choice::Everything {
            reason: "several meeting apps are producing audio".to_string(),
        },
        _ => Choice::Everything {
            reason: "no meeting app is producing audio".to_string(),
        },
    }
}

/// One output device and the processes with a running stream on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSessions {
    pub device: String,
    pub default: bool,
    pub active: Vec<u32>,
}

/// The output device the meeting plays on when that is not the default one (a
/// headset's chat output): process loopback of such a call delivers silence, the
/// loopback of that device has the meeting.
pub fn meeting_output(outputs: &[OutputSessions], tree: &[u32]) -> Option<String> {
    let plays_on = |o: &&OutputSessions| o.active.iter().any(|pid| tree.contains(pid));
    if outputs.iter().filter(|o| o.default).any(|o| plays_on(&o)) {
        return None;
    }
    outputs.iter().find(plays_on).map(|o| o.device.clone())
}

#[cfg(any(windows, target_os = "macos"))]
pub fn snapshot() -> Vec<Proc> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    system
        .processes()
        .values()
        .map(|p| Proc {
            pid: p.pid().as_u32(),
            parent: p.parent().map(sysinfo::Pid::as_u32),
            name: p.name().to_string_lossy().into_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: Option<u32>, name: &str) -> Proc {
        Proc {
            pid,
            parent,
            name: name.to_string(),
        }
    }

    fn zoom_and_chrome() -> Vec<Proc> {
        vec![
            proc(1, None, "launchd"),
            proc(10, Some(1), "zoom.us"),
            proc(11, Some(10), "zoom.us"),
            proc(12, Some(10), "CptHost"),
            proc(20, Some(1), "Google Chrome"),
            proc(21, Some(20), "Google Chrome Helper"),
        ]
    }

    #[test]
    fn roots_skip_children_with_the_same_name() {
        assert_eq!(roots_named(&zoom_and_chrome(), "ZOOM.US"), vec![10]);
        assert!(roots_named(&zoom_and_chrome(), "zoom").is_empty());
    }

    #[test]
    fn the_tree_holds_every_descendant() {
        let mut tree = tree_of(&zoom_and_chrome(), 10);
        tree.sort_unstable();
        assert_eq!(tree, vec![10, 11, 12]);
        assert_eq!(tree_of(&zoom_and_chrome(), 99), vec![99]);
    }

    #[test]
    fn a_native_app_wins_over_the_browser() {
        let found = find_meeting_app(&zoom_and_chrome(), MACOS_APPS).unwrap();
        assert_eq!(found, ("zoom.us".to_string(), 10));
    }

    #[test]
    fn the_browser_is_used_when_no_native_app_runs() {
        let procs = vec![proc(20, Some(1), "Google Chrome")];
        let found = find_meeting_app(&procs, MACOS_APPS).unwrap();
        assert_eq!(found, ("Google Chrome".to_string(), 20));
    }

    #[test]
    fn two_native_apps_are_ambiguous() {
        let procs = vec![proc(10, None, "zoom.us"), proc(30, None, "MSTeams")];
        let error = find_meeting_app(&procs, MACOS_APPS)
            .unwrap_err()
            .to_string();
        assert!(error.contains("several meeting apps"), "{error}");
        assert!(error.contains("MSTeams") && error.contains("zoom.us"));
    }

    #[test]
    fn two_names_of_one_app_are_one_app() {
        let procs = vec![
            proc(5, None, "Teams.exe"),
            proc(6, None, "ms-teams.exe"),
            proc(7, None, "other.exe"),
        ];
        let found = find_meeting_app(&procs, WINDOWS_APPS).unwrap();
        assert_eq!(found, ("ms-teams.exe".to_string(), 6));
    }

    #[test]
    fn nothing_running_is_unavailable() {
        let error = find_meeting_app(&[proc(1, None, "launchd")], MACOS_APPS).unwrap_err();
        assert!(matches!(error, AudioError::Unavailable(_)));
    }

    fn zoom_idle_chrome_in_a_call() -> Vec<Proc> {
        let mut procs = zoom_and_chrome();
        procs.push(proc(22, Some(20), "Google Chrome Helper (Audio)"));
        procs
    }

    #[test]
    fn the_caption_window_decides_even_if_another_app_is_louder() {
        let procs = zoom_idle_chrome_in_a_call();
        let choice = choose(&procs, MACOS_APPS, Some(10), |p| [22].contains(&p));
        assert_eq!(
            choice,
            Choice::Process {
                label: "zoom.us".to_string(),
                pid: 10,
                why: Why::CaptionWindow
            }
        );
        assert!(choice.describe().contains("caption window"));
        let gone = choose(&procs, MACOS_APPS, Some(999), |p| [].contains(&p));
        assert!(matches!(gone, Choice::Process { pid: 999, .. }));
    }

    #[test]
    fn an_idle_app_is_never_picked_without_captions() {
        let procs = zoom_idle_chrome_in_a_call();
        // Zoom is open but silent; the call is in Chrome, whose audio helper plays.
        let choice = choose(&procs, MACOS_APPS, None, |p| [22].contains(&p));
        assert!(matches!(
            choice,
            Choice::Process {
                pid: 20,
                why: Why::ProducingAudio,
                ..
            }
        ));
        let nobody = choose(&procs, MACOS_APPS, None, |p| [].contains(&p));
        assert!(matches!(nobody, Choice::Everything { .. }));
        assert!(
            nobody
                .describe()
                .contains("no meeting app is producing audio")
        );
    }

    #[test]
    fn a_native_app_wins_over_a_browser_and_two_natives_are_ambiguous() {
        let mut procs = zoom_idle_chrome_in_a_call();
        assert!(matches!(
            choose(&procs, MACOS_APPS, None, |p| [11, 22].contains(&p)),
            Choice::Process { pid: 10, .. }
        ));
        procs.push(proc(30, Some(1), "MSTeams"));
        let both = choose(&procs, MACOS_APPS, None, |p| [10, 30].contains(&p));
        assert!(matches!(both, Choice::Everything { .. }));
        assert!(both.describe().contains("several"));
    }

    #[test]
    fn resolve_takes_pids_names_and_meeting() {
        let procs = zoom_and_chrome();
        assert_eq!(
            resolve(&procs, " 42 ", MACOS_APPS).unwrap(),
            ("pid 42".to_string(), 42)
        );
        assert_eq!(
            resolve(&procs, "Google Chrome", MACOS_APPS).unwrap(),
            ("Google Chrome".to_string(), 20)
        );
        assert_eq!(resolve(&procs, "meeting", MACOS_APPS).unwrap().1, 10);
        assert_eq!(resolve(&procs, "", MACOS_APPS).unwrap().1, 10);
        assert!(resolve(&procs, "nothing", MACOS_APPS).is_err());
    }

    fn output(device: &str, default: bool, active: &[u32]) -> OutputSessions {
        OutputSessions {
            device: device.to_string(),
            default,
            active: active.to_vec(),
        }
    }

    #[test]
    fn a_meeting_on_another_output_is_captured_from_that_device() {
        let tree = [34496, 13172];
        let headset = [
            output("Speakers", true, &[777]),
            output("Headset - Chat", false, &[34496]),
        ];
        assert_eq!(
            meeting_output(&headset, &tree).as_deref(),
            Some("Headset - Chat")
        );
        let speakers = [
            output("Speakers", true, &[13172]),
            output("Headset - Chat", false, &[34496]),
        ];
        assert_eq!(meeting_output(&speakers, &tree), None);
        let idle = [output("Headset - Chat", false, &[777])];
        assert_eq!(meeting_output(&idle, &tree), None);
    }
}
