//! Windows that host web content (Chrome, Edge, Firefox, Safari...).

use super::node::WindowInfo;
use super::selectors::BrowserSel;

/// A window of a browser: the class and the process both say so.
pub fn is_browser(window: &WindowInfo, sel: &BrowserSel) -> bool {
    sel.classes.contains(&window.class_name)
        && window
            .process
            .as_deref()
            .is_some_and(|p| sel.processes.iter().any(|b| b.eq_ignore_ascii_case(p)))
}

/// The page title without the browser's own ` - Google Chrome` tail.
pub fn page_title<'a>(title: &'a str, sel: &BrowserSel) -> &'a str {
    let lower = title.to_lowercase();
    for name in &sel.names {
        let name = name.to_lowercase();
        for dash in [" - ", " \u{2013} ", " \u{2014} "] {
            let tail = format!("{dash}{name}");
            // Lowercasing can change byte lengths, so the cut is taken from the end of the original.
            if lower.ends_with(&tail)
                && let Some(cut) = title.len().checked_sub(tail.len())
                && title.is_char_boundary(cut)
            {
                return &title[..cut];
            }
        }
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_browser_needs_its_process_not_only_the_electron_class() {
        let sel = BrowserSel::default();
        let window = |class: &str, process: Option<&str>| WindowInfo {
            handle: 1,
            title: String::new(),
            class_name: class.to_string(),
            pid: 1,
            process: process.map(str::to_string),
        };
        assert!(is_browser(
            &window("Chrome_WidgetWin_1", Some("Chrome.exe")),
            &sel
        ));
        assert!(is_browser(&window("BrowserWindow", Some("Safari")), &sel));
        assert!(!is_browser(
            &window("Chrome_WidgetWin_1", Some("slack.exe")),
            &sel
        ));
        assert!(!is_browser(&window("Chrome_WidgetWin_1", None), &sel));
        assert!(!is_browser(&window("Notepad", Some("chrome.exe")), &sel));
    }

    #[test]
    fn strips_the_browser_name_only_at_the_end() {
        let sel = BrowserSel::default();
        assert_eq!(page_title("Meet - abc - Google Chrome", &sel), "Meet - abc");
        assert_eq!(page_title("Meet - abc \u{2014} Safari", &sel), "Meet - abc");
        assert_eq!(page_title("Google Chrome tips", &sel), "Google Chrome tips");
        assert_eq!(page_title("Meet - abc", &sel), "Meet - abc");
    }
}
