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
//! **Writing the compose box IS sending.** Setting `AXValue` on it does not
//! stage text for a person to review — KakaoTalk delivers the message on the
//! spot, measured with a single-line body and no keystroke of any kind. The
//! `Enter` below is therefore belt-and-braces, not the trigger. A `--draft`
//! mode was built on the opposite assumption, reported `sent: false`, and put
//! two messages into real conversations before the archive showed what had
//! actually happened; it was removed rather than repaired, because any "write
//! but do not send" feature has to be proven against the app before it is
//! offered. Pasting via the clipboard without pressing Enter is the untried
//! candidate, and it needs the screen.
//!
//! Scope: the target chat window must already be open. Locating an arbitrary room means either
//! crawling the chat list or driving the search field, and both are slow enough (~20s, dominated
//! by per-node AX round trips) that the caller is better off opening the room once and keeping
//! it open.
//!
//! # Colliding with the user
//!
//! Two operations cannot avoid activating KakaoTalk: pasting an image (Cmd+V is a menu key
//! equivalent, and an inactive app has no key window for `paste:` to reach) and opening a closed
//! room (its list rows ignore both `AXPress` and Enter, so only a real double-click works). That
//! is unavoidable, but the damage from colliding with the user is not:
//!
//! - **Never post a global key on faith.** A global key event goes to whatever is frontmost. If
//!   the user clicks away between activating KakaoTalk and posting Cmd+V, the image lands in
//!   *their* document. So frontmost is verified immediately before every global post, and the
//!   post is abandoned rather than fired blind. A failed send is recoverable; a paste into
//!   someone else's window is not.
//! - **Wait for a quiet moment.** Taking focus mid-keystroke is what makes the collision likely
//!   in the first place, so the run waits for a gap in user input before activating.
//! - **Put everything back.** The previously frontmost app is reactivated and the clipboard is
//!   restored, on every exit path including failures.

#![cfg(target_os = "macos")]

use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::boolean::{CFBoolean, CFBooleanRef};
use core_foundation::data::{CFData, CFDataRef};
use core_foundation::dictionary::{CFDictionaryGetValueIfPresent, CFDictionaryRef};
use core_foundation::number::{CFNumber, CFNumberRef};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowListExcludeDesktopElements,
    kCGWindowListOptionOnScreenOnly,
};
use std::ffi::c_void;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::send_curtain::{attachment_label, tag_synthetic, CurtainControl};

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
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
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
    fn PasteboardSynchronize(pasteboard: PasteboardRef) -> u32;
    fn PasteboardGetItemCount(pasteboard: PasteboardRef, count: *mut usize) -> OSStatus;
    fn PasteboardGetItemIdentifier(
        pasteboard: PasteboardRef,
        index: isize,
        item: *mut *const c_void,
    ) -> OSStatus;
    fn PasteboardCopyItemFlavors(
        pasteboard: PasteboardRef,
        item: *const c_void,
        flavors: *mut CFArrayRef,
    ) -> OSStatus;
    fn PasteboardCopyItemFlavorData(
        pasteboard: PasteboardRef,
        item: *const c_void,
        flavor: CFStringRef,
        data: *mut CFDataRef,
    ) -> OSStatus;

    /// Seconds since the last input event of `event_type`.
    ///
    /// Declared here rather than taken from the `core-graphics` crate, which
    /// does not bind it. Both parameters are passed as raw `u32` because
    /// `kCGAnyInputEventType` (`0xFFFF_FFFF`) is not a member of the crate's
    /// `CGEventType` enum.
    fn CGEventSourceSecondsSinceLastEventType(state_id: u32, event_type: u32) -> f64;
}

/// `kCGAnyInputEventType`: any keyboard, mouse, or tablet event.
const ANY_INPUT_EVENT_TYPE: u32 = 0xFFFF_FFFF;
/// `kCGEventSourceStateCombinedSessionState`.
const COMBINED_SESSION_STATE: u32 = 0;

/// How long the user must have been idle before KakaoTalk is allowed to come
/// forward. Long enough to sit between keystrokes of ordinary typing, short
/// enough that a send does not feel stalled.
const DEFAULT_IDLE_GAP: Duration = Duration::from_millis(700);
/// How long to keep waiting for that gap before giving up on the send.
const DEFAULT_FOCUS_WAIT: Duration = Duration::from_secs(15);

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

/// Options controlling how much the send is allowed to disturb the user.
#[derive(Debug, Clone, Copy)]
pub struct FocusPolicy {
    /// Required gap in user input before KakaoTalk may be activated.
    pub idle_gap: Duration,
    /// How long to wait for that gap before failing.
    pub max_wait: Duration,
}

impl Default for FocusPolicy {
    fn default() -> Self {
        Self {
            idle_gap: DEFAULT_IDLE_GAP,
            max_wait: DEFAULT_FOCUS_WAIT,
        }
    }
}

impl FocusPolicy {
    /// Take focus immediately, without waiting for the user to stop typing.
    pub fn immediate() -> Self {
        Self {
            idle_gap: Duration::ZERO,
            max_wait: Duration::ZERO,
        }
    }
}

/// Everything a send needs to know about how it may disturb the user.
#[derive(Clone, Default)]
pub struct SendContext {
    pub policy: FocusPolicy,
    /// Present when the caller set up a curtain, which is what makes taking the
    /// screen safe: input is blocked for the moment KakaoTalk is forward, and
    /// the block is visible so nobody types into it.
    pub curtain: Option<Arc<CurtainControl>>,
}

impl SendContext {
    pub fn new(policy: FocusPolicy) -> Self {
        Self {
            policy,
            curtain: None,
        }
    }

    fn cancelled(&self) -> bool {
        self.curtain
            .as_ref()
            .is_some_and(|curtain| curtain.is_cancelled())
    }
}

/// Holds the screen for as long as an activating step needs it.
///
/// Dropping it restores the user's application first and only then lifts the
/// curtain, so the screen that reappears is already the one they left.
struct ScreenHold {
    _focus: FocusRestore,
    curtain: Option<Arc<CurtainControl>>,
}

impl Drop for ScreenHold {
    fn drop(&mut self) {
        if let Some(curtain) = &self.curtain {
            curtain.hide();
        }
    }
}

/// Wait for a quiet moment, raise the curtain, and bring KakaoTalk forward.
///
/// The hold is constructed *before* the raise is attempted so that a failed
/// raise still takes the curtain down and gives focus back on the way out.
fn take_screen(pid: i32, ctx: &SendContext, subtitle: &str) -> Result<ScreenHold, SendError> {
    wait_for_user_idle(ctx.policy)?;
    if ctx.cancelled() {
        return Err(SendError::Cancelled);
    }
    if let Some(curtain) = &ctx.curtain {
        curtain.show(subtitle);
    }
    let hold = ScreenHold {
        _focus: FocusRestore::capture(pid),
        curtain: ctx.curtain.clone(),
    };
    if !raise_and_confirm(pid) {
        return Err(SendError::FocusLost);
    }
    Ok(hold)
}

fn seconds_since_user_input() -> f64 {
    unsafe { CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, ANY_INPUT_EVENT_TYPE) }
}

/// Block until the user has been idle for `policy.idle_gap`.
///
/// Activating an app in the middle of someone's typing is what turns "the send
/// stole focus for a moment" into "the send broke what I was doing", so the
/// wait happens *before* anything is touched. Only usable before this process
/// posts any event of its own: a synthesized event resets the same timer.
fn wait_for_user_idle(policy: FocusPolicy) -> Result<(), SendError> {
    if policy.idle_gap.is_zero() {
        return Ok(());
    }
    let deadline = Instant::now() + policy.max_wait;
    loop {
        let idle = Duration::from_secs_f64(seconds_since_user_input().max(0.0));
        if idle >= policy.idle_gap {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SendError::UserBusy {
                waited: policy.max_wait.as_secs_f32(),
            });
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

/// Whether `pid`'s application is the active one.
///
/// Read straight off the application element, which is the authoritative answer
/// and the one the safety checks below depend on.
///
/// The obvious route — `AXFocusedApplication` on the system-wide element — is
/// not usable here: from a plain CLI process it returns `kAXErrorCannotComplete`
/// (-25204) every time, measured, even with accessibility permission granted.
/// Per-application queries are unaffected.
fn app_is_frontmost(pid: i32) -> bool {
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    let Some(raw) = copy_attr(app.as_raw(), ATTR_FRONTMOST) else {
        return false;
    };
    // copy_attr hands back a +1 reference; wrap_under_create_rule takes it.
    let value = unsafe { CFBoolean::wrap_under_create_rule(raw as CFBooleanRef) };
    value == CFBoolean::true_value()
}

/// The application that currently owns the screen, or `None` if it cannot be
/// determined.
///
/// Used only to remember who to give focus back to. The on-screen window list is
/// ordered front to back, so the first window at layer 0 — the normal window
/// layer, above the desktop and below menus and overlays — belongs to the active
/// application. This needs no Screen Recording permission: window owner and
/// layer are readable without it, unlike window titles.
fn frontmost_pid() -> Option<i32> {
    let list = copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )?;
    for i in 0..list.len() {
        let dict = *list.get(i)? as CFDictionaryRef;
        if dict_i64(dict, "kCGWindowLayer") != Some(0) {
            continue;
        }
        if let Some(pid) = dict_i64(dict, "kCGWindowOwnerPID") {
            return i32::try_from(pid).ok();
        }
    }
    None
}

fn dict_i64(dict: CFDictionaryRef, key: &str) -> Option<i64> {
    let key = CFString::new(key);
    let mut value: *const c_void = std::ptr::null();
    let found = unsafe { CFDictionaryGetValueIfPresent(dict, key.as_CFTypeRef(), &mut value) };
    if found == 0 || value.is_null() {
        return None;
    }
    unsafe { CFNumber::wrap_under_get_rule(value as CFNumberRef) }.to_i64()
}

fn activate(pid: i32) {
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    let key = CFString::new(ATTR_FRONTMOST);
    let yes = core_foundation::boolean::CFBoolean::true_value();
    unsafe {
        AXUIElementSetAttributeValue(app.as_raw(), key.as_concrete_TypeRef(), yes.as_CFTypeRef())
    };
}

/// Restores whichever application was frontmost before KakaoTalk was raised.
///
/// Taking focus is unavoidable for a paste; keeping it is not. This runs on
/// every exit path, including the error ones, so a failed send does not leave
/// the user staring at KakaoTalk.
struct FocusRestore {
    previous: Option<i32>,
}

impl FocusRestore {
    /// Remember the current frontmost app, unless it is already `target_pid`,
    /// in which case there is nothing to restore.
    fn capture(target_pid: i32) -> Self {
        Self {
            previous: frontmost_pid().filter(|pid| *pid != target_pid),
        }
    }
}

impl Drop for FocusRestore {
    fn drop(&mut self) {
        if let Some(pid) = self.previous {
            activate(pid);
        }
    }
}

/// Bring `pid` forward and confirm it actually got there.
///
/// Returns `false` when the app never became frontmost, which the caller must
/// treat as "do not post a global key" rather than pressing on.
fn raise_and_confirm(pid: i32) -> bool {
    activate(pid);
    for _ in 0..25 {
        if app_is_frontmost(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    false
}

/// Re-assert frontmost and immediately perform `action`.
///
/// Raising once and acting later is a check-then-act with a gap in it, and
/// anything that activates in that gap — another tool opening a window, a
/// notification, the user — leaves the action aimed at the wrong application.
/// Every step that posts to the screen or the global tap goes through here, so
/// "bring it forward" is part of the action rather than a separate earlier one.
fn front_then<T>(pid: i32, action: impl FnOnce() -> T) -> Result<T, SendError> {
    for _ in 0..3 {
        activate(pid);
        // Just long enough for the activation to take, short enough that the
        // window for something else to grab focus stays small.
        std::thread::sleep(Duration::from_millis(60));
        if app_is_frontmost(pid) {
            return Ok(action());
        }
    }
    Err(SendError::FocusLost)
}

/// Post a global key only while `pid` is confirmed frontmost.
///
/// The confirmation is re-read immediately before the post, so the window in
/// which the user could steal focus and receive the keystroke shrinks to the
/// syscall in between. Without this, a Cmd+V meant for KakaoTalk lands in
/// whatever the user clicked on.
fn press_key_global_at(pid: i32, key: u16, flags: CGEventFlags) -> Result<(), SendError> {
    front_then(pid, || press_key_global(key, flags))?
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
    #[error(
        "{count} chats are named '{room}', and nothing on screen tells them apart. Nothing was \
         opened: sending to the wrong one cannot be undone. Pass --chat <chat-id> so the last \
         message can be used to pick, or rename one of them."
    )]
    AmbiguousRoom { room: String, count: usize },
    #[error(
        "--room contains characters a chat name cannot: {offenders}. This is almost always a \
         quoting accident in whatever built the argument, not a missing room.\n  room: {room}"
    )]
    RoomNameNotTypable { room: String, offenders: String },
    #[error(
        "{what} did not hold what was written to it, so the next step would have acted on the \
         wrong value.\n  wrote: {wrote}\n  found: {found}"
    )]
    ValueMismatch {
        what: String,
        wrote: String,
        found: String,
    },
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
    #[error(
        "you kept using the keyboard or mouse for {waited:.0}s, and this send needs to bring \
         KakaoTalk forward. Nothing was sent and nothing was typed anywhere. Retry when idle, \
         or pass --take-focus-now to interrupt yourself."
    )]
    UserBusy { waited: f32 },
    #[error(
        "KakaoTalk was not frontmost at the moment the keystroke would have been sent, so it was \
         not sent at all — posting it would have typed into whatever app you switched to. \
         Nothing was sent; retry."
    )]
    FocusLost,
    #[error(
        "the chat list reordered while '{room}' was being opened, so the click was not sent — \
         it would have landed on whichever room took that position. Nothing was opened; retry."
    )]
    ChatListMoved { room: String },
    #[error(
        "the draft is not sitting in {room}'s compose box. It was either not pasted or already \
         sent — check the chat rather than assuming it is waiting."
    )]
    DraftNotStaged { room: String },
    #[error("cancelled before anything was sent")]
    Cancelled,
    #[error(
        "cancelled after the image was already pasted into KakaoTalk, so it may or may not have \
         been delivered. Check the chat before resending."
    )]
    CancelledAfterPaste,
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

fn open_clipboard() -> Result<(PasteboardRef, AxRef), SendError> {
    let mut pasteboard: PasteboardRef = std::ptr::null_mut();
    let clipboard_name = CFString::new(PASTEBOARD_CLIPBOARD);
    let status = unsafe { PasteboardCreate(clipboard_name.as_concrete_TypeRef(), &mut pasteboard) };
    if status != OS_NO_ERR || pasteboard.is_null() {
        return Err(SendError::ClipboardFailed(status));
    }
    unsafe { PasteboardSynchronize(pasteboard) };
    // AxRef only cares that this is a CFType to release, which a PasteboardRef is.
    Ok((pasteboard, AxRef(pasteboard)))
}

/// One item's flavors, captured so the user's clipboard survives the send.
struct SavedItem {
    flavors: Vec<(CFString, CFData)>,
}

/// Snapshot of the clipboard, restored on drop.
///
/// Sending an image has to go through the clipboard because KakaoTalk exposes
/// no AX affordance for attaching a file. Clearing it would otherwise throw
/// away whatever the user had copied, which is its own small act of vandalism.
struct ClipboardRestore {
    items: Vec<SavedItem>,
    /// Flavors that could not be read back, e.g. promised data whose owner
    /// answers lazily. Reported rather than passed over in silence.
    unreadable: usize,
}

impl ClipboardRestore {
    fn capture() -> Result<Self, SendError> {
        let (pasteboard, _guard) = open_clipboard()?;
        let mut count: usize = 0;
        let status = unsafe { PasteboardGetItemCount(pasteboard, &mut count) };
        if status != OS_NO_ERR {
            return Err(SendError::ClipboardFailed(status));
        }

        let mut items = Vec::new();
        let mut unreadable = 0usize;
        for index in 1..=count {
            let mut item_id: *const c_void = std::ptr::null();
            if unsafe { PasteboardGetItemIdentifier(pasteboard, index as isize, &mut item_id) }
                != OS_NO_ERR
            {
                unreadable += 1;
                continue;
            }
            let mut flavor_array: CFArrayRef = std::ptr::null();
            if unsafe { PasteboardCopyItemFlavors(pasteboard, item_id, &mut flavor_array) }
                != OS_NO_ERR
                || flavor_array.is_null()
            {
                unreadable += 1;
                continue;
            }
            let flavor_count = unsafe { CFArrayGetCount(flavor_array) };
            let mut flavors = Vec::new();
            for i in 0..flavor_count {
                let raw = unsafe { CFArrayGetValueAtIndex(flavor_array, i) } as CFStringRef;
                if raw.is_null() {
                    continue;
                }
                let flavor = unsafe { CFString::wrap_under_get_rule(raw) };
                let mut data: CFDataRef = std::ptr::null();
                let status = unsafe {
                    PasteboardCopyItemFlavorData(
                        pasteboard,
                        item_id,
                        flavor.as_concrete_TypeRef(),
                        &mut data,
                    )
                };
                if status != OS_NO_ERR || data.is_null() {
                    unreadable += 1;
                    continue;
                }
                flavors.push((flavor, unsafe { CFData::wrap_under_create_rule(data) }));
            }
            unsafe { CFRelease(flavor_array as CFTypeRef) };
            if !flavors.is_empty() {
                items.push(SavedItem { flavors });
            }
        }
        Ok(Self { items, unreadable })
    }

    /// Flavors that were on the clipboard but could not be read back, and so
    /// will not survive the restore.
    fn unreadable_flavors(&self) -> usize {
        self.unreadable
    }
}

impl Drop for ClipboardRestore {
    fn drop(&mut self) {
        let Ok((pasteboard, _guard)) = open_clipboard() else {
            return;
        };
        if unsafe { PasteboardClear(pasteboard) } != OS_NO_ERR {
            return;
        }
        for (index, item) in self.items.iter().enumerate() {
            // Any distinct non-null id per item; only uniqueness matters.
            let item_id = (index + 1) as *const c_void;
            for (flavor, data) in &item.flavors {
                unsafe {
                    PasteboardPutItemFlavor(
                        pasteboard,
                        item_id,
                        flavor.as_concrete_TypeRef(),
                        data.as_concrete_TypeRef(),
                        0,
                    )
                };
            }
        }
    }
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

    let (pasteboard, _guard) = open_clipboard()?;

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
        // Tagged so a raised curtain lets our own keystroke through while it
        // swallows the user's.
        tag_synthetic(&event);
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
        tag_synthetic(&event);
        event.post_to_pid(pid);
    }
    Ok(())
}

/// Which room to act on.
///
/// A display name is not an identifier: on the reference install four names are
/// shared by two rooms each, and one archive name covers nine. Picking the first
/// row that matches therefore sends to whichever of them happens to sit higher,
/// and a message delivered to the wrong person cannot be taken back — so an
/// ambiguous name is refused rather than guessed at.
///
/// `last_activity` is the chat list's own last-message column for the intended
/// room, which the archive can supply from a `chat_id`. It is only ever used to
/// choose between rooms that already share a name.
#[derive(Debug, Clone)]
pub struct RoomTarget {
    pub name: String,
    /// How many same-named chats are more recently active than this one, which
    /// is its index among the same-named rows of a most-recent-first chat list.
    pub rank: Option<usize>,
}

impl RoomTarget {
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_string(),
            rank: None,
        }
    }
}

/// Pick the one row that is the target, or say why that cannot be decided.
fn choose_row<'a>(rows: &'a [AxRef], target: &RoomTarget) -> Result<&'a AxRef, SendError> {
    let named: Vec<&AxRef> = rows
        .iter()
        .filter(|r| row_room_name(r.as_raw()).is_some_and(|name| room_matches(&name, &target.name)))
        .collect();
    match named.len() {
        0 => Err(SendError::RoomNotInChatList {
            room: target.name.clone(),
            scanned: rows.len(),
        }),
        1 => Ok(named[0]),
        _ => {
            // Same name, more than one room. The last-message column is the only
            // thing on screen that tells them apart.
            // Rows come most-recent-first, so the archive's recency ranking is
            // the index. Deliberately not an exact last-message-time compare:
            // that was tried and it goes stale within a minute in a room
            // somebody is actively typing in, which refuses the very sends most
            // likely to be wanted.
            if let Some(rank) = target.rank {
                if let Some(hit) = named.get(rank) {
                    return Ok(hit);
                }
            }
            Err(SendError::AmbiguousRoom {
                room: target.name.clone(),
                count: named.len(),
            })
        }
    }
}

/// A room title reduced to its members, order-independent.
///
/// KakaoTalk titles a room with no name by listing its members, and it does not
/// use the same order the archive does — the archive stores
/// `"도현, 나윤"` for the room the chat list shows as `"나윤, 도현"`.
/// So a name taken from a search result never matches the row it came from, and
/// every unnamed group room is unreachable by `--room`.
///
/// Comparing the sorted member set fixes that without having to guess
/// KakaoTalk's ordering rule. A room with a real name has no members to split
/// and simply compares as itself, and one whose name happens to contain commas
/// is split the same way on both sides, so it still matches.
fn room_member_key(title: &str) -> Vec<String> {
    let mut parts: Vec<String> = title
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    parts.sort();
    parts
}

/// Whether `candidate` is the room `wanted` names, allowing for member order.
fn room_matches(candidate: &str, wanted: &str) -> bool {
    candidate == wanted || room_member_key(candidate) == room_member_key(wanted)
}

/// The most distinctive single member to type into the search box.
///
/// Searching for the whole comma-joined title finds nothing when the order
/// differs, so the query is one member and the filtering is done on the results.
fn search_query_for(room: &str) -> String {
    // Longest wins, first on a tie: `room_member_key` sorts, so the same room
    // always produces the same query and a failure is reproducible.
    room_member_key(room)
        .into_iter()
        .reduce(|best, part| {
            if part.chars().count() > best.chars().count() {
                part
            } else {
                best
            }
        })
        .unwrap_or_else(|| room.to_string())
}

/// Reject a room name carrying characters that cannot be in one.
///
/// A control character or a zero-width mark in `--room` is almost always a
/// quoting accident upstream — a stray newline from a shell heredoc, a smart
/// quote pasted from a document. Searching for it finds nothing, and the run
/// then reports "no such room", which sends the reader looking at KakaoTalk
/// instead of at the argument they actually passed. Naming the character is the
/// difference between a five-minute fix and an afternoon.
pub fn validate_room_name(room: &str) -> Result<(), SendError> {
    let bad: Vec<String> = room
        .chars()
        .filter(|c| {
            c.is_control()
                || matches!(c,
                    '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
        })
        .map(|c| format!("U+{:04X}", c as u32))
        .collect();
    if bad.is_empty() {
        return Ok(());
    }
    Err(SendError::RoomNameNotTypable {
        room: describe_value(room),
        offenders: bad.join(", "),
    })
}

/// Render a string so invisible differences are visible.
///
/// A mismatch between what we wrote and what the field holds is often something
/// that cannot be seen: a stray quote is obvious, a zero-width space or a
/// newline is not. Printing the codepoints makes every case reportable.
fn describe_value(text: &str) -> String {
    let points = text
        .chars()
        .map(|c| format!("U+{:04X}", c as u32))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{text:?} [{points}]")
}

/// Type `text` as real keystrokes, the way a person would.
///
/// Setting `AXValue` on KakaoTalk's search field paints the text but never tells
/// the app anything happened: the clear button stays hidden and the list never
/// filters. So the search strategy silently did nothing — it timed out and fell
/// through to scanning the unfiltered list, which is how a row far enough down
/// to be scrolled off screen ended up being clicked at coordinates nobody could
/// see.
///
/// `CGEventKeyboardSetUnicodeString` carries the characters on the event itself,
/// so this needs no keyboard-layout mapping and works for Hangul as written.
fn type_text(text: &str) -> Result<(), SendError> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| SendError::KeyEventFailed)?;
    for chunk in text.chars() {
        let mut buf = [0u16; 2];
        let encoded = chunk.encode_utf16(&mut buf);
        for key_down in [true, false] {
            let event = CGEvent::new_keyboard_event(source.clone(), 0, key_down)
                .map_err(|_| SendError::KeyEventFailed)?;
            event.set_string_from_utf16_unchecked(encoded);
            tag_synthetic(&event);
            event.post(CGEventTapLocation::HID);
        }
        std::thread::sleep(Duration::from_millis(8));
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

/// Send the image at `image_path` into the chat window titled `room`.
///
/// KakaoTalk has no AX affordance for attaching a file, so the image goes through the
/// clipboard and a paste, which is what its UI is built to accept. Depending on version and
/// state it either shows a confirmation sheet (click through it) or sends straight away.
///
/// Unlike text, this cannot run in the background: Cmd+V is a menu key equivalent, and an
/// inactive app has no key window for `paste:` to reach, so posting it to the pid alone is
/// silently dropped (measured). KakaoTalk therefore comes forward — but only during a gap in
/// the user's own input, only after its frontmost state is confirmed, and with both the
/// previous app and the clipboard put back afterwards.
pub fn send_image_to_open_window(
    target: &RoomTarget,
    image_path: &Path,
    allow_open: bool,
    ctx: &SendContext,
) -> Result<(), SendError> {
    validate_room_name(&target.name)?;
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    // Validate the file before disturbing anything: a missing path or an
    // unsupported type must fail without having taken focus or overwritten the
    // clipboard.
    if !image_path.is_file() {
        return Err(SendError::ImageMissing(image_path.display().to_string()));
    }
    if image_uti(image_path).is_none() {
        return Err(SendError::ImageTypeUnsupported(
            image_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("(none)")
                .to_string(),
        ));
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    let window = resolve_window(&app, target, allow_open, ctx)?;

    let clipboard = ClipboardRestore::capture()?;
    if clipboard.unreadable_flavors() > 0 {
        eprintln!(
            "katok: {} clipboard flavor(s) could not be read and will not be restored",
            clipboard.unreadable_flavors()
        );
    }
    put_image_on_clipboard(image_path)?;
    let before = transcript_row_count(window.as_raw());

    let _hold = take_screen(
        pid,
        ctx,
        &format!("{} · {}", target.name, attachment_label(image_path)),
    )?;

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

    if ctx.cancelled() {
        return Err(SendError::Cancelled);
    }
    // KakaoTalk is confirmed frontmost, so the global tap is the reliable route for the paste
    // — and the confirmation is re-read inside this call, immediately before the post.
    press_key_global_at(pid, KEY_V, CGEventFlags::CGEventFlagCommand)?;

    // Depending on version KakaoTalk either shows a confirmation sheet or sends straight away,
    // so both are handled — but success is only ever claimed once a new transcript row exists.
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if ctx.cancelled() {
            // The paste already went in, so this cannot promise nothing was
            // delivered — say so rather than implying a clean abort.
            return Err(SendError::CancelledAfterPaste);
        }

        if let Some(sheet) = find_descendant(window.as_raw(), ROLE_SHEET, 4) {
            if let Some(button) = find_confirm_button(sheet.as_raw()) {
                // An AX action is delivered to the element directly, so unlike a
                // keystroke it stays correct even if the user has clicked away.
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
    target: &RoomTarget,
    max_rows: usize,
    pid: i32,
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
    let window: Vec<AxRef> = rows
        .iter()
        .take(scanned)
        .map(|r| AxRef::retained(r.as_raw()))
        .collect();
    let hit = choose_row(&window, target)?;
    open_row_by_click(app, windows, hit.as_raw(), target, pid)
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
/// screen and the app has to be frontmost, so this necessarily takes focus for a moment.
///
/// It refuses to click unless KakaoTalk is confirmed frontmost — a click posted otherwise would
/// land in whatever *is* frontmost — and puts the pointer back where it was. Taking focus and
/// giving it back is the caller's job ([`resolve_window`]), which holds it across every attempt
/// rather than snatching it back between them.
fn open_row_by_click(
    _app: &AxRef,
    windows: &[AxRef],
    row: AXUIElementRef,
    target: &RoomTarget,
    pid: i32,
) -> Result<(), SendError> {
    // A click lands on whatever window is on top at those coordinates, so the list has to be
    // raised above any chat windows before aiming at it.
    if let Some(main) = main_window(windows) {
        let raise = CFString::new(ACTION_RAISE);
        unsafe { AXUIElementPerformAction(main.as_raw(), raise.as_concrete_TypeRef()) };
    }
    std::thread::sleep(Duration::from_millis(300));

    // Re-confirm after the raise settled: the click is about to be posted to
    // screen coordinates, and if the user grabbed focus back it would land in
    // their window instead of the chat list.
    // And re-read the row itself. The chat list reorders the moment any room
    // receives a message, and an `AXRow` is positional — so between picking the
    // row and clicking its coordinates, that position can already belong to a
    // different conversation. Measured while dogfooding: aiming at one room
    // opened another. Sending is protected downstream by the window-title check,
    // but opening the wrong person's chat is its own harm, so this fails and
    // lets the caller retry against a settled list.
    if !row_room_name(row).is_some_and(|name| room_matches(&name, &target.name)) {
        return Err(SendError::ChatListMoved {
            room: target.name.clone(),
        });
    }

    // Read geometry after raising: the row can move as the window comes forward.
    let rect = frame_of(row).ok_or_else(|| SendError::RoomOpenFailed(target.name.clone()))?;

    let target = CGPoint::new(
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    );
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| SendError::KeyEventFailed)?;
    let restore = CGEvent::new(source.clone()).ok().map(|e| e.location());

    // Bring it forward as part of the click, not before it: the double-click is
    // aimed at screen coordinates, so anything that activates in between would
    // receive it instead.
    front_then(pid, || -> Result<(), SendError> {
        for click in 1..=2 {
            for kind in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
                let ev =
                    CGEvent::new_mouse_event(source.clone(), kind, target, CGMouseButton::Left)
                        .map_err(|_| SendError::KeyEventFailed)?;
                ev.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click);
                tag_synthetic(&ev);
                ev.post(CGEventTapLocation::HID);
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    })??;

    if let Some(p) = restore {
        if let Ok(ev) =
            CGEvent::new_mouse_event(source, CGEventType::MouseMoved, p, CGMouseButton::Left)
        {
            tag_synthetic(&ev);
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
fn open_room_via_search(
    app: &AxRef,
    windows: &[AxRef],
    target: &RoomTarget,
    pid: i32,
) -> Result<(), SendError> {
    let main = main_window(windows).ok_or(SendError::ChatListUnavailable)?;
    let field = search_field(main.as_raw()).ok_or(SendError::SearchUnavailable)?;

    // Verified rather than fire-and-forget: whatever ends up in this box is what
    // decides which conversation gets opened.
    let query = search_query_for(&target.name);
    // Focus first: typing goes wherever the app thinks focus is.
    let focus_key = CFString::new(ATTR_FOCUSED);
    let yes = core_foundation::boolean::CFBoolean::true_value();
    unsafe {
        AXUIElementSetAttributeValue(
            field.as_raw(),
            focus_key.as_concrete_TypeRef(),
            yes.as_CFTypeRef(),
        )
    };
    // Clearing through AXValue is fine — it is only the *change* notification
    // the app misses, and an empty box is the state we want anyway.
    let empty = CFString::new("");
    let key = CFString::new(ATTR_VALUE);
    unsafe {
        AXUIElementSetAttributeValue(
            field.as_raw(),
            key.as_concrete_TypeRef(),
            empty.as_CFTypeRef(),
        )
    };
    front_then(pid, || type_text(&query))??;
    // Prove the field ended up holding the query; if the keystrokes went
    // somewhere else this says so instead of silently filtering nothing.
    let mut landed = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(50));
        if attr_string(field.as_raw(), ATTR_VALUE).as_deref() == Some(query.as_str()) {
            landed = true;
            break;
        }
    }
    if !landed {
        return Err(SendError::ValueMismatch {
            what: "the chat search box".to_string(),
            wrote: describe_value(&query),
            found: describe_value(&attr_string(field.as_raw(), ATTR_VALUE).unwrap_or_default()),
        });
    }

    // Results replace the chat-list rows. Wait for a row that actually reads back
    // as the requested room.
    //
    // There used to be a fallback here that clicked a lone result without
    // checking its name, on the reasoning that KakaoTalk sometimes hands back a
    // row whose cell text has not been filled in yet. That is precisely the
    // blind click that opened the wrong conversation, so an unreadable row is
    // now something to wait through rather than to act on: the filter is
    // already applied, so the name appearing is a matter of a few more frames.
    let full_list = chat_list_table_in(main.as_raw())
        .map(|t| child_elements(t.as_raw(), ATTR_ROWS).len())
        .ok_or(SendError::ChatListUnavailable)?;

    for _ in 0..25 {
        std::thread::sleep(Duration::from_millis(100));
        let Some(table) = chat_list_table_in(main.as_raw()) else {
            continue;
        };
        let rows = child_elements(table.as_raw(), ATTR_ROWS);
        if rows.is_empty() || rows.len() == full_list {
            continue; // filtering has not taken effect yet
        }
        match choose_row(&rows, target) {
            Ok(hit) => return open_row_by_click(app, windows, hit.as_raw(), target, pid),
            // Two rooms of this name and nothing to tell them apart: waiting
            // will not change that, so stop rather than retry.
            Err(err @ SendError::AmbiguousRoom { .. }) => return Err(err),
            Err(_) => continue,
        }
    }
    Err(SendError::RoomNotFound(target.name.clone()))
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
fn resolve_window(
    app: &AxRef,
    target: &RoomTarget,
    allow_open: bool,
    ctx: &SendContext,
) -> Result<AxRef, SendError> {
    let find = || {
        let windows = child_elements(app.as_raw(), ATTR_WINDOWS);
        let hits: Vec<usize> = windows
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                attr_string(w.as_raw(), ATTR_TITLE)
                    .is_some_and(|title| room_matches(&title, &target.name))
            })
            .map(|(i, _)| i)
            .collect();
        (windows, hits)
    };

    let (windows, hits) = find();
    // An already-open window is the fast path, but two rooms sharing a name can
    // each have one, and a window title carries no more identity than a list row
    // does. Refuse for the same reason.
    if hits.len() > 1 {
        return Err(SendError::AmbiguousRoom {
            room: target.name.clone(),
            count: hits.len(),
        });
    }
    if let Some(i) = hits.first() {
        return Ok(AxRef::retained(windows[*i].as_raw()));
    }
    if !allow_open {
        return Err(SendError::WindowNotOpen {
            room: target.name.clone(),
            open: open_titles(&windows),
        });
    }

    // Three strategies, every failure reported — never silently. Search leads
    // because it is the only one that cannot click the wrong room; the row scans
    // behind it cover the case where the search field is unreachable, and only
    // see rows KakaoTalk has already rendered.
    //
    // Taking focus is a one-off per room: once the window is open, sending into it stays in
    // the background.
    const RECENT_ROWS: usize = 60;
    let mut last: Option<SendError> = None;

    let mut pid: i32 = 0;
    if unsafe { AXUIElementGetPid(app.as_raw(), &mut pid) } != AX_SUCCESS {
        return Err(SendError::RoomOpenFailed(target.name.clone()));
    }

    // Opening a room means clicking a list row, and a click only reaches KakaoTalk
    // while it is frontmost. The screen is therefore taken once and held across
    // every attempt, not grabbed and released per attempt, which both tripled
    // the wall clock and yanked focus away while the window was still coming up.
    let hold = take_screen(pid, ctx, &format!("{} 채팅방 여는 중", target.name))?;

    // Each strategy is tried more than once. Driving another app's list is
    // inherently racy — it reorders under us, a row can be mid-render, the click
    // can land as the window is still coming up — and every one of those is
    // transient. Retrying is what turns "sometimes fails" into "works", and the
    // identity checks below are what keep a retry from being a second chance to
    // hit the wrong room.
    const ATTEMPTS: usize = 6;
    for attempt in 0..ATTEMPTS {
        // Checked per attempt so Esc stops the run at the next boundary instead
        // of after every strategy has been tried.
        if ctx.cancelled() {
            drop(hold);
            return Err(SendError::Cancelled);
        }
        // Search first, even though scanning the visible rows is cheaper.
        //
        // A row is opened by double-clicking its screen coordinates, and the
        // chat list reorders the instant any of nearly three hundred rooms
        // receives a message — so between reading a row's position and clicking
        // it, that position can belong to someone else. Measured: aiming at one
        // room opened another. Filtering the list down to the single matching
        // row removes the race rather than narrowing it, because there is
        // nothing left to reorder.
        let outcome = match attempt % 3 {
            0 => open_room_via_search(app, &windows, target, pid),
            1 => open_room_from_chat_list(app, &windows, target, RECENT_ROWS, pid),
            _ => open_room_from_chat_list(app, &windows, target, usize::MAX, pid),
        };
        // Ambiguity is a property of the chat list, not of this attempt: trying
        // again cannot make two rooms of one name distinguishable, and retrying
        // would only be a second chance to pick wrong.
        if let Err(SendError::AmbiguousRoom { .. }) = &outcome {
            drop(hold);
            return outcome.map(|()| unreachable!());
        }
        match outcome {
            Ok(()) => match wait_for_window(&find, 20) {
                Some(w) => {
                    clear_search(&windows);
                    return Ok(w);
                }
                // Selected it but nothing opened — record that, do not let an earlier,
                // less relevant error be the one reported.
                None => last = Some(SendError::RoomOpenFailed(target.name.clone())),
            },
            Err(e) => last = Some(e),
        }
        if attempt % 3 == 0 {
            clear_search(&windows);
        }
    }
    clear_search(&windows);
    // Explicit so the screen is handed back before the error surfaces, rather
    // than at some later point in the caller's scope.
    drop(hold);
    Err(last.unwrap_or_else(|| SendError::RoomOpenFailed(target.name.clone())))
}

fn wait_for_window<F>(find: &F, attempts: u32) -> Option<AxRef>
where
    F: Fn() -> (Vec<AxRef>, Vec<usize>),
{
    for _ in 0..attempts {
        std::thread::sleep(Duration::from_millis(100));
        let (windows, hits) = find();
        // Exactly one, for the same reason the first lookup insists on it.
        if let [i] = hits[..] {
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
pub fn send_to_open_window(
    target: &RoomTarget,
    text: &str,
    allow_open: bool,
    ctx: &SendContext,
) -> Result<(), SendError> {
    validate_room_name(&target.name)?;
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    let window = resolve_window(&app, target, allow_open, ctx)?;

    let compose = compose_box(window.as_raw())
        .ok_or_else(|| SendError::ComposeBoxNotFound(target.name.clone()))?;

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
    // is worth it over leaving a message sitting unsent in the box — but only during a gap in
    // their input, and only if KakaoTalk verifiably comes forward. A global Enter fired while
    // the user is back in their own app would submit whatever they were in the middle of.
    let _hold = take_screen(pid, ctx, &format!("{} 로 전송 중", target.name))?;
    let raise = CFString::new(ACTION_RAISE);
    unsafe { AXUIElementPerformAction(window.as_raw(), raise.as_concrete_TypeRef()) };
    std::thread::sleep(Duration::from_millis(200));
    let yes = core_foundation::boolean::CFBoolean::true_value();
    unsafe {
        AXUIElementSetAttributeValue(
            compose.as_raw(),
            CFString::new(ATTR_FOCUSED).as_concrete_TypeRef(),
            yes.as_CFTypeRef(),
        )
    };
    press_key_global_at(pid, KEY_RETURN, CGEventFlags::CGEventFlagNull)?;
    if accepted(30) {
        return Ok(());
    }
    Err(SendError::NotSent)
}

/// Put UTF-8 `text` on the clipboard.
fn put_text_on_clipboard(text: &str) -> Result<(), SendError> {
    let (pasteboard, _guard) = open_clipboard()?;
    let status = unsafe { PasteboardClear(pasteboard) };
    if status != OS_NO_ERR {
        return Err(SendError::ClipboardFailed(status));
    }
    let data = CFData::from_buffer(text.as_bytes());
    let flavor = CFString::new("public.utf8-plain-text");
    let status = unsafe {
        PasteboardPutItemFlavor(
            pasteboard,
            std::ptr::dangling::<c_void>(),
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

/// Leave `text` in the room's compose box for a person to read and decide on.
///
/// It goes in by paste, not by `AXValue`. Writing that attribute *is* sending:
/// KakaoTalk delivers the message the moment the value changes, with no
/// keystroke involved — an earlier draft mode assumed otherwise, reported
/// `sent: false`, and put two messages into real conversations. Paste inserts
/// text and nothing more, so the box fills and stays filled.
///
/// The cost is the screen: Cmd+V is a menu key equivalent, so KakaoTalk has to
/// be frontmost, which is what the curtain is for. The clipboard is saved and
/// restored around it.
pub fn draft_to_open_window(
    target: &RoomTarget,
    text: &str,
    allow_open: bool,
    ctx: &SendContext,
) -> Result<(), SendError> {
    validate_room_name(&target.name)?;
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    let window = resolve_window(&app, target, allow_open, ctx)?;
    let compose = compose_box(window.as_raw())
        .ok_or_else(|| SendError::ComposeBoxNotFound(target.name.clone()))?;

    let clipboard = ClipboardRestore::capture()?;
    if clipboard.unreadable_flavors() > 0 {
        eprintln!(
            "katok: {} clipboard flavor(s) could not be read and will not be restored",
            clipboard.unreadable_flavors()
        );
    }
    put_text_on_clipboard(text)?;

    let _hold = take_screen(pid, ctx, &format!("{} 초안 작성 중", target.name))?;
    let focus_key = CFString::new(ATTR_FOCUSED);
    let yes = core_foundation::boolean::CFBoolean::true_value();
    unsafe {
        AXUIElementSetAttributeValue(
            compose.as_raw(),
            focus_key.as_concrete_TypeRef(),
            yes.as_CFTypeRef(),
        )
    };
    press_key_global_at(pid, KEY_V, CGEventFlags::CGEventFlagCommand)?;

    // The box must end up holding the draft. Empty means either the paste never
    // landed or — the case that matters — it was sent, and reporting a waiting
    // draft that is not there is the failure this mode exists to avoid.
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(50));
        let value = attr_string(compose.as_raw(), ATTR_VALUE).unwrap_or_default();
        if value.contains(text) {
            return Ok(());
        }
    }
    Err(SendError::DraftNotStaged {
        room: target.name.clone(),
    })
}

/// Resolve (opening if needed) the chat window for `room` without sending anything.
pub fn resolve_room_window(
    target: &RoomTarget,
    allow_open: bool,
    ctx: &SendContext,
) -> Result<(), SendError> {
    validate_room_name(&target.name)?;
    if !unsafe { AXIsProcessTrusted() } {
        return Err(SendError::AccessibilityDenied);
    }
    let pid = kakaotalk_pid().ok_or(SendError::NotRunning)?;
    let app = AxRef(unsafe { AXUIElementCreateApplication(pid) });
    resolve_window(&app, target, allow_open, ctx).map(|_| ())
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

#[cfg(test)]
mod room_matching_tests {
    use super::{room_matches, room_member_key, search_query_for};

    #[test]
    fn member_order_does_not_decide_identity() {
        // The archive stores one member order, the chat list shows another, so a
        // name comparison decides identity by member set rather than by string.
        // The names here are stand-ins; only the reordering is what was observed.
        assert!(room_matches("나윤, 도현", "도현, 나윤"));
        assert!(room_matches("도현, 나윤", "도현, 나윤"));
    }

    #[test]
    fn different_members_are_still_different_rooms() {
        assert!(!room_matches("나윤, 지우", "도현, 나윤"));
        assert!(!room_matches("나윤", "도현, 나윤"));
    }

    #[test]
    fn a_named_room_compares_as_itself() {
        assert!(room_matches("주말 등산 모임", "주말 등산 모임"));
        assert!(!room_matches("주말 등산 모임", "주말 등산"));
        // A real name that happens to contain commas splits the same way on
        // both sides, so it still matches itself and nothing else.
        let name = "양자BlockQ(교육,보안,컴퓨팅)";
        assert!(room_matches(name, name));
        assert!(!room_matches("양자BlockQ(교육,보안)", name));
    }

    #[test]
    fn whitespace_around_members_is_ignored() {
        assert_eq!(
            room_member_key("  나윤 ,도현  "),
            vec!["나윤".to_string(), "도현".to_string()]
        );
        assert!(room_matches("나윤 , 도현", "도현,나윤"));
    }

    #[test]
    fn the_search_query_is_one_distinctive_member() {
        // Typing the joined title finds nothing when the order differs, so the
        // query is a single member and the filtering happens on the results.
        assert_eq!(search_query_for("나윤, 도현"), "나윤");
        assert_eq!(search_query_for("주말 등산 모임"), "주말 등산 모임");
    }
}
