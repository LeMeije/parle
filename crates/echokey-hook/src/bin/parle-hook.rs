//! `parle-hook` — Parle's keyboard-hook helper process.
//!
//! Why this exists: a WH_KEYBOARD_LL hook proc is a callback Windows *waits*
//! for. Overrun LowLevelHooksTimeout (~300 ms) and the OS bypasses the hook and
//! delivers the key natively — for the Copilot chord that means the shell
//! launches Copilot instead of Parle starting dictation. The proc itself is
//! trivial, but its *thread* still has to be scheduled, and for roughly five
//! seconds after launch the main app is initialising Tauri, WebView2, a CUDA
//! context and possibly a multi-GB model. Thread priority, EcoQoS opt-out and
//! arming 6 ms into process start were all tried and none of them survive that
//! contention. So the hook lives here instead, in a process whose entire job is
//! to own the hook: it is armed within milliseconds of exec and has nothing to
//! compete with.
//!
//! The app is the pipe *server* and starts us with the pipe name and its own
//! pid. We forward hotkey events up and take binding/suppression/recording
//! updates down. If the pipe breaks or the parent dies we exit immediately: an
//! orphaned helper would go on swallowing keys system-wide with nobody left to
//! receive them.
//!
//! Load-bearing rules, carried over verbatim from the in-process hook:
//! - Never swallow a key-down and let its key-up escape (stuck-modifier bug).
//!   Swallow both or neither.
//! - After swallowing the Copilot F23 while LWin is held, inject a dummy VK
//!   0xFF so Windows counts Win as "used as a modifier" and does not open the
//!   Start menu on release (the PowerToys trick).
//! - Skip events flagged LLKHF_INJECTED (our own synthetic input).
//! - The hook proc allocates nothing, locks nothing and never blocks. In
//!   particular it must never log: file I/O there would cost keypresses.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    // Non-Windows builds exist only so `cargo build --workspace` succeeds on
    // macOS; there is no low-level keyboard hook to install there.
}

#[cfg(windows)]
fn main() {
    win::main();
}

#[cfg(windows)]
mod win {
    use echokey_hook::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
    use std::sync::OnceLock;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, OpenProcess, ProcessPowerThrottling,
        SetProcessInformation, SetThreadPriority, WaitForSingleObject, INFINITE,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, PROCESS_SYNCHRONIZE, THREAD_PRIORITY_HIGHEST,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, SendInput, UnregisterHotKey, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
        KEYEVENTF_KEYUP, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VIRTUAL_KEY, VK_F23, VK_LCONTROL,
        VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
        UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL,
        WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    const VK_LAUNCH_APP1: u32 = 0xB6;
    const VK_DUMMY: u16 = 0xFF;
    const VK_ESCAPE: u32 = 0x1B;
    const COPILOT_HOTKEY_ID: i32 = 0xE001;

    /// Bounded and pre-allocated: `try_send` on it neither allocates nor waits,
    /// which is what the hook proc needs. Far beyond any real burst of hotkeys.
    const EVENT_CAPACITY: usize = 256;

    // The hook proc cannot carry a closure, so its state lives in statics — and
    // all of it is atomic, because a mutex inside the proc would be a blocking
    // call inside a callback Windows is timing.
    static EVENT_TX: OnceLock<crossbeam_channel::Sender<[u8; EVENT_FRAME]>> = OnceLock::new();
    static BINDINGS: AtomicU64 = AtomicU64::new(0);
    static RECORDING: AtomicBool = AtomicBool::new(false);
    static SUPPRESS_COPILOT: AtomicBool = AtomicBool::new(true);
    static LSHIFT_DOWN: AtomicBool = AtomicBool::new(false);
    static LWIN_DOWN: AtomicBool = AtomicBool::new(false);
    /// While true, we are mid-chord and swallowing the Copilot F23 events.
    static COPILOT_ACTIVE: AtomicBool = AtomicBool::new(false);
    /// The connected pipe, as a raw handle (HANDLE is not Sync).
    static PIPE: AtomicIsize = AtomicIsize::new(0);

    pub fn main() {
        let Some((pipe_name, parent_pid)) = parse_args() else {
            log("usage: parle-hook --pipe <name> --parent <pid>");
            return;
        };
        log(&format!(
            "parle-hook starting: pipe={pipe_name} parent={parent_pid}"
        ));

        // We have no window at all, which makes us a prime EcoQoS throttling
        // candidate — and a throttled hook is a bypassed hook.
        disable_power_throttling();

        let (tx, rx) = crossbeam_channel::bounded::<[u8; EVENT_FRAME]>(EVENT_CAPACITY);
        // Must be in place before the hook can fire.
        let _ = EVENT_TX.set(tx);

        // Nothing above this line touches the disk or the network: the hook is
        // armed within milliseconds of exec, which is the whole point.
        let hook: HHOOK = unsafe {
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), None, 0) {
                Ok(h) => h,
                Err(e) => {
                    log(&format!("SetWindowsHookExW failed: {e}"));
                    return;
                }
            }
        };
        unsafe {
            if SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST).is_err() {
                log("could not raise hook thread priority");
            }
        }
        log("WH_KEYBOARD_LL hook installed");

        // Belt and braces against the parent vanishing without closing the job
        // object (it should never happen, but an orphan here swallows keys
        // system-wide with nobody listening).
        std::thread::spawn(move || watch_parent(parent_pid));
        std::thread::spawn(move || ipc_thread(pipe_name, rx));

        // Second line of defence for the Copilot chord: RegisterHotKey makes
        // the OS claim the combination and post WM_HOTKEY here, with no timeout
        // to overrun. In practice the Windows shell already owns the chord and
        // this fails with 0x80070581 — kept because it costs nothing and would
        // catch anything the hook missed on a machine where it does succeed.
        let copilot_hotkey = unsafe {
            RegisterHotKey(
                None,
                COPILOT_HOTKEY_ID,
                MOD_WIN | MOD_SHIFT | MOD_NOREPEAT,
                VK_F23.0 as u32,
            )
        };
        match &copilot_hotkey {
            Ok(()) => log("Copilot chord claimed via RegisterHotKey"),
            Err(e) => log(&format!("RegisterHotKey for Copilot chord failed: {e}")),
        }

        // LL hooks are serviced through the installing thread's message queue:
        // without this pump the hook proc never runs.
        unsafe {
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == COPILOT_HOTKEY_ID {
                    // The hook missed this press but the OS handed it to us, so
                    // Copilot did NOT launch. WM_HOTKEY has no key-up; Toggle
                    // only needs the down edge.
                    let bindings = WireBindings::unpack(BINDINGS.load(Ordering::Relaxed));
                    if let Some((id, _)) = bindings.binding_for(KEY_COPILOT) {
                        send_event(id, PHASE_DOWN);
                        send_event(id, PHASE_UP);
                    }
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if copilot_hotkey.is_ok() {
                let _ = UnregisterHotKey(None, COPILOT_HOTKEY_ID);
            }
            let _ = UnhookWindowsHookEx(hook);
        }
        log("message pump ended; exiting");
    }

    fn parse_args() -> Option<(String, u32)> {
        let mut pipe = None;
        let mut parent = None;
        let args: Vec<String> = std::env::args().collect();
        let mut i = 1;
        while i + 1 < args.len() {
            match args[i].as_str() {
                "--pipe" => pipe = Some(args[i + 1].clone()),
                "--parent" => parent = args[i + 1].parse::<u32>().ok(),
                _ => {}
            }
            i += 1;
        }
        Some((pipe?, parent?))
    }

    // -- The hook proc ------------------------------------------------------
    //
    // Everything below runs inside a callback Windows is timing. No
    // allocations, no locks, no logging, no blocking calls.

    unsafe extern "system" fn ll_keyboard_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
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
        let phase = if is_down { PHASE_DOWN } else { PHASE_UP };

        // Track chord modifiers.
        if vk == VK_LSHIFT.0 as u32 {
            LSHIFT_DOWN.store(is_down, Ordering::SeqCst);
        }
        if vk == VK_LWIN.0 as u32 {
            LWIN_DOWN.store(is_down, Ordering::SeqCst);
        }

        let bindings = WireBindings::unpack(BINDINGS.load(Ordering::Relaxed));

        // -- Copilot key: LShift+LWin+F23 chord (or discrete VK_LAUNCH_APP1). --
        let is_copilot_f23 = vk == VK_F23.0 as u32
            && (COPILOT_ACTIVE.load(Ordering::SeqCst)
                || (LSHIFT_DOWN.load(Ordering::SeqCst) && LWIN_DOWN.load(Ordering::SeqCst)));
        let is_copilot_app1 = vk == VK_LAUNCH_APP1;
        if is_copilot_f23 || is_copilot_app1 {
            if let Some((id, swallow)) = bindings.binding_for(KEY_COPILOT) {
                if is_down && !COPILOT_ACTIVE.load(Ordering::SeqCst) {
                    COPILOT_ACTIVE.store(true, Ordering::SeqCst);
                    send_event(id, PHASE_DOWN);
                    if is_copilot_f23 && swallow {
                        // Mark Win as "used as modifier" so releasing it doesn't
                        // open the Start menu after we swallow F23.
                        inject_dummy_key();
                    }
                } else if is_up {
                    COPILOT_ACTIVE.store(false, Ordering::SeqCst);
                    send_event(id, PHASE_UP);
                }
                if swallow {
                    // Swallow BOTH down and up (never split a swallow).
                    return LRESULT(1);
                }
                return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
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

        // -- Bare modifier bindings (L/R discriminated by vkCode). ------------
        let key = vk_to_wire(vk);
        if key != KEY_NONE {
            if let Some((id, swallow)) = bindings.binding_for(key) {
                // Auto-repeat suppression for held modifiers: Windows repeats
                // key-down; forward only transitions.
                send_event(id, phase);
                if swallow {
                    // Swallow the bare-modifier binding completely so it stops
                    // acting as a modifier while bound (both down and up).
                    return LRESULT(1);
                }
                return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
            }
            if bindings.cancel_key == key && RECORDING.load(Ordering::SeqCst) {
                if is_down {
                    send_event(HK_CANCEL, PHASE_DOWN);
                }
                return LRESULT(1);
            }
        }

        // Escape cancel (only while recording).
        if vk == VK_ESCAPE
            && RECORDING.load(Ordering::SeqCst)
            && bindings.cancel_key == KEY_ESCAPE
        {
            if is_down {
                send_event(HK_CANCEL, PHASE_DOWN);
            }
            return LRESULT(1);
        }

        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    /// Hand an event to the writer thread. `try_send` on a pre-allocated
    /// bounded channel: never allocates, never waits, drops rather than stall
    /// the hook if the app somehow stopped draining.
    fn send_event(id: u8, phase: u8) {
        if let Some(tx) = EVENT_TX.get() {
            let _ = tx.try_send(encode_hotkey(id, phase));
        }
    }

    fn vk_to_wire(vk: u32) -> u8 {
        let vk = VIRTUAL_KEY(vk as u16);
        match vk {
            v if v == VK_LSHIFT => KEY_LEFT_SHIFT,
            v if v == VK_RSHIFT => KEY_RIGHT_SHIFT,
            v if v == VK_LCONTROL => KEY_LEFT_CTRL,
            v if v == VK_RCONTROL => KEY_RIGHT_CTRL,
            v if v == VK_LMENU => KEY_LEFT_ALT,
            // Right Alt is AltGr on many layouts; supported but not default.
            v if v == VK_RMENU => KEY_RIGHT_ALT,
            v if v == VK_LWIN => KEY_LEFT_CMD,
            v if v == VK_RWIN => KEY_RIGHT_CMD,
            _ => KEY_NONE,
        }
    }

    fn inject_dummy_key() {
        unsafe {
            let inputs = [key_input(VK_DUMMY, false), key_input(VK_DUMMY, true)];
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

    // -- IPC ----------------------------------------------------------------

    /// Connect to the app's pipe, then read commands until it breaks. The
    /// writer runs on its own thread because the two directions block
    /// independently; concurrent read and write on one duplex pipe is fine.
    fn ipc_thread(pipe_name: String, rx: crossbeam_channel::Receiver<[u8; EVENT_FRAME]>) {
        // Two unidirectional pipes, NOT one duplex pipe. These handles are
        // synchronous, and Windows serialises I/O per file object: a blocking
        // ReadFile parked on a handle stalls any concurrent WriteFile on that
        // same handle until it completes. With one duplex pipe the reader waits
        // for a keypress that cannot arrive while the writer is wedged behind
        // it — a deadlock on both sides. One handle, one direction.
        let Some(handle) = connect(&format!("{pipe_name}-c"), GENERIC_READ.0) else {
            log("could not connect to the app command pipe; exiting");
            exit();
        };
        let Some(evt) = connect(&format!("{pipe_name}-e"), GENERIC_WRITE.0) else {
            log("could not connect to the app event pipe; exiting");
            exit();
        };
        PIPE.store(evt.0 as isize, Ordering::SeqCst);
        log("connected to app");

        // Events queued while we were connecting are still in the channel and
        // go out now: a press in the first milliseconds is not lost.
        std::thread::spawn(move || {
            for frame in rx {
                let h = HANDLE(PIPE.load(Ordering::SeqCst) as *mut _);
                if !write_all(h, &frame) {
                    log("event write failed; exiting");
                    exit();
                }
            }
        });

        let mut frame = [0u8; CMD_FRAME];
        loop {
            if !read_exact(handle, &mut frame) {
                log("app pipe closed; exiting");
                exit();
            }
            match frame[0] {
                CMD_BINDINGS => {
                    let b = WireBindings::decode(&frame);
                    BINDINGS.store(b.pack(), Ordering::Relaxed);
                    log(&format!("bindings updated: {b:?}"));
                }
                CMD_SUPPRESS_COPILOT => {
                    SUPPRESS_COPILOT.store(frame[1] != 0, Ordering::SeqCst);
                }
                CMD_RECORDING => {
                    RECORDING.store(frame[1] != 0, Ordering::SeqCst);
                }
                tag => log(&format!("ignoring unknown command tag {tag:#04x}")),
            }
        }
    }

    fn connect(pipe_name: &str, access: u32) -> Option<HANDLE> {
        let wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
        // The app creates the pipe before spawning us, so this normally
        // succeeds first try; the retry only covers a slow scheduler.
        for _ in 0..200 {
            let h = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    access,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            };
            if let Ok(h) = h {
                return Some(h);
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        None
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
        if h.is_invalid() {
            return false;
        }
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

    // -- Lifetime -----------------------------------------------------------

    /// The app assigns us to a kill-on-close job object, which covers even a
    /// hard kill of the app. This covers the sliver between our spawn and that
    /// assignment, and any future in which the job object is unavailable.
    fn watch_parent(pid: u32) {
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) };
        match handle {
            Ok(h) => {
                unsafe { WaitForSingleObject(h, INFINITE) };
                let _ = unsafe { CloseHandle(h) };
                log("parent process exited; exiting");
            }
            Err(e) => log(&format!("cannot watch parent {pid} ({e}); exiting")),
        }
        exit();
    }

    fn exit() -> ! {
        // The hook is torn down by the kernel on process exit; going through
        // the message pump would need the pump to still be alive, and by the
        // time we get here it may not be.
        unsafe { windows::Win32::System::Threading::ExitProcess(0) }
    }

    fn disable_power_throttling() {
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            // 0 with the bit set in ControlMask = explicitly DISABLE throttling.
            StateMask: 0,
        };
        let ok = unsafe {
            SetProcessInformation(
                HANDLE(GetCurrentProcess().0),
                ProcessPowerThrottling,
                &state as *const _ as *const _,
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
        if let Err(e) = ok {
            log(&format!("could not disable power throttling: {e}"));
        }
    }

    // -- Logging ------------------------------------------------------------
    //
    // The app is GUI-subsystem and so are we, so stdout goes nowhere. Its own
    // log is parle.log next to it; ours is parle-hook.log. NEVER call this from
    // the hook proc.

    fn log_file() -> &'static std::sync::Mutex<Option<std::fs::File>> {
        static F: OnceLock<std::sync::Mutex<Option<std::fs::File>>> = OnceLock::new();
        F.get_or_init(|| {
            let file = std::env::var_os("LOCALAPPDATA")
                .map(std::path::PathBuf::from)
                .map(|d| d.join("EchoKey"))
                .and_then(|dir| {
                    std::fs::create_dir_all(&dir).ok()?;
                    // Truncated per run, like the app's log.
                    std::fs::File::create(dir.join("parle-hook.log")).ok()
                });
            std::sync::Mutex::new(file)
        })
    }

    fn log(msg: &str) {
        if let Ok(mut guard) = log_file().lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{msg}");
                let _ = f.flush();
            }
        }
    }
}
