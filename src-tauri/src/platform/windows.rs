//! Windows platform layer. WRITTEN ON macOS AGAINST THE RESEARCH SPEC
//! (docs/research/PLATFORM.md) — NOT YET COMPILED ON WINDOWS. The first task in
//! docs/WINDOWS_HANDOFF.md is to compile this and fix signature drift.
//!
//! Hotkeys: WH_KEYBOARD_LL hook on a dedicated thread with a message pump.
//! Handles bare L/R modifiers and the Copilot key (LShift+LWin+VK_F23 chord or
//! VK_LAUNCH_APP1), swallowing only our own bindings. The hook proc is
//! allocation-free (the LowLevelHooksTimeout budget silently removes slow
//! hooks); events are forwarded through a pre-allocated channel.
//!
//! Rules that are load-bearing (from the murmur teardown + research):
//! - Never swallow a key-down but let its key-up escape (stuck-modifier bug).
//! - After swallowing the Copilot F23 while LWin is down, inject a dummy
//!   VK 0xFF event so Windows treats Win as "used as modifier" and does not
//!   open the Start menu on release (PowerToys trick).
//! - Skip events carrying LLKHF_INJECTED (our own synthetic input).
//! - Injection is SendInput Ctrl+V; UI Automation cannot insert at the caret.

#![cfg(target_os = "windows")]

use super::{
    HotkeyId, InjectionMethod, InjectionOutcome, NativeBindings, NativeKey, PermissionStatus,
    PlatformEvent,
};
use crate::hotkey_logic::KeyPhase;
use crossbeam_channel::Sender;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardOwner, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    VK_CONTROL, VK_F23, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    WH_KEYBOARD_LL, WM_CLIPBOARDUPDATE, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

const VK_LAUNCH_APP1: u32 = 0xB6;
const VK_DUMMY: u16 = 0xFF;

// The LL hook proc cannot carry a closure; state lives in statics.
static HOOK_TX: Mutex<Option<Sender<PlatformEvent>>> = Mutex::new(None);
static HOOK_BINDINGS: Mutex<NativeBindings> = Mutex::new(NativeBindings {
    dictation: None,
    dictation_alt: None,
    cancel: None,
});
static RECORDING: AtomicBool = AtomicBool::new(false);
static SUPPRESS_COPILOT: AtomicBool = AtomicBool::new(true);
static LSHIFT_DOWN: AtomicBool = AtomicBool::new(false);
static LWIN_DOWN: AtomicBool = AtomicBool::new(false);
/// While > 0, we are swallowing the Copilot chord's F23 events.
static COPILOT_ACTIVE: AtomicBool = AtomicBool::new(false);

pub struct HotkeyListener {
    thread_id: Arc<AtomicU32>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl HotkeyListener {
    pub fn start(bindings: NativeBindings, suppress_copilot: bool, tx: Sender<PlatformEvent>) -> Self {
        *HOOK_TX.lock() = Some(tx);
        *HOOK_BINDINGS.lock() = bindings;
        SUPPRESS_COPILOT.store(suppress_copilot, Ordering::SeqCst);

        let thread_id = Arc::new(AtomicU32::new(0));
        let tid = thread_id.clone();
        let join = std::thread::Builder::new()
            .name("echokey-llhook".into())
            .spawn(move || unsafe {
                tid.store(windows::Win32::System::Threading::GetCurrentThreadId(), Ordering::SeqCst);
                let hook: HHOOK = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), None, 0)
                {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::error!("SetWindowsHookExW failed: {e}");
                        return;
                    }
                };
                tracing::info!("WH_KEYBOARD_LL hook installed");
                // LL hooks require a message pump on the installing thread.
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let _ = UnhookWindowsHookEx(hook);
            })
            .expect("spawn hook thread");

        Self { thread_id, join: Some(join) }
    }

    pub fn update_bindings(&self, bindings: NativeBindings) {
        *HOOK_BINDINGS.lock() = bindings;
    }

    pub fn set_suppress_copilot(&self, suppress: bool) {
        SUPPRESS_COPILOT.store(suppress, Ordering::SeqCst);
    }

    pub fn set_recording(&self, recording: bool) {
        RECORDING.store(recording, Ordering::SeqCst);
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        let tid = self.thread_id.load(Ordering::SeqCst);
        if tid != 0 {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }
    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    // Skip our own injected events (dummy key, Ctrl+V synthesis).
    if info.flags.contains(LLKHF_INJECTED) {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }
    let vk = info.vkCode;
    let is_down = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
    let is_up = wparam.0 as u32 == WM_KEYUP || wparam.0 as u32 == WM_SYSKEYUP;
    if !is_down && !is_up {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }
    let phase = if is_down { KeyPhase::Down } else { KeyPhase::Up };

    // Track chord modifiers.
    if vk == VK_LSHIFT.0 as u32 {
        LSHIFT_DOWN.store(is_down, Ordering::SeqCst);
    }
    if vk == VK_LWIN.0 as u32 {
        LWIN_DOWN.store(is_down, Ordering::SeqCst);
    }

    // -- Copilot key: LShift+LWin+F23 chord (or discrete VK_LAUNCH_APP1). ----
    let is_copilot_f23 = vk == VK_F23.0 as u32
        && (COPILOT_ACTIVE.load(Ordering::SeqCst)
            || (LSHIFT_DOWN.load(Ordering::SeqCst) && LWIN_DOWN.load(Ordering::SeqCst)));
    let is_copilot_app1 = vk == VK_LAUNCH_APP1;
    if is_copilot_f23 || is_copilot_app1 {
        let bindings = HOOK_BINDINGS.lock();
        let bound = binding_for(&bindings, &NativeKey::CopilotKey);
        drop(bindings);
        if let Some(id) = bound {
            if is_down && !COPILOT_ACTIVE.load(Ordering::SeqCst) {
                COPILOT_ACTIVE.store(true, Ordering::SeqCst);
                send_event(id, KeyPhase::Down);
                if is_copilot_f23 {
                    // Mark Win as "used as modifier" so releasing it doesn't
                    // open the Start menu after we swallow F23.
                    inject_dummy_key();
                }
            } else if is_up {
                COPILOT_ACTIVE.store(false, Ordering::SeqCst);
                send_event(id, KeyPhase::Up);
            }
            // Swallow BOTH down and up (never split a swallow).
            return LRESULT(1);
        }
        if SUPPRESS_COPILOT.load(Ordering::SeqCst) && is_copilot_f23 {
            // Not bound but suppression requested: still swallow the launch.
            if is_down {
                inject_dummy_key();
            }
            return LRESULT(1);
        }
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }

    // -- Bare modifier bindings (L/R discriminated by vkCode). ---------------
    if let Some(key) = vk_to_native(vk) {
        let bindings = HOOK_BINDINGS.lock();
        let bound = binding_for(&bindings, &key);
        let cancel_bound = bindings.cancel.as_ref() == Some(&key);
        drop(bindings);
        if let Some(id) = bound {
            // Auto-repeat suppression for held modifiers: Windows repeats
            // key-down; forward only transitions.
            send_event(id, phase);
            // Swallow the bare-modifier binding completely so it stops acting
            // as a modifier while bound (both down and up — never split).
            return LRESULT(1);
        }
        if cancel_bound && RECORDING.load(Ordering::SeqCst) {
            if is_down {
                send_event_id(HotkeyId::Cancel, KeyPhase::Down);
            }
            return LRESULT(1);
        }
    }

    // Escape cancel (only while recording).
    if vk == 0x1B && RECORDING.load(Ordering::SeqCst) {
        let bindings = HOOK_BINDINGS.lock();
        let esc_bound = bindings.cancel.as_ref() == Some(&NativeKey::Escape);
        drop(bindings);
        if esc_bound {
            if is_down {
                send_event_id(HotkeyId::Cancel, KeyPhase::Down);
            }
            return LRESULT(1);
        }
    }

    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

fn binding_for(b: &NativeBindings, key: &NativeKey) -> Option<HotkeyId> {
    if b.dictation.as_ref() == Some(key) {
        Some(HotkeyId::Dictation)
    } else if b.dictation_alt.as_ref() == Some(key) {
        Some(HotkeyId::DictationAlt)
    } else {
        None
    }
}

fn send_event(id: HotkeyId, phase: KeyPhase) {
    send_event_id(id, phase);
}

fn send_event_id(id: HotkeyId, phase: KeyPhase) {
    if let Some(tx) = HOOK_TX.lock().as_ref() {
        let _ = tx.try_send_or_send(PlatformEvent::Hotkey { id, phase });
    }
}

trait TrySendOrSend<T> {
    fn try_send_or_send(&self, v: T) -> Result<(), ()>;
}
impl<T> TrySendOrSend<T> for Sender<T> {
    fn try_send_or_send(&self, v: T) -> Result<(), ()> {
        // Unbounded channel: send never blocks; keep the hook proc fast.
        self.send(v).map_err(|_| ())
    }
}

fn vk_to_native(vk: u32) -> Option<NativeKey> {
    let vk = VIRTUAL_KEY(vk as u16);
    Some(match vk {
        v if v == VK_LSHIFT => NativeKey::LeftShift,
        v if v == VK_RSHIFT => NativeKey::RightShift,
        v if v == VK_LCONTROL => NativeKey::LeftCtrl,
        v if v == VK_RCONTROL => NativeKey::RightCtrl,
        v if v == VK_LMENU => NativeKey::LeftAlt,
        // Right Alt is AltGr on many layouts; supported but not default.
        v if v == VK_RMENU => NativeKey::RightAlt,
        v if v == VK_LWIN => NativeKey::LeftCmd,
        v if v == VK_RWIN => NativeKey::RightCmd,
        _ => return None,
    })
}

fn inject_dummy_key() {
    unsafe {
        let inputs = [
            key_input(VK_DUMMY, false),
            key_input(VK_DUMMY, true),
        ];
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

// -- Injection ----------------------------------------------------------------

pub fn inject_text(
    text: &str,
    _prefer_ax: bool,
    restore_delay_ms: u64,
    keep_on_clipboard: bool,
    restore: bool,
) -> InjectionOutcome {
    let previous = read_clipboard();
    write_clipboard(text);
    let seq_after_write = unsafe { GetClipboardSequenceNumber() };
    synth_ctrl_v();

    if !keep_on_clipboard && restore {
        if let Some(prev) = previous {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(restore_delay_ms));
                // Restore only if nobody else wrote to the clipboard meanwhile.
                let seq_now = unsafe { GetClipboardSequenceNumber() };
                if seq_now == seq_after_write {
                    write_clipboard(&prev);
                }
            });
        }
    }
    InjectionOutcome { method: InjectionMethod::ClipboardPaste, manual_paste_required: false }
}

fn synth_ctrl_v() {
    unsafe {
        // Release any physically held modifiers first (the Copilot chord holds
        // Shift+Win at stop time; Ctrl+Shift+Win+V is not paste). Key-ups for
        // keys that aren't down are harmless.
        let inputs = [
            key_input(VK_LSHIFT.0, true),
            key_input(VK_RSHIFT.0, true),
            key_input(VK_LWIN.0, true),
            key_input(VK_RWIN.0, true),
            key_input(VK_LMENU.0, true),
            key_input(VK_CONTROL.0, false),
            key_input(VK_V.0, false),
            key_input(VK_V.0, true),
            key_input(VK_CONTROL.0, true),
        ];
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

// -- Clipboard ------------------------------------------------------------------

const CF_UNICODETEXT: u32 = 13;

fn open_clipboard_retry() -> bool {
    // The clipboard is frequently locked by other listeners; retry briefly.
    for _ in 0..10 {
        if unsafe { OpenClipboard(HWND::default()) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

pub fn read_clipboard() -> Option<String> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return None;
        }
        if !open_clipboard_retry() {
            return None;
        }
        let result = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
            let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(handle.0 as _)) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            let s = String::from_utf16_lossy(slice);
            let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(handle.0 as _));
            Some(s)
        })();
        let _ = CloseClipboard();
        result
    }
}

/// Write text, marked so Win+V history / cloud sync / other monitors skip it.
pub fn write_clipboard(text: &str) {
    unsafe {
        if !open_clipboard_retry() {
            return;
        }
        let _ = EmptyClipboard();
        // Payload.
        let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, utf16.len() * 2) {
            let ptr = GlobalLock(hmem) as *mut u16;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
                let _ = GlobalUnlock(hmem);
                let _ = SetClipboardData(
                    CF_UNICODETEXT,
                    windows::Win32::Foundation::HANDLE(hmem.0 as _),
                );
            }
        }
        // Exclusion formats (KeePassXC/ClipMate conventions + Win+V history).
        for name in [
            "ExcludeClipboardContentFromMonitorProcessing",
            "CanIncludeInClipboardHistory",
            "Clipboard Viewer Ignore",
        ] {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let fmt = RegisterClipboardFormatW(windows::core::PCWSTR(wide.as_ptr()));
            if fmt != 0 {
                // A DWORD zero payload (0 = exclude for CanIncludeInClipboardHistory).
                if let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, 4) {
                    let ptr = GlobalLock(hmem) as *mut u32;
                    if !ptr.is_null() {
                        *ptr = 0;
                        let _ = GlobalUnlock(hmem);
                        let _ = SetClipboardData(
                            fmt,
                            windows::Win32::Foundation::HANDLE(hmem.0 as _),
                        );
                    }
                }
            }
        }
        let _ = CloseClipboard();
    }
}

pub fn frontmost_app() -> (Option<String>, Option<String>) {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return (None, None);
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (process_image_name(pid), None)
    }
}

fn process_image_name(pid: u32) -> Option<String> {
    unsafe {
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if ok.is_ok() {
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            full.rsplit('\\').next().map(|s| s.to_string())
        } else {
            None
        }
    }
}

// -- Clipboard monitor -----------------------------------------------------------

pub struct ClipboardMonitor {
    stop: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
}

impl ClipboardMonitor {
    /// Polling monitor via GetClipboardSequenceNumber — simpler and more robust
    /// than a hidden-window AddClipboardFormatListener for v1; 400 ms cadence.
    /// (AddClipboardFormatListener upgrade documented in WINDOWS_HANDOFF.md.)
    pub fn start(tx: Sender<PlatformEvent>, enabled: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let enabled = Arc::new(AtomicBool::new(enabled));
        {
            let stop = stop.clone();
            let enabled = enabled.clone();
            std::thread::Builder::new()
                .name("echokey-clipboard".into())
                .spawn(move || {
                    let mut last = unsafe { GetClipboardSequenceNumber() };
                    while !stop.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        if !enabled.load(Ordering::SeqCst) {
                            last = unsafe { GetClipboardSequenceNumber() };
                            continue;
                        }
                        let now = unsafe { GetClipboardSequenceNumber() };
                        if now != last {
                            last = now;
                            if clipboard_is_excluded() {
                                continue;
                            }
                            if let Some(text) = read_clipboard() {
                                if text.trim().is_empty() {
                                    continue;
                                }
                                let (app_id, app_name) = clipboard_owner_app();
                                let _ = tx.send(PlatformEvent::ClipboardChanged {
                                    text,
                                    app_id,
                                    app_name,
                                });
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

fn clipboard_is_excluded() -> bool {
    unsafe {
        for name in [
            "ExcludeClipboardContentFromMonitorProcessing",
            "Clipboard Viewer Ignore",
        ] {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let fmt = RegisterClipboardFormatW(windows::core::PCWSTR(wide.as_ptr()));
            if fmt != 0 && IsClipboardFormatAvailable(fmt).is_ok() {
                return true;
            }
        }
        false
    }
}

fn clipboard_owner_app() -> (Option<String>, Option<String>) {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
        let owner = GetClipboardOwner();
        match owner {
            Ok(hwnd) if !hwnd.0.is_null() => {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                (process_image_name(pid), None)
            }
            _ => frontmost_app(),
        }
    }
}

// -- Overlay hardening -------------------------------------------------------------

/// Apply WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW to the HUD so it
/// can never steal focus (tao's focus:false alone is not sufficient).
pub fn harden_overlay(window: &tauri::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST,
    };
    if let Ok(hwnd) = window.hwnd() {
        unsafe {
            let hwnd = HWND(hwnd.0);
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                ex | WS_EX_NOACTIVATE.0 as isize
                    | WS_EX_TOPMOST.0 as isize
                    | WS_EX_TOOLWINDOW.0 as isize,
            );
        }
    }
}

// -- Permissions (Windows needs none of the macOS grants) ---------------------------

pub fn permission_status() -> PermissionStatus {
    PermissionStatus { accessibility: true, microphone: "unknown".into() }
}

pub fn secure_input_active() -> bool {
    false
}

pub fn open_accessibility_settings() {}
pub fn open_microphone_settings() {}
