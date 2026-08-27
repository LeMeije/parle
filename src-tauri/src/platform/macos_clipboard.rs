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
                .name("echokey-clipboard".into())
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
                        // 150ms rather than 400ms: this is two cheap reads, and
                        // it cuts the misattribution window by nearly two
                        // thirds as well as making a copy appear sooner.
                        std::thread::sleep(std::time::Duration::from_millis(150));
                        if !enabled.load(Ordering::SeqCst) {
                            last = macos::pasteboard_change_count();
                            prev_app = macos::frontmost_app();
                            continue;
                        }
                        let now = macos::pasteboard_change_count();
                        if now != last {
                            last = now;
                            if macos::clipboard_is_concealed() {
                                prev_app = macos::frontmost_app();
                                continue;
                            }
                            if let Some(text) = macos::read_clipboard() {
                                if text.trim().is_empty() {
                                    prev_app = macos::frontmost_app();
                                    continue;
                                }
                                let (app_id, app_name) = prev_app.clone();
                                let _ = tx.send(PlatformEvent::ClipboardChanged { text, app_id, app_name });
                            }
                        }
                        prev_app = macos::frontmost_app();
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
