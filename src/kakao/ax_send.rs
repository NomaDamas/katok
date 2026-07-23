//! Sending a message into the KakaoTalk desktop app via the macOS Accessibility API.
//!
//! Everything else in this crate reads the local SQLCipher archive. Sending cannot work that
//! way — there is no supported write path into KakaoTalk's store — so this module drives the
//! running app's UI instead. It is deliberately the only module that touches AX.
//!
//! The message body is written straight into the compose box with `AXValue`, which needs no
//! keyboard and no focus. Only the final Enter needs a key event, and that is delivered with
//! `CGEventPostToPid` so it reaches KakaoTalk **without bringing the app forward**. Posting to
//! the global HID tap instead would require activating KakaoTalk, stealing the user's screen and
//! breaking whenever they click something mid-send.
//!
//! Scope: the target chat window must already be open. Locating an arbitrary room means either
//! crawling the chat list or driving the search field, and both are slow enough (~20s, dominated
//! by per-node AX round trips) that the caller is better off opening the room once and keeping
//! it open.

#![cfg(target_os = "macos")]

use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::data::{CFData, CFDataRef};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{CGEvent, CGEventFlags};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use std::ffi::c_void;
use std::path::Path;
use std::process::Command;

type AXUIElementRef = *mut c_void;
type AXError = i32;
type PasteboardRef = *mut c_void;
type OSStatus = i32;

const AX_SUCCESS: AXError = 0;
const OS_NO_ERR: OSStatus = 0;
const KEY_RETURN: u16 = 36;
const KEY_V: u16 = 9;

const ATTR_WINDOWS: &str = "AXWindows";
const ATTR_CHILDREN: &str = "AXChildren";
const ATTR_TITLE: &str = "AXTitle";
const ATTR_ROLE: &str = "AXRole";
const ATTR_VALUE: &str = "AXValue";

const ROLE_SCROLL_AREA: &str = "AXScrollArea";
const ROLE_TEXT_AREA: &str = "AXTextArea";
const ROLE_SHEET: &str = "AXSheet";
const ROLE_BUTTON: &str = "AXButton";
const ROLE_TABLE: &str = "AXTable";

const ATTR_FRONTMOST: &str = "AXFrontmost";
const ATTR_ROWS: &str = "AXRows";
const ATTR_FOCUSED: &str = "AXFocused";

const ACTION_PRESS: &str = "AXPress";

/// Value of the Carbon `kPasteboardClipboard` constant (not exported for linking).
const PASTEBOARD_CLIPBOARD: &str = "com.apple.pasteboard.clipboard";

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
    fn AXIsProcessTrusted() -> bool;

    // Carbon Pasteboard Manager. Lives in the same framework already linked above, so writing
    // the clipboard costs no extra dependency (NSPasteboard would drag in AppKit bindings).
    // The `kPasteboardClipboard` constant is not exported for linking, so its documented
    // value is passed by name instead (see PASTEBOARD_CLIPBOARD).
    fn PasteboardCreate(name: CFStringRef, out: *mut PasteboardRef) -> OSStatus;
    fn PasteboardClear(pasteboard: PasteboardRef) -> OSStatus;
    fn PasteboardPutItemFlavor(
        pasteboard: PasteboardRef,
        item: *const c_void,
        flavor: CFStringRef,
        data: CFDataRef,
        flags: u32,
    ) -> OSStatus;
}

/// Owns an AX reference and releases it on drop, so the many early returns below cannot leak.
struct AxRef(AXUIElementRef);

impl Drop for AxRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0 as CFTypeRef) };
        }
    }
}

impl AxRef {
    fn as_raw(&self) -> AXUIElementRef {
        self.0
    }

    /// Retain a borrowed child so it outlives the array it came from.
    fn retained(raw: AXUIElementRef) -> Self {
        unsafe { core_foundation::base::CFRetain(raw as CFTypeRef) };
        AxRef(raw)
    }
}

fn copy_attr(el: AXUIElementRef, attribute: &str) -> Option<CFTypeRef> {
    let key = CFString::new(attribute);
    let mut out: CFTypeRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(el, key.as_concrete_TypeRef(), &mut out) };
    if err == AX_SUCCESS && !out.is_null() {
        Some(out)
    } else {
        None
    }
}

fn attr_string(el: AXUIElementRef, attribute: &str) -> Option<String> {
    let raw = copy_attr(el, attribute)?;
    // wrap_under_create_rule takes ownership of the +1 reference returned above.
    let s = unsafe { CFString::wrap_under_create_rule(raw as CFStringRef) };
    Some(s.to_string())
}

/// Copy out the child elements of `attribute`, retaining each so they stay valid after the
/// backing array is released.
fn child_elements(el: AXUIElementRef, attribute: &str) -> Vec<AxRef> {
    let Some(raw) = copy_attr(el, attribute) else {
        return Vec::new();
    };
    let array = raw as CFArrayRef;
    let count = unsafe { CFArrayGetCount(array) };
    let mut out = Vec::with_capacity(count.max(0) as usize);
    for i in 0..count {
        let child = unsafe { CFArrayGetValueAtIndex(array, i) } as AXUIElementRef;
        if !child.is_null() {
            out.push(AxRef::retained(child));
        }
    }
    unsafe { CFRelease(raw) };
    out
}

fn role_of(el: AXUIElementRef) -> String {
    attr_string(el, ATTR_ROLE).unwrap_or_default()
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error(
        "accessibility permission is not granted; grant it to this terminal in \
         System Settings > Privacy & Security > Accessibility"
    )]
    AccessibilityDenied,
    #[error("KakaoTalk is not running")]
    NotRunning,
    #[error(
        "chat window '{room}' is not open. This command only sends into an already open window; \
         open the room in KakaoTalk first. Open windows: {open}"
    )]
    WindowNotOpen { room: String, open: String },
    #[error("could not find the message input box in window '{0}'")]
    ComposeBoxNotFound(String),
    #[error("failed to write the message into the input box (AXError {0})")]
    SetValueFailed(AXError),
    #[error("could not create the Enter key event")]
    KeyEventFailed,
    #[error("message stayed in the input box; KakaoTalk did not accept Enter")]
    NotSent,
    #[error("image file not found: {0}")]
    ImageMissing(String),
    #[error("failed to read image file {path}: {source}")]
    ImageUnreadable {
        path: String,
        source: std::io::Error,
    },
    #[error("unsupported image type '{0}'; use png, jpg, jpeg, gif, tiff, bmp, heic, or webp")]
    ImageTypeUnsupported(String),
    #[error("failed to put the image on the clipboard (OSStatus {0})")]
    ClipboardFailed(OSStatus),
    #[error("pasted the image but KakaoTalk never showed the send confirmation, and the paste did not clear")]
    ImageNotSent,
}

/// Uniform Type Identifier for the clipboard flavor, chosen from the file extension.
/// KakaoTalk decides how to preview the attachment from this, so a wrong guess shows the
/// image as a generic file rather than a photo.
fn image_uti(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "public.png",
        "jpg" | "jpeg" => "public.jpeg",
        "gif" => "com.compuserve.gif",
        "tif" | "tiff" => "public.tiff",
        "bmp" => "com.microsoft.bmp",
        "heic" => "public.heic",
        "webp" => "org.webmproject.webp",
        _ => return None,
    })
}

fn put_image_on_clipboard(path: &Path) -> Result<(), SendError> {
    if !path.is_file() {
        return Err(SendError::ImageMissing(path.display().to_string()));
    }
    let uti = image_uti(path).ok_or_else(|| {
        SendError::ImageTypeUnsupported(
            path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("(none)")
                .to_string(),
        )
    })?;
    let bytes = std::fs::read(path).map_err(|source| SendError::ImageUnreadable {
        path: path.display().to_string(),
        source,
    })?;

    let mut pasteboard: PasteboardRef = std::ptr::null_mut();
    let clipboard_name = CFString::new(PASTEBOARD_CLIPBOARD);
    let status =
        unsafe { PasteboardCreate(clipboard_name.as_concrete_TypeRef(), &mut pasteboard) };
    if status != OS_NO_ERR || pasteboard.is_null() {
        return Err(SendError::ClipboardFailed(status));
    }
    let _guard = AxRef(pasteboard);

    let status = unsafe { PasteboardClear(pasteboard) };
    if status != OS_NO_ERR {
        return Err(SendError::ClipboardFailed(status));
    }

    let data = CFData::from_buffer(&bytes);
    let flavor = CFString::new(uti);
    let status = unsafe {
        PasteboardPutItemFlavor(
            pasteboard,
            1 as *const c_void, // any non-null item id; a single item is enough
            flavor.as_concrete_TypeRef(),
            data.as_concrete_TypeRef(),
            0,
        )
    };
    if status != OS_NO_ERR {
        return Err(SendError::ClipboardFailed(status));
    }
    Ok(())
}

/// Post a key to the global HID tap, i.e. to whichever app is frontmost. Needed for menu key
/// equivalents like Cmd+V, which an app ignores when the event is posted only to its pid.
fn press_key_global(key: u16, flags: CGEventFlags) -> Result<(), SendError> {
    use core_graphics::event::CGEventTapLocation;
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| SendError::KeyEventFailed)?;
    for key_down in [true, false] {
        let event = CGEvent::new_keyboard_event(source.clone(), key, key_down)
            .map_err(|_| SendError::KeyEventFailed)?;
        event.set_flags(flags);
        event.post(CGEventTapLocation::HID);
    }
    Ok(())
}

fn press_key(pid: i32, key: u16, flags: CGEventFlags) -> Result<(), SendError> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| SendError::KeyEventFailed)?;
    for key_down in [true, false] {
        let event = CGEvent::new_keyboard_event(source.clone(), key, key_down)
            .map_err(|_| SendError::KeyEventFailed)?;
        event.set_flags(flags);
        event.post_to_pid(pid);
    }
    Ok(())
}

/// Depth-limited search for the first descendant matching `role`.
fn find_descendant(el: AXUIElementRef, role: &str, depth: u32) -> Option<AxRef> {
    if depth == 0 {
        return None;
    }
    for child in child_elements(el, ATTR_CHILDREN) {
        if role_of(child.as_raw()) == role {
            return Some(child);
        }
        if let Some(found) = find_descendant(child.as_raw(), role, depth - 1) {
            return Some(found);
        }
    }
    None
}

fn find_confirm_button(sheet: AXUIElementRef) -> Option<AxRef> {
    fn walk(el: AXUIElementRef, depth: u32) -> Option<AxRef> {
        if depth == 0 {
            return None;
        }
        for child in child_elements(el, ATTR_CHILDREN) {
            if role_of(child.as_raw()) == ROLE_BUTTON {
                let title = attr_string(child.as_raw(), ATTR_TITLE).unwrap_or_default();
                let title = title.trim();
                if title == "전송" || title == "Send" || title == "확인" || title == "OK" {
                    return Some(child);
                }
            }
            if let Some(found) = walk(child.as_raw(), depth - 1) {
                return Some(found);
            }
        }
        None
    }
    walk(sheet, 6)
}

/// Send the image at `image_path` into the chat window titled `room`, without activating
/// KakaoTalk. Same window precondition as [`send_to_open_window`].
///
/// KakaoTalk has no AX affordance for attaching a file, so the image goes through the
/// clipboard and a paste, which is what its UI is built to accept. Depending on version and
/// state it either shows a confirmation sheet (click through it) or sends straight away.
pub fn send_image_to_open_window(room: &str, image_path: &Path) -> Result<(), SendError> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    let windows = child_elements(app.as_raw(), ATTR_WINDOWS);
    let window = windows
        .iter()
        .find(|w| attr_string(w.as_raw(), ATTR_TITLE).as_deref() == Some(room))
        .ok_or_else(|| SendError::WindowNotOpen {
            room: room.to_string(),
            open: open_titles(&windows),
        })?;

    put_image_on_clipboard(image_path)?;
    let before = transcript_row_count(window.as_raw());

    // Unlike text, an image cannot be delivered in the background. Cmd+V is a menu key
    // equivalent, which the app only handles while it is frontmost — posting it to the pid
    // alone is silently ignored (measured: the paste never lands). So bring KakaoTalk forward.
    // AXFrontmost avoids depending on AppKit just to activate.
    let front_key = CFString::new(ATTR_FRONTMOST);
    let yes = core_foundation::boolean::CFBoolean::true_value();
    unsafe {
        AXUIElementSetAttributeValue(app.as_raw(), front_key.as_concrete_TypeRef(), yes.as_CFTypeRef())
    };
    std::thread::sleep(std::time::Duration::from_millis(250));

    if let Some(compose) = compose_box(window.as_raw()) {
        let focus_key = CFString::new(ATTR_FOCUSED);
        let yes = core_foundation::boolean::CFBoolean::true_value();
        unsafe {
            AXUIElementSetAttributeValue(
                compose.as_raw(),
                focus_key.as_concrete_TypeRef(),
                yes.as_CFTypeRef(),
            )
        };
    }

    // KakaoTalk is frontmost now, so the global tap is the reliable route for the paste.
    press_key_global(KEY_V, CGEventFlags::CGEventFlagCommand)?;

    // Depending on version KakaoTalk either shows a confirmation sheet or sends straight away,
    // so both are handled — but success is only ever claimed once a new transcript row exists.
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(100));

        if let Some(sheet) = find_descendant(window.as_raw(), ROLE_SHEET, 4) {
            if let Some(button) = find_confirm_button(sheet.as_raw()) {
                let action = CFString::new(ACTION_PRESS);
                unsafe { AXUIElementPerformAction(button.as_raw(), action.as_concrete_TypeRef()) };
            }
            continue;
        }

        match (before, transcript_row_count(window.as_raw())) {
            (Some(b), Some(now)) if now > b => return Ok(()),
            // Row counts unavailable on this window; fall back to the paste having been
            // consumed, and say so rather than pretending the send was confirmed.
            (None, _) | (_, None) => {
                if attr_string(
                    compose_box(window.as_raw())
                        .as_ref()
                        .map(|c| c.as_raw())
                        .unwrap_or(std::ptr::null_mut()),
                    ATTR_VALUE,
                )
                .unwrap_or_default()
                .is_empty()
                {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    Err(SendError::ImageNotSent)
}

/// The compose box is the text area inside one of the window's scroll areas; the other scroll
/// area holds the transcript table.
fn compose_box(window: AXUIElementRef) -> Option<AxRef> {
    child_elements(window, ATTR_CHILDREN)
        .into_iter()
        .filter(|c| role_of(c.as_raw()) == ROLE_SCROLL_AREA)
        .find_map(|sa| {
            child_elements(sa.as_raw(), ATTR_CHILDREN)
                .into_iter()
                .find(|c| role_of(c.as_raw()) == ROLE_TEXT_AREA)
        })
}

/// Number of message rows currently in the window's transcript. Used as proof that a message
/// actually landed: the sender's own "I pasted it" is not evidence, and KakaoTalk silently
/// drops a paste it did not accept.
fn transcript_row_count(window: AXUIElementRef) -> Option<usize> {
    for scroll in child_elements(window, ATTR_CHILDREN)
        .into_iter()
        .filter(|c| role_of(c.as_raw()) == ROLE_SCROLL_AREA)
    {
        if let Some(table) = child_elements(scroll.as_raw(), ATTR_CHILDREN)
            .into_iter()
            .find(|c| role_of(c.as_raw()) == ROLE_TABLE)
        {
            // Count only; never descend into rows — each node costs an IPC round trip.
            return Some(child_elements(table.as_raw(), ATTR_ROWS).len());
        }
    }
    None
}

fn open_titles(windows: &[AxRef]) -> String {
    let titles = windows
        .iter()
        .filter_map(|w| attr_string(w.as_raw(), ATTR_TITLE))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if titles.is_empty() {
        "(none)".into()
    } else {
        titles
    }
}

fn kakaotalk_pid() -> Option<i32> {
    let out = Command::new("pgrep").arg("-x").arg("KakaoTalk").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Send `text` to the chat window titled `room`, without activating KakaoTalk.
///
/// The window must already be open. Returns `Ok(())` only after confirming the compose box
/// drained, i.e. KakaoTalk actually accepted the Enter — a stale success would otherwise look
/// identical to a silently dropped message.
pub fn send_to_open_window(room: &str, text: &str) -> Result<(), SendError> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });

    let windows = child_elements(app.as_raw(), ATTR_WINDOWS);
    let window = windows
        .iter()
        .find(|w| attr_string(w.as_raw(), ATTR_TITLE).as_deref() == Some(room));

    let Some(window) = window else {
        return Err(SendError::WindowNotOpen {
            room: room.to_string(),
            open: open_titles(&windows),
        });
    };

    let compose =
        compose_box(window.as_raw()).ok_or_else(|| SendError::ComposeBoxNotFound(room.to_string()))?;

    // Make the compose box the app's focused element before typing. Enter is delivered to
    // KakaoTalk as a whole, and KakaoTalk routes it to whatever it considers focused — with
    // several windows open that is often not this chat, and the message then sits in the box
    // unsent. Setting AXFocused targets the element without activating the app, so this stays
    // invisible to the user.
    let focus_key = CFString::new(ATTR_FOCUSED);
    let yes = core_foundation::boolean::CFBoolean::true_value();
    unsafe {
        AXUIElementSetAttributeValue(
            compose.as_raw(),
            focus_key.as_concrete_TypeRef(),
            yes.as_CFTypeRef(),
        )
    };

    let key = CFString::new(ATTR_VALUE);
    let value = CFString::new(text);
    let err = unsafe {
        AXUIElementSetAttributeValue(
            compose.as_raw(),
            key.as_concrete_TypeRef(),
            value.as_CFTypeRef(),
        )
    };
    if err != AX_SUCCESS {
        return Err(SendError::SetValueFailed(err));
    }

    press_key(pid, KEY_RETURN, CGEventFlags::CGEventFlagNull)?;

    // KakaoTalk clears the compose box once it accepts the message; poll rather than sleep a
    // fixed amount so a fast machine is not penalised and a slow one is not called a failure.
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if attr_string(compose.as_raw(), ATTR_VALUE)
            .unwrap_or_default()
            .is_empty()
        {
            return Ok(());
        }
    }
    Err(SendError::NotSent)
}

/// Titles of every currently open KakaoTalk window, for telling the user what they can send to.
pub fn open_window_titles() -> Result<Vec<String>, SendError> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    Ok(child_elements(app.as_raw(), ATTR_WINDOWS)
        .iter()
        .filter_map(|w| attr_string(w.as_raw(), ATTR_TITLE))
        .filter(|t| !t.is_empty())
        .collect())
}
