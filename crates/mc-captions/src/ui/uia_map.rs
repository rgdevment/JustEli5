//! Pure tables for the Windows backend, kept out of the `cfg(windows)` code so
//! they can be tested anywhere.

use super::backend::UiError;

const UIA_E_ELEMENTNOTAVAILABLE: u32 = 0x8004_0201;
const UIA_E_TIMEOUT: u32 = 0x8013_1505;
const RPC_E_CALL_REJECTED: u32 = 0x8001_0001;
const RPC_E_CALL_CANCELED: u32 = 0x8001_0002;
const RPC_E_SERVER_DIED: u32 = 0x8001_0007;
const RPC_E_DISCONNECTED: u32 = 0x8001_0108;
const RPC_E_SERVER_DIED_DNE: u32 = 0x8001_0012;
const CO_E_OBJNOTCONNECTED: u32 = 0x8004_01FD;
const RPC_S_SERVER_UNAVAILABLE: u32 = 0x8007_06BA;
const E_ACCESSDENIED: u32 = 0x8007_0005;

/// Maps a failed HRESULT from a UI Automation call.
///
/// `window_alive` says whether the target window still exists: an element that
/// is no longer available inside a living window only needs a new search
/// (`Stale`), inside a dead one the window is gone.
pub fn map_hresult(code: u32, window_alive: bool) -> UiError {
    match code {
        UIA_E_ELEMENTNOTAVAILABLE
        | RPC_E_DISCONNECTED
        | CO_E_OBJNOTCONNECTED
        | RPC_E_SERVER_DIED
        | RPC_E_SERVER_DIED_DNE
        | RPC_S_SERVER_UNAVAILABLE => {
            if window_alive {
                UiError::Stale
            } else {
                UiError::WindowGone
            }
        }
        // The target is busy or slow: try again, it is not an error of ours.
        UIA_E_TIMEOUT | RPC_E_CALL_REJECTED | RPC_E_CALL_CANCELED => UiError::Budget,
        E_ACCESSDENIED => UiError::Backend(
            "access denied: is Teams/Zoom running elevated (as administrator)?".to_string(),
        ),
        other => UiError::Backend(format!("HRESULT 0x{other:08X}")),
    }
}

/// `ControlTypeName` as the Python `uiautomation` package spells it, from a
/// UIA ControlType id (50000..=50040).
pub fn control_type_name(id: i32) -> Option<&'static str> {
    const NAMES: [&str; 41] = [
        "ButtonControl",
        "CalendarControl",
        "CheckBoxControl",
        "ComboBoxControl",
        "EditControl",
        "HyperlinkControl",
        "ImageControl",
        "ListItemControl",
        "ListControl",
        "MenuControl",
        "MenuBarControl",
        "MenuItemControl",
        "ProgressBarControl",
        "RadioButtonControl",
        "ScrollBarControl",
        "SliderControl",
        "SpinnerControl",
        "StatusBarControl",
        "TabControl",
        "TabItemControl",
        "TextControl",
        "ToolBarControl",
        "ToolTipControl",
        "TreeControl",
        "TreeItemControl",
        "CustomControl",
        "GroupControl",
        "ThumbControl",
        "DataGridControl",
        "DataItemControl",
        "DocumentControl",
        "SplitButtonControl",
        "WindowControl",
        "PaneControl",
        "HeaderControl",
        "HeaderItemControl",
        "TableControl",
        "TitleBarControl",
        "SeparatorControl",
        "SemanticZoomControl",
        "AppBarControl",
    ];
    let index = usize::try_from(id.checked_sub(50000)?).ok()?;
    NAMES.get(index).copied()
}

/// The role for a ControlType id; ids outside the table keep their number.
pub fn role_for(id: i32) -> super::node::Role {
    match control_type_name(id) {
        Some(name) => super::node::Role::from_uia(name),
        None => super::node::Role::Other(format!("ControlType{id}")),
    }
}
