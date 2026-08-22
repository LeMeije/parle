//! macOS platform layer.
//!
//! Hotkeys: an active CGEventTap on a dedicated CFRunLoop thread (the only way
//! to see Fn/Globe and discriminate left/right modifiers, and the only way to
//! swallow our own keys). Requires Accessibility.
//!
//! Injection: Accessibility text insertion fast path, then clipboard + a
//! synthetic Cmd-V via CGEvent, restoring the previous clipboard after a delay.
//! Secure input (password fields) is detected and degrades to clipboard-only.

use super::{
    HotkeyId, InjectionMethod, InjectionOutcome, NativeBindings, NativeKey, PermissionStatus,
    PlatformEvent,
};
use crate::hotkey_logic::KeyPhase;
use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop, CFRunLoopGetCurrent, CFRunLoopRun};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CGKeyCode, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use crossbeam_channel::Sender;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// Modifier keycodes from HIToolbox Events.h.
const KVK_FUNCTION: CGKeyCode = 63;
const KVK_ANSI_V: CGKeyCode = 9;
const KVK_ESCAPE: CGKeyCode = 53;
const KVK_LSHIFT: CGKeyCode = 56;
const KVK_RSHIFT: CGKeyCode = 60;
const KVK_LCTRL: CGKeyCode = 59;
const KVK_RCTRL: CGKeyCode = 62;
const KVK_LOPT: CGKeyCode = 58;
const KVK_ROPT: CGKeyCode = 61;
const KVK_LCMD: CGKeyCode = 55;
const KVK_RCMD: CGKeyCode = 54;

const NX_SECONDARY_FN_MASK: u64 = 0x00800000;

// -- Permissions -----------------------------------------------------------

extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

pub fn permission_status() -> PermissionStatus {
    PermissionStatus {
        accessibility: unsafe { AXIsProcessTrusted() },
        microphone: microphone_status(),
    }
}

pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Ask macOS to register THIS binary in the Accessibility list and show the
/// system grant prompt. Crucial after rebuilds: a stale list entry bound to an
/// old signature makes the toggle a no-op; this call creates a fresh entry
/// bound to the current binary.
pub fn request_accessibility_access() {
    use accessibility_sys::{kAXTrustedCheckOptionPrompt, AXIsProcessTrustedWithOptions};
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let dict = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
        let _ = AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef());
    }
}

/// Open System Settings at the Accessibility pane (best-effort).
pub fn open_accessibility_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

pub fn open_microphone_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        .spawn();
}

fn microphone_status() -> String {
    use objc2::runtime::AnyClass;
    use objc2::{msg_send, sel};
    // AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio.
    // 0 notDetermined, 1 restricted, 2 denied, 3 authorized.
    unsafe {
        let Some(cls) = AnyClass::get(c"AVCaptureDevice") else {
            return "unknown".into();
        };
        // AVMediaTypeAudio == "soun".
        let media = objc2_foundation::NSString::from_str("soun");
        let status: i64 = msg_send![cls, authorizationStatusForMediaType: &*media];
        let _ = sel!(authorizationStatusForMediaType:);
        match status {
            0 => "undetermined".into(),
            1 | 2 => "denied".into(),
            3 => "granted".into(),
            _ => "unknown".into(),
        }
    }
}

/// Fire the system microphone prompt (no-op once the status is determined).
/// The onboarding UI polls permission_status, so no completion plumbing needed.
pub fn request_microphone_access() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, Bool};
    unsafe {
        let Some(cls) = AnyClass::get(c"AVCaptureDevice") else {
            tracing::error!("AVCaptureDevice class missing — AVFoundation not linked");
            return;
        };
        let media = objc2_foundation::NSString::from_str("soun");
        let handler = block2::RcBlock::new(move |granted: Bool| {
            tracing::info!("microphone permission response: granted={}", granted.as_bool());
        });
        let _: () = msg_send![cls, requestAccessForMediaType: &*media, completionHandler: &*handler];
    }
}

extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

pub fn secure_input_active() -> bool {
    unsafe { IsSecureEventInputEnabled() }
}

// -- Hotkey listener --------------------------------------------------------

/// Set while a bound dictation modifier is physically held; any other key
/// going down during that window aborts the gesture (Fn+C, Fn+arrow, ...).
static BOUND_MOD_HELD: AtomicBool = AtomicBool::new(false);

pub struct HotkeyListener {
    state: Arc<Mutex<NativeBindings>>,
    recording: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    runloop: Arc<Mutex<Option<CFRunLoopHandle>>>,
}

// CFRunLoop is not Send; we only ever store its pointer to stop it. Wrap it.
struct CFRunLoopHandle(core_foundation::runloop::CFRunLoopRef);
unsafe impl Send for CFRunLoopHandle {}

impl HotkeyListener {
    /// Spawn the event-tap thread. `tx` receives Hotkey events.
    pub fn start(bindings: NativeBindings, tx: Sender<PlatformEvent>) -> Self {
        let state = Arc::new(Mutex::new(bindings));
        let recording = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let runloop = Arc::new(Mutex::new(None));

        {
            let state = state.clone();
            let recording = recording.clone();
            let stop = stop.clone();
            let runloop = runloop.clone();
            std::thread::Builder::new()
                .name("echokey-cgeventtap".into())
                .spawn(move || run_tap(state, recording, stop, runloop, tx))
                .expect("spawn event tap");
        }
        Self { state, recording, stop, runloop }
    }

    pub fn update_bindings(&self, bindings: NativeBindings) {
        *self.state.lock() = bindings;
    }

    pub fn set_recording(&self, recording: bool) {
        self.recording.store(recording, Ordering::SeqCst);
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(rl) = self.runloop.lock().take() {
            unsafe {
                core_foundation::runloop::CFRunLoopStop(rl.0);
            }
        }
    }
}

fn run_tap(
    state: Arc<Mutex<NativeBindings>>,
    recording: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    runloop_slot: Arc<Mutex<Option<CFRunLoopHandle>>>,
    tx: Sender<PlatformEvent>,
) {
    let current = CFRunLoop::get_current();

    // We must NOT swallow another app's key; we only swallow our own bindings.
    let callback = {
        let state = state.clone();
        let recording = recording.clone();
        move |_proxy, event_type, event: &CGEvent| -> Option<CGEvent> {
            match event_type {
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                    // Re-enable handled by the outer loop via the enable flag;
                    // return the event unmodified.
                    return Some(event.to_owned());
                }
                _ => {}
            }
            let swallow = handle_event(&state, &recording, event_type, event, &tx);
            if swallow {
                None
            } else {
                Some(event.to_owned())
            }
        }
    };

    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default, // active tap: can swallow
        vec![
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ],
        callback,
    );

    let Ok(tap) = tap else {
        tracing::error!("failed to create CGEventTap (Accessibility not granted?)");
        return;
    };

    unsafe {
        let loop_source = tap
            .mach_port
            .create_runloop_source(0)
            .expect("runloop source");
        current.add_source(&loop_source, kCFRunLoopCommonModes);
        tap.enable();
        *runloop_slot.lock() = Some(CFRunLoopHandle(CFRunLoopGetCurrent()));
    }

    tracing::info!("CGEventTap active");
    // CFRunLoopRun blocks; Drop stops it via CFRunLoopStop.
    while !stop.load(Ordering::SeqCst) {
        unsafe {
            // Run the loop in short bursts so we notice the stop flag and can
            // re-enable the tap if the OS disabled it. (Common modes is a
            // pseudo-mode invalid for RunInMode; default mode is correct here.)
            let _ = CFRunLoopRun;
            core_foundation::runloop::CFRunLoopRunInMode(
                core_foundation::runloop::kCFRunLoopDefaultMode,
                0.25,
                false as u8,
            );
        }
        tap.enable(); // idempotent re-enable after any timeout disable
    }
}

/// Returns true if the event should be swallowed.
fn handle_event(
    state: &Arc<Mutex<NativeBindings>>,
    recording: &Arc<AtomicBool>,
    event_type: CGEventType,
    event: &CGEvent,
    tx: &Sender<PlatformEvent>,
) -> bool {
    let bindings = state.lock().clone();
    let is_recording = recording.load(Ordering::SeqCst);

    match event_type {
        CGEventType::FlagsChanged => {
            let keycode =
                event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as CGKeyCode;
            let flags = event.get_flags();
            let key = keycode_to_native(keycode);
            let Some(key) = key else { return false };

            // Fn key: down = fn flag set, up = cleared.
            let phase = if key == NativeKey::Fn {
                if flags.contains(CGEventFlags::from_bits_truncate(NX_SECONDARY_FN_MASK)) {
                    KeyPhase::Down
                } else {
                    KeyPhase::Up
                }
            } else {
                // Modifier down when its mask is present.
                if modifier_flag_present(keycode, flags) {
                    KeyPhase::Down
                } else {
                    KeyPhase::Up
                }
            };
            dispatch_key(&bindings, is_recording, &key, phase, tx)
        }
        CGEventType::KeyDown | CGEventType::KeyUp => {
            let keycode =
                event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as CGKeyCode;
            let phase = if matches!(event_type, CGEventType::KeyDown) { KeyPhase::Down } else { KeyPhase::Up };
            // Another key while the dictation modifier is held: the user is
            // chording (Fn+C emoji, Fn+arrow paging) — abort the gesture and
            // pass the key through so the chord still works.
            if phase == KeyPhase::Down && BOUND_MOD_HELD.load(Ordering::SeqCst) {
                let _ = tx.send(PlatformEvent::AbortGesture);
            }
            if keycode == KVK_ESCAPE {
                if is_recording {
                    if let Some(ci) = bindings.cancel.as_ref() {
                        if *ci == NativeKey::Escape && phase == KeyPhase::Down {
                            let _ = tx.send(PlatformEvent::Hotkey { id: HotkeyId::Cancel, phase });
                            return true; // consume Escape only while recording
                        }
                    }
                }
                return false;
            }
            false
        }
        _ => false,
    }
}

fn dispatch_key(
    bindings: &NativeBindings,
    is_recording: bool,
    key: &NativeKey,
    phase: KeyPhase,
    tx: &Sender<PlatformEvent>,
) -> bool {
    let mut swallow = false;
    if let Some(w) = bindings.dictation.as_ref().filter(|w| &w.key == key) {
        // Hold/hybrid gestures need the other-key abort; DoubleTap taps are
        // too brief to matter and must not suppress normal chording.
        if w.swallow {
            BOUND_MOD_HELD.store(phase == KeyPhase::Down, Ordering::SeqCst);
        }
        let _ = tx.send(PlatformEvent::Hotkey { id: HotkeyId::Dictation, phase });
        swallow |= w.swallow;
    }
    if let Some(w) = bindings.dictation_alt.as_ref().filter(|w| &w.key == key) {
        if w.swallow {
            BOUND_MOD_HELD.store(phase == KeyPhase::Down, Ordering::SeqCst);
        }
        let _ = tx.send(PlatformEvent::Hotkey { id: HotkeyId::DictationAlt, phase });
        swallow |= w.swallow;
    }
    if bindings.cancel.as_ref() == Some(key) && is_recording {
        let _ = tx.send(PlatformEvent::Hotkey { id: HotkeyId::Cancel, phase });
        swallow = true;
    }
    swallow
}

fn keycode_to_native(keycode: CGKeyCode) -> Option<NativeKey> {
    Some(match keycode {
        KVK_FUNCTION => NativeKey::Fn,
        KVK_LSHIFT => NativeKey::LeftShift,
        KVK_RSHIFT => NativeKey::RightShift,
        KVK_LCTRL => NativeKey::LeftCtrl,
        KVK_RCTRL => NativeKey::RightCtrl,
        KVK_LOPT => NativeKey::LeftAlt,
        KVK_ROPT => NativeKey::RightAlt,
        KVK_LCMD => NativeKey::LeftCmd,
        KVK_RCMD => NativeKey::RightCmd,
        _ => return None,
    })
}

fn modifier_flag_present(keycode: CGKeyCode, flags: CGEventFlags) -> bool {
    let mask = match keycode {
        KVK_LSHIFT | KVK_RSHIFT => CGEventFlags::CGEventFlagShift,
        KVK_LCTRL | KVK_RCTRL => CGEventFlags::CGEventFlagControl,
        KVK_LOPT | KVK_ROPT => CGEventFlags::CGEventFlagAlternate,
        KVK_LCMD | KVK_RCMD => CGEventFlags::CGEventFlagCommand,
        _ => return false,
    };
    flags.contains(mask)
}

// -- Injection --------------------------------------------------------------

/// Original clipboard awaiting restoration. Chained dictations within the
/// restore window carry the OLDEST original forward instead of restoring a
/// transcript over the user's real clipboard.
static PENDING_RESTORE: Mutex<Option<String>> = Mutex::new(None);
static RESTORE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Insert `text` at the cursor in the focused app.
///
/// `keep_on_clipboard` = the user's "also copy to clipboard" setting: the
/// transcript stays on the clipboard and no restore happens.
/// `restore` = restore the previous clipboard after paste-injection.
pub fn inject_text(
    text: &str,
    prefer_ax: bool,
    restore_delay_ms: u64,
    keep_on_clipboard: bool,
    restore: bool,
) -> InjectionOutcome {
    if secure_input_active() {
        // Concealed: other clipboard managers must not capture what may be a
        // password-field dictation.
        write_clipboard_marked(text, true);
        return InjectionOutcome {
            method: InjectionMethod::ClipboardOnly,
            manual_paste_required: true,
        };
    }

    if prefer_ax && ax_insert_text(text) {
        if keep_on_clipboard {
            write_clipboard_marked(text, false);
        }
        return InjectionOutcome { method: InjectionMethod::AxInsert, manual_paste_required: false };
    }

    // Clipboard + Cmd-V.
    let previous = read_clipboard();
    write_clipboard_marked(text, false);
    let after_write = pasteboard_change_count();
    synth_cmd_v();

    if keep_on_clipboard {
        // Transcript intentionally stays; drop any pending restore.
        *PENDING_RESTORE.lock() = None;
    } else if restore {
        // Carry the oldest original across chained dictations.
        {
            let mut pending = PENDING_RESTORE.lock();
            if pending.is_none() {
                *pending = previous;
            }
        }
        let generation = RESTORE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(restore_delay_ms));
            // A newer injection superseded this restore; it will do the work.
            if RESTORE_GENERATION.load(std::sync::atomic::Ordering::SeqCst) != generation {
                return;
            }
            // Only restore if nothing else wrote to the clipboard meanwhile.
            if pasteboard_change_count() == after_write {
                if let Some(prev) = PENDING_RESTORE.lock().take() {
                    write_clipboard_marked(&prev, false);
                }
            } else {
                *PENDING_RESTORE.lock() = None;
            }
        });
    }
    InjectionOutcome { method: InjectionMethod::ClipboardPaste, manual_paste_required: false }
}

fn ax_insert_text(text: &str) -> bool {
    use accessibility_sys::{
        kAXErrorSuccess, kAXFocusedUIElementAttribute, kAXSelectedTextAttribute,
        AXUIElementCopyAttributeValue, AXUIElementCreateSystemWide, AXUIElementRef,
        AXUIElementSetAttributeValue,
    };
    use core_foundation::base::{CFRelease, CFTypeRef};
    use core_foundation::string::CFString;
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return false;
        }
        let focused_attr = CFString::from_static_string(kAXFocusedUIElementAttribute);
        let mut focused: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(
            system,
            focused_attr.as_concrete_TypeRef(),
            &mut focused as *mut _,
        );
        CFRelease(system as CFTypeRef);
        if err != kAXErrorSuccess || focused.is_null() {
            return false;
        }
        let sel_attr = CFString::from_static_string(kAXSelectedTextAttribute);
        let value = CFString::new(text);
        let set_err = AXUIElementSetAttributeValue(
            focused as AXUIElementRef,
            sel_attr.as_concrete_TypeRef(),
            value.as_concrete_TypeRef() as CFTypeRef,
        );
        CFRelease(focused);
        set_err == kAXErrorSuccess
    }
}

fn synth_cmd_v() {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        return;
    };
    if let Ok(down) = CGEvent::new_keyboard_event(src.clone(), KVK_ANSI_V, true) {
        down.set_flags(CGEventFlags::CGEventFlagCommand);
        down.post(CGEventTapLocation::HID);
    }
    if let Ok(up) = CGEvent::new_keyboard_event(src, KVK_ANSI_V, false) {
        up.set_flags(CGEventFlags::CGEventFlagCommand);
        up.post(CGEventTapLocation::HID);
    }
}

// -- Clipboard + frontmost app ----------------------------------------------

pub fn read_clipboard() -> Option<String> {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        let ty = NSString::from_str("public.utf8-plain-text");
        pb.stringForType(&ty).map(|s| s.to_string())
    }
}

pub fn write_clipboard(text: &str) {
    write_clipboard_impl(text, false, false);
}

/// Injection writes are marked org.nspasteboard.TransientType so clipboard
/// managers (including our own monitor) skip them; `concealed` adds
/// ConcealedType for possibly-sensitive content (secure-input path).
pub fn write_clipboard_marked(text: &str, concealed: bool) {
    write_clipboard_impl(text, true, concealed);
}

fn write_clipboard_impl(text: &str, transient: bool, concealed: bool) {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let ty = NSString::from_str("public.utf8-plain-text");
        let value = NSString::from_str(text);
        pb.setString_forType(&value, &ty);
        if transient {
            let t = NSString::from_str("org.nspasteboard.TransientType");
            pb.setString_forType(&NSString::from_str(""), &t);
        }
        if concealed {
            let c = NSString::from_str("org.nspasteboard.ConcealedType");
            pb.setString_forType(&NSString::from_str(""), &c);
        }
    }
}

pub fn pasteboard_change_count() -> i64 {
    use objc2_app_kit::NSPasteboard;
    unsafe { NSPasteboard::generalPasteboard().changeCount() as i64 }
}

/// (bundle_id, app_name) of the frontmost application.
pub fn frontmost_app() -> (Option<String>, Option<String>) {
    use objc2_app_kit::NSWorkspace;
    unsafe {
        let ws = NSWorkspace::sharedWorkspace();
        match ws.frontmostApplication() {
            Some(app) => (
                app.bundleIdentifier().map(|s| s.to_string()),
                app.localizedName().map(|s| s.to_string()),
            ),
            None => (None, None),
        }
    }
}

/// Concealed/transient pasteboard types we must not capture (password managers).
pub fn clipboard_is_concealed() -> bool {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        let Some(types) = pb.types() else { return false };
        for t in types.iter() {
            let s = t.to_string();
            if s == "org.nspasteboard.ConcealedType"
                || s == "org.nspasteboard.TransientType"
                || s == "org.nspasteboard.AutoGeneratedType"
            {
                return true;
            }
        }
        false
    }
}
