//! A full-screen curtain that makes a send atomic with respect to the user.
//!
//! Sending an image, or opening a closed room, has to bring KakaoTalk forward
//! (see [`super::ax_send`]). The old failure was not the stolen focus itself but
//! what happened when the user kept working through it: a click elsewhere during
//! the activation window sent the paste into *their* document.
//!
//! Rather than hoping the collision does not happen, this removes the
//! possibility. For the second or two the send needs the screen, a session-level
//! `CGEventTap` swallows real keyboard and mouse input while letting through the
//! events this process synthesizes, which are tagged with
//! [`SYNTHETIC_EVENT_TAG`] in `kCGEventSourceUserData`.
//!
//! **Blocking input is only safe if the user can see it.** Swallowed keystrokes
//! are dropped, not queued, so an invisible block would silently eat whatever
//! someone typed into it — worse than the problem being solved. The curtain is
//! therefore not decoration: it is the thing that makes people take their hands
//! off the keyboard. There is deliberately no "block without showing" mode.
//!
//! Two constraints shape the implementation:
//!
//! - The curtain windows must never become key. Our process activating would
//!   take frontmost away from KakaoTalk and the paste would die — which is the
//!   whole reason for the activation policy being `Accessory` and the windows
//!   being non-activating panels.
//! - Because the windows never become key, they cannot receive keystrokes, so
//!   the tap itself is the only input authority. Cancellation (Esc, or a click
//!   on the cancel button) is decided inside the tap callback, not by AppKit.
//!
//! AppKit demands the main thread, so [`run_with_curtain`] keeps the UI and the
//! tap on the main run loop and moves the caller's work to a worker thread.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use core_foundation::runloop::{kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};
use core_graphics::geometry::CGPoint;
use objc2::rc::{autoreleasepool, Retained};
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSFont, NSScreen,
    NSTextAlignment, NSTextField, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowLevel,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// Marker written into `kCGEventSourceUserData` on every event this process
/// synthesizes, so the tap can let our own keystrokes through while dropping the
/// user's. Verified to survive the round trip through a session tap.
pub const SYNTHETIC_EVENT_TAG: i64 = 0x4B41_544F_4B01;

const ESC_KEYCODE: i64 = 53;

/// Stamp an event as ours before posting it.
pub fn tag_synthetic(event: &CGEvent) {
    event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, SYNTHETIC_EVENT_TAG);
}

fn is_synthetic(event: &CGEvent) -> bool {
    event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == SYNTHETIC_EVENT_TAG
}

/// Shared, cheap-to-clone "the user asked to stop" flag.
#[derive(Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// What the worker thread can ask the curtain to do.
///
/// The worker never touches AppKit; it flips these and the main-thread pump
/// applies them.
pub struct CurtainControl {
    /// Raised only while the send genuinely needs the screen, so an ordinary
    /// background text send never covers anything.
    shown: AtomicBool,
    subtitle: Mutex<String>,
    revision: AtomicUsize,
    cancel: CancelFlag,
}

impl CurtainControl {
    /// Raise the curtain and start swallowing user input.
    pub fn show(&self, subtitle: &str) {
        *self.subtitle.lock().expect("curtain subtitle") = subtitle.to_string();
        self.revision.fetch_add(1, Ordering::SeqCst);
        self.shown.store(true, Ordering::SeqCst);
        // Give the main-thread pump a moment to actually put it up, so input is
        // already blocked by the time the caller starts driving the UI.
        std::thread::sleep(Duration::from_millis(120));
    }

    /// Lower the curtain and hand input back.
    pub fn hide(&self) {
        self.shown.store(false, Ordering::SeqCst);
    }

    pub fn cancel_flag(&self) -> CancelFlag {
        self.cancel.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// Rect of the cancel button in global display coordinates (origin top-left),
/// which is the space `CGEvent::location` reports in.
#[derive(Clone, Copy, Default)]
struct CancelRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl CancelRect {
    fn contains(&self, point: CGPoint) -> bool {
        self.w > 0.0
            && point.x >= self.x
            && point.x <= self.x + self.w
            && point.y >= self.y
            && point.y <= self.y + self.h
    }
}

/// Run `work` on a worker thread with a curtain available to it.
///
/// Returns whatever `work` returned. The curtain is always taken down and input
/// always handed back, including when `work` panics — the tap and the windows
/// are owned by this function's scope, not by the worker.
pub fn run_with_curtain<T, F>(title: &str, work: F) -> std::thread::Result<T>
where
    F: FnOnce(Arc<CurtainControl>) -> T + Send + 'static,
    T: Send + 'static,
{
    let Some(mtm) = MainThreadMarker::new() else {
        // Without the main thread there is no AppKit, so run unguarded rather
        // than refusing outright; every caller still checks frontmost itself.
        let control = Arc::new(CurtainControl {
            shown: AtomicBool::new(false),
            subtitle: Mutex::new(String::new()),
            revision: AtomicUsize::new(0),
            cancel: CancelFlag::new(),
        });
        return Ok(work(control));
    };

    let control = Arc::new(CurtainControl {
        shown: AtomicBool::new(false),
        subtitle: Mutex::new(String::new()),
        revision: AtomicUsize::new(0),
        cancel: CancelFlag::new(),
    });

    let cancel_rect = Arc::new(Mutex::new(CancelRect::default()));
    let _tap = install_input_tap(control.cancel.clone(), Arc::clone(&cancel_rect));

    let mut curtain = autoreleasepool(|_| CurtainWindows::new(mtm, title));

    let worker_control = Arc::clone(&control);
    let done = Arc::new(AtomicBool::new(false));
    let worker_done = Arc::clone(&done);
    let handle = std::thread::spawn(move || {
        let result = work(worker_control);
        worker_done.store(true, Ordering::SeqCst);
        result
    });

    let mut visible = false;
    let mut last_revision = 0usize;
    let started = Instant::now();
    while !done.load(Ordering::SeqCst) {
        let want = control.shown.load(Ordering::SeqCst);
        let revision = control.revision.load(Ordering::SeqCst);
        autoreleasepool(|_| {
            if want && (!visible || revision != last_revision) {
                let subtitle = control.subtitle.lock().expect("curtain subtitle").clone();
                curtain.show(&subtitle, started.elapsed());
                *cancel_rect.lock().expect("cancel rect") = curtain.cancel_rect();
                last_revision = revision;
                visible = true;
            } else if !want && visible {
                curtain.hide();
                *cancel_rect.lock().expect("cancel rect") = CancelRect::default();
                visible = false;
            } else if visible {
                curtain.tick(started.elapsed());
            }
        });
        // Servicing this run loop is what delivers tap callbacks and lets the
        // windows draw; the timeout doubles as the animation frame interval.
        //
        // The mode has to be a real one. `kCFRunLoopCommonModes` is a pseudo-mode
        // that only means anything when *adding* a source; passing it here makes
        // CFRunLoopRunInMode reject the call and return immediately, which turns
        // this loop into a busy spin that never draws the curtain or delivers a
        // single tap callback.
        CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            Duration::from_millis(80),
            true,
        );
    }

    autoreleasepool(|_| curtain.hide());
    handle.join()
}

/// Swallow real user input for as long as the returned tap is alive.
///
/// Returns `None` when the tap cannot be created (no accessibility permission).
/// That is reported by the caller rather than silently proceeding unguarded:
/// see [`run_with_curtain`], where the send still verifies frontmost itself.
fn install_input_tap(
    cancel: CancelFlag,
    cancel_rect: Arc<Mutex<CancelRect>>,
) -> Option<TapGuard<'static>> {
    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGEventType::ScrollWheel,
        ],
        move |_, event_type, event| {
            // Our own synthesized input is what the send is made of; dropping it
            // would block the very paste this is protecting.
            if is_synthetic(event) {
                return CallbackResult::Keep;
            }
            let armed = cancel_rect.lock().map(|rect| rect.w > 0.0).unwrap_or(false);
            if !armed {
                // Curtain is down: this is an ordinary moment in the user's day.
                return CallbackResult::Keep;
            }
            match event_type {
                CGEventType::KeyDown => {
                    if event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE)
                        == ESC_KEYCODE
                    {
                        cancel.cancel();
                    }
                }
                CGEventType::LeftMouseDown => {
                    let inside = cancel_rect
                        .lock()
                        .map(|rect| rect.contains(event.location()))
                        .unwrap_or(false);
                    if inside {
                        cancel.cancel();
                    }
                }
                _ => {}
            }
            CallbackResult::Drop
        },
    )
    .ok()?;

    let source = tap.mach_port().create_runloop_source(0).ok()?;
    let run_loop = CFRunLoop::get_current();
    unsafe { run_loop.add_source(&source, kCFRunLoopCommonModes) };
    tap.enable();
    Some(TapGuard {
        _tap: tap,
        source,
        run_loop,
    })
}

/// Removes the tap from the run loop on drop, so input comes back even on an
/// error path or a panic.
struct TapGuard<'a> {
    _tap: CGEventTap<'a>,
    source: core_foundation::runloop::CFRunLoopSource,
    run_loop: CFRunLoop,
}

impl Drop for TapGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            self.run_loop
                .remove_source(&self.source, kCFRunLoopCommonModes)
        };
    }
}

const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
const CANCEL_LABEL: &str = "취소 (Esc)";

struct CurtainWindows {
    windows: Vec<Retained<NSWindow>>,
    title_label: Option<Retained<NSTextField>>,
    subtitle_label: Option<Retained<NSTextField>>,
    cancel_label: Option<Retained<NSTextField>>,
    cancel_rect: CancelRect,
    title: String,
    visible: bool,
}

impl CurtainWindows {
    fn new(mtm: MainThreadMarker, title: &str) -> Self {
        let app = NSApplication::sharedApplication(mtm);
        // Accessory, never Regular: activating our own process would take
        // frontmost away from KakaoTalk and kill the paste.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let mut windows = Vec::new();
        let mut title_label = None;
        let mut subtitle_label = None;
        let mut cancel_label = None;
        let mut cancel_rect = CancelRect::default();

        let screens = NSScreen::screens(mtm);
        let main_height = NSScreen::mainScreen(mtm)
            .map(|screen| screen.frame().size.height)
            .unwrap_or(0.0);

        for (index, screen) in screens.iter().enumerate() {
            let frame = screen.frame();
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer_screen(
                    NSWindow::alloc(mtm),
                    frame,
                    NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                    NSBackingStoreType::Buffered,
                    false,
                    Some(&screen),
                )
            };
            window.setOpaque(false);
            window.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                0.05, 0.05, 0.07, 0.62,
            )));
            // Above normal windows and full-screen apps alike.
            window.setLevel(NSWindowLevel::from(1000isize));
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary,
            );
            // The curtain informs; the tap enforces. Hit-testing it would make
            // it swallow our *own* synthesized clicks — it is the topmost window,
            // so a click aimed at a chat-list row would land on the curtain
            // instead of KakaoTalk and the room would never open. The user's
            // clicks are already dropped upstream by the tap, and the cancel
            // button is handled there too, so nothing needs this window to be
            // clickable.
            window.setIgnoresMouseEvents(true);

            // Only the primary screen carries the text; the others just dim.
            if index == 0 {
                let content: Retained<NSView> = window.contentView().expect("curtain content view");
                let width = frame.size.width;
                let mid_y = frame.size.height / 2.0;

                let title_field = label(mtm, title, 34.0, 1.0);
                title_field.setFrame(NSRect::new(
                    NSPoint::new(0.0, mid_y + 26.0),
                    NSSize::new(width, 48.0),
                ));
                content.addSubview(&title_field);

                let subtitle_field = label(mtm, "", 17.0, 0.72);
                subtitle_field.setFrame(NSRect::new(
                    NSPoint::new(0.0, mid_y - 6.0),
                    NSSize::new(width, 26.0),
                ));
                content.addSubview(&subtitle_field);

                let cancel_field = label(mtm, CANCEL_LABEL, 15.0, 0.85);
                let cancel_w = 176.0;
                let cancel_h = 40.0;
                let cancel_x = (width - cancel_w) / 2.0;
                let cancel_y = mid_y - 86.0;
                cancel_field.setFrame(NSRect::new(
                    NSPoint::new(cancel_x, cancel_y),
                    NSSize::new(cancel_w, cancel_h),
                ));
                cancel_field.setDrawsBackground(true);
                cancel_field.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                    1.0, 1.0, 1.0, 0.14,
                )));
                content.addSubview(&cancel_field);

                // CGEvent locations use a top-left origin; Cocoa frames a
                // bottom-left one, so the hit rect has to be flipped.
                cancel_rect = CancelRect {
                    x: frame.origin.x + cancel_x,
                    y: main_height - (frame.origin.y + cancel_y + cancel_h),
                    w: cancel_w,
                    h: cancel_h,
                };

                title_label = Some(title_field);
                subtitle_label = Some(subtitle_field);
                cancel_label = Some(cancel_field);
            }

            windows.push(window);
        }

        Self {
            windows,
            title_label,
            subtitle_label,
            cancel_label,
            cancel_rect,
            title: title.to_string(),
            visible: false,
        }
    }

    fn cancel_rect(&self) -> CancelRect {
        if self.visible {
            self.cancel_rect
        } else {
            CancelRect::default()
        }
    }

    fn show(&mut self, subtitle: &str, elapsed: Duration) {
        if let Some(field) = &self.subtitle_label {
            field.setStringValue(&NSString::from_str(subtitle));
        }
        if let Some(field) = &self.cancel_label {
            field.setStringValue(&NSString::from_str(CANCEL_LABEL));
        }
        for window in &self.windows {
            window.orderFrontRegardless();
        }
        self.visible = true;
        self.tick(elapsed);
    }

    fn tick(&self, elapsed: Duration) {
        let Some(field) = &self.title_label else {
            return;
        };
        let frame = (elapsed.as_millis() / 90) as usize % SPINNER.len();
        let text = format!("{}  {}", SPINNER[frame], self.title);
        field.setStringValue(&NSString::from_str(&text));
    }

    fn hide(&mut self) {
        for window in &self.windows {
            window.orderOut(None);
        }
        self.visible = false;
    }
}

fn label(mtm: MainThreadMarker, text: &str, size: f64, alpha: f64) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setAlignment(NSTextAlignment::Center);
    field.setFont(Some(&NSFont::systemFontOfSize(size)));
    field.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        1.0, 1.0, 1.0, alpha,
    )));
    field.setDrawsBackground(false);
    field
}

/// Short description of an attachment for the curtain subtitle.
///
/// The file name only — never the path, which would put directory structure on
/// a screen the user might be sharing.
pub fn attachment_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("첨부 파일")
        .to_string()
}
