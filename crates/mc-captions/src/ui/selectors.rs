//! Everything the meeting platforms may change between releases, as data. The defaults
//! are the values the Python implementation uses; a `capture.selectors` block in
//! the config can override any of them without a new build.

use serde::{Deserialize, Serialize};

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TeamsSel {
    pub classes: Vec<String>,
    pub required_title: String,
    pub caption_keywords: Vec<String>,
    pub meeting_keywords: Vec<String>,
    pub reject_keywords: Vec<String>,
    pub noise: Vec<String>,
    /// Labels of the profile card Teams shows on hover, never a speaker.
    pub card_labels: Vec<String>,
    pub max_name_words: usize,
    pub max_name_chars: usize,
    pub root_automation_id: String,
    pub root_depths: Vec<usize>,
    pub walk_depths: Vec<usize>,
    pub title_prefixes: Vec<String>,
    pub title_suffixes: Vec<String>,
    pub cache_ttl_s: f64,
}

impl Default for TeamsSel {
    fn default() -> Self {
        Self {
            classes: strings(&["TeamsWebView"]),
            required_title: "microsoft teams".to_string(),
            caption_keywords: strings(&["subtítulos", "subtitulos", "captions", "subtitles"]),
            meeting_keywords: strings(&["meeting", "reunión", "call", "llamada", "join"]),
            reject_keywords: strings(&["chat", "teams and channels"]),
            noise: strings(&["Micrófono", "Microphone", "Muted", "Silenciado"]),
            card_labels: strings(&[
                "Organización",
                "Organization",
                "Cargo",
                "Puesto",
                "Job title",
                "Departamento",
                "Department",
                "Ubicación",
                "Location",
                "Correo electrónico",
                "Email",
                "Teléfono",
                "Phone",
                "Empresa",
                "Company",
            ]),
            max_name_words: 7,
            max_name_chars: 60,
            root_automation_id: "RootWebArea".to_string(),
            root_depths: vec![10, 15, 20],
            walk_depths: vec![14, 20, 25],
            title_prefixes: strings(&[
                "Subtítulos | ",
                "Subtitulos | ",
                "Captions | ",
                "Subtitles | ",
            ]),
            title_suffixes: strings(&[" | Ventana anclada", " | Pinned window"]),
            cache_ttl_s: 10.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ZoomSel {
    pub meeting_classes: Vec<String>,
    pub title_hints: Vec<String>,
    pub home_class: String,
    /// Windows of these classes are never taken for a Zoom meeting by title
    /// (a Teams meeting is titled "reunión" / "meeting" too).
    pub exclude_classes: Vec<String>,
    pub caption_window_class: String,
    pub caption_window_depth: usize,
    pub list_depth: usize,
    pub list_name_hints: Vec<String>,
    pub fallback_depth: usize,
    pub cache_ttl_s: f64,
}

impl Default for ZoomSel {
    fn default() -> Self {
        Self {
            meeting_classes: strings(&["ConfMultiTabContentWndClass", "ZPContentViewWndClass"]),
            title_hints: strings(&["webinar", "meeting", "reunión", "zoom"]),
            home_class: "ZPPTMainFrmWndClassEx".to_string(),
            exclude_classes: strings(&["TeamsWebView", "ZPPTEnhanceLoginMainWindowClassEx"]),
            caption_window_class: "ZConfCCRecieveWndExClass".to_string(),
            caption_window_depth: 5,
            list_depth: 3,
            list_name_hints: strings(&["caption", "subtitle"]),
            fallback_depth: 10,
            cache_ttl_s: 8.0,
        }
    }
}

/// Windows that host web content. `Chrome_WidgetWin_1` is also Electron's class
/// (Slack), so it never identifies a platform on its own, only a window title does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserSel {
    /// Windows classes; macOS windows get the synthesized `BrowserWindow`.
    pub classes: Vec<String>,
    /// Executables (macOS: bundle names) of the browsers, compared ignoring case:
    /// another Electron app with the same class is not a browser.
    pub processes: Vec<String>,
    /// Trailing ` - <name>` that browsers append to the page title.
    pub names: Vec<String>,
}

impl Default for BrowserSel {
    fn default() -> Self {
        Self {
            classes: strings(&["Chrome_WidgetWin_1", "MozillaWindowClass", "BrowserWindow"]),
            processes: strings(&[
                "chrome.exe",
                "msedge.exe",
                "brave.exe",
                "firefox.exe",
                "opera.exe",
                "vivaldi.exe",
                "arc.exe",
                "Google Chrome",
                "Google Chrome Beta",
                "Google Chrome Canary",
                "Microsoft Edge",
                "Brave Browser",
                "Arc",
                "Firefox",
                "Safari",
                "Opera",
                "Vivaldi",
            ]),
            names: strings(&[
                "Google Chrome",
                "Microsoft Edge",
                "Brave",
                "Arc",
                "Mozilla Firefox",
                "Firefox",
                "Safari",
                "Opera",
                "Vivaldi",
            ]),
        }
    }
}

/// Google Meet in a browser. Unverified until a real dump: the captions are an
/// ARIA region labelled "Captions" with one block per speaker (name, then text).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MeetSel {
    /// Meeting page titles start with one of these (compared lowercased).
    pub title_prefixes: Vec<String>,
    pub region_names: Vec<String>,
    /// Texts inside the region that are interface, not speech (exact, lowercased).
    pub noise: Vec<String>,
    pub search_depth: usize,
    pub walk_depth: usize,
    /// In a flattened tree a text is a speaker's name when it has at most this many words...
    pub name_max_words: usize,
    /// ...and none of these characters (a name is not a sentence).
    pub sentence_punctuation: String,
    pub cache_ttl_s: f64,
}

impl Default for MeetSel {
    fn default() -> Self {
        Self {
            title_prefixes: strings(&["Meet - ", "Meet \u{2013} ", "Meet \u{2014} "]),
            region_names: strings(&["Captions", "Subtítulos", "Subtitulos"]),
            noise: strings(&[
                "Jump to bottom",
                "Ir al final",
                "Captions are on",
                "Los subtítulos están activados",
            ]),
            search_depth: 60,
            walk_depth: 14,
            name_max_words: 5,
            sentence_punctuation: ".?!\u{2026},;:\u{bf}\u{a1}".to_string(),
            cache_ttl_s: 10.0,
        }
    }
}

/// Teams in a browser tab: the same caption DOM as the desktop WebView2, read
/// with [`TeamsSel`]; only the window choice differs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TeamsWebSel {
    pub title_hints: Vec<String>,
    pub cache_ttl_s: f64,
}

impl Default for TeamsWebSel {
    fn default() -> Self {
        Self {
            title_hints: strings(&["microsoft teams"]),
            cache_ttl_s: 10.0,
        }
    }
}

/// Slack huddles, in the desktop app (Electron) or a browser tab. Unverified
/// until a real dump: captions are a list with the speaker's name and the text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlackSel {
    /// Executables of the desktop app (macOS: the bundle name), compared ignoring case.
    pub app_processes: Vec<String>,
    /// Classes of the desktop app's windows; macOS windows get `SlackWindow`.
    pub app_classes: Vec<String>,
    /// A browser window is Slack when its page title is "<hint> | ..." or "... - <hint>".
    pub title_hints: Vec<String>,
    /// A window with one of these in its title is the huddle itself and wins.
    pub huddle_hints: Vec<String>,
    pub region_names: Vec<String>,
    pub noise: Vec<String>,
    pub search_depth: usize,
    pub walk_depth: usize,
    /// In a flattened tree a text is a speaker's name when it has at most this many words...
    pub name_max_words: usize,
    /// ...and none of these characters (a name is not a sentence).
    pub sentence_punctuation: String,
    pub cache_ttl_s: f64,
}

impl Default for SlackSel {
    fn default() -> Self {
        Self {
            app_processes: strings(&["slack.exe", "slack"]),
            app_classes: strings(&["Chrome_WidgetWin_1", "SlackWindow"]),
            title_hints: strings(&["slack"]),
            huddle_hints: strings(&["huddle"]),
            region_names: strings(&[
                "Captions",
                "Live captions",
                "Subtítulos",
                "Subtitulos",
                "Transcript",
            ]),
            noise: strings(&[
                "Captions are only visible to you",
                "Los subtítulos solo son visibles para ti",
                "Los subtítulos solo los ves tú",
            ]),
            search_depth: 60,
            walk_depth: 14,
            name_max_words: 5,
            sentence_punctuation: ".?!\u{2026},;:\u{bf}\u{a1}".to_string(),
            cache_ttl_s: 10.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Selectors {
    /// Bumped when the defaults change; `mc doctor` prints it.
    pub version: u32,
    pub teams: TeamsSel,
    pub zoom: ZoomSel,
    pub browser: BrowserSel,
    pub meet: MeetSel,
    pub teams_web: TeamsWebSel,
    pub slack: SlackSel,
}

impl Default for Selectors {
    fn default() -> Self {
        Self {
            version: 5,
            teams: TeamsSel::default(),
            zoom: ZoomSel::default(),
            browser: BrowserSel::default(),
            meet: MeetSel::default(),
            teams_web: TeamsWebSel::default(),
            slack: SlackSel::default(),
        }
    }
}

impl Selectors {
    /// Rejects values that would make a parser match everything or nothing
    /// (an empty keyword is a substring of every title).
    pub fn validate(&self) -> Result<(), String> {
        let t = &self.teams;
        let z = &self.zoom;
        let lists: [(&str, &Vec<String>); 23] = [
            ("teams.classes", &t.classes),
            ("teams.caption_keywords", &t.caption_keywords),
            ("teams.meeting_keywords", &t.meeting_keywords),
            ("teams.reject_keywords", &t.reject_keywords),
            ("teams.noise", &t.noise),
            ("teams.title_prefixes", &t.title_prefixes),
            ("teams.title_suffixes", &t.title_suffixes),
            ("zoom.meeting_classes", &z.meeting_classes),
            ("zoom.title_hints", &z.title_hints),
            ("zoom.list_name_hints", &z.list_name_hints),
            ("browser.classes", &self.browser.classes),
            ("browser.names", &self.browser.names),
            ("browser.processes", &self.browser.processes),
            ("meet.title_prefixes", &self.meet.title_prefixes),
            ("meet.region_names", &self.meet.region_names),
            ("meet.noise", &self.meet.noise),
            ("teams_web.title_hints", &self.teams_web.title_hints),
            ("slack.app_processes", &self.slack.app_processes),
            ("slack.app_classes", &self.slack.app_classes),
            ("slack.title_hints", &self.slack.title_hints),
            ("slack.huddle_hints", &self.slack.huddle_hints),
            ("slack.region_names", &self.slack.region_names),
            ("slack.noise", &self.slack.noise),
        ];
        for (name, list) in lists {
            if list.iter().any(String::is_empty) {
                return Err(format!("selectors.{name} contains an empty string"));
            }
        }
        if t.root_automation_id.is_empty() {
            return Err("selectors.teams.root_automation_id is empty".to_string());
        }
        if t.required_title.is_empty() {
            return Err("selectors.teams.required_title is empty".to_string());
        }
        if t.root_depths.is_empty() || t.walk_depths.is_empty() {
            return Err("selectors.teams depth lists must not be empty".to_string());
        }
        let depths = [
            (
                "meet",
                self.meet.search_depth,
                self.meet.walk_depth,
                self.meet.name_max_words,
            ),
            (
                "slack",
                self.slack.search_depth,
                self.slack.walk_depth,
                self.slack.name_max_words,
            ),
        ];
        for (name, search, walk, words) in depths {
            if search == 0 || walk == 0 || words == 0 {
                return Err(format!(
                    "selectors.{name} depths and name_max_words must be at least 1"
                ));
            }
        }
        let ttls = [
            ("teams", t.cache_ttl_s),
            ("zoom", z.cache_ttl_s),
            ("meet", self.meet.cache_ttl_s),
            ("teams_web", self.teams_web.cache_ttl_s),
            ("slack", self.slack.cache_ttl_s),
        ];
        for (name, ttl) in ttls {
            if !ttl.is_finite() || ttl < 0.0 {
                return Err(format!(
                    "selectors.{name}.cache_ttl_s must be a number >= 0"
                ));
            }
        }
        Ok(())
    }
}
