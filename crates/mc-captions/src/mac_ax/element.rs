//! Thin, safe reads of `AXUIElement` attributes; every unsafe call sits here.

use std::ptr::NonNull;

use objc2_application_services::{
    AXCopyMultipleAttributeOptions, AXError, AXUIElement, AXValue, AXValueType,
};
use objc2_core_foundation::{CFArray, CFBoolean, CFRetained, CFString, CFType};

use crate::ui::ax_map::{AxText, is_absent};

pub type ElementRef = CFRetained<AXUIElement>;

/// An `AXError` code that is not `Success`.
pub type Code = i32;

fn name(attribute: &'static str) -> CFRetained<CFString> {
    CFString::from_static_str(attribute)
}

fn check(error: AXError) -> Result<(), Code> {
    if error == AXError::Success {
        Ok(())
    } else {
        Err(error.0)
    }
}

/// `AXUIElementSetMessagingTimeout` binds one element only, so it is set on each one before reading it.
pub fn set_timeout(element: &AXUIElement, seconds: f32) {
    // SAFETY: plain call on an element we hold; a failure keeps the default timeout.
    let _ = unsafe { element.set_messaging_timeout(seconds) };
}

pub fn app_element(pid: i32, timeout_s: f32) -> ElementRef {
    // SAFETY: creating an application element is valid for any pid.
    let app = unsafe { AXUIElement::new_application(pid) };
    set_timeout(&app, timeout_s);
    app
}

/// The system-wide element's timeout is the global default for every app.
pub fn set_global_timeout(seconds: f32) {
    // SAFETY: creating the system-wide element has no preconditions.
    let system = unsafe { AXUIElement::new_system_wide() };
    set_timeout(&system, seconds);
}

fn copy_value(
    element: &AXUIElement,
    attribute: &'static str,
) -> Result<Option<CFRetained<CFType>>, Code> {
    let mut out: *const CFType = std::ptr::null();
    // SAFETY: `out` is valid for one pointer write; success hands back a +1 object (Copy rule).
    let (error, value) = unsafe {
        let error = element.copy_attribute_value(&name(attribute), NonNull::from(&mut out));
        (
            error,
            NonNull::new(out.cast_mut()).map(|p| CFRetained::from_raw(p)),
        )
    };
    match check(error) {
        Ok(()) => Ok(value),
        Err(code) if is_absent(code) => Ok(None),
        Err(code) => Err(code),
    }
}

fn string_of(value: &CFType) -> String {
    value
        .downcast_ref::<CFString>()
        .map(ToString::to_string)
        .unwrap_or_default()
}

fn elements_of(value: CFRetained<CFType>) -> Vec<ElementRef> {
    let Ok(array) = value.downcast::<CFArray>() else {
        return Vec::new();
    };
    // SAFETY: AX defines `AXWindows` and `AXChildren` as arrays of `AXUIElement`.
    let typed = unsafe { CFRetained::cast_unchecked::<CFArray<AXUIElement>>(array) };
    typed.to_vec()
}

pub fn string_attr(element: &AXUIElement, attribute: &'static str) -> Result<String, Code> {
    Ok(copy_value(element, attribute)?
        .map(|v| string_of(&v))
        .unwrap_or_default())
}

pub fn element_attr(
    element: &AXUIElement,
    attribute: &'static str,
) -> Result<Vec<ElementRef>, Code> {
    Ok(copy_value(element, attribute)?
        .map(elements_of)
        .unwrap_or_default())
}

/// Switches the web accessibility tree of a Chromium/Electron app on or off.
pub fn set_manual_accessibility(app: &AXUIElement, on: bool) -> Result<(), Code> {
    // SAFETY: plain attribute write of a CFBoolean on an element we hold.
    check(unsafe { app.set_attribute_value(&name("AXManualAccessibility"), CFBoolean::new(on)) })
}

/// Everything the tree needs from one element.
pub struct Attrs {
    pub role: String,
    pub subrole: String,
    pub text: AxText,
    pub identifier: String,
    pub dom_id: String,
    pub children: Vec<ElementRef>,
}

const ATTRS: [&str; 8] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXDescription",
    "AXValue",
    "AXIdentifier",
    "AXDOMIdentifier",
    "AXChildren",
];

/// The attribute names of `read_attrs`, built once and reused for every node.
pub struct Request(CFRetained<CFArray<CFString>>);

impl Request {
    pub fn new() -> Self {
        let names: Vec<_> = ATTRS.iter().map(|a| CFString::from_str(a)).collect();
        Self(CFArray::from_retained_objects(&names))
    }
}

/// A value, or `Err` when AX stored a real error placeholder for it (an absent attribute reads as `None`).
fn slot(values: &CFArray<CFType>, index: usize) -> Result<Option<CFRetained<CFType>>, Code> {
    let Some(value) = values.get(index) else {
        return Ok(None);
    };
    let Some(placeholder) = value.downcast_ref::<AXValue>() else {
        return Ok(Some(value));
    };
    let mut code: i32 = 0;
    // SAFETY: `code` is valid for the one `AXError` an error placeholder holds.
    let is_error = unsafe {
        placeholder.r#type() == AXValueType::AXError
            && placeholder.value(AXValueType::AXError, NonNull::from(&mut code).cast())
    };
    match is_error {
        true if is_absent(code) => Ok(None),
        true => Err(code),
        false => Ok(Some(value)),
    }
}

/// One cross-process call per element; an attribute that failed for a real reason fails the read.
pub fn read_attrs(element: &AXUIElement, request: &Request, timeout_s: f32) -> Result<Attrs, Code> {
    set_timeout(element, timeout_s);
    let mut out: *const CFArray = std::ptr::null();
    // SAFETY: `out` is valid for one pointer write; success hands back a +1 array in request order.
    let (error, values) = unsafe {
        let error = element.copy_multiple_attribute_values(
            request.0.as_opaque(),
            AXCopyMultipleAttributeOptions::empty(),
            NonNull::from(&mut out),
        );
        (
            error,
            NonNull::new(out.cast_mut()).map(|p| CFRetained::from_raw(p)),
        )
    };
    check(error)?;
    let values = values.ok_or(AXError::Failure.0)?;
    // SAFETY: the returned array holds `CFType` values.
    let values = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(values) };
    let string = |index| {
        Ok::<_, Code>(
            slot(&values, index)?
                .map(|v| string_of(&v))
                .unwrap_or_default(),
        )
    };
    Ok(Attrs {
        role: string(0)?,
        subrole: string(1)?,
        text: AxText {
            title: string(2)?,
            description: string(3)?,
            value: string(4)?,
        },
        identifier: string(5)?,
        dom_id: string(6)?,
        children: slot(&values, 7)?.map(elements_of).unwrap_or_default(),
    })
}
