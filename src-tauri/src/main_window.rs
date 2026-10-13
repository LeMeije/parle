//! Showing and hiding the main window without a white flash.
//!
//! The main window is only ever hidden, never closed (Parle lives in the menu
//! bar). Reopening it after a while showed a white window for a split second
//! before the app appeared. Ben, 13/10/2026: "no other app behaves like this".
//! The cause, measured in a standalone probe (session scratchpad): WebKit
//! suspends a hidden page somewhere between 10 and 30 seconds after it is
//! hidden. A snapshot asked of it at 10 s came back in milliseconds; at 30 s
//! none came back at all, with either inactive-scheduling policy and either
//! snapshot setting. So on a later show the page has to wake before it can
//! draw, and until it does there is nothing to put on screen.
//!
//! The fix is a photograph taken while the page is still awake: just before
//! the window hides (WKWebView `takeSnapshotWithConfiguration`, 1 to 7 ms in
//! the probe), and once shortly after launch for the window a login launch
//! never showed. When the window comes back the photograph is laid over the
//! web view first, and once the page reports a freshly painted frame
//! (`main_window_painted`, sent by App.tsx two animation frames after
//! `visibilitychange`) it fades out over the live page. A solid colour would
//! only have traded the white for a blank.
//!
//! Bounded both ways: hiding never waits more than SNAPSHOT_WAIT for the
//! photograph, and the photograph is removed after OVERLAY_MAX whatever
//! happens, so it can never sit over a live window for long. Memory: one
//! window-sized image while the window is hidden, released once the live page
//! has taken over.
//!
//! Everything here runs on the main thread (AppKit and WebKit require it),
//! which is also why the state is a thread_local.

use tauri::WebviewWindow;

/// Bring the main window back, over its last photograph on macOS. Call on the
/// main thread.
pub fn show(main: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    imp::show(main);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = main.show();
        let _ = main.set_focus();
    }
}

/// Photograph the main window, then hide it. Call on the main thread.
pub fn hide(main: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    imp::hide(main);
    #[cfg(not(target_os = "macos"))]
    let _ = main.hide();
}

/// The page has painted a fresh frame: fade the photograph out. Any thread.
pub fn painted(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread(imp::painted);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// The page has rendered for the first time. If the window is hidden (a
/// login launch) take the first photograph now, while the page is awake.
/// Any thread.
pub fn ready(main: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        use tauri::Manager;
        let window = main.clone();
        let _ = main.app_handle().run_on_main_thread(move || imp::prime(&window));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = main;
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::NSRect;
    use std::cell::RefCell;
    use std::time::{Duration, Instant};
    use tauri::{Manager, WebviewWindow};

    /// Longest a hide waits for its photograph before hiding without one.
    const SNAPSHOT_WAIT: Duration = Duration::from_millis(250);
    /// Longest the photograph may cover the window if the page never reports
    /// a paint. Long enough for a page waking from suspension on a Mac that is
    /// swapping, short enough that a stuck page cannot look frozen for long.
    const OVERLAY_MAX: Duration = Duration::from_millis(2000);
    /// Crossfade from the photograph to the live page.
    const FADE_SECS: f64 = 0.12;

    #[derive(Default)]
    struct State {
        /// Bumped by every show and hide. A late snapshot or timer acts only
        /// if nothing has happened since it was armed.
        generation: u64,
        /// A hide waiting for its photograph.
        pending_hide: Option<u64>,
        /// The photograph for the next show.
        snapshot: Option<Retained<AnyObject>>,
        /// The photograph currently over the web view, with its show.
        overlay: Option<(u64, Retained<AnyObject>)>,
        /// When the window was shown over the photograph, for the log line.
        shown_at: Option<Instant>,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
    }

    fn show_and_focus(main: &WebviewWindow) {
        let _ = main.show();
        let _ = main.set_focus();
    }

    /// Ask WebKit for a photograph of the page as it is now.
    ///
    /// afterScreenUpdates NO: the default (nil configuration) waits for the
    /// page's next screen update, which a hidden page may never produce.
    unsafe fn snapshot(webview: *mut AnyObject, done: impl Fn(Option<Retained<AnyObject>>) + 'static) {
        let config = AnyClass::get(c"WKSnapshotConfiguration").and_then(|cls| {
            let raw: *mut AnyObject = msg_send![cls, new];
            Retained::from_raw(raw)
        });
        if let Some(config) = &config {
            let _: () = msg_send![&**config, setAfterScreenUpdates: Bool::NO];
        }
        let config_ptr: *const AnyObject = config.as_deref().map_or(std::ptr::null(), |c| c as *const AnyObject);
        let handler = block2::RcBlock::new(move |image: *mut AnyObject, error: *mut AnyObject| {
            if image.is_null() {
                tracing::warn!("main window: snapshot failed: {}", describe(error));
            }
            done(Retained::retain(image));
        });
        let _: () = msg_send![
            webview,
            takeSnapshotWithConfiguration: config_ptr,
            completionHandler: &*handler
        ];
    }

    pub fn hide(main: &WebviewWindow) {
        let (gen, mid_reveal) = STATE.with_borrow_mut(|s| {
            s.generation += 1;
            s.shown_at = None;
            (s.generation, s.overlay.take())
        });
        if let Some((_, view)) = mid_reveal {
            // Hidden again before the page painted: a new photograph could be
            // of a page that has not drawn yet, so keep the one on screen.
            unsafe {
                let image: *mut AnyObject = msg_send![&*view, image];
                let image = Retained::retain(image);
                let _: () = msg_send![&*view, removeFromSuperview];
                STATE.with_borrow_mut(|s| s.snapshot = image);
            }
            let _ = main.hide();
            return;
        }
        if !main.is_visible().unwrap_or(false) {
            let _ = main.hide();
            return;
        }
        STATE.with_borrow_mut(|s| s.pending_hide = Some(gen));
        let window = main.clone();
        let asked = main.with_webview(move |wv| unsafe {
            let window = window.clone();
            let started = Instant::now();
            snapshot(wv.inner() as *mut AnyObject, move |image| {
                let hide_now = STATE.with_borrow_mut(|s| {
                    // Stored even when the timeout already hid the window: it
                    // is still the last frame before the hide. Not after a
                    // show, though, which makes it out of date.
                    if s.generation == gen && image.is_some() {
                        s.snapshot = image;
                    }
                    let hide_now = s.pending_hide == Some(gen);
                    if hide_now {
                        s.pending_hide = None;
                    }
                    hide_now
                });
                tracing::debug!("main window: snapshot taken in {} ms", started.elapsed().as_millis());
                if hide_now {
                    let _ = window.hide();
                }
            });
        });
        if let Err(e) = asked {
            tracing::warn!("main window: could not reach the web view for a snapshot: {e}");
            STATE.with_borrow_mut(|s| s.pending_hide = None);
            let _ = main.hide();
            return;
        }
        // A slow photograph must never hold the window on screen.
        let app = main.app_handle().clone();
        let window = main.clone();
        std::thread::spawn(move || {
            std::thread::sleep(SNAPSHOT_WAIT);
            let _ = app.run_on_main_thread(move || {
                let still_waiting = STATE.with_borrow_mut(|s| {
                    let waiting = s.pending_hide == Some(gen);
                    if waiting {
                        s.pending_hide = None;
                        // An older photograph would show a stale window.
                        s.snapshot = None;
                    }
                    waiting
                });
                if still_waiting {
                    tracing::warn!(
                        "main window: no snapshot within {} ms, hiding without one",
                        SNAPSHOT_WAIT.as_millis()
                    );
                    let _ = window.hide();
                }
            });
        });
    }

    pub fn show(main: &WebviewWindow) {
        if main.is_visible().unwrap_or(true) {
            show_and_focus(main);
            return;
        }
        let (gen, have_snapshot) = STATE.with_borrow_mut(|s| {
            s.generation += 1;
            s.pending_hide = None;
            (s.generation, s.snapshot.is_some())
        });
        if !have_snapshot {
            tracing::info!("main window: no snapshot to show over, showing it plainly");
            show_and_focus(main);
            return;
        }
        let window = main.clone();
        let asked = main.with_webview(move |wv| unsafe {
            let image = STATE.with_borrow_mut(|s| if s.generation == gen { s.snapshot.take() } else { None });
            if let Some(view) = image.and_then(|image| overlay_on(wv.inner() as *mut AnyObject, &image)) {
                STATE.with_borrow_mut(|s| {
                    s.overlay = Some((gen, view));
                    s.shown_at = Some(Instant::now());
                });
                arm_overlay_limit(&window, gen);
            }
            show_and_focus(&window);
        });
        if let Err(e) = asked {
            tracing::warn!("main window: could not reach the web view to show the snapshot: {e}");
            show_and_focus(main);
        }
    }

    /// The first photograph, for a window a login launch never showed.
    pub fn prime(main: &WebviewWindow) {
        let gen = STATE.with_borrow(|s| s.generation);
        let needed = STATE.with_borrow(|s| s.snapshot.is_none() && s.overlay.is_none());
        if !needed || main.is_visible().unwrap_or(true) {
            return;
        }
        let _ = main.with_webview(move |wv| unsafe {
            snapshot(wv.inner() as *mut AnyObject, move |image| {
                STATE.with_borrow_mut(|s| {
                    if s.generation == gen && s.snapshot.is_none() {
                        s.snapshot = image;
                    }
                });
            });
        });
    }

    /// An NSImageView holding the photograph, placed exactly over the web view.
    unsafe fn overlay_on(webview: *mut AnyObject, image: &AnyObject) -> Option<Retained<AnyObject>> {
        let cls = AnyClass::get(c"NSImageView")?;
        let superview: *mut AnyObject = msg_send![webview, superview];
        if superview.is_null() {
            tracing::warn!("main window: the web view has no superview to hold the snapshot");
            return None;
        }
        let frame: NSRect = msg_send![webview, frame];
        let view: *mut AnyObject = msg_send![cls, imageViewWithImage: image];
        let view = Retained::retain(view)?;
        let _: () = msg_send![&*view, setFrame: frame];
        // NSImageScaleAxesIndependently: the photograph is the web view's own
        // size, so this is a 1:1 fill, never a letterbox.
        let _: () = msg_send![&*view, setImageScaling: 1usize];
        // Width and height sizable, in case the window resizes mid-fade.
        let _: () = msg_send![&*view, setAutoresizingMask: 18usize];
        let _: () = msg_send![&*view, setWantsLayer: Bool::YES];
        // NSWindowAbove, relative to the web view.
        let _: () = msg_send![superview, addSubview: &*view, positioned: 1isize, relativeTo: webview];
        Some(view)
    }

    /// An NSError's description, for the log.
    unsafe fn describe(error: *mut AnyObject) -> String {
        if error.is_null() {
            return "no error given".into();
        }
        let text: *mut AnyObject = msg_send![error, description];
        if text.is_null() {
            return "unknown error".into();
        }
        let utf8: *const std::ffi::c_char = msg_send![text, UTF8String];
        if utf8.is_null() {
            return "unknown error".into();
        }
        std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }

    fn arm_overlay_limit(main: &WebviewWindow, gen: u64) {
        let app = main.app_handle().clone();
        std::thread::spawn(move || {
            std::thread::sleep(OVERLAY_MAX);
            let _ = app.run_on_main_thread(move || {
                let stale = STATE.with_borrow_mut(|s| match s.overlay.take() {
                    Some((g, view)) if g == gen => {
                        s.shown_at = None;
                        Some(view)
                    }
                    other => {
                        s.overlay = other;
                        None
                    }
                });
                if let Some(view) = stale {
                    tracing::warn!(
                        "main window: page never reported a paint, snapshot removed after {} ms",
                        OVERLAY_MAX.as_millis()
                    );
                    unsafe { fade_out(view) };
                }
            });
        });
    }

    pub fn painted() {
        let (view, shown_at) = STATE.with_borrow_mut(|s| (s.overlay.take(), s.shown_at.take()));
        if let Some((_, view)) = view {
            if let Some(t) = shown_at {
                tracing::info!(
                    "main window: shown over its snapshot, page painted {} ms later",
                    t.elapsed().as_millis()
                );
            }
            unsafe { fade_out(view) };
        }
    }

    /// Crossfade the photograph away, then take it out of the view tree.
    unsafe fn fade_out(view: Retained<AnyObject>) {
        let Some(ctx_cls) = AnyClass::get(c"NSAnimationContext") else {
            let _: () = msg_send![&*view, removeFromSuperview];
            return;
        };
        let _: () = msg_send![ctx_cls, beginGrouping];
        let ctx: *mut AnyObject = msg_send![ctx_cls, currentContext];
        let _: () = msg_send![ctx, setDuration: FADE_SECS];
        let finished = view.clone();
        let done = block2::RcBlock::new(move || unsafe {
            let _: () = msg_send![&*finished, removeFromSuperview];
        });
        let _: () = msg_send![ctx, setCompletionHandler: &*done];
        let animator: *mut AnyObject = msg_send![&*view, animator];
        let _: () = msg_send![animator, setAlphaValue: 0.0f64];
        let _: () = msg_send![ctx_cls, endGrouping];
    }
}
