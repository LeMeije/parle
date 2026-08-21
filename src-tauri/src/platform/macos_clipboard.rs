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
                    while !stop.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        if !enabled.load(Ordering::SeqCst) {
                            last = macos::pasteboard_change_count();
                            continue;
                        }
                        let now = macos::pasteboard_change_count();
                        if now != last {
                            last = now;
                            if macos::clipboard_is_concealed() {
                                continue;
                            }
                            if let Some(text) = macos::read_clipboard() {
                                if text.trim().is_empty() {
                                    continue;
                                }
                                let (app_id, app_name) = macos::frontmost_app();
                                let _ = tx.send(PlatformEvent::ClipboardChanged { text, app_id, app_name });
                            }
                        }
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
