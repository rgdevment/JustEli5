//! The `mc doctor --uia` report: what the backend sees and what the sources
//! would pick, with advice. Pure over a [`UiBackend`] and an injected timer.

use serde::Serialize;

use super::backend::{Budget, NodeIdent, Target, UiBackend, UiError};
use super::blocks::KnownSpeakers;
use super::node::WindowInfo;
use super::platform::Platform;
use super::selectors::Selectors;

#[derive(Debug, Clone, Serialize)]
pub struct WindowRow {
    pub index: usize,
    pub handle: u64,
    pub class: String,
    pub pid: u32,
    pub process: Option<String>,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PickReport {
    pub platform: &'static str,
    pub window_index: usize,
    pub tier: String,
    pub discovery_ms: f64,
    pub discovery_nodes: usize,
    pub container_path: Option<Vec<usize>>,
    pub whole_window_fallback: bool,
    pub poll_ms: f64,
    pub poll_nodes: usize,
    pub captions: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiaReport {
    pub selectors_version: u32,
    pub windows: Vec<WindowRow>,
    pub picks: Vec<PickReport>,
    pub advice: Vec<String>,
}

/// Node counts are read from the snapshots; `timer` returns milliseconds from
/// any monotonic origin (injected so tests are deterministic).
pub fn uia_report(
    backend: &mut dyn UiBackend,
    selectors: &Selectors,
    max_nodes: usize,
    timer: &mut dyn FnMut() -> f64,
) -> Result<UiaReport, UiError> {
    let listed = backend.windows()?;
    let windows: Vec<WindowRow> = listed
        .iter()
        .enumerate()
        .map(|(index, w)| {
            // Other applications' windows (mail, documents, browser tabs) are
            // not ours to print: only their class and the size of the title.
            let candidate = super::dump::is_candidate(w, selectors);
            WindowRow {
                index,
                handle: w.handle,
                class: w.class_name.clone(),
                pid: w.pid,
                process: if candidate { w.process.clone() } else { None },
                title: if candidate {
                    super::anon::mask_title(&w.title, selectors)
                } else {
                    super::anon::hidden_title(&w.title)
                },
            }
        })
        .collect();
    let mut picks = Vec::new();
    let mut advice = Vec::new();

    for platform in Platform::ALL {
        let Some((index, tier)) = platform.pick(&listed, selectors) else {
            continue;
        };
        picks.push(read_pick(
            backend,
            selectors,
            platform,
            &listed[index],
            index,
            tier,
            max_nodes,
            timer,
            &mut advice,
        ));
    }
    if Platform::Teams.pick(&listed, selectors).is_none()
        && Platform::TeamsWeb.pick(&listed, selectors).is_some()
    {
        advice.push(
            "Teams: only a browser tab was found; use --source teams-web (teams reads the desktop app)."
                .to_string(),
        );
    }
    if picks.is_empty() {
        advice.push(
            "No meeting window found (Teams, Zoom, Meet, Teams web, Slack): open the meeting (or the caption pop-out)."
                .to_string(),
        );
    }
    Ok(UiaReport {
        selectors_version: selectors.version,
        windows,
        picks,
        advice,
    })
}

#[allow(clippy::too_many_arguments)]
fn read_pick(
    backend: &mut dyn UiBackend,
    selectors: &Selectors,
    platform: Platform,
    window: &WindowInfo,
    index: usize,
    tier: String,
    max_nodes: usize,
    timer: &mut dyn FnMut() -> f64,
    advice: &mut Vec<String>,
) -> PickReport {
    let name = platform.name();
    let mut report = PickReport {
        platform: name,
        window_index: index,
        tier,
        discovery_ms: 0.0,
        discovery_nodes: 0,
        container_path: None,
        whole_window_fallback: false,
        poll_ms: 0.0,
        poll_nodes: 0,
        captions: 0,
        error: None,
    };
    let depth = platform.discovery_depth(selectors).max(14);
    let whole = Target {
        window: window.handle,
        path: Vec::new(),
        expect: None,
    };
    let started = timer();
    let tree = match backend.snapshot(&whole, &Budget::new(depth, max_nodes)) {
        Ok(tree) => tree,
        Err(error) => {
            report.error = Some(error.to_string());
            advice.push(match error {
                UiError::Budget => format!(
                    "{name}: the window tree exceeds {max_nodes} nodes; raise capture limits."
                ),
                UiError::Backend(ref m) if m.contains("access denied") => {
                    "Access denied: one of the apps runs elevated; run both at the same level."
                        .to_string()
                }
                other => format!("{name}: could not read the window ({other})."),
            });
            return report;
        }
    };
    report.discovery_ms = timer() - started;
    report.discovery_nodes = tree.count();
    if platform.is_chromium() && report.discovery_nodes < 50 {
        advice.push(format!(
            "{}{} tree is tiny: Chromium accessibility may be off. Retry in a few seconds, or start Narrator once.",
            name[..1].to_uppercase(),
            &name[1..]
        ));
    }
    let path = platform.container(&tree, selectors);
    report.whole_window_fallback = path.as_ref().is_some_and(Vec::is_empty);
    if path.is_none() {
        advice.push(match platform {
            Platform::Zoom => {
                "Zoom: no caption list found; turn on Closed Captions in the meeting.".to_string()
            }
            _ => format!("{name}: no caption container found; turn on captions in the meeting."),
        });
    }
    report.container_path = path.clone();
    if let Some(path) = path {
        let ident = tree.at(&path).map(NodeIdent::of);
        let target = Target {
            window: window.handle,
            path,
            expect: ident,
        };
        let depth = platform.poll_depth(selectors);
        let started = timer();
        match backend.snapshot(&target, &Budget::new(depth, max_nodes)) {
            Ok(poll_tree) => {
                report.poll_ms = timer() - started;
                report.poll_nodes = poll_tree.count();
                report.captions = platform
                    .captions(&poll_tree, selectors, &mut KnownSpeakers::new())
                    .len();
            }
            Err(error) => report.error = Some(error.to_string()),
        }
        if report.poll_ms > 250.0 {
            advice.push(format!(
                "{name}: a poll takes {:.0} ms (> 250 ms); the capture will lag.",
                report.poll_ms
            ));
        }
    }
    report
}

pub fn format_report(report: &UiaReport) -> String {
    let mut out = format!("selectors v{}\nwindows:\n", report.selectors_version);
    for w in &report.windows {
        out.push_str(&format!(
            "  [{}] {:<28} pid {:<6} {:<16} {}\n",
            w.index,
            w.class,
            w.pid,
            w.process.as_deref().unwrap_or("-"),
            w.title
        ));
    }
    for p in &report.picks {
        out.push_str(&format!(
            "{}: window [{}] ({}), discovery {:.0} ms / {} nodes, container {}{}, poll {:.0} ms / {} nodes, {} captions{}\n",
            p.platform,
            p.window_index,
            p.tier,
            p.discovery_ms,
            p.discovery_nodes,
            p.container_path
                .as_ref()
                .map_or("none".to_string(), |path| format!("{path:?}")),
            if p.whole_window_fallback { " (whole window)" } else { "" },
            p.poll_ms,
            p.poll_nodes,
            p.captions,
            p.error.as_ref().map_or(String::new(), |e| format!(", error: {e}")),
        ));
    }
    for line in &report.advice {
        out.push_str(&format!("advice: {line}\n"));
    }
    out
}
