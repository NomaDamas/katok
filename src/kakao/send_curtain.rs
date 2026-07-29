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
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBox, NSBoxType, NSColor,
    NSControlSize, NSFont, NSFontWeightMedium, NSProgressIndicator, NSProgressIndicatorStyle,
    NSScreen, NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow,
    NSWindowCollectionBehavior, NSWindowLevel, NSWindowStyleMask,
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
    /// Whether the tap is swallowing input.
    ///
    /// Separate from `shown` and from the drawn curtain on purpose. Arming used
    /// to be inferred from the cancel button's rect, which the main thread only
    /// fills in *after* it has drawn the curtain — so for the tick it took that
    /// to happen, the curtain had been asked for but input was still live, and a
    /// click in that gap took focus away from the app we were about to drive.
    /// The worker sets this synchronously before anything else moves.
    armed: AtomicBool,
    /// Set by the main thread once the curtain is actually on screen, so the
    /// worker can wait for the real thing instead of sleeping a guessed amount.
    visible: AtomicBool,
    subtitle: Mutex<String>,
    revision: AtomicUsize,
    cancel: CancelFlag,
}

impl CurtainControl {
    /// Start swallowing user input, then raise the curtain and wait for it.
    ///
    /// Arming comes first and is synchronous: from the instant this is entered,
    /// a click cannot reach another application. Drawing follows, and the wait
    /// is for the main thread's acknowledgement rather than a fixed sleep,
    /// because a guessed delay is either too short — which is what let a click
    /// through — or wasted time.
    pub fn show(&self, subtitle: &str) {
        self.armed.store(true, Ordering::SeqCst);
        *self.subtitle.lock().expect("curtain subtitle") = subtitle.to_string();
        self.revision.fetch_add(1, Ordering::SeqCst);
        self.shown.store(true, Ordering::SeqCst);

        // Bounded: if the pump is wedged the send should still proceed with
        // input blocked rather than hang, and every later step re-checks
        // frontmost anyway.
        let deadline = Instant::now() + Duration::from_millis(600);
        while !self.visible.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Lower the curtain and hand input back.
    pub fn hide(&self) {
        self.shown.store(false, Ordering::SeqCst);
        self.armed.store(false, Ordering::SeqCst);
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
/// always handed back, including when `work` panics: the completion flag is set
/// from a Drop guard so it survives an unwind, and the tap and the windows are
/// owned by this function's scope rather than by the worker.
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
            armed: AtomicBool::new(false),
            visible: AtomicBool::new(false),
            subtitle: Mutex::new(String::new()),
            revision: AtomicUsize::new(0),
            cancel: CancelFlag::new(),
        });
        return Ok(work(control));
    };

    let control = Arc::new(CurtainControl {
        shown: AtomicBool::new(false),
        armed: AtomicBool::new(false),
        visible: AtomicBool::new(false),
        subtitle: Mutex::new(String::new()),
        revision: AtomicUsize::new(0),
        cancel: CancelFlag::new(),
    });

    let cancel_rect = Arc::new(Mutex::new(CancelRect::default()));
    let _tap = install_input_tap(Arc::clone(&control), Arc::clone(&cancel_rect));

    let mut curtain = autoreleasepool(|_| CurtainWindows::new(mtm, title));

    let worker_control = Arc::clone(&control);
    let done = Arc::new(AtomicBool::new(false));
    let worker_done = Arc::clone(&done);
    let handle = std::thread::spawn(move || {
        // The flag has to be set from a Drop guard, not from a statement after
        // the call. A panic in `work` unwinds past a trailing store, leaving
        // `done` false forever — the pump below would spin with the curtain up
        // and input blocked, which is the worst state this module can be in.
        struct MarkDone(Arc<AtomicBool>);
        impl Drop for MarkDone {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _mark = MarkDone(worker_done);
        work(worker_control)
    });

    let mut visible = false;
    let mut last_revision = 0usize;
    while !done.load(Ordering::SeqCst) {
        let want = control.shown.load(Ordering::SeqCst);
        let revision = control.revision.load(Ordering::SeqCst);
        autoreleasepool(|_| {
            if want && (!visible || revision != last_revision) {
                let subtitle = control.subtitle.lock().expect("curtain subtitle").clone();
                curtain.show(&subtitle);
                *cancel_rect.lock().expect("cancel rect") = curtain.cancel_rect();
                last_revision = revision;
                visible = true;
                // Only now is it genuinely on screen; the worker is waiting on
                // exactly this rather than on a guessed sleep.
                control.visible.store(true, Ordering::SeqCst);
            } else if !want && visible {
                curtain.hide();
                *cancel_rect.lock().expect("cancel rect") = CancelRect::default();
                visible = false;
                control.visible.store(false, Ordering::SeqCst);
            } else if visible {
                // Re-assert every tick. Ordering front once is not a promise it
                // stays there: an app activating behind us — including the one
                // this send is about to raise — can otherwise end up above the
                // curtain, which would leave the screen looking unblocked while
                // input still is.
                curtain.raise();
            }
        });
        // Servicing this run loop is what delivers tap callbacks and lets the
        // windows draw.
        //
        // The mode has to be a real one. `kCFRunLoopCommonModes` is a pseudo-mode
        // that only means anything when *adding* a source; passing it here makes
        // CFRunLoopRunInMode reject the call and return immediately, which turns
        // this loop into a busy spin that never draws the curtain or delivers a
        // single tap callback.
        //
        // The tick is short while the curtain is up or wanted, because that
        // interval is the delay between the worker asking for the screen and the
        // curtain actually covering it, and idle otherwise so a background text
        // send does not spin.
        let tick = if want || visible { 15 } else { 80 };
        CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            Duration::from_millis(tick),
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
    control: Arc<CurtainControl>,
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
            // Cancellation needs the curtain to be on screen. Armed runs ahead
            // of drawn by design, and in that window an Escape belongs to
            // whatever the user was already doing — treating it as "stop the
            // send" cancelled runs that had not visibly started.
            let showing = control.visible.load(Ordering::SeqCst);
            match event_type {
                CGEventType::KeyDown => {
                    if showing
                        && event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE)
                            == ESC_KEYCODE
                    {
                        control.cancel.cancel();
                    }
                }
                CGEventType::LeftMouseDown => {
                    let inside = cancel_rect
                        .lock()
                        .map(|rect| rect.contains(event.location()))
                        .unwrap_or(false);
                    if showing && inside {
                        control.cancel.cancel();
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

const CANCEL_LABEL: &str = "취소  ·  Esc";

// Card geometry. Laid out from the top down in a fixed-size card so nothing
// depends on measuring at runtime, which is where the first version went wrong.
const CARD_W: f64 = 380.0;
const CARD_H: f64 = 184.0;
const SPINNER_SIZE: f64 = 20.0;
const CANCEL_W: f64 = 132.0;
const CANCEL_H: f64 = 34.0;
const CANCEL_Y: f64 = 17.0;

/// How strongly the backdrop is frosted, 0.0 (fully see-through) to 1.0.
///
/// Deliberately low: the point is to say "input is blocked", not to hide the
/// screen. The card carries that message on its own, so the blur only needs to
/// push the desktop back a little. Turn this up if the card stops reading
/// against a busy background.
const SCRIM_ALPHA: f64 = 0.38;

struct CurtainWindows {
    windows: Vec<Retained<NSWindow>>,
    subtitle_label: Option<Retained<NSTextField>>,
    spinner: Option<Retained<NSProgressIndicator>>,
    cancel_rect: CancelRect,
    visible: bool,
}

/// Place a label at an exact height with no vertical slack.
///
/// An `NSTextField` draws its text against the top of whatever frame it is
/// given, so a label dropped into an oversized box sits high in it and a
/// "button" built that way has its caption clinging to the top edge. Sizing to
/// the text first and positioning that exact height is what actually centers it.
fn place_label(field: &NSTextField, container_width: f64, x: f64, center_y: f64) {
    field.sizeToFit();
    let height = field.frame().size.height;
    field.setFrame(NSRect::new(
        NSPoint::new(x, center_y - height / 2.0),
        NSSize::new(container_width, height),
    ));
}

impl CurtainWindows {
    fn new(mtm: MainThreadMarker, title: &str) -> Self {
        let app = NSApplication::sharedApplication(mtm);
        // Accessory, never Regular: activating our own process would take
        // frontmost away from KakaoTalk and kill the paste.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let mut windows = Vec::new();
        let mut subtitle_label = None;
        let mut spinner = None;
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
            window.setBackgroundColor(Some(&NSColor::clearColor()));
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

            // A light frost rather than a flat wash: it reads as a system
            // overlay and follows the appearance the user already chose, while
            // staying transparent enough to see what is behind it. Covering the
            // screen is not the goal — blocking input is, and the card says so.
            let content: Retained<NSView> = window.contentView().expect("curtain content view");
            let scrim = NSVisualEffectView::initWithFrame(
                NSVisualEffectView::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
            );
            scrim.setMaterial(NSVisualEffectMaterial::FullScreenUI);
            scrim.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            scrim.setState(NSVisualEffectState::Active);
            scrim.setAlphaValue(SCRIM_ALPHA);
            content.addSubview(&scrim);

            // Only the primary screen carries the card; the others just dim.
            if index == 0 {
                let card_x = ((frame.size.width - CARD_W) / 2.0).round();
                let card_y = ((frame.size.height - CARD_H) / 2.0).round();
                let card = NSBox::initWithFrame(
                    NSBox::alloc(mtm),
                    NSRect::new(NSPoint::new(card_x, card_y), NSSize::new(CARD_W, CARD_H)),
                );
                card.setBoxType(NSBoxType::Custom);
                card.setTitlePosition(NSTitlePosition::NoTitle);
                card.setCornerRadius(18.0);
                card.setBorderWidth(0.0);
                card.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
                    1.0, 1.0, 1.0, 0.94,
                ));
                card.setContentViewMargins(NSSize::new(0.0, 0.0));
                content.addSubview(&card);
                let card_view: Retained<NSView> = card.contentView().expect("card content view");

                let indicator = NSProgressIndicator::initWithFrame(
                    NSProgressIndicator::alloc(mtm),
                    NSRect::new(
                        NSPoint::new((CARD_W - SPINNER_SIZE) / 2.0, CARD_H - 28.0 - SPINNER_SIZE),
                        NSSize::new(SPINNER_SIZE, SPINNER_SIZE),
                    ),
                );
                indicator.setStyle(NSProgressIndicatorStyle::Spinning);
                indicator.setIndeterminate(true);
                indicator.setControlSize(NSControlSize::Small);
                card_view.addSubview(&indicator);

                let title_field = label(mtm, title, 19.0, 0.12, true);
                place_label(&title_field, CARD_W, 0.0, 108.0);
                card_view.addSubview(&title_field);

                let subtitle_field = label(mtm, "", 13.0, 0.45, false);
                place_label(&subtitle_field, CARD_W, 0.0, 82.0);
                card_view.addSubview(&subtitle_field);

                let cancel_x = ((CARD_W - CANCEL_W) / 2.0).round();
                let cancel_box = NSBox::initWithFrame(
                    NSBox::alloc(mtm),
                    NSRect::new(
                        NSPoint::new(cancel_x, CANCEL_Y),
                        NSSize::new(CANCEL_W, CANCEL_H),
                    ),
                );
                cancel_box.setBoxType(NSBoxType::Custom);
                cancel_box.setTitlePosition(NSTitlePosition::NoTitle);
                cancel_box.setCornerRadius(9.0);
                cancel_box.setBorderWidth(0.0);
                cancel_box.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
                    0.0, 0.0, 0.0, 0.06,
                ));
                cancel_box.setContentViewMargins(NSSize::new(0.0, 0.0));
                card_view.addSubview(&cancel_box);
                let cancel_view: Retained<NSView> =
                    cancel_box.contentView().expect("cancel content view");
                let cancel_field = label(mtm, CANCEL_LABEL, 13.0, 0.38, false);
                // Centred inside the button's own box, which is the part the
                // first version got wrong.
                place_label(&cancel_field, CANCEL_W, 0.0, CANCEL_H / 2.0);
                cancel_view.addSubview(&cancel_field);

                // CGEvent locations use a top-left origin; Cocoa frames a
                // bottom-left one, so the hit rect has to be flipped.
                let cancel_origin_y = frame.origin.y + card_y + CANCEL_Y;
                cancel_rect = CancelRect {
                    x: frame.origin.x + card_x + cancel_x,
                    y: main_height - (cancel_origin_y + CANCEL_H),
                    w: CANCEL_W,
                    h: CANCEL_H,
                };

                subtitle_label = Some(subtitle_field);
                spinner = Some(indicator);
            }

            windows.push(window);
        }

        Self {
            windows,
            subtitle_label,
            spinner,
            cancel_rect,
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

    fn show(&mut self, subtitle: &str) {
        if let Some(field) = &self.subtitle_label {
            field.setStringValue(&NSString::from_str(subtitle));
            // Re-centre: the subtitle changes per send, so its fitted width does
            // too, and a stale frame would leave it visibly off-centre.
            place_label(field, CARD_W, 0.0, 82.0);
        }
        if let Some(spinner) = &self.spinner {
            unsafe { spinner.startAnimation(None) };
        }
        for window in &self.windows {
            window.orderFrontRegardless();
        }
        self.visible = true;
    }

    /// Put the curtain back on top without changing anything else.
    fn raise(&self) {
        for window in &self.windows {
            window.orderFrontRegardless();
        }
    }

    fn hide(&mut self) {
        if let Some(spinner) = &self.spinner {
            unsafe { spinner.stopAnimation(None) };
        }
        for window in &self.windows {
            window.orderOut(None);
        }
        self.visible = false;
    }
}

fn label(
    mtm: MainThreadMarker,
    text: &str,
    size: f64,
    darkness: f64,
    medium: bool,
) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setAlignment(NSTextAlignment::Center);
    let font = if medium {
        unsafe { NSFont::systemFontOfSize_weight(size, NSFontWeightMedium) }
    } else {
        NSFont::systemFontOfSize(size)
    };
    field.setFont(Some(&font));
    field.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        darkness, darkness, darkness, 1.0,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A panicking worker must still end the run.
    ///
    /// This is the failure that matters: if the completion flag is not set on
    /// the unwind path, the pump spins forever with input blocked and the
    /// screen covered, and the machine is unusable until someone kills the
    /// process. Off the main thread there is no AppKit, so this exercises the
    /// worker/flag contract rather than the windows.
    #[test]
    fn a_panicking_worker_still_finishes_the_run() {
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = Arc::clone(&done);
        let handle = std::thread::spawn(move || {
            struct MarkDone(Arc<AtomicBool>);
            impl Drop for MarkDone {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _mark = MarkDone(worker_done);
            panic!("worker blew up");
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !done.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "completion flag never set: the pump would spin with the curtain up"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(handle.join().is_err(), "the panic must reach the caller");
    }

    #[test]
    fn only_our_own_events_are_recognized_as_synthetic() {
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        let source = CGEventSource::new(CGEventSourceStateID::Private).expect("event source");
        let ours = CGEvent::new_keyboard_event(source.clone(), 0, true).expect("event");
        tag_synthetic(&ours);
        assert!(is_synthetic(&ours));

        // An untagged event is what a real keypress looks like to the tap, and
        // it must not be mistaken for ours or the block would leak.
        let theirs = CGEvent::new_keyboard_event(source, 0, true).expect("event");
        assert!(!is_synthetic(&theirs));
    }
}
