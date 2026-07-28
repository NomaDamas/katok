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
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
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
const ROLE_CELL: &str = "AXCell";
const ROLE_STATIC_TEXT: &str = "AXStaticText";
const ROLE_TEXT_FIELD: &str = "AXTextField";

const ATTR_DESCRIPTION: &str = "AXDescription";

const ATTR_FRONTMOST: &str = "AXFrontmost";
const ATTR_ROWS: &str = "AXRows";
const ATTR_FOCUSED: &str = "AXFocused";
const ATTR_POSITION: &str = "AXPosition";
const ATTR_SIZE: &str = "AXSize";

const AX_VALUE_CG_POINT: u32 = 1;
const AX_VALUE_CG_SIZE: u32 = 2;

const ACTION_PRESS: &str = "AXPress";
const ACTION_RAISE: &str = "AXRaise";

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
    fn AXValueGetValue(value: CFTypeRef, the_type: u32, out: *mut c_void) -> bool;

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
    #[error("could not find KakaoTalk's chat list; is the main window open?")]
    ChatListUnavailable,
    #[error("'{room}' is not in the chat list ({scanned} rooms checked). Check the exact name with --list-rooms")]
    RoomNotInChatList { room: String, scanned: usize },
    #[error("selected '{0}' in the chat list but its window did not open")]
    RoomOpenFailed(String),
    #[error("could not reach KakaoTalk's chat search box")]
    SearchUnavailable,
    #[error("no chat named '{0}' found in the list or by search. Check the exact name with --list-rooms")]
    RoomNotFound(String),
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
    let status = unsafe { PasteboardCreate(clipboard_name.as_concrete_TypeRef(), &mut pasteboard) };
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
            std::ptr::dangling::<c_void>(), // any non-null item id; a single item is enough
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
pub fn send_image_to_open_window(
    room: &str,
    image_path: &Path,
    allow_open: bool,
) -> Result<(), SendError> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    let window = resolve_window(&app, room, allow_open)?;

    put_image_on_clipboard(image_path)?;
    let before = transcript_row_count(window.as_raw());

    // Unlike text, an image cannot be delivered in the background. Cmd+V is a menu key
    // equivalent, which the app only handles while it is frontmost — posting it to the pid
    // alone is silently ignored (measured: the paste never lands). So bring KakaoTalk forward.
    // AXFrontmost avoids depending on AppKit just to activate.
    let front_key = CFString::new(ATTR_FRONTMOST);
    let yes = core_foundation::boolean::CFBoolean::true_value();
    unsafe {
        AXUIElementSetAttributeValue(
            app.as_raw(),
            front_key.as_concrete_TypeRef(),
            yes.as_CFTypeRef(),
        )
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

/// The main window: the one carrying the chat list.
///
/// A chat window also holds a scroll area with a table (its transcript), so "has a table" alone
/// picks the wrong window whenever a chat is open. The reliable difference is the compose box:
/// every chat window has one, the main window does not.
fn main_window(windows: &[AxRef]) -> Option<&AxRef> {
    // Structural guesses all failed on real data: a chat window also has a table (its
    // transcript), a read-only channel window also lacks a compose box, and a chat window also
    // has a text field (search-within-chat). So score candidates by the property actually
    // needed — how many of their rows yield a room name — and take the best. A transcript
    // scores near zero because message rows carry timestamps and bodies, not room names.
    windows
        .iter()
        .filter_map(|w| chat_list_table_in(w.as_raw()).map(|t| (w, t)))
        .max_by_key(|(_, table)| {
            child_elements(table.as_raw(), ATTR_ROWS)
                .iter()
                .take(MAIN_WINDOW_SAMPLE)
                .filter(|r| row_room_name(r.as_raw()).is_some())
                .count()
        })
        .filter(|(_, table)| {
            child_elements(table.as_raw(), ATTR_ROWS)
                .iter()
                .take(MAIN_WINDOW_SAMPLE)
                .any(|r| row_room_name(r.as_raw()).is_some())
        })
        .map(|(w, _)| w)
}

/// Rows sampled per candidate window when identifying the chat list. Kept small because every
/// row costs several AX round trips.
const MAIN_WINDOW_SAMPLE: usize = 10;

fn chat_list_table_in(window: AXUIElementRef) -> Option<AxRef> {
    child_elements(window, ATTR_CHILDREN)
        .into_iter()
        .filter(|c| role_of(c.as_raw()) == ROLE_SCROLL_AREA)
        .find_map(|scroll| {
            child_elements(scroll.as_raw(), ATTR_CHILDREN)
                .into_iter()
                .find(|c| role_of(c.as_raw()) == ROLE_TABLE)
        })
}

fn chat_list_table(windows: &[AxRef]) -> Option<AxRef> {
    chat_list_table_in(main_window(windows)?.as_raw())
}

/// Room name of a chat-list row: `AXRow > AXCell > first AXStaticText`.
///
/// Kept deliberately shallow. A generic descendant crawl costs ~160 node visits per row, which
/// is what makes a full-list scan take ~20s (see references/performance.md); this reads a
/// handful of attributes and stops at the first static text.
fn row_room_name(row: AXUIElementRef) -> Option<String> {
    let cell = child_elements(row, ATTR_CHILDREN)
        .into_iter()
        .find(|c| role_of(c.as_raw()) == ROLE_CELL)?;
    child_elements(cell.as_raw(), ATTR_CHILDREN)
        .into_iter()
        .filter(|c| role_of(c.as_raw()) == ROLE_STATIC_TEXT)
        .filter_map(|t| attr_string(t.as_raw(), ATTR_VALUE))
        .find(|v| !is_row_metadata(v))
}

/// A chat row carries the name alongside an unread badge and a timestamp, all as static text,
/// and their order is not stable across rows. Filter the two that are never a room name.
fn is_row_metadata(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return true;
    }
    // unread count
    if v.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // timestamps and relative dates
    if v.contains("오전") || v.contains("오후") || v.contains("AM") || v.contains("PM") {
        return true;
    }
    if v == "어제" || v == "오늘" || v == "Yesterday" || v == "Today" {
        return true;
    }
    // absolute dates such as "2026. 7. 23." or "7/23/26"
    v.chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '/' | '-' | ':' | ' '))
}

/// Open `room` from the chat list and wait for its window.
///
/// Rows expose no `AXPress`, so the row is selected and Enter is sent — the same route the
/// app takes for a keyboard user. Enter goes to the pid, so this does not activate KakaoTalk.
fn open_room_from_chat_list(
    app: &AxRef,
    windows: &[AxRef],
    room: &str,
    max_rows: usize,
) -> Result<(), SendError> {
    let table = chat_list_table(windows).ok_or(SendError::ChatListUnavailable)?;
    let rows = child_elements(table.as_raw(), ATTR_ROWS);
    if rows.is_empty() {
        return Err(SendError::ChatListUnavailable);
    }

    // Rows are ordered most-recent-first, so the common case exits after a handful of reads.
    // Each name costs several AX round trips, so a bounded prefix keeps the fast path fast and
    // leaves the long tail to search.
    let scanned = rows.len().min(max_rows);
    let target = rows
        .iter()
        .take(scanned)
        .find(|r| row_room_name(r.as_raw()).as_deref() == Some(room))
        .ok_or(SendError::RoomNotInChatList {
            room: room.to_string(),
            scanned,
        })?;

    open_row_by_click(app, windows, target.as_raw(), room)
}

/// Screen frame of an element, or `None` when it exposes no geometry.
fn frame_of(el: AXUIElementRef) -> Option<CGRect> {
    let mut point = CGPoint::new(0.0, 0.0);
    let mut size = CGSize::new(0.0, 0.0);
    let pos_raw = copy_attr(el, ATTR_POSITION)?;
    let ok_pos = unsafe {
        AXValueGetValue(
            pos_raw,
            AX_VALUE_CG_POINT,
            &mut point as *mut CGPoint as *mut c_void,
        )
    };
    unsafe { CFRelease(pos_raw) };
    let size_raw = copy_attr(el, ATTR_SIZE)?;
    let ok_size = unsafe {
        AXValueGetValue(
            size_raw,
            AX_VALUE_CG_SIZE,
            &mut size as *mut CGSize as *mut c_void,
        )
    };
    unsafe { CFRelease(size_raw) };
    if ok_pos && ok_size && size.width > 0.0 && size.height > 0.0 {
        Some(CGRect::new(&point, &size))
    } else {
        None
    }
}

/// Open a chat-list row by double-clicking it.
///
/// Rows advertise only `AXShowDefaultUI`/`AXShowAlternateUI` — they ignore `AXPress` and they
/// ignore Enter even when selected and focused with the app frontmost (both measured). A real
/// double-click is the only thing KakaoTalk acts on, which means the row has to be visible on
/// screen and the app has to be frontmost, so this necessarily takes focus for a moment. The
/// pointer is put back where it was afterwards.
fn open_row_by_click(
    app: &AxRef,
    windows: &[AxRef],
    row: AXUIElementRef,
    room: &str,
) -> Result<(), SendError> {
    let front = CFString::new(ATTR_FRONTMOST);
    let yes = core_foundation::boolean::CFBoolean::true_value();
    unsafe {
        AXUIElementSetAttributeValue(
            app.as_raw(),
            front.as_concrete_TypeRef(),
            yes.as_CFTypeRef(),
        )
    };
    // A click lands on whatever window is on top at those coordinates, so the list has to be
    // raised above any chat windows before aiming at it.
    if let Some(main) = main_window(windows) {
        let raise = CFString::new(ACTION_RAISE);
        unsafe { AXUIElementPerformAction(main.as_raw(), raise.as_concrete_TypeRef()) };
    }
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Read geometry after raising: the row can move as the window comes forward.
    let rect = frame_of(row).ok_or_else(|| SendError::RoomOpenFailed(room.to_string()))?;

    let target = CGPoint::new(
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    );
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| SendError::KeyEventFailed)?;
    let restore = CGEvent::new(source.clone()).ok().map(|e| e.location());

    for click in 1..=2 {
        for (kind, _) in [
            (CGEventType::LeftMouseDown, ()),
            (CGEventType::LeftMouseUp, ()),
        ] {
            let ev = CGEvent::new_mouse_event(source.clone(), kind, target, CGMouseButton::Left)
                .map_err(|_| SendError::KeyEventFailed)?;
            ev.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click);
            ev.post(CGEventTapLocation::HID);
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }

    if let Some(p) = restore {
        if let Ok(ev) =
            CGEvent::new_mouse_event(source, CGEventType::MouseMoved, p, CGMouseButton::Left)
        {
            ev.post(CGEventTapLocation::HID);
        }
    }
    Ok(())
}

/// Reveal the chat list's search field, clicking the search button if it is not showing yet.
fn search_field(window: AXUIElementRef) -> Option<AxRef> {
    let field = |w: AXUIElementRef| {
        child_elements(w, ATTR_CHILDREN)
            .into_iter()
            .find(|c| role_of(c.as_raw()) == ROLE_TEXT_FIELD)
    };
    if let Some(f) = field(window) {
        return Some(f);
    }
    let button = child_elements(window, ATTR_CHILDREN)
        .into_iter()
        .find(|c| {
            role_of(c.as_raw()) == ROLE_BUTTON
                && matches!(
                    attr_string(c.as_raw(), ATTR_DESCRIPTION).as_deref(),
                    Some("검색") | Some("Search")
                )
        })?;
    let action = CFString::new(ACTION_PRESS);
    unsafe { AXUIElementPerformAction(button.as_raw(), action.as_concrete_TypeRef()) };
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(60));
        if let Some(f) = field(window) {
            return Some(f);
        }
    }
    None
}

/// Open `room` by typing it into the chat-list search box and taking the first matching result.
///
/// Needed because AX only materialises the rows KakaoTalk has actually rendered — a room far
/// down the list is invisible to a plain row scan, so searching is the only way to reach it.
fn open_room_via_search(app: &AxRef, windows: &[AxRef], room: &str) -> Result<(), SendError> {
    let main = main_window(windows).ok_or(SendError::ChatListUnavailable)?;
    let field = search_field(main.as_raw()).ok_or(SendError::SearchUnavailable)?;

    let key = CFString::new(ATTR_VALUE);
    let value = CFString::new(room);
    let err = unsafe {
        AXUIElementSetAttributeValue(
            field.as_raw(),
            key.as_concrete_TypeRef(),
            value.as_CFTypeRef(),
        )
    };
    if err != AX_SUCCESS {
        return Err(SendError::SearchUnavailable);
    }

    // Results replace the chat-list rows. Prefer an exact name match, but accept a lone result:
    // the query was the full room name, and under rapid access KakaoTalk sometimes hands back a
    // row whose cell text has not been filled in yet, which would otherwise fail a name compare
    // on the very row the user asked for.
    let full_list = chat_list_table_in(main.as_raw())
        .map(|t| child_elements(t.as_raw(), ATTR_ROWS).len())
        .ok_or(SendError::ChatListUnavailable)?;

    for _ in 0..25 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let Some(table) = chat_list_table_in(main.as_raw()) else {
            continue;
        };
        let rows = child_elements(table.as_raw(), ATTR_ROWS);
        if rows.is_empty() || rows.len() == full_list {
            continue; // filtering has not taken effect yet
        }
        if let Some(hit) = rows
            .iter()
            .find(|r| row_room_name(r.as_raw()).as_deref() == Some(room))
        {
            return open_row_by_click(app, windows, hit.as_raw(), room);
        }
        if rows.len() == 1 {
            return open_row_by_click(app, windows, rows[0].as_raw(), room);
        }
    }
    Err(SendError::RoomNotFound(room.to_string()))
}

/// Clear the search box so the user's chat list is left exactly as it was found.
///
/// KakaoTalk re-filters on a *change* of the field's value, so writing "" over a value that is
/// already "" is a no-op and would leave the list visibly filtered. Write a throwaway value
/// first to guarantee a change, then verify the list actually came back.
fn clear_search(windows: &[AxRef]) {
    let Some(main) = main_window(windows) else {
        return;
    };
    let Some(field) = child_elements(main.as_raw(), ATTR_CHILDREN)
        .into_iter()
        .find(|c| role_of(c.as_raw()) == ROLE_TEXT_FIELD)
    else {
        return;
    };

    // Only write when there is something to clear. Writing a placeholder first to "force a
    // change" backfires: a space is itself a query, and if the following empty write does not
    // register the list is left filtered down to nothing.
    if attr_string(field.as_raw(), ATTR_VALUE)
        .unwrap_or_default()
        .is_empty()
    {
        return;
    }

    let key = CFString::new(ATTR_VALUE);
    let empty = CFString::new("");
    for _ in 0..3 {
        unsafe {
            AXUIElementSetAttributeValue(
                field.as_raw(),
                key.as_concrete_TypeRef(),
                empty.as_CFTypeRef(),
            )
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        if attr_string(field.as_raw(), ATTR_VALUE)
            .unwrap_or_default()
            .is_empty()
        {
            return;
        }
    }
}

/// Find the chat window for `room`, opening it from the chat list when it is not already up.
fn resolve_window(app: &AxRef, room: &str, allow_open: bool) -> Result<AxRef, SendError> {
    let find = || {
        let windows = child_elements(app.as_raw(), ATTR_WINDOWS);
        let hit = windows
            .iter()
            .position(|w| attr_string(w.as_raw(), ATTR_TITLE).as_deref() == Some(room));
        (windows, hit)
    };

    let (windows, hit) = find();
    if let Some(i) = hit {
        return Ok(AxRef::retained(windows[i].as_raw()));
    }
    if !allow_open {
        return Err(SendError::WindowNotOpen {
            room: room.to_string(),
            open: open_titles(&windows),
        });
    }

    // Two strategies, tried in order and both reported on failure — never silently.
    // The row scan is far cheaper but only sees rows KakaoTalk has rendered, so search is the
    // route to anything further down the list.
    // Ordered cheapest-first. Opening a row means selecting it and pressing Enter, and Enter
    // reaches whichever window KakaoTalk considers key — with a chat already open that is not
    // the list, so the first two attempts only work when the list happens to be key. The last
    // attempt therefore activates KakaoTalk, which does take focus but is a one-off per room;
    // sending afterwards stays in the background.
    const RECENT_ROWS: usize = 60;
    let mut last: Option<SendError> = None;

    for attempt in 0..4 {
        if attempt == 3 {
            let key = CFString::new(ATTR_FRONTMOST);
            let yes = core_foundation::boolean::CFBoolean::true_value();
            unsafe {
                AXUIElementSetAttributeValue(
                    app.as_raw(),
                    key.as_concrete_TypeRef(),
                    yes.as_CFTypeRef(),
                )
            };
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let outcome = match attempt {
            0 => open_room_from_chat_list(app, &windows, room, RECENT_ROWS),
            1 => open_room_via_search(app, &windows, room),
            _ => open_room_from_chat_list(app, &windows, room, usize::MAX),
        };
        match outcome {
            Ok(()) => match wait_for_window(&find, 20) {
                Some(w) => {
                    clear_search(&windows);
                    return Ok(w);
                }
                // Selected it but nothing opened — record that, do not let an earlier,
                // less relevant error be the one reported.
                None => last = Some(SendError::RoomOpenFailed(room.to_string())),
            },
            Err(e) => last = Some(e),
        }
        if attempt == 1 {
            clear_search(&windows);
        }
    }
    clear_search(&windows);
    Err(last.unwrap_or_else(|| SendError::RoomOpenFailed(room.to_string())))
}

fn wait_for_window<F>(find: &F, attempts: u32) -> Option<AxRef>
where
    F: Fn() -> (Vec<AxRef>, Option<usize>),
{
    for _ in 0..attempts {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (windows, hit) = find();
        if let Some(i) = hit {
            return Some(AxRef::retained(windows[i].as_raw()));
        }
    }
    None
}

fn kakaotalk_pid() -> Option<i32> {
    let out = Command::new("pgrep")
        .arg("-x")
        .arg("KakaoTalk")
        .output()
        .ok()?;
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
pub fn send_to_open_window(room: &str, text: &str, allow_open: bool) -> Result<(), SendError> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    let window = resolve_window(&app, room, allow_open)?;

    let compose = compose_box(window.as_raw())
        .ok_or_else(|| SendError::ComposeBoxNotFound(room.to_string()))?;

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

    // KakaoTalk clears the compose box once it accepts the message; poll rather than sleep a
    // fixed amount so a fast machine is not penalised and a slow one is not called a failure.
    let accepted = |attempts: u32| {
        for _ in 0..attempts {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if attr_string(compose.as_raw(), ATTR_VALUE)
                .unwrap_or_default()
                .is_empty()
            {
                return true;
            }
        }
        false
    };

    press_key(pid, KEY_RETURN, CGEventFlags::CGEventFlagNull)?;
    if accepted(20) {
        return Ok(());
    }

    // Enter posted to the pid is routed by KakaoTalk to its key window, and with several chats
    // open that is often not this one. Falling back costs the user's focus for a moment, which
    // is worth it over leaving a message sitting unsent in the box.
    let yes = core_foundation::boolean::CFBoolean::true_value();
    let front = CFString::new(ATTR_FRONTMOST);
    unsafe {
        AXUIElementSetAttributeValue(
            app.as_raw(),
            front.as_concrete_TypeRef(),
            yes.as_CFTypeRef(),
        )
    };
    let raise = CFString::new(ACTION_RAISE);
    unsafe { AXUIElementPerformAction(window.as_raw(), raise.as_concrete_TypeRef()) };
    std::thread::sleep(std::time::Duration::from_millis(200));
    unsafe {
        AXUIElementSetAttributeValue(
            compose.as_raw(),
            CFString::new(ATTR_FOCUSED).as_concrete_TypeRef(),
            yes.as_CFTypeRef(),
        )
    };
    press_key_global(KEY_RETURN, CGEventFlags::CGEventFlagNull)?;
    if accepted(30) {
        return Ok(());
    }
    Err(SendError::NotSent)
}

/// Resolve (opening if needed) the chat window for `room` without sending anything.
pub fn resolve_room_window(room: &str, allow_open: bool) -> Result<(), SendError> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    resolve_window(&app, room, allow_open).map(|_| ())
}

/// Room names from the chat list, newest first. Used to find the exact name to pass to `--room`.
pub fn chat_list_rooms(limit: usize) -> Result<Vec<String>, SendError> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    let windows = child_elements(app.as_raw(), ATTR_WINDOWS);
    let table = chat_list_table(&windows).ok_or(SendError::ChatListUnavailable)?;
    Ok(child_elements(table.as_raw(), ATTR_ROWS)
        .iter()
        .take(limit)
        .filter_map(|r| row_room_name(r.as_raw()))
        .collect())
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
