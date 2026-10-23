//! Pure tables for the macOS backend (AXUIElement), kept out of the
//! `cfg(target_os = "macos")` code so they can be tested anywhere.

use std::time::Duration;

use super::backend::UiError;
use super::node::{Role, WindowInfo};
use super::selectors::Selectors;
use super::source::Platform;

/// `AXRole` (refined by `AXSubrole`) to the control-type vocabulary the
/// parsers expect (the names UI Automation uses on Windows).
///
/// | AXRole                                                   | Role                  |
/// |----------------------------------------------------------|-----------------------|
/// | AXWindow                                                 | Window                |
/// | AXWebArea                                                | Document              |
/// | AXGroup                                                  | Group                 |
/// | AXScrollArea, AXSplitGroup, AXLayoutArea, AXBrowser      | Pane                  |
/// | AXStaticText, AXHeading, AXListMarker                    | Text                  |
/// | AXList                                                   | List                  |
/// | AXRow                                                    | ListItem              |
/// | AXTextField, AXTextArea, AXSearchField                   | Edit                  |
/// | AXButton, AXMenuButton, AXPopUpButton, AXDisclosureTriangle | Button             |
/// | AXCheckBox, AXRadioButton, AXLink, AXImage, AXMenu, ...  | CheckBox, RadioButton, Hyperlink, Image, Menu, ... |
/// | anything else                                            | `Other(<name without "AX">)` |
///
/// An empty or `AXUnknown` role falls back to the subrole, so a generic
/// element that only says what it is through `AXSubrole` keeps that name.
pub fn role_for(role: &str, subrole: &str) -> Role {
    let name = match role {
        "AXWindow" => return Role::Window,
        "AXWebArea" => return Role::Document,
        "AXGroup" => return Role::Group,
        "AXScrollArea" | "AXSplitGroup" | "AXLayoutArea" | "AXBrowser" => return Role::Pane,
        "AXStaticText" | "AXHeading" | "AXListMarker" => return Role::Text,
        "AXList" => return Role::List,
        "AXRow" => return Role::ListItem,
        "AXTextField" | "AXTextArea" | "AXSearchField" => return Role::Edit,
        "AXButton" | "AXMenuButton" | "AXPopUpButton" | "AXDisclosureTriangle" => {
            return Role::Button;
        }
        "AXCheckBox" => "CheckBox",
        "AXRadioButton" => "RadioButton",
        "AXLink" => "Hyperlink",
        "AXImage" => "Image",
        "AXMenu" => "Menu",
        "AXMenuBar" => "MenuBar",
        "AXMenuItem" | "AXMenuBarItem" => "MenuItem",
        "AXToolbar" => "ToolBar",
        "AXTabGroup" => "Tab",
        "AXOutline" => "Tree",
        "AXTable" => "Table",
        "AXCell" => "DataItem",
        "AXComboBox" => "ComboBox",
        "AXProgressIndicator" | "AXBusyIndicator" => "ProgressBar",
        "AXSlider" => "Slider",
        "AXScrollBar" => "ScrollBar",
        "" | "AXUnknown" if !subrole.is_empty() => subrole,
        "" | "AXUnknown" => "Custom",
        other => other,
    };
    Role::parse(name.strip_prefix("AX").unwrap_or(name))
}

/// The meeting apps we read, and how they expose their tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    Teams,
    Zoom,
    Slack,
    /// Chrome, Edge, Brave, Arc: Teams, Meet or Slack in a tab.
    Chromium,
    /// Safari, Firefox: native accessibility, nothing to switch on.
    Browser,
}

impl AppKind {
    /// Chromium and Electron build their web tree only after a client sets `AXManualAccessibility`.
    pub fn needs_manual_accessibility(self) -> bool {
        matches!(self, Self::Teams | Self::Slack | Self::Chromium)
    }
}

/// The app and bundle name of an executable path; a nested `.app` (a helper process) never owns windows, so it is rejected.
pub fn classify_exe(path: &str) -> Option<(AppKind, &str)> {
    let mut bundles = path.split('/').filter_map(|part| part.strip_suffix(".app"));
    let bundle = bundles.next()?;
    if bundles.next().is_some() {
        return None;
    }
    let kind = match bundle {
        "Microsoft Teams" | "Microsoft Teams (work or school)" | "Microsoft Teams classic" => {
            AppKind::Teams
        }
        "zoom.us" | "Zoom" => AppKind::Zoom,
        "Slack" => AppKind::Slack,
        "Google Chrome"
        | "Google Chrome Beta"
        | "Google Chrome Canary"
        | "Microsoft Edge"
        | "Brave Browser"
        | "Arc"
        | "Opera"
        | "Vivaldi" => AppKind::Chromium,
        "Safari" | "Firefox" => AppKind::Browser,
        _ => return None,
    };
    Some((kind, bundle))
}

/// The Windows window class the default selectors look for: macOS windows have none, and this keeps the pickers shared.
pub fn window_class(kind: AppKind, title: &str) -> &'static str {
    match kind {
        AppKind::Teams => "TeamsWebView",
        AppKind::Zoom => {
            let lower = title.to_lowercase();
            if lower.contains("meeting") || lower.contains("webinar") {
                "ConfMultiTabContentWndClass"
            } else if lower == "zoom" || lower.contains("workplace") {
                "ZPPTMainFrmWndClassEx"
            } else {
                "ZoomWindow"
            }
        }
        AppKind::Slack => "SlackWindow",
        // Teams, Meet and Slack in a tab are told apart by their page title, not by class.
        AppKind::Chromium | AppKind::Browser => "BrowserWindow",
    }
}

/// The attributes of one element that carry text, already read.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AxText {
    pub title: String,
    pub description: String,
    pub value: String,
}

/// Drops object replacement characters (inline attachments) and NULs.
pub fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\u{FFFC}' | '\0'))
        .collect()
}

fn first_non_empty<'a>(candidates: &[&'a str]) -> &'a str {
    candidates
        .iter()
        .copied()
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

/// `(name, value)` as UIA reports them: static text carries its text in `AXValue`, which UIA calls the `Name`.
pub fn name_and_value(role: &Role, text: &AxText) -> (String, Option<String>) {
    let (title, description, value) = (
        clean_text(&text.title),
        clean_text(&text.description),
        clean_text(&text.value),
    );
    if matches!(role, Role::Text) {
        return (
            first_non_empty(&[&title, &value, &description]).to_string(),
            None,
        );
    }
    let name = first_non_empty(&[&title, &description]).to_string();
    let value = (!value.is_empty() && value != name).then_some(value);
    (name, value)
}

/// UIA names Chromium's page document `RootWebArea`; AX only has the role, so the web area gets that id.
pub fn automation_id_for(role: &str, identifier: &str, dom_id: &str) -> String {
    let id = first_non_empty(&[identifier, dom_id]);
    if id.is_empty() && role == "AXWebArea" {
        "RootWebArea".to_string()
    } else {
        id.to_string()
    }
}

/// Whether to switch on `AXManualAccessibility` for an app: when one of its windows is one the platform's picker would
/// take (so the rules are the pickers', not a copy). `None` (doctor) means any platform; Zoom's tree is native.
pub fn wants_manual(platform: Option<Platform>, windows: &[WindowInfo], sel: &Selectors) -> bool {
    Platform::ALL
        .into_iter()
        .filter(|p| *p != Platform::Zoom && platform.is_none_or(|wanted| wanted == *p))
        .any(|p| windows.iter().any(|w| p.is_window(w, sel)))
}

/// Wait before the next try to switch on accessibility after `attempts` failures: 1 s doubling up to 30 s.
pub fn retry_delay(attempts: u32) -> Duration {
    Duration::from_secs((1u64 << attempts.min(5)).min(30))
}

pub const AX_ERROR_FAILURE: i32 = -25200;
pub const AX_ERROR_ILLEGAL_ARGUMENT: i32 = -25201;
pub const AX_ERROR_INVALID_ELEMENT: i32 = -25202;
pub const AX_ERROR_CANNOT_COMPLETE: i32 = -25204;
pub const AX_ERROR_ATTRIBUTE_UNSUPPORTED: i32 = -25205;
pub const AX_ERROR_NOT_IMPLEMENTED: i32 = -25208;
pub const AX_ERROR_API_DISABLED: i32 = -25211;
pub const AX_ERROR_NO_VALUE: i32 = -25212;

/// The attribute is simply not there (not an error of the element).
pub fn is_absent(code: i32) -> bool {
    matches!(
        code,
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE | AX_ERROR_NOT_IMPLEMENTED
    )
}

pub const PERMISSION_HELP: &str = "Accessibility permission is not granted: enable the terminal or app running `mc` in System Settings > Privacy & Security > Accessibility (then restart it); `mc doctor --uia --ax-prompt` asks macOS to show the prompt";

/// Maps a failed `AXError`; an invalid element of a living app is `Stale`, of a dead one `WindowGone`.
pub fn map_ax_error(code: i32, app_alive: bool) -> UiError {
    match code {
        AX_ERROR_INVALID_ELEMENT => {
            if app_alive {
                UiError::Stale
            } else {
                UiError::WindowGone
            }
        }
        // A messaging timeout also returns this: the app is busy or gone.
        AX_ERROR_CANNOT_COMPLETE => {
            if app_alive {
                UiError::Budget
            } else {
                UiError::WindowGone
            }
        }
        AX_ERROR_API_DISABLED => UiError::Init(PERMISSION_HELP.to_string()),
        AX_ERROR_NOT_IMPLEMENTED => UiError::Backend(
            "the app does not support the accessibility API (Chromium apps need AXManualAccessibility)"
                .to_string(),
        ),
        other => UiError::Backend(format!("AXError {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_use_the_uia_vocabulary() {
        assert_eq!(role_for("AXStaticText", ""), Role::Text);
        assert_eq!(role_for("AXWebArea", ""), Role::Document);
        assert_eq!(role_for("AXRow", ""), Role::ListItem);
        assert_eq!(role_for("AXLink", ""), Role::Other("Hyperlink".into()));
        assert_eq!(role_for("AXSplitter", ""), Role::Other("Splitter".into()));
    }

    #[test]
    fn unknown_roles_fall_back_to_the_subrole() {
        assert_eq!(role_for("", "AXFoo"), Role::Other("Foo".into()));
        assert_eq!(role_for("AXUnknown", ""), Role::Other("Custom".into()));
        assert_eq!(role_for("AXGroup", "AXFoo"), Role::Group);
    }

    #[test]
    fn executables_are_classified_by_bundle() {
        let teams = "/Applications/Microsoft Teams.app/Contents/MacOS/MSTeams";
        assert_eq!(
            classify_exe(teams),
            Some((AppKind::Teams, "Microsoft Teams"))
        );
        let slack = "/Applications/Slack.app/Contents/MacOS/Slack";
        assert_eq!(classify_exe(slack).map(|c| c.0), Some(AppKind::Slack));
        let zoom = "/Applications/zoom.us.app/Contents/MacOS/zoom.us";
        assert_eq!(classify_exe(zoom).map(|c| c.0), Some(AppKind::Zoom));
        let helper = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper";
        assert_eq!(classify_exe(helper), None);
        assert_eq!(classify_exe("/usr/bin/ssh"), None);
        assert_eq!(
            classify_exe("/Applications/Notes.app/Contents/MacOS/Notes"),
            None
        );
    }

    #[test]
    fn only_chromium_based_apps_need_the_manual_switch() {
        assert!(AppKind::Teams.needs_manual_accessibility());
        assert!(AppKind::Slack.needs_manual_accessibility());
        assert!(AppKind::Chromium.needs_manual_accessibility());
        assert!(!AppKind::Zoom.needs_manual_accessibility());
        assert!(!AppKind::Browser.needs_manual_accessibility());
    }

    #[test]
    fn window_classes_match_the_default_selectors() {
        let sel = super::super::Selectors::default();
        assert!(
            sel.teams
                .classes
                .iter()
                .any(|c| c == window_class(AppKind::Teams, "x"))
        );
        let meeting = window_class(AppKind::Zoom, "Zoom Meeting");
        assert!(sel.zoom.meeting_classes.iter().any(|c| c == meeting));
        assert_eq!(
            window_class(AppKind::Zoom, "Zoom Workplace"),
            sel.zoom.home_class
        );
        // A Teams tab is not the Teams app: the desktop class never goes to a browser.
        assert_eq!(
            window_class(AppKind::Chromium, "Sprint | Microsoft Teams"),
            "BrowserWindow"
        );
        assert_eq!(window_class(AppKind::Browser, "Inbox"), "BrowserWindow");
        assert!(
            sel.browser
                .classes
                .iter()
                .any(|c| c == window_class(AppKind::Browser, "x"))
        );
        assert!(
            sel.slack
                .app_classes
                .iter()
                .any(|c| c == window_class(AppKind::Slack, "x"))
        );
    }

    fn app_window(kind: AppKind, bundle: &str, title: &str) -> WindowInfo {
        WindowInfo {
            handle: 1,
            title: title.to_string(),
            class_name: window_class(kind, title).to_string(),
            pid: 1,
            process: Some(bundle.to_string()),
        }
    }

    #[test]
    fn manual_accessibility_follows_the_pickers() {
        let sel = Selectors::default();
        let teams = [app_window(AppKind::Teams, "Microsoft Teams", "Chat")];
        assert!(wants_manual(Some(Platform::Teams), &teams, &sel));
        assert!(!wants_manual(Some(Platform::Zoom), &teams, &sel));
        assert!(!wants_manual(Some(Platform::Meet), &teams, &sel));

        let tab = |title| [app_window(AppKind::Chromium, "Google Chrome", title)];
        let teams_tab = tab("Sprint | Microsoft Teams");
        assert!(wants_manual(Some(Platform::TeamsWeb), &teams_tab, &sel));
        assert!(!wants_manual(Some(Platform::Teams), &teams_tab, &sel));
        assert!(!wants_manual(Some(Platform::TeamsWeb), &tab("Inbox"), &sel));
        assert!(wants_manual(None, &teams_tab, &sel));
        let meet = tab("Meet - abc-defg-hij - Google Chrome");
        assert!(wants_manual(Some(Platform::Meet), &meet, &sel));
        assert!(!wants_manual(Some(Platform::Slack), &meet, &sel));
        assert!(wants_manual(
            Some(Platform::Slack),
            &tab("Huddle | Acme | Slack"),
            &sel
        ));
        let slack = [app_window(AppKind::Slack, "Slack", "Acme")];
        assert!(wants_manual(Some(Platform::Slack), &slack, &sel));
        assert!(!wants_manual(Some(Platform::Meet), &slack, &sel));

        // Safari and Firefox have native accessibility, and Zoom is never switched.
        let zoom = [app_window(AppKind::Zoom, "zoom.us", "Zoom Meeting")];
        assert!(!wants_manual(None, &zoom, &sel));
    }

    #[test]
    fn retry_backs_off_and_caps() {
        assert_eq!(retry_delay(0), Duration::from_secs(1));
        assert_eq!(retry_delay(3), Duration::from_secs(8));
        assert_eq!(retry_delay(40), Duration::from_secs(30));
    }

    #[test]
    fn static_text_name_comes_from_its_value() {
        let text = AxText {
            value: "hello\u{FFFC}".into(),
            ..AxText::default()
        };
        assert_eq!(name_and_value(&Role::Text, &text), ("hello".into(), None));
    }

    #[test]
    fn fields_keep_their_value_apart() {
        let text = AxText {
            title: "Search".into(),
            description: "ignored".into(),
            value: "abc".into(),
        };
        assert_eq!(
            name_and_value(&Role::Edit, &text),
            ("Search".into(), Some("abc".into()))
        );
        let same = AxText {
            title: "x".into(),
            value: "x".into(),
            ..AxText::default()
        };
        assert_eq!(name_and_value(&Role::Button, &same), ("x".into(), None));
        let described = AxText {
            description: "Mute".into(),
            ..AxText::default()
        };
        assert_eq!(name_and_value(&Role::Button, &described).0, "Mute");
    }

    #[test]
    fn web_area_gets_the_chromium_root_id() {
        assert_eq!(automation_id_for("AXWebArea", "", ""), "RootWebArea");
        assert_eq!(automation_id_for("AXWebArea", "", "app"), "app");
        assert_eq!(automation_id_for("AXGroup", "id", "dom"), "id");
        assert_eq!(automation_id_for("AXGroup", "", ""), "");
    }

    #[test]
    fn errors_map_by_liveness() {
        assert_eq!(map_ax_error(AX_ERROR_INVALID_ELEMENT, true), UiError::Stale);
        assert_eq!(
            map_ax_error(AX_ERROR_INVALID_ELEMENT, false),
            UiError::WindowGone
        );
        assert_eq!(
            map_ax_error(AX_ERROR_CANNOT_COMPLETE, true),
            UiError::Budget
        );
        assert!(matches!(
            map_ax_error(AX_ERROR_API_DISABLED, true),
            UiError::Init(m) if m.contains("Privacy & Security")
        ));
        assert!(matches!(
            map_ax_error(AX_ERROR_FAILURE, true),
            UiError::Backend(_)
        ));
        assert!(is_absent(AX_ERROR_NO_VALUE));
        assert!(!is_absent(AX_ERROR_ILLEGAL_ARGUMENT));
    }
}
