//! `UiBackend` over the macOS Accessibility API (`AXUIElement`).
//!
//! Compile-checked from Windows (`--target aarch64-apple-darwin`); behaviour is verified by hand on a Mac
//! (README, M-mac). Whatever can be decided without a Mac lives in `ui::ax_map` and `ui::ax_tree`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use super::element::{
    Attrs, ElementRef, Request, app_element, element_attr, read_attrs, set_global_timeout,
    set_manual_accessibility, string_attr,
};
use super::is_trusted;
use crate::ui::ax_map::{
    AX_ERROR_API_DISABLED, AX_ERROR_INVALID_ELEMENT, AppKind, PERMISSION_HELP, automation_id_for,
    classify_exe, map_ax_error, name_and_value, retry_delay, role_for, wants_manual, window_class,
};
use crate::ui::ax_tree::{AxSource, Read, Walk, read_tree};
use crate::ui::backend::{Budget, Target, UiBackend, UiError};
use crate::ui::mirror::Mirror;
use crate::ui::node::{UiNode, WindowInfo};
use crate::ui::selectors::Selectors;
use crate::ui::source::Platform;

/// Timeouts and permission behaviour.
#[derive(Debug, Clone)]
pub struct MacOptions {
    /// Bounds one call into a hung app (a deadline cannot interrupt a single call).
    pub messaging_ms: u32,
    /// Ask macOS to show the Accessibility prompt when not trusted.
    pub prompt: bool,
    /// The platform being captured: decides which apps get `AXManualAccessibility`; `None` (doctor) is any.
    pub platform: Option<Platform>,
    /// The selectors that decide which windows are a platform's, hence which apps need the switch.
    pub selectors: Selectors,
}

impl Default for MacOptions {
    fn default() -> Self {
        Self {
            // Short: one call into a hung app must not delay a stop.
            messaging_ms: 1500,
            prompt: false,
            platform: None,
            selectors: Selectors::default(),
        }
    }
}

/// A listed window; AX has no window handle, so the handle is ours, kept across listings by element equality.
struct Known {
    info: WindowInfo,
    element: ElementRef,
}

enum Manual {
    On,
    /// A failed attempt: try again after `next`.
    Retry {
        attempts: u32,
        next: Instant,
    },
}

pub struct MacAxBackend {
    options: MacOptions,
    request: Request,
    system: System,
    known: Vec<Known>,
    next_handle: u64,
    manual: HashMap<u32, Manual>,
    /// Live elements of the last whole-window snapshot, by window handle.
    mirror: Option<(u64, Mirror<ElementRef>)>,
    truncated: bool,
}

fn exe_refresh() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet)
}

fn app_running(system: &mut System, pid: u32) -> bool {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
        exe_refresh(),
    ) > 0
}

impl MacAxBackend {
    /// `Init` with the steps to grant the permission when the process is not a trusted accessibility client.
    pub fn new(options: MacOptions) -> Result<Self, UiError> {
        if !is_trusted(options.prompt) {
            return Err(UiError::Init(PERMISSION_HELP.to_string()));
        }
        set_global_timeout(options.messaging_ms as f32 / 1000.0);
        Ok(Self {
            options,
            request: Request::new(),
            system: System::new(),
            known: Vec::new(),
            next_handle: 1,
            manual: HashMap::new(),
            mirror: None,
            truncated: false,
        })
    }

    fn timeout_s(&self) -> f32 {
        self.options.messaging_ms as f32 / 1000.0
    }

    /// Meeting apps that run now: `(pid, kind, bundle name)`, by pid.
    fn running_apps(&mut self) -> Vec<(u32, AppKind, String)> {
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, exe_refresh());
        let mut apps: Vec<_> = self
            .system
            .processes()
            .iter()
            .filter_map(|(pid, process)| {
                let (kind, bundle) = classify_exe(process.exe()?.to_str()?)?;
                Some((pid.as_u32(), kind, bundle.to_string()))
            })
            .collect();
        apps.sort_by_key(|app| app.0);
        apps
    }

    /// Switches the app's web tree on; the pid is only remembered once that worked, failures retry with backoff.
    fn ensure_manual(&mut self, pid: u32, app: &ElementRef) {
        let attempts = match self.manual.get(&pid) {
            Some(Manual::On) => return,
            Some(Manual::Retry { next, .. }) if Instant::now() < *next => return,
            Some(Manual::Retry { attempts, .. }) => *attempts,
            None => 0,
        };
        let state = match set_manual_accessibility(app, true) {
            Ok(()) => Manual::On,
            Err(_) => Manual::Retry {
                attempts: attempts + 1,
                next: Instant::now() + retry_delay(attempts),
            },
        };
        self.manual.insert(pid, state);
    }

    /// One app's windows as `(element, title)`; an app we cannot read this time keeps its previous windows.
    fn list_app(
        &mut self,
        pid: u32,
        kind: AppKind,
        bundle: &str,
        previous: &[Known],
        found: &mut Vec<Known>,
        deadline: Instant,
    ) -> Result<(), UiError> {
        let keep_previous = |found: &mut Vec<Known>| {
            found.extend(
                previous
                    .iter()
                    .filter(|k| k.info.pid == pid)
                    .map(|k| Known {
                        info: k.info.clone(),
                        element: k.element.clone(),
                    }),
            );
        };
        if Instant::now() > deadline {
            keep_previous(found);
            return Ok(());
        }
        let app = app_element(i32::try_from(pid).unwrap_or(0), self.timeout_s());
        let elements = match element_attr(&app, "AXWindows") {
            Ok(elements) => elements,
            Err(AX_ERROR_API_DISABLED) => return Err(UiError::Init(PERMISSION_HELP.to_string())),
            Err(_) => {
                keep_previous(found);
                return Ok(());
            }
        };
        let titled: Vec<_> = elements
            .into_iter()
            .map(|e| {
                let title = string_attr(&e, "AXTitle").unwrap_or_default();
                (e, title)
            })
            .collect();
        // Chromium builds its web tree only for a client that asks, and only the relevant apps are asked.
        if kind.needs_manual_accessibility() {
            let windows: Vec<WindowInfo> = titled
                .iter()
                .map(|(_, title)| WindowInfo {
                    handle: 0,
                    title: title.clone(),
                    class_name: window_class(kind, title).to_string(),
                    pid,
                    process: Some(bundle.to_string()),
                })
                .collect();
            if wants_manual(self.options.platform, &windows, &self.options.selectors) {
                self.ensure_manual(pid, &app);
            }
        }
        for (element, title) in titled {
            let handle = match previous
                .iter()
                .find(|k| k.info.pid == pid && k.element == element)
            {
                Some(known) => known.info.handle,
                None => {
                    self.next_handle += 1;
                    self.next_handle - 1
                }
            };
            found.push(Known {
                info: WindowInfo {
                    handle,
                    class_name: window_class(kind, &title).to_string(),
                    title,
                    pid,
                    process: Some(bundle.to_string()),
                },
                element,
            });
        }
        Ok(())
    }
}

impl Drop for MacAxBackend {
    fn drop(&mut self) {
        // Hand the apps back as we found them: web accessibility costs them CPU.
        for (pid, state) in &self.manual {
            if matches!(state, Manual::On) {
                let app = app_element(i32::try_from(*pid).unwrap_or(0), 1.0);
                let _ = set_manual_accessibility(&app, false);
            }
        }
    }
}

/// Reads elements of one app, mapping AX errors with that app's liveness.
struct Reader<'a> {
    system: &'a mut System,
    request: &'a Request,
    pid: u32,
    timeout_s: f32,
}

impl AxSource for Reader<'_> {
    type Elem = ElementRef;

    fn read(&mut self, element: &ElementRef) -> Result<Read<ElementRef>, UiError> {
        let attrs = read_attrs(element, self.request, self.timeout_s)
            .map_err(|code| map_ax_error(code, app_running(self.system, self.pid)))?;
        let Attrs {
            role,
            subrole,
            text,
            identifier,
            dom_id,
            children,
        } = attrs;
        let kind = role_for(&role, &subrole);
        let (name, value) = name_and_value(&kind, &text);
        let mut node = UiNode::new(kind, &name);
        node.automation_id = automation_id_for(&role, &identifier, &dom_id);
        node.value = value;
        Ok(Read { node, children })
    }
}

impl UiBackend for MacAxBackend {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, UiError> {
        let apps = self.running_apps();
        self.manual
            .retain(|pid, _| apps.iter().any(|a| a.0 == *pid));
        let previous = std::mem::take(&mut self.known);
        let mut found = Vec::new();
        // A listing is bounded: past two messaging timeouts the remaining apps keep their old windows.
        let deadline =
            Instant::now() + Duration::from_millis(u64::from(self.options.messaging_ms) * 2);
        for (pid, kind, bundle) in &apps {
            self.list_app(*pid, *kind, bundle, &previous, &mut found, deadline)?;
        }
        self.known = found;
        Ok(self.known.iter().map(|k| k.info.clone()).collect())
    }

    fn snapshot(&mut self, target: &Target, budget: &Budget) -> Result<UiNode, UiError> {
        if budget.expired() {
            return Err(UiError::Budget);
        }
        if !self.alive(target.window) {
            self.mirror = None;
            return Err(UiError::WindowGone);
        }
        let Some(known) = self.known.iter().find(|k| k.info.handle == target.window) else {
            return Err(UiError::WindowGone);
        };
        let (pid, window) = (known.info.pid, known.element.clone());
        let root = if target.path.is_empty() {
            window
        } else {
            match &self.mirror {
                Some((handle, mirror)) if *handle == target.window => {
                    mirror.at(&target.path).ok_or(UiError::Stale)?.elem.clone()
                }
                _ => return Err(UiError::Stale),
            }
        };
        let mut reader = Reader {
            system: &mut self.system,
            request: &self.request,
            pid,
            timeout_s: self.options.messaging_ms as f32 / 1000.0,
        };
        let Walk {
            tree,
            mirror,
            truncated,
        } = match read_tree(&mut reader, &root, budget) {
            Ok(walk) => walk,
            // The window itself no longer exists.
            Err(UiError::Stale) if target.path.is_empty() => {
                self.mirror = None;
                return Err(UiError::WindowGone);
            }
            Err(e) => return Err(e),
        };
        self.truncated = truncated;
        if let Some(expect) = &target.expect
            && !expect.matches(&tree)
        {
            return Err(UiError::Stale);
        }
        if target.path.is_empty() {
            self.mirror = Some((target.window, mirror));
        }
        Ok(tree)
    }

    fn alive(&mut self, window: u64) -> bool {
        let Some(known) = self.known.iter().find(|k| k.info.handle == window) else {
            return false;
        };
        let (pid, element) = (known.info.pid, known.element.clone());
        app_running(&mut self.system, pid)
            && !matches!(
                string_attr(&element, "AXRole"),
                Err(AX_ERROR_INVALID_ELEMENT)
            )
    }

    fn truncated(&self) -> bool {
        self.truncated
    }
}
