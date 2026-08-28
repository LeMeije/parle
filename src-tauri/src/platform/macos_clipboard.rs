//! Clipboard change poller (macOS has no push notification). Runs on its own
//! thread, polls the pasteboard changeCount, and emits ClipboardChanged for
//! new, non-concealed text. The app decides whether to store it (exclusion list).

use super::PlatformEvent;
use crate::platform::macos;
use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct ClipboardMonitor {
    stop: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
}

impl ClipboardMonitor {
    pub fn start(tx: Sender<PlatformEvent>, enabled: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let enabled = Arc::new(AtomicBool::new(enabled));
        {
            let stop = stop.clone();
            let enabled = enabled.clone();
            std::thread::Builder::new()
                .name("parle-clipboard".into())
                .spawn(move || {
                    let mut last = macos::pasteboard_change_count();
                    // Who was frontmost at the PREVIOUS sample.
                    //
                    // macOS has no equivalent of `GetClipboardOwner`, so the
                    // best available answer to "which app wrote this?" is a
                    // frontmost reading. The reading taken when the change is
                    // NOTICED is the wrong one: a copy affordance that returns
                    // focus, which is exactly what a password manager panel, a
                    // browser extension pop-up or a Quick Access overlay does,
                    // has already yielded by then, so the app named is whatever
                    // was underneath. `excluded_apps` is then matched against
                    // the browser and never fires, and the secret is stored and
                    // replicated.
                    //
                    // The sample from just BEFORE the change is the better
                    // proxy, and it is the one that matters for secrets. It is
                    // still a heuristic, and it is wrong in the opposite
                    // direction when the user switches TO an app and copies
                    // immediately, so the poll below is also tightened to
                    // shrink the window either way. The OS-marked path
                    // (`clipboard_is_concealed`) is what actually catches the
                    // big password managers; this list is the user's own
                    // additions, which carry no marker at all.
                    let mut prev_app = macos::frontmost_app();
                    while !stop.load(Ordering::SeqCst) {
                        // Every iteration inside an autorelease pool.
                        //
                        // This runs on a bare `std::thread`, which has no pool
                        // of its own, so the autoreleased NSString and
                        // NSRunningApplication values these calls produce had
                        // nowhere to be drained from. Measured on this machine:
                        // about 82 bytes per sample, growing linearly. At the
                        // 150ms poll that is roughly 1.9 MB an hour in a
                        // menu-bar app meant to stay running for weeks, and
                        // tightening the poll from 400ms tripled the rate while
                        // also adding a `frontmost_app()` call on every
                        // iteration rather than only on a change.
                        // The sleep is OUTSIDE the pool: holding one across
                        // 150ms of sleep defeats the point of draining it.
                        std::thread::sleep(std::time::Duration::from_millis(150));
                        // Every ObjC call inside one. `continue` cannot cross a
                        // closure boundary, so the body returns the next
                        // `prev_app` and an optional event instead of jumping.
                        let (next_app, event) = objc2::rc::autoreleasepool(|_| {
                            if !enabled.load(Ordering::SeqCst) {
                                last = macos::pasteboard_change_count();
                                return (macos::frontmost_app(), None);
                            }
                            let now = macos::pasteboard_change_count();
                            if now == last {
                                return (macos::frontmost_app(), None);
                            }
                            last = now;
                            // Our OWN write, identified by change count rather
                            // than by a marker on the content. Marking it
                            // TransientType also worked, and told every other
                            // clipboard manager to bin the row the user had
                            // just deliberately copied.
                            if macos::we_wrote_change(now) {
                                return (macos::frontmost_app(), None);
                            }
                            if macos::clipboard_is_concealed() {
                                return (macos::frontmost_app(), None);
                            }
                            let event = match macos::read_clipboard() {
                                // RE-CHECK the change count before believing
                                // the text.
                                //
                                // `clipboard_is_concealed()` and
                                // `read_clipboard()` are two separate
                                // NSPasteboard queries with no session between
                                // them, so a password manager writing in the gap
                                // has its secret read under a judgement made
                                // about the PREVIOUS content. Windows brackets
                                // its equivalent pair with
                                // `GetClipboardSequenceNumber` and says why;
                                // macOS has no clipboard lock, but it has the
                                // same counter, so the same guard applies.
                                Some(_) if macos::pasteboard_change_count() != now => {
                                    tracing::debug!(
                                        "clipboard changed while we were reading it; \
                                         discarding the capture"
                                    );
                                    None
                                }
                                Some(text) if !text.trim().is_empty() => {
                                    let (app_id, app_name) = prev_app.clone();
                                    Some(PlatformEvent::ClipboardChanged { text, app_id, app_name })
                                }
                                _ => None,
                            };
                            (macos::frontmost_app(), event)
                        });
                        if let Some(ev) = event {
                            let _ = tx.send(ev);
                        }
                        prev_app = next_app;
                    }
                })
                .expect("spawn clipboard monitor");
        }
        Self { stop, enabled }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
    }
}

impl Drop for ClipboardMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}
