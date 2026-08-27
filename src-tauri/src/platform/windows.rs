//! Windows platform layer.
//!
//! Hotkeys: the WH_KEYBOARD_LL hook does NOT live here. It lives in the
//! `parle-hook` helper process (crates/echokey-hook) and this module supervises
//! it. A hook proc must return within LowLevelHooksTimeout (~300 ms) or Windows
//! bypasses it and delivers the key natively — which for the Copilot chord
//! means the shell launches Copilot. The proc is trivial, but its thread still
//! has to be *scheduled*, and for ~5 s after launch this process is busy with
//! Tauri, WebView2, a CUDA context and possibly a multi-GB model. Arming 6 ms
//! into startup, THREAD_PRIORITY_HIGHEST, the EcoQoS opt-out and an
//! allocation-free bounded channel were all tried; presses still leaked. Only
//! taking the hook out of this process fixes it, so [`HotkeyListener`] is now a
//! supervisor: it launches the helper, feeds it bindings over a named pipe,
//! receives hotkey events back, restarts it if it dies, and holds it in a
//! kill-on-close job object so it can never outlive us swallowing keys.
//!
//! The load-bearing hook rules (never split a swallow, the dummy-VK Start-menu
//! trick, skipping LLKHF_INJECTED, an allocation-free proc) moved with the hook
//! and are documented at its new home.
//!
//! Text injection, the clipboard monitor and overlay hardening stay here.
//! Injection is SendInput Ctrl+V; UI Automation cannot insert at the caret.

#![cfg(target_os = "windows")]

use super::{
    HotkeyId, InjectionMethod, InjectionOutcome, NativeBindings, NativeKey, PermissionStatus,
    PlatformEvent,
};
use crate::hotkey_logic::KeyPhase;
use crossbeam_channel::Sender;
use echokey_hook as wire;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardOwner, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    VK_CONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RSHIFT, VK_RWIN, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::WM_CLIPBOARDUPDATE;

/// Both directions use fixed-size frames; these are the two buffer sizes for
/// the pipe. Hotkey traffic is a handful of bytes per press.
const PIPE_BUFFER: u32 = 4096;

// -- Hotkeys: supervising the parle-hook helper -------------------------------

/// Public shape unchanged from the in-process hook (start / update_bindings /
/// set_suppress_copilot / set_recording) so state.rs and lib.rs are unaffected.
pub struct HotkeyListener {
    inner: Arc<Supervisor>,
}

struct Supervisor {
    stop: AtomicBool,
    /// Desired state. Re-sent in full every time a helper connects, so a
    /// restarted helper always comes up matching the app.
    bindings: Mutex<wire::WireBindings>,
    suppress: AtomicBool,
    recording: AtomicBool,
    /// The connected pipe, as a raw HANDLE (HANDLE is not Sync). 0 = none.
    pipe: AtomicIsize,
    /// Guards `pipe` for the whole of a write, so the supervisor cannot close
    /// the handle out from under a sender.
    write_lock: Mutex<()>,
    /// Kill-on-close job object owning the helper. If this process dies for any
    /// reason — including the `libc::_exit(0)` on quit — the kernel closes this
    /// handle and Windows kills the helper with it. An orphaned helper would go
    /// on swallowing keys system-wide with nobody left to receive them.
    job: AtomicIsize,
    child: Mutex<Option<std::process::Child>>,
}

impl HotkeyListener {
    pub fn start(
        bindings: NativeBindings,
        suppress_copilot: bool,
        tx: Sender<PlatformEvent>,
    ) -> Self {
        let inner = Arc::new(Supervisor {
            stop: AtomicBool::new(false),
            bindings: Mutex::new(to_wire(&bindings)),
            suppress: AtomicBool::new(suppress_copilot),
            recording: AtomicBool::new(false),
            pipe: AtomicIsize::new(0),
            write_lock: Mutex::new(()),
            job: AtomicIsize::new(create_kill_on_close_job()),
            child: Mutex::new(None),
        });
        let sup = inner.clone();
        std::thread::Builder::new()
            .name("echokey-hook-sup".into())
            .spawn(move || sup.run(tx))
            .expect("spawn hook supervisor");
        Self { inner }
    }

    pub fn update_bindings(&self, bindings: NativeBindings) {
        let w = to_wire(&bindings);
        *self.inner.bindings.lock() = w;
        self.inner.send(&w.encode());
    }

    pub fn set_suppress_copilot(&self, suppress: bool) {
        self.inner.suppress.store(suppress, Ordering::SeqCst);
        self.inner
            .send(&wire::encode_flag(wire::CMD_SUPPRESS_COPILOT, suppress));
    }

    pub fn set_recording(&self, recording: bool) {
        self.inner.recording.store(recording, Ordering::SeqCst);
        self.inner
            .send(&wire::encode_flag(wire::CMD_RECORDING, recording));
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.kill_child();
        // Closing the job kills anything still assigned to it — the belt to the
        // kill_child braces.
        let job = self.inner.job.swap(0, Ordering::SeqCst);
        if job != 0 {
            unsafe {
                let _ = CloseHandle(HANDLE(job as *mut _));
            }
        }
        // The supervisor thread may be parked in ConnectNamedPipe waiting for a
        // helper we just killed; it is left to be reaped at process exit. In
        // practice this Drop only runs at teardown (quit goes through
        // libc::_exit, which never runs it at all).
    }
}

impl Supervisor {
    fn run(self: Arc<Self>, tx: Sender<PlatformEvent>) {
        let mut generation: u64 = 0;
        let mut failures: u32 = 0;
        while !self.stop.load(Ordering::SeqCst) {
            generation += 1;
            // Per-process, per-attempt: a stale helper can never attach to a
            // fresh app, and a fresh helper can never land on a dead pipe.
            let name = format!(
                "{}{}-{}",
                wire::PIPE_PREFIX,
                std::process::id(),
                generation
            );
            let started = std::time::Instant::now();
            if let Err(e) = self.serve_once(&name, &tx) {
                tracing::warn!("hook helper session ended: {e}");
            }
            self.kill_child();
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            // A helper that ran for a while and then died is worth restarting
            // immediately-ish; one that dies on arrival is not worth spinning on.
            if started.elapsed() >= std::time::Duration::from_secs(5) {
                failures = 0;
            } else {
                failures = (failures + 1).min(5);
            }
            std::thread::sleep(std::time::Duration::from_millis(500 << failures));
        }
        tracing::info!("hook supervisor stopped");
    }

    /// One helper lifetime: create the pipe, launch the helper, hand it the
    /// current state, then pump its events until the pipe breaks.
    fn serve_once(&self, pipe_name: &str, tx: &Sender<PlatformEvent>) -> Result<(), String> {
        // Two unidirectional pipes, NOT one duplex pipe. These handles are
        // synchronous, and Windows serialises I/O per file object: the blocking
        // ReadFile this thread parks in would stall every concurrent WriteFile
        // on the same handle until a key arrives. That deadlocked the UI thread
        // on startup. One handle, one direction.
        let cmd = create_pipe(&format!("{pipe_name}-c"), PIPE_ACCESS_OUTBOUND)?;
        let evt = match create_pipe(&format!("{pipe_name}-e"), PIPE_ACCESS_INBOUND) {
            Ok(h) => h,
            Err(e) => {
                unsafe { let _ = CloseHandle(cmd); }
                return Err(e);
            }
        };
        let result = self.serve_connected(cmd, evt, pipe_name, tx);
        unsafe { let _ = CloseHandle(evt); }
        let server = cmd;
        {
            // Hold the write lock across the close so no sender can touch a
            // handle that is being freed.
            let _g = self.write_lock.lock();
            self.pipe.store(0, Ordering::SeqCst);
            unsafe {
                let _ = CloseHandle(server);
            }
        }
        result
    }

    fn serve_connected(
        &self,
        cmd: HANDLE,
        evt: HANDLE,
        pipe_name: &str,
        tx: &Sender<PlatformEvent>,
    ) -> Result<(), String> {
        self.spawn_helper(pipe_name)?;
        // Blocks until the helper connects. It does so within milliseconds:
        // connecting is the first thing it does after arming the hook. The
        // helper opens the command pipe first, so accept them in that order.
        accept(cmd, "cmd")?;
        accept(evt, "evt")?;
        self.pipe.store(cmd.0 as isize, Ordering::SeqCst);
        tracing::info!("hook helper connected");

        // Full state, not a delta: the helper starts blank and a restart must
        // not silently lose a binding.
        let bindings = *self.bindings.lock();
        self.send(&bindings.encode());
        self.send(&wire::encode_flag(
            wire::CMD_SUPPRESS_COPILOT,
            self.suppress.load(Ordering::SeqCst),
        ));
        self.send(&wire::encode_flag(
            wire::CMD_RECORDING,
            self.recording.load(Ordering::SeqCst),
        ));

        let mut frame = [0u8; wire::EVENT_FRAME];
        loop {
            if !read_exact(evt, &mut frame) {
                return Err("helper pipe closed".into());
            }
            if frame[0] != wire::EV_HOTKEY {
                tracing::warn!("ignoring unknown event tag {:#04x}", frame[0]);
                continue;
            }
            let (Some(id), Some(phase)) = (hotkey_from_wire(frame[1]), phase_from_wire(frame[2]))
            else {
                tracing::warn!("ignoring malformed hotkey event {frame:?}");
                continue;
            };
            if tx.send(PlatformEvent::Hotkey { id, phase }).is_err() {
                return Err("dispatcher gone".into());
            }
        }
    }

    fn spawn_helper(&self, pipe_name: &str) -> Result<(), String> {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        /// No console window for a GUI app's helper.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let exe = helper_path().ok_or_else(|| {
            "parle-hook.exe not found next to the app binary; hotkeys are disabled".to_string()
        })?;
        let child = std::process::Command::new(&exe)
            .arg("--pipe")
            .arg(pipe_name)
            .arg("--parent")
            .arg(std::process::id().to_string())
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("could not launch {}: {e}", exe.display()))?;

        let job = self.job.load(Ordering::SeqCst);
        if job != 0 {
            let h = HANDLE(child.as_raw_handle() as *mut _);
            if let Err(e) = unsafe { AssignProcessToJobObject(HANDLE(job as *mut _), h) } {
                // Not fatal: the helper also watches our pid and exits with us.
                tracing::warn!("could not put the hook helper in the job object: {e}");
            }
        }
        *self.child.lock() = Some(child);
        tracing::info!("launched hook helper {}", exe.display());
        Ok(())
    }

    fn kill_child(&self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Send one command frame. Silently does nothing when no helper is
    /// connected — the full state is re-sent on the next connect.
    ///
    /// Callers include the recording path, so this must not stall: it can only
    /// block if the helper stops reading AND the pipe's 4 KB buffer (hundreds
    /// of frames) fills, which means the helper is already wedged.
    fn send(&self, frame: &[u8; wire::CMD_FRAME]) {
        let _g = self.write_lock.lock();
        let raw = self.pipe.load(Ordering::SeqCst);
        if raw == 0 {
            return;
        }
        if !write_all(HANDLE(raw as *mut _), frame) {
            tracing::warn!("write to hook helper failed");
        }
    }
}

/// Where the helper lives. Tauri drops sidecars beside the app binary with the
/// target triple stripped, and cargo puts every workspace binary in the same
/// target directory, so in both installed and dev layouts it is simply "next to
/// us". No path is hardcoded.
fn helper_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for name in [
        "parle-hook.exe",
        // Belt and braces if a bundle ever keeps the triple or the subfolder.
        "parle-hook-x86_64-pc-windows-msvc.exe",
    ] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    let p = dir.join("binaries").join("parle-hook.exe");
    p.is_file().then_some(p)
}

/// ConnectNamedPipe, treating ERROR_PIPE_CONNECTED as success.
///
/// The helper is spawned before we call this, so it routinely connects in the
/// window between CreateNamedPipe and ConnectNamedPipe. Windows reports that as
/// ERROR_PIPE_CONNECTED, which means "already connected" — a success, not a
/// failure. Treating it as an error tore the session down and retried until the
/// race happened to fall the other way.
fn accept(pipe: HANDLE, which: &str) -> Result<(), String> {
    const ERROR_PIPE_CONNECTED: i32 = 535;
    match unsafe { ConnectNamedPipe(pipe, None) } {
        Ok(()) => Ok(()),
        Err(e) if e.code().0 as i32 & 0xFFFF == ERROR_PIPE_CONNECTED => Ok(()),
        Err(e) => Err(format!("ConnectNamedPipe({which}) failed: {e}")),
    }
}

fn create_pipe(name: &str, access: windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES) -> Result<HANDLE, String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let h = unsafe {
        CreateNamedPipeW(
            windows::core::PCWSTR(wide.as_ptr()),
            access,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            None,
        )
    };
    if h.is_invalid() {
        return Err(format!(
            "CreateNamedPipeW({name}) failed: {}",
            windows::core::Error::from_win32()
        ));
    }
    Ok(h)
}

fn read_exact(h: HANDLE, buf: &mut [u8]) -> bool {
    let mut got = 0usize;
    while got < buf.len() {
        let mut n = 0u32;
        if unsafe { ReadFile(h, Some(&mut buf[got..]), Some(&mut n), None) }.is_err() {
            return false;
        }
        if n == 0 {
            return false;
        }
        got += n as usize;
    }
    true
}

fn write_all(h: HANDLE, buf: &[u8]) -> bool {
    let mut sent = 0usize;
    while sent < buf.len() {
        let mut n = 0u32;
        if unsafe { WriteFile(h, Some(&buf[sent..]), Some(&mut n), None) }.is_err() {
            return false;
        }
        if n == 0 {
            return false;
        }
        sent += n as usize;
    }
    true
}

fn create_kill_on_close_job() -> isize {
    unsafe {
        let job = match CreateJobObjectW(None, windows::core::PCWSTR::null()) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("CreateJobObjectW failed ({e}); relying on the helper's parent watch");
                return 0;
            }
        };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Err(e) = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            tracing::warn!("SetInformationJobObject failed: {e}");
        }
        job.0 as isize
    }
}

// -- Wire conversions ---------------------------------------------------------

fn key_to_wire(k: &NativeKey) -> u8 {
    match k {
        NativeKey::Fn => wire::KEY_FN,
        NativeKey::LeftShift => wire::KEY_LEFT_SHIFT,
        NativeKey::RightShift => wire::KEY_RIGHT_SHIFT,
        NativeKey::LeftCtrl => wire::KEY_LEFT_CTRL,
        NativeKey::RightCtrl => wire::KEY_RIGHT_CTRL,
        NativeKey::LeftAlt => wire::KEY_LEFT_ALT,
        NativeKey::RightAlt => wire::KEY_RIGHT_ALT,
        NativeKey::LeftCmd => wire::KEY_LEFT_CMD,
        NativeKey::RightCmd => wire::KEY_RIGHT_CMD,
        NativeKey::CopilotKey => wire::KEY_COPILOT,
        NativeKey::Escape => wire::KEY_ESCAPE,
    }
}

fn to_wire(b: &NativeBindings) -> wire::WireBindings {
    wire::WireBindings {
        dictation_key: b.dictation.as_ref().map_or(wire::KEY_NONE, |w| key_to_wire(&w.key)),
        dictation_swallow: b.dictation.as_ref().is_some_and(|w| w.swallow),
        dictation_alt_key: b
            .dictation_alt
            .as_ref()
            .map_or(wire::KEY_NONE, |w| key_to_wire(&w.key)),
        dictation_alt_swallow: b.dictation_alt.as_ref().is_some_and(|w| w.swallow),
        cancel_key: b.cancel.as_ref().map_or(wire::KEY_NONE, key_to_wire),
    }
}

fn hotkey_from_wire(id: u8) -> Option<HotkeyId> {
    Some(match id {
        wire::HK_DICTATION => HotkeyId::Dictation,
        wire::HK_DICTATION_ALT => HotkeyId::DictationAlt,
        wire::HK_CANCEL => HotkeyId::Cancel,
        wire::HK_PALETTE => HotkeyId::Palette,
        _ => return None,
    })
}

fn phase_from_wire(p: u8) -> Option<KeyPhase> {
    Some(match p {
        wire::PHASE_DOWN => KeyPhase::Down,
        wire::PHASE_UP => KeyPhase::Up,
        _ => return None,
    })
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
    press_enter: bool,
    view: super::FieldView,
) -> InjectionOutcome {
    // A genuinely SECURE FIELD: hand it over concealed and let the user paste.
    //
    // macOS grew this gate in round 11 and Windows did not, in the same commit
    // that split the writer so only the excluded variant declares
    // `CanUploadToCloudClipboard = 0`. The result was the worst arrangement of
    // the two: the pipeline classified the dictation as secret and refused to
    // put it in Parle's own LAN-only history, and this function handed the same
    // string to Windows Cloud Clipboard, which uploads it to Microsoft and
    // syncs it to the user's other machines.
    if view.is_secure == Some(true) {
        write_clipboard_excluded(text);
        return InjectionOutcome {
            method: InjectionMethod::ClipboardOnly,
            manual_paste_required: true,
        };
    }

    // Snapshot the MARKING as well as the text, exactly as macOS does.
    //
    // The restore put the user's original back with `write_clipboard`, which
    // since round 11 declares no exclusion formats at all. Copy a password out
    // of a password manager, dictate anywhere with the shipped restore default
    // on, and Parle republished it with the owner's own "do not keep this, do
    // not upload this" statement deleted.
    //
    // The two reads are BRACKETED by the sequence number, because they are two
    // separate clipboard sessions. `read_clipboard` ignores markers entirely,
    // so it will happily capture a password manager's marked secret; if the
    // clipboard then turned over to ordinary content before the probe opened,
    // the probe answered "not excluded" and the restore republished that secret
    // with `CanUploadToCloudClipboard = 0` deleted. That is the exact leak this
    // pair was added to close. `read_clipboard_unless_excluded` forty lines
    // away already brackets its own read for the same reason.
    let seq_before = unsafe { GetClipboardSequenceNumber() };
    let mut previous = read_clipboard();
    let mut previous_excluded = clipboard_is_excluded();
    if unsafe { GetClipboardSequenceNumber() } != seq_before {
        // The text and the marking may belong to different content. Marking is
        // the safe direction: over-marking costs a Win+V entry, under-marking
        // uploads a secret to Microsoft and pushes it to the user's other
        // machines.
        previous_excluded = true;
        // And DROP the text. Correcting the marking while keeping the payload
        // republishes content that is no longer what the user has, over
        // whatever they copied in the window. `read_clipboard_unless_excluded`
        // answers this same signal with `return None`: the bracket borrowed
        // that mechanism and not its conclusion.
        previous = None;
    }
    write_clipboard_inner(text, view.conceal);
    // The number the WRITER recorded, not a fresh read.
    //
    // `write_clipboard_inner` reads the sequence number inside its clipboard
    // session and stores it in `OUR_LAST_WRITE`; reading it again out here
    // happens after `CloseClipboard`, so the two are equal only if closing the
    // clipboard does not move the counter. Nothing in this repo establishes
    // that, and Windows clipboard behaviour has never been checked on hardware.
    // Taking the writer's own number is correct whichever way it goes.
    let seq_after_write = OUR_LAST_WRITE.load(std::sync::atomic::Ordering::SeqCst);
    synth_ctrl_v();
    if press_enter {
        std::thread::sleep(std::time::Duration::from_millis(160));
        unsafe {
            const VK_RETURN: u16 = 0x0D;
            let inputs = [key_input(VK_RETURN, false), key_input(VK_RETURN, true)];
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }

    if !keep_on_clipboard && restore {
        // The OLDEST original is carried across chained dictations, exactly as
        // macOS does it.
        //
        // Without this, a second dictation inside `restore_delay_ms` snapshots
        // the FIRST dictation's transcript as "the user's previous clipboard",
        // and its thread is the one whose sequence number matches. The user's
        // real clipboard is gone and a Parle transcript is put back in its
        // place, re-marked from a probe of Parle's own unmarked write. macOS
        // fixed this and the fix never crossed.
        {
            let mut pending = PENDING_RESTORE.lock();
            if pending.is_none() {
                if let Some(prev) = previous {
                    *pending = Some((prev, previous_excluded));
                }
            }
        }
        let generation = RESTORE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(restore_delay_ms));
            // Only the LAST dictation in a chain restores, and only if nobody
            // else has written the clipboard since our write.
            if RESTORE_GENERATION.load(std::sync::atomic::Ordering::SeqCst) != generation {
                return;
            }
            let seq_now = unsafe { GetClipboardSequenceNumber() };
            if seq_now != seq_after_write {
                return;
            }
            let taken = PENDING_RESTORE.lock().take();
            if let Some((prev, excluded)) = taken {
                write_clipboard_inner(&prev, excluded);
            }
        });
    } else if keep_on_clipboard {
        // The transcript is meant to stay; drop any pending restore.
        *PENDING_RESTORE.lock() = None;
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
            let h = windows::Win32::Foundation::HGLOBAL(handle.0 as _);
            // Bounded for the same reason as the merged reader above.
            let cap = GlobalSize(h) / 2;
            if cap == 0 {
                return None;
            }
            let ptr = GlobalLock(h) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while len < cap && *ptr.add(len) != 0 {
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

/// Clipboard formats that mean "do not capture this", in ONE place.
///
/// These used to be two separate lists, and they had drifted: `write_clipboard`
/// declared three formats when marking Parle's own output, while
/// `clipboard_is_excluded` consulted only two of them on the way IN. The same
/// file treated `CanIncludeInClipboardHistory` as meaningful when it wrote it
/// and meaningless when it read it, so an app that opts out only that way had
/// its secret captured, stored, and replicated to the user's Mac, whose own
/// stricter NSPasteboard rules never got a vote.
///
/// Split by SEMANTICS, because these two groups are not read the same way.
///
/// Presence-only markers: the format existing at all is the whole signal. Both
/// are long-standing clipboard-manager conventions (KeePassXC, ClipMate).
const EXCLUDE_MARKER_FORMATS: [&str; 2] = [
    "ExcludeClipboardContentFromMonitorProcessing",
    "Clipboard Viewer Ignore",
];

/// DWORD-valued formats where the VALUE decides: 0 means "no", non-zero means
/// "yes, allowed". Checking presence alone would wrongly exclude content from
/// an app that explicitly opted IN by writing 1.
///
/// `CanUploadToCloudClipboard` is the most on-point signal there is for this
/// app: it is Windows' way of saying "do not move this off this machine", which
/// is precisely what LAN sync would do. Nothing consulted it before.
const EXCLUDE_DWORD_FORMATS: [&str; 2] =
    ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"];

fn register_format(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { RegisterClipboardFormatW(windows::core::PCWSTR(wide.as_ptr())) }
}

/// Write text with a claim about the CONTENT.
///
/// Only a concealed write declares the exclusion formats. The unconditional
/// version told Win+V and Cloud Clipboard to discard the row the user had just
/// deliberately pressed Copy on in Parle's own palette, which is the same
/// defect the macOS side fixed and this one did not mirror. Self-capture is
/// suppressed by sequence number, not by relabelling the user's data.
pub fn write_clipboard_marked(text: &str, concealed: bool) {
    if concealed {
        write_clipboard_excluded(text);
    } else {
        write_clipboard(text);
    }
}

/// The clipboard sequence number of the last write PARLE made.
///
/// Self-capture is suppressed by identity, matching macOS. Marking every write
/// with the exclusion formats also worked and told Win+V and Cloud Clipboard to
/// discard the row the user had just deliberately pressed Copy on.
/// The oldest clipboard a chain of dictations displaced, and its marking.
static PENDING_RESTORE: parking_lot::Mutex<Option<(String, bool)>> =
    parking_lot::Mutex::new(None);
/// Only the newest scheduled restore may fire.
static RESTORE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static OUR_LAST_WRITE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Did WE write the clipboard's current contents?
pub fn we_wrote_change(seq: u32) -> bool {
    OUR_LAST_WRITE.load(std::sync::atomic::Ordering::SeqCst) == seq
}

/// Write text as the USER's own copy: no exclusion markers.
pub fn write_clipboard(text: &str) {
    write_clipboard_inner(text, false);
}

/// Write text marked so Win+V history, Cloud Clipboard and other monitors skip
/// it. For content we have reason to believe is a secret, not merely for our
/// own writes.
pub fn write_clipboard_excluded(text: &str) {
    write_clipboard_inner(text, true);
}

fn write_clipboard_inner(text: &str, exclude: bool) {
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
        if exclude {
            // Every format we would HONOUR on the way in, we also declare on
            // the way out. Same two lists, so they cannot drift apart again.
            for name in EXCLUDE_MARKER_FORMATS.iter().chain(EXCLUDE_DWORD_FORMATS.iter()) {
                let fmt = register_format(name);
                if fmt != 0 {
                    // A DWORD zero payload (0 = exclude).
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
        }
        // Read and stored BEFORE the close, matching macOS, where round 11
        // moved the equivalent store above the payload and wrote down why:
        // between the change becoming visible and us recording that it was
        // ours, a monitor poll sees `we_wrote_change` as false. Two failures
        // came out of that one line. Our own dictation could be captured as a
        // clipboard row and replicated, which walks a withheld transcript back
        // into history through the side door; and another process's write
        // landing in the window was recorded as ours and then skipped, losing a
        // real user copy.
        OUR_LAST_WRITE.store(GetClipboardSequenceNumber(), std::sync::atomic::Ordering::SeqCst);
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
                            // Our OWN write, by identity rather than by
                            // relabelling the user's data. See OUR_LAST_WRITE.
                            if we_wrote_change(now) {
                                continue;
                            }
                            if let Some(text) = read_clipboard_unless_excluded() {
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

/// Does the current clipboard carry any "do not capture this" marker?
///
/// Reads the markers, the DWORD values AND the text in ONE clipboard session.
///
/// The previous version opened the clipboard up to twice for the DWORD formats
/// and then `read_clipboard` opened it a third time, with nothing carrying the
/// decision and the payload across. Between the check and the read any process
/// can `EmptyClipboard` and write new content, so a password manager copying in
/// that window had its secret read under a decision made about somebody else's
/// data. `open_clipboard_retry` is ten attempts at 10ms, so the window was
/// bounded at roughly 300ms of contention rather than the microseconds it was
/// before the DWORD formats were honoured.
///
/// Returns `None` when the capture must be skipped, `Some(text)` otherwise.
/// Did the clipboard's current owner ask that this content not be kept?
///
/// The sibling of macOS's `clipboard_is_concealed`. `read_clipboard_unless_excluded`
/// answers "may I capture this" by collapsing every refusal into `None`, which
/// is the right answer for the monitor and useless to the RESTORE path, which
/// has to put the content back exactly as it found it, marking included.
fn clipboard_is_excluded() -> bool {
    unsafe {
        if !open_clipboard_retry() {
            // Cannot tell. The conservative reading is that it was marked, so
            // the restore re-declares the exclusion rather than dropping it.
            return true;
        }
        let excluded = (|| -> bool {
            for name in EXCLUDE_MARKER_FORMATS {
                let fmt = register_format(name);
                if fmt != 0 && IsClipboardFormatAvailable(fmt).is_ok() {
                    return true;
                }
            }
            for name in EXCLUDE_DWORD_FORMATS {
                let fmt = register_format(name);
                if fmt == 0 || IsClipboardFormatAvailable(fmt).is_err() {
                    continue;
                }
                let Ok(handle) = GetClipboardData(fmt) else {
                    return true;
                };
                let h = windows::Win32::Foundation::HGLOBAL(handle.0 as _);
                if GlobalSize(h) < 4 {
                    return true;
                }
                let ptr = GlobalLock(h) as *const u32;
                if ptr.is_null() {
                    return true;
                }
                let v = *ptr;
                let _ = GlobalUnlock(h);
                if v == 0 {
                    return true;
                }
            }
            false
        })();
        let _ = CloseClipboard();
        excluded
    }
}

fn read_clipboard_unless_excluded() -> Option<String> {
    unsafe {
        // The sequence number before and after. If it moves, the content we
        // judged is not the content we read, so the capture is discarded.
        let before = GetClipboardSequenceNumber();
        if !open_clipboard_retry() {
            return None;
        }
        let out = (|| -> Option<String> {
            for name in EXCLUDE_MARKER_FORMATS {
                let fmt = register_format(name);
                if fmt != 0 && IsClipboardFormatAvailable(fmt).is_ok() {
                    return None;
                }
            }
            // These carry a DWORD where the VALUE decides: 0 means no,
            // non-zero means the app explicitly opted in. Absence is not a
            // refusal, it is an app that never expressed a preference.
            for name in EXCLUDE_DWORD_FORMATS {
                let fmt = register_format(name);
                if fmt == 0 || IsClipboardFormatAvailable(fmt).is_err() {
                    continue;
                }
                let Ok(handle) = GetClipboardData(fmt) else {
                    // Present but unreadable: somebody expressed a preference
                    // we cannot read, and the only safe reading of that is no.
                    return None;
                };
                let h = windows::Win32::Foundation::HGLOBAL(handle.0 as _);
                // The allocation is written by ANOTHER process. Reading four
                // bytes without asking how many there are is an unchecked
                // cross-process dereference; `GlobalAlloc` granularity makes a
                // fault unlikely, which is not the same as correct.
                if GlobalSize(h) < 4 {
                    return None;
                }
                let ptr = GlobalLock(h) as *const u32;
                if ptr.is_null() {
                    return None;
                }
                let v = *ptr;
                let _ = GlobalUnlock(h);
                if v == 0 {
                    return None;
                }
            }
            if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
                return None;
            }
            let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
            let h = windows::Win32::Foundation::HGLOBAL(handle.0 as _);
            // BOUNDED by the allocation, like the DWORD read above.
            //
            // The NUL scan had no bound, and this buffer belongs to whichever
            // process owns the clipboard. An app that publishes a
            // CF_UNICODETEXT handle whose buffer is not NUL-terminated makes
            // this walk off the end: an access violation at best, and at worst
            // whatever is mapped next gets appended to the captured text,
            // stored in history, and replicated to the user's other machine.
            //
            // Bounding the DWORD read and not this one was the inconsistency:
            // the comment three lines up called the unchecked version "an
            // unchecked cross-process dereference" while this did the same
            // thing over an unbounded range.
            let cap = GlobalSize(h) / 2;
            if cap == 0 {
                return None;
            }
            let ptr = GlobalLock(h) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while len < cap && *ptr.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(h);
            Some(text)
        })();
        let _ = CloseClipboard();
        if GetClipboardSequenceNumber() != before {
            // The clipboard changed under us, so the marker check and the text
            // may not describe the same content. The next poll will see it.
            return None;
        }
        out
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
    PermissionStatus { accessibility: true, microphone: microphone_status() }
}

/// Windows 11 has no runtime consent prompt for unpackaged Win32 apps: mic
/// access is decided by the CapabilityAccessManager consent store, which the
/// Settings app writes. Read it so the UI can show real state instead of
/// "unknown". Both gates must allow: the global one and the desktop-app one.
fn microphone_status() -> String {
    const BASE: &str =
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    let global = consent_value(BASE);
    let desktop = consent_value(&format!(r"{BASE}\NonPackaged"));
    match (global.as_deref(), desktop.as_deref()) {
        (Some("Deny"), _) | (_, Some("Deny")) => "denied".into(),
        (Some("Allow"), _) => "granted".into(),
        _ => "unknown".into(),
    }
}

fn consent_value(subkey: &str) -> Option<String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let path = HSTRING::from(subkey);
    let name = HSTRING::from("Value");
    let mut buf = [0u16; 32];
    let mut cb = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &path,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut cb),
        )
    };
    if rc.is_err() {
        return None;
    }
    // cb counts bytes including the NUL terminator.
    let len = (cb as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len.min(buf.len())]))
}

/// macOS's system-wide secure-event-input flag has no Windows equivalent, so
/// this is always false and callers must not read it as "not a password field".
///
/// Use [`focused_field_is_secure`] for that question. This is kept only so the
/// two platforms expose the same surface.
pub fn secure_input_active() -> bool {
    false
}

/// Is the focused control a password field?
///
/// The classic Win32 answer, which covers `EDIT` controls created with
/// `ES_PASSWORD` and everything built on them. `GetGUIThreadInfo` for the
/// foreground thread gives the focused HWND without attaching to its input
/// queue, so this is cheap and has no side effects.
///
/// Returns `None` when we cannot tell rather than `false`, because "we could
/// not read the focus" and "the focus is an ordinary field" are different
/// answers and the caller decides what to do about each.
///
/// KNOWN GAP, stated rather than papered over: this does NOT see a WinUI
/// `PasswordBox` or a Chromium `<input type=password>`, both of which draw
/// their own controls. Those need UI Automation's `UIA_IsPasswordPropertyId`,
/// which is a larger dependency. Until that lands, a dictation into a browser
/// password field on Windows is NOT recognised as secure. That is a real hole
/// and it is written down here so it is not mistaken for coverage: this
/// function returning `Some(false)` means "the classic check says no", not
/// "this is definitely not a password field".
pub fn focused_field_is_secure() -> Option<bool> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, SendMessageTimeoutW,
        GUITHREADINFO, SMTO_ABORTIFHUNG,
    };
    // EM_GETPASSWORDCHAR, not the ES_PASSWORD style bit.
    //
    // The style-bit version was wrong in the dangerous direction. 0x0020 is
    // only ES_PASSWORD for windows of the EDIT class; every common control
    // class gives bit 5 its own meaning, and the class was never checked. A
    // focused tree view, owner-draw list or left-text checkbox therefore
    // reported SECURE, and the dictation was silently dropped with nothing but
    // a log line: round 9's headline failure reached through a different
    // mechanism, on the platform the branch is named after.
    //
    // EM_GETPASSWORDCHAR is meaningless outside an edit control, so a window
    // that is not one simply does not answer, and a style bit cannot spoof it.
    // A non-zero result is the masking character, which is what a password
    // field has and an ordinary edit does not.
    //
    // SMTO_ABORTIFHUNG with a short timeout, because this runs on the dictation
    // path and a hung foreground application must not stall it.
    const EM_GETPASSWORDCHAR: u32 = 0x00D2;
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        let tid = GetWindowThreadProcessId(fg, None);
        if tid == 0 {
            return None;
        }
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(tid, &mut gti).is_err() {
            return None;
        }
        if gti.hwndFocus.0.is_null() {
            return None;
        }
        let mut out: usize = 0;
        let rc = SendMessageTimeoutW(
            gti.hwndFocus,
            EM_GETPASSWORDCHAR,
            None,
            None,
            SMTO_ABORTIFHUNG,
            100,
            Some(&mut out as *mut usize),
        );
        if rc.0 == 0 {
            // Timed out, or the window does not handle the message. Either way
            // we could not tell, which is a THIRD answer and not a `false`:
            // the caller keeps such a dictation locally rather than dropping it
            // or replicating it.
            return None;
        }
        Some(out != 0)
    }
}

pub fn open_accessibility_settings() {}

/// Deep-link to Settings > Privacy & security > Microphone. This is the only
/// actionable path on Windows: there is no prompt API for unpackaged apps.
/// Windows has no per-app local network permission; the firewall is the
/// equivalent gate, so send the user there.
pub fn open_local_network_settings() {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", "ms-settings:windowsdefender"])
        .spawn();
}

pub fn open_microphone_settings() {
    let _ = std::process::Command::new("explorer")
        .arg("ms-settings:privacy-microphone")
        .spawn();
}

/// Windows: hiding our window returns focus automatically; explicit
/// activation of another process is restricted (SetForegroundWindow rules).
pub fn activate_app(_bundle_id: &str) -> bool {
    false
}

// -- Machine info -------------------------------------------------------------

/// Installed physical RAM in MB, via GlobalMemoryStatusEx. echokey-asr keeps
/// itself free of the `windows` dependency, so the real value is measured here
/// and published to its registry at startup (see registry::set_total_ram_mb).
pub fn total_ram_mb() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(status.ullTotalPhys / 1_048_576)
}

/// Opt out of Windows power throttling (EcoQoS).
///
/// Windows 11 throttles processes with no visible window — which is Parle's
/// normal state, sitting in the tray. A throttled process's low-level keyboard
/// hook can exceed LowLevelHooksTimeout (~300 ms), at which point Windows
/// bypasses the hook and delivers the key natively: the Copilot key opens
/// Copilot instead of starting dictation. Thread priority alone can't fix this,
/// because throttling caps the whole process's execution speed.
pub fn disable_power_throttling() {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, SetProcessInformation, ProcessPowerThrottling,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE,
    };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0, // 0 with the bit in ControlMask = explicitly DISABLE throttling
    };
    let ok = unsafe {
        SetProcessInformation(
            HANDLE(GetCurrentProcess().0),
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    match ok {
        Ok(()) => tracing::info!("power throttling disabled for this process"),
        Err(e) => tracing::warn!("could not disable power throttling: {e}"),
    }
}

/// Lower/restore the CALLING thread's priority around long background work.
/// Model prewarm is a multi-GB CUDA load; at normal priority it competes with
/// the keyboard hook thread, and a hook that overruns LowLevelHooksTimeout gets
/// bypassed — the keypress is delivered natively and dictation misses it.
pub fn set_background_priority(on: bool) {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL,
    };
    let level = if on { THREAD_PRIORITY_BELOW_NORMAL } else { THREAD_PRIORITY_NORMAL };
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), level);
    }
}
