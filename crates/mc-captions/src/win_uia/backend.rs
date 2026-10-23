//! `UiBackend` over Windows UI Automation (raw view, cached snapshots).
//!
//! Written and compile-checked on Linux (`--target x86_64-pc-windows-gnu`);
//! behaviour is verified by hand on Windows (plan, M1-M6). Every unsafe call
//! sits in a small helper with its safety argument.

use std::collections::HashMap;

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{
    AutomationElementMode_Full, CUIAutomation8, IUIAutomation, IUIAutomation2,
    IUIAutomationCacheRequest, IUIAutomationElement, IUIAutomationTreeWalker, TreeScope_Element,
    TreeScope_Subtree, UIA_AutomationIdPropertyId, UIA_ClassNamePropertyId,
    UIA_ControlTypePropertyId, UIA_NamePropertyId, UIA_NativeWindowHandlePropertyId,
    UIA_ProcessIdPropertyId,
};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::core::{BSTR, PWSTR};

use super::{ComGuard, init_com};
use crate::ui::backend::{Budget, Target, UiBackend, UiError};
use crate::ui::mirror::Mirror;
use crate::ui::node::{UiNode, WindowInfo};
use crate::ui::uia_map::{map_hresult, role_for};

/// Timeouts that bound a call into a hung application (a deadline cannot
/// interrupt a single COM call).
#[derive(Debug, Clone, Copy)]
pub struct WinOptions {
    pub connection_ms: u32,
    pub transaction_ms: u32,
}

impl Default for WinOptions {
    fn default() -> Self {
        Self {
            // Short: one call into a hung app must not delay a stop (M5).
            connection_ms: 1500,
            transaction_ms: 1500,
        }
    }
}

pub struct WinUiaBackend {
    auto: IUIAutomation,
    walker: IUIAutomationTreeWalker,
    cr_win: IUIAutomationCacheRequest,
    cr_tree: IUIAutomationCacheRequest,
    /// Live elements of the last whole-window snapshot, by window handle.
    mirror: Option<(u64, Mirror<IUIAutomationElement>)>,
    processes: HashMap<u32, Option<String>>,
    // Last: COM must outlive every interface above (fields drop in order).
    _com: Option<ComGuard>,
}

fn hr(error: &windows::core::Error, window_alive: bool) -> UiError {
    map_hresult(error.code().0 as u32, window_alive)
}

fn text(value: windows::core::Result<BSTR>) -> String {
    // Lossy: a string cut in the middle of an emoji holds a lone surrogate.
    value
        .map(|b| String::from_utf16_lossy(&b))
        .unwrap_or_default()
}

fn hwnd(handle: u64) -> HWND {
    HWND(handle as usize as *mut core::ffi::c_void)
}

fn window_alive(handle: u64) -> bool {
    // SAFETY: `IsWindow` accepts any value and only reads window-manager state.
    handle != 0 && unsafe { IsWindow(Some(hwnd(handle))) }.as_bool()
}

fn process_image(pid: u32) -> Option<String> {
    // SAFETY: the handle is opened with query-only rights, used for one call
    // into a buffer we own, and closed before returning.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        path.rsplit(['\\', '/']).next().map(str::to_string)
    }
}

fn cache_request(
    auto: &IUIAutomation,
    scope: windows::Win32::UI::Accessibility::TreeScope,
    properties: &[windows::Win32::UI::Accessibility::UIA_PROPERTY_ID],
) -> Result<IUIAutomationCacheRequest, UiError> {
    // SAFETY: plain COM calls on interfaces we own.
    unsafe {
        let request = auto
            .CreateCacheRequest()
            .map_err(|e| UiError::Init(e.to_string()))?;
        for property in properties {
            request
                .AddProperty(*property)
                .map_err(|e| UiError::Init(e.to_string()))?;
        }
        request
            .SetTreeScope(scope)
            .and_then(|()| request.SetAutomationElementMode(AutomationElementMode_Full))
            .map_err(|e| UiError::Init(e.to_string()))?;
        Ok(request)
    }
}

impl WinUiaBackend {
    /// Must be called on the thread that will use it (it joins that thread's
    /// COM apartment); the type is `!Send`.
    pub fn new(options: WinOptions) -> Result<Self, UiError> {
        // A thread already in a single-threaded apartment still can use UI
        // Automation; there is just nothing of ours to balance.
        const RPC_E_CHANGED_MODE: u32 = 0x8001_0106;
        let com = match init_com() {
            Ok(guard) => Some(guard),
            Err(e) if e.code().0 as u32 == RPC_E_CHANGED_MODE => None,
            Err(e) => return Err(UiError::Init(format!("COM: {e}"))),
        };
        // SAFETY: COM calls on a thread that just initialised COM.
        unsafe {
            let auto: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| UiError::Init(format!("UI Automation unavailable: {e}")))?;
            if let Ok(auto2) = windows::core::Interface::cast::<IUIAutomation2>(&auto) {
                let _ = auto2.SetConnectionTimeout(options.connection_ms);
                let _ = auto2.SetTransactionTimeout(options.transaction_ms);
            }
            // The raw view, like the Python `uiautomation` package: with the
            // default control view, depths and child indexes would differ.
            let walker = auto
                .RawViewWalker()
                .map_err(|e| UiError::Init(e.to_string()))?;
            let cr_win = cache_request(
                &auto,
                TreeScope_Element,
                &[
                    UIA_NamePropertyId,
                    UIA_ClassNamePropertyId,
                    UIA_ProcessIdPropertyId,
                    UIA_NativeWindowHandlePropertyId,
                ],
            )?;
            let cr_tree = cache_request(
                &auto,
                TreeScope_Subtree,
                &[
                    UIA_NamePropertyId,
                    UIA_ControlTypePropertyId,
                    UIA_AutomationIdPropertyId,
                    UIA_ClassNamePropertyId,
                ],
            )?;
            let raw = auto
                .RawViewCondition()
                .map_err(|e| UiError::Init(e.to_string()))?;
            cr_tree
                .SetTreeFilter(&raw)
                .map_err(|e| UiError::Init(e.to_string()))?;
            Ok(Self {
                auto,
                walker,
                cr_win,
                cr_tree,
                mirror: None,
                processes: HashMap::new(),
                _com: com,
            })
        }
    }

    fn info_of(&mut self, element: &IUIAutomationElement) -> Option<WindowInfo> {
        // SAFETY: reading cached properties of an element we hold.
        unsafe {
            // Window handles are 32 bits wide whatever the pointer size.
            let handle = element.CachedNativeWindowHandle().ok()?.0 as usize as u64 & 0xFFFF_FFFF;
            if handle == 0 {
                return None;
            }
            let pid = u32::try_from(element.CachedProcessId().unwrap_or(0)).unwrap_or(0);
            let process = self
                .processes
                .entry(pid)
                .or_insert_with(|| process_image(pid))
                .clone();
            Some(WindowInfo {
                handle,
                title: text(element.CachedName()),
                class_name: text(element.CachedClassName()),
                pid,
                process,
            })
        }
    }
}

struct Convert<'a> {
    budget: &'a Budget,
    count: usize,
}

/// Cached subtree to `UiNode` (and the mirror of live elements), clipped to the
/// budget's depth and node count.
fn convert(
    element: &IUIAutomationElement,
    depth: usize,
    state: &mut Convert<'_>,
    alive: bool,
) -> Result<(UiNode, Mirror<IUIAutomationElement>), UiError> {
    // The subtree is already cached (one cross-process call), so converting it
    // is cheap: an oversized tree is truncated, not thrown away.
    state.count += 1;
    // SAFETY: reading cached properties of elements returned by UIA.
    let (control_type, name, aid, class) = unsafe {
        (
            element.CachedControlType().map(|t| t.0).unwrap_or(0),
            text(element.CachedName()),
            text(element.CachedAutomationId()),
            text(element.CachedClassName()),
        )
    };
    let mut node = UiNode {
        role: role_for(control_type),
        name,
        automation_id: aid,
        class_name: class,
        value: None,
        runtime_id: None,
        children: Vec::new(),
    };
    let mut mirror = Mirror::leaf(element.clone());
    if depth < state.budget.max_depth {
        // SAFETY: the cache request covers the subtree, so children are cached.
        let children = unsafe { element.GetCachedChildren() };
        let array = match children {
            Ok(array) => Some(array),
            // An empty error means "no children cached" (a leaf).
            Err(e) if e.code().0 == 0 => None,
            Err(e) => return Err(hr(&e, alive)),
        };
        if let Some(array) = array {
            // SAFETY: indexes come from the array's own length.
            let length = unsafe { array.Length() }.unwrap_or(0);
            for index in 0..length {
                if state.count >= state.budget.max_nodes {
                    break;
                }
                // SAFETY: `index` is below the array's length.
                let child = unsafe { array.GetElement(index) }.map_err(|e| hr(&e, alive))?;
                let (child_node, child_mirror) = convert(&child, depth + 1, state, alive)?;
                node.children.push(child_node);
                mirror.children.push(child_mirror);
            }
        }
    }
    Ok((node, mirror))
}

impl UiBackend for WinUiaBackend {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, UiError> {
        // A pid can be reused: names are only trusted within one listing.
        self.processes.clear();
        // SAFETY: COM calls on interfaces owned by `self`, on its thread.
        unsafe {
            let root = self.auto.GetRootElement().map_err(|e| hr(&e, true))?;
            let mut windows = Vec::new();
            let mut next = self
                .walker
                .GetFirstChildElementBuildCache(&root, &self.cr_win);
            // The walker signals "no more" with an empty error (code 0); any
            // other failure is real and must not look like the end of the list.
            loop {
                let element = match next {
                    Ok(element) => element,
                    Err(e) if e.code().0 == 0 => break,
                    Err(e) => return Err(hr(&e, true)),
                };
                if let Some(info) = self.info_of(&element) {
                    windows.push(info);
                }
                next = self
                    .walker
                    .GetNextSiblingElementBuildCache(&element, &self.cr_win);
            }
            Ok(windows)
        }
    }

    fn snapshot(&mut self, target: &Target, budget: &Budget) -> Result<UiNode, UiError> {
        // The deadline can only be honoured before the cross-process call.
        if budget.expired() {
            return Err(UiError::Budget);
        }
        if !window_alive(target.window) {
            self.mirror = None;
            return Err(UiError::WindowGone);
        }
        // SAFETY: COM calls on interfaces owned by `self`, on its thread.
        let root = unsafe {
            if target.path.is_empty() {
                self.auto
                    .ElementFromHandleBuildCache(hwnd(target.window), &self.cr_tree)
                    .map_err(|e| hr(&e, window_alive(target.window)))?
            } else {
                let mirror = match &self.mirror {
                    Some((handle, mirror)) if *handle == target.window => mirror,
                    _ => return Err(UiError::Stale),
                };
                let node = mirror.at(&target.path).ok_or(UiError::Stale)?;
                node.elem
                    .BuildUpdatedCache(&self.cr_tree)
                    .map_err(|e| hr(&e, window_alive(target.window)))?
            }
        };
        let mut state = Convert { budget, count: 0 };
        let alive = window_alive(target.window);
        let (tree, mirror) = convert(&root, 0, &mut state, alive)?;
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
        window_alive(window)
    }
}
