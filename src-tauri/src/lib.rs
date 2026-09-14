//! Parle app entry: plugins, tray, windows, platform listeners, dispatcher.

mod commands;
mod hotkey_logic;
mod hud;
mod icons;
mod pipeline;
mod platform;
mod refine;
mod sync;
mod state;

use state::AppState;
use std::sync::Arc;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

/// Make the OS agree with the `launch_at_login` setting, at every startup.
///
/// The setting used to be written ONLY by the toggle in Settings, which assumes
/// the two can never drift. They can, and they did: renaming the app orphaned
/// the LaunchAgent (it still pointed at the old bundle, which no longer
/// existed), so the agent failed at every login with EX_CONFIG while Settings
/// went on showing "launch at login" as ON. A setting that reports a state the
/// system is not in is worse than one that is simply off, because nothing about
/// the UI invites you to go and look.
///
/// Reconciling here makes the stored value the source of truth and the OS a
/// cache of it, so a drift of this kind repairs itself on the next launch
/// instead of persisting silently until someone happens to check.
fn reconcile_autostart(app: &tauri::AppHandle, state: &Arc<AppState>) {
    use tauri_plugin_autostart::ManagerExt;

    let want = state.settings.lock().launch_at_login;
    let mgr = app.autolaunch();
    // Sample the OS state once and carry it: asking twice invites the two
    // reads to disagree with each other.
    let have = match mgr.is_enabled() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("could not read the autostart state ({e}); leaving it alone");
            return;
        }
    };
    if want == have {
        return;
    }
    let r = if want { mgr.enable() } else { mgr.disable() };
    match r {
        Ok(()) => tracing::info!("autostart reconciled to the saved setting (launch_at_login = {want})"),
        Err(e) => tracing::warn!("could not reconcile autostart to {want} ({e})"),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Where the app writes its log. Same directory as settings/history.
fn log_path() -> Option<std::path::PathBuf> {
    // `data_dir()`, NOT a second hand-rolled copy of the same path.
    //
    // This used to build the path itself and `create_dir_all` it, and it runs
    // FIRST at startup. That defeated the data-directory migration outright:
    // by the time `data_dir()` looked, the new directory already existed, so it
    // skipped the rename and the app started on an empty history beside two
    // gigabytes of models it could no longer see. Observed on the first launch
    // after the rename, on the author's own machine.
    let dir = parle_core::settings::data_dir();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("parle.log"))
}

/// Take an exclusive, process-lifetime lock on `path`. True if this process
/// now holds it; false if another live process does (or the file cannot be
/// opened). The handle is deliberately leaked so the lock lasts exactly as
/// long as the process.
///
/// Unix: `flock(LOCK_EX | LOCK_NB)`. Windows: opening with a zero share mode,
/// which fails with a sharing violation while any other process has the file
/// open. Both are released by the kernel on exit, crash included.
fn acquire_instance_lock(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).create(true).open(path) else {
            return false;
        };
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return false;
        }
        std::mem::forget(f);
        true
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new().read(true).write(true).create(true).share_mode(0).open(path) {
            Ok(f) => {
                std::mem::forget(f);
                true
            }
            Err(_) => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        true
    }
}

#[cfg(target_os = "windows")]
fn dirs_next_local() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
}
#[cfg(not(target_os = "windows"))]
fn dirs_next_local() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Library/Application Support"))
}

pub fn run() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "parle=info,parle_lib=info,parle_core=info,parle_audio=info,parle_asr=info".into());
    // Release builds are GUI-subsystem: stdout goes nowhere, so without this the
    // app has no diagnostics at all on an installed machine.
    //
    // One generation is kept. The run before this one is the run worth reading:
    // when something goes wrong the user's first move is to restart the app, and
    // with a plain `File::create` that restart destroys the only record of the
    // failure. That is exactly how the 2026-08-28 overlay incident lost its
    // evidence. Two files, not a rotating set: the log is small and the question
    // is always "what happened last time", never "what happened last week".
    //
    // ROTATED ONLY BY THE INSTANCE THAT WILL RUN. The single-instance guard
    // lives inside the Tauri builder, so a second launch (a double-click while
    // the app is in the menu bar, a Spotlight slip) reached this code first,
    // renamed the LIVE log out from under the running instance and created an
    // empty one, then exited. The running app carried on writing into what was
    // now `parle.log.1`, and the run before it, the one worth reading, had been
    // deleted by that rename. Observed on the author's own machine on
    // 04/09/2026: a 0-byte `parle.log` beside a `parle.log.1` still growing.
    //
    // A process-held lock file decides who rotates. The lock is released by the
    // OS when the process ends, however it ends, so a crash never wedges it.
    let owns_log = log_path().map(|p| acquire_instance_lock(&p.with_extension("lock"))).unwrap_or(false);
    match log_path().filter(|_| owns_log).and_then(|p| {
        let _ = std::fs::rename(&p, p.with_extension("log.1"));
        std::fs::File::create(p).ok()
    }) {
        Some(f) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .init(),
        // No lock (another instance is running and owns the log) or no data
        // dir: log to stderr, which for a GUI launch goes nowhere, and touch
        // nothing on disk.
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }

    // Publish real RAM to the model registry before anything asks it for a
    // recommendation; its Windows fallback is a conservative 16 GB guess.
    #[cfg(target_os = "windows")]
    if let Some(mb) = platform::windows::total_ram_mb() {
        parle_asr::registry::set_total_ram_mb(mb);
        tracing::info!("detected {mb} MB RAM");
    }

    // The Copilot key must never reach the shell — a stray Copilot window
    // stealing focus mid-dictation is exactly what the hook exists to prevent.
    // Arming inside setup() left a ~0.9 s gap after process start (window and
    // tray creation) in which presses fell through and launched Copilot, so the
    // hook goes up here instead: settings are read straight from disk, before
    // any Tauri initialisation. Recording itself needs no model, so a press
    // during prewarm still starts a recording normally.
    // Parle lives in the tray with no visible window, which makes it a
    // throttling candidate; a throttled hook misses keys. Do this before the
    // hook goes up.
    #[cfg(target_os = "windows")]
    platform::windows::disable_power_throttling();

    // Bounded and pre-allocated: the keyboard hook sends from inside its proc,
    // where an allocating send can overrun LowLevelHooksTimeout and cost a
    // keypress. Capacity is far beyond any real burst of hotkey/clipboard events.
    let (platform_tx, platform_rx) =
        crossbeam_channel::bounded::<platform::PlatformEvent>(1024);
    #[cfg(target_os = "windows")]
    let early_hotkeys = {
        let s = parle_core::settings::Settings::load(&parle_core::settings::settings_path())
            .unwrap_or_default();
        if s.onboarding_complete {
            let bindings = state::bindings_from(&s);
            tracing::info!("arming native hotkeys at startup: {bindings:?}");
            Some(platform::windows::HotkeyListener::start(
                bindings,
                s.hotkeys.suppress_copilot,
                platform_tx.clone(),
            ))
        } else {
            tracing::info!("native hotkeys not armed (onboarding incomplete)");
            None
        }
    };

    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    builder
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Second launch: surface the main window.
            hud::show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::add_custom_model,
            commands::remove_custom_model,
            commands::set_settings,
            commands::sync_status,
            commands::sync_set_enabled,
            commands::sync_set_device_name,
            commands::sync_set_kinds,
            commands::sync_now,
            commands::sync_start_pairing,
            commands::sync_cancel_pairing,
            commands::sync_pair_with,
            commands::sync_unpair,
            commands::search_history,
            commands::pin_item,
            commands::delete_item,
            commands::clear_history,
            commands::update_item_text,
            commands::copy_item,
            commands::paste_item,
            commands::list_models,
            commands::download_model,
            commands::cancel_download,
            commands::delete_model,
            commands::select_model,
            commands::engine_status,
            commands::machine_profile,
            commands::dict_list,
            commands::dict_add,
            commands::dict_set_enabled,
            commands::dict_delete,
            commands::start_recording,
            commands::refine_status,
            commands::refine_test,
            commands::stop_recording,
            commands::cancel_recording,
            commands::permission_status,
            commands::request_microphone,
            commands::request_accessibility,
            commands::repair_accessibility,
            commands::set_app_icon,
            commands::restart_app,
            commands::insert_mark,
            commands::pipeline_state,
            commands::open_permission_settings,
            commands::list_audio_devices,
            commands::recommended_setup,
            commands::complete_onboarding,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let state = AppState::new(&handle);
            app.manage(state.clone());

            // Adopt the hook armed before Tauri started, so spawn_platform
            // doesn't install a second one.
            #[cfg(target_os = "windows")]
            if let Some(listener) = early_hotkeys {
                listener.update_bindings(state.native_bindings());
                *state.hotkeys.lock() = Some(listener);
            }

            if let Err(e) = hud::create_hud(&handle) {
                tracing::error!("HUD creation failed (continuing without overlay): {e}");
            }
            setup_tray(&handle)?;
            spawn_platform(&handle, state.clone(), platform_tx, platform_rx);
            state.set_app_handle(handle.clone());
            register_chord_shortcuts(&handle, &state);
            reconcile_autostart(&handle, &state);

            // Pre-warm the model so the first dictation is instant.
            if state.settings.lock().onboarding_complete {
                state.prewarm_async(handle.clone());
            }

            // Apply the chosen app icon (runtime surfaces only at startup).
            {
                let icon_id = state.settings.lock().appearance.app_icon.clone();
                if icon_id != "default" {
                    let _ = icons::apply_app_icon(&handle, &icon_id);
                }
            }

            // Accessibility can be granted while we're running (onboarding, or
            // a re-grant after a rebuild). Poll until the listener arms so the
            // user never needs an app restart after granting.
            {
                let state = state.clone();
                let handle2 = handle.clone();
                std::thread::Builder::new()
                    .name("parle-perm-watch".into())
                    .spawn(move || loop {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        if state.hotkeys.lock().is_some() {
                            break; // armed; job done
                        }
                        if state.settings.lock().onboarding_complete {
                            state.ensure_hotkey_listener();
                            if state.hotkeys.lock().is_some() {
                                let _ = handle2.emit("permissions-changed", ());
                                break;
                            }
                        }
                    })
                    .expect("spawn perm watcher");
            }

            // Retention pruning at startup.
            {
                let s = state.settings.lock();
                let _ = state
                    .store
                    .lock()
                    .prune(s.history.retention_days, s.history.max_items);
            }

            // A launch the USER asked for SHOWS THE WINDOW. Only a launch the
            // system asked for starts quietly.
            //
            // macOS hid the window on every onboarded launch, so double-clicking
            // the app bounced the Dock icon and then appeared to do nothing:
            // the window flashed up and vanished, and the only evidence the app
            // had started was a small menu bar icon the user was not looking
            // for. Reported from real use, and it read as "the app is broken".
            //
            // Autostart registers with `--hidden`, which is the signal that
            // this launch is the login one. Windows was already gated on it and
            // macOS was not, so the two platforms disagreed about the same
            // question. They no longer do.
            // `args_os`: `args()` panics on a non-Unicode argument, which
            // Windows can hand us.
            let launched_by_system = std::env::args_os().any(|a| a == "--hidden");
            if state.settings.lock().onboarding_complete && launched_by_system {
                if let Some(main) = handle.get_webview_window(hud::MAIN_LABEL) {
                    let _ = main.hide();
                }
                // The Dock and Cmd-Tab presence follows the main window:
                // Accessory while it is hidden, Regular once it is shown
                // again (see hud.rs).
                #[cfg(target_os = "macos")]
                let _ = handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window hides it (tray app keeps running).
            if window.label() == hud::MAIN_LABEL {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                    // Window gone -> back to menu-bar-only (out of Cmd+Tab/Dock).
                    #[cfg(target_os = "macos")]
                    let _ = window
                        .app_handle()
                        .set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Parle")
        .run(|handle, event| {
            // Dock icon click / `open` on a running instance: bring the window back.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                hud::show_main(handle);
            }
            if let tauri::RunEvent::Exit = event {
                // Take sync down POLITELY first. _exit skips every destructor,
                // including Discovery's, so without this the _parle._tcp
                // record stayed advertised on the LAN until its TTL lapsed and
                // peers kept dialling a closed port on a machine that had quit.
                {
                    use tauri::Manager;
                    if let Some(state) = handle.try_state::<std::sync::Arc<state::AppState>>() {
                        state.sync.stop();
                    }
                }
                // whisper.cpp's ggml-metal static device destructor calls
                // ggml_abort during atexit (upstream teardown bug), turning
                // every normal quit into a "quit unexpectedly" dialog. All our
                // state is already durable (settings write atomically, SQLite
                // is WAL); skip C/C++ atexit handlers entirely.
                unsafe { libc::_exit(0) };
            }
        });
}

/// Windows taskbar theme, for the "auto" tray style. `SystemUsesLightTheme`
/// governs the taskbar and tray; `AppsUseLightTheme` governs app windows and is
/// a different setting entirely.
#[cfg(target_os = "windows")]
fn taskbar_is_dark() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut val: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut val as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    // Missing key means the Windows default: a dark taskbar.
    if rc.is_ok() { val == 0 } else { true }
}

/// The tray image for a given style setting and recording state.
///
/// Styles: "auto" | "badge" | "light" | "dark" | "color" | "template".
/// macOS uses a template image the OS inverts itself; Windows inverts nothing,
/// which is why the filled blue badge (a miniature of the app icon) is the
/// sane default there — it reads on either taskbar without a per-theme pair.
pub(crate) fn tray_icon_for(style: &str, recording: bool) -> tauri::image::Image<'static> {
    macro_rules! img {
        ($idle:literal, $rec:literal) => {
            tauri::image::Image::from_bytes(if recording {
                include_bytes!($rec).as_slice()
            } else {
                include_bytes!($idle).as_slice()
            })
            .expect("tray icon decode")
        };
    }
    match style {
        "badge" => img!("../icons/tray-badge.png", "../icons/tray-badge-recording.png"),
        "light" => img!("../icons/tray-light.png", "../icons/tray-light-recording.png"),
        "dark" => img!("../icons/tray-dark.png", "../icons/tray-dark-recording.png"),
        "color" => img!("../icons/tray-color.png", "../icons/tray-color-recording.png"),
        "template" => img!("../icons/tray.png", "../icons/tray-recording.png"),
        // "auto" and anything unrecognised.
        _ => {
            #[cfg(target_os = "macos")]
            {
                img!("../icons/tray.png", "../icons/tray-recording.png")
            }
            #[cfg(target_os = "windows")]
            {
                if taskbar_is_dark() {
                    img!("../icons/tray-light.png", "../icons/tray-light-recording.png")
                } else {
                    img!("../icons/tray-dark.png", "../icons/tray-dark-recording.png")
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                img!("../icons/tray.png", "../icons/tray-recording.png")
            }
        }
    }
}

/// The user's chosen tray style, or the platform default before settings load.
pub(crate) fn tray_style_of(app: &AppHandle) -> String {
    app.try_state::<Arc<AppState>>()
        .map(|s| s.settings.lock().appearance.tray_style.clone())
        .unwrap_or_else(|| parle_core::settings::default_tray_style().to_string())
}

/// The overlay style, for code that needs to know whether to draw one at all.
pub(crate) fn overlay_style_of(app: &AppHandle) -> String {
    app.try_state::<Arc<AppState>>()
        .map(|s| s.settings.lock().overlay.style.clone())
        .unwrap_or_else(|| "pill".to_string())
}

/// Whether this style is a macOS template image (the OS tints it itself).
pub(crate) fn tray_is_template(style: &str) -> bool {
    matches!(style, "template") || (cfg!(target_os = "macos") && style == "auto")
}

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open Parle").build(app)?;
    let toggle = MenuItemBuilder::with_id("toggle", "Start dictation").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Parle").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&open)
        .item(&toggle)
        .separator()
        .item(&quit)
        .build()?;

    let tray_style = tray_style_of(app);
    let tray_style = tray_style.as_str();
    TrayIconBuilder::with_id("parle-tray")
        // Template mode is alpha-only: macOS discards the colours and tints the
        // shape itself. Correct for the monochrome glyph, but it would flatten
        // the colour badge into a solid silhouette — so only claim it when the
        // chosen style really is a template.
        .icon(tray_icon_for(tray_style, false))
        .icon_as_template(tray_is_template(tray_style))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => hud::show_main(app),
            "toggle" => {
                // Recorder init can block briefly — never on the main thread.
                let state = app.state::<Arc<AppState>>().inner().clone();
                std::thread::spawn(move || {
                    if state.pipeline.is_recording() {
                        state.external_stop();
                    } else {
                        state.pipeline_start(pipeline::DictationMode::Standard);
                    }
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Spawn the native listeners and the event dispatcher thread.
fn spawn_platform(
    app: &AppHandle,
    state: Arc<AppState>,
    tx: crossbeam_channel::Sender<platform::PlatformEvent>,
    rx: crossbeam_channel::Receiver<platform::PlatformEvent>,
) {
    *state.platform_tx.lock() = Some(tx.clone());
    #[cfg(target_os = "macos")]
    {
        // The native listener swallows bound keys system-wide, so it must never
        // arm before the user has completed onboarding and chosen a key.
        if state.settings.lock().onboarding_complete && platform::macos::accessibility_trusted() {
            let listener = platform::macos::HotkeyListener::start(state.native_bindings(), tx.clone());
            *state.hotkeys.lock() = Some(listener);
        } else {
            tracing::info!("native hotkeys not armed (onboarding incomplete or no Accessibility)");
        }
        let enabled = state.settings.lock().history.clipboard_capture;
        let monitor = platform::macos_clipboard::ClipboardMonitor::start(tx.clone(), enabled);
        *state.clipboard_monitor.lock() = Some(monitor);
    }

    #[cfg(target_os = "windows")]
    {
        // Normally already armed at process start; this covers the case where
        // onboarding completed during THIS run.
        if state.hotkeys.lock().is_none() && state.settings.lock().onboarding_complete {
            tracing::info!("arming native hotkeys late: {:?}", state.native_bindings());
            let listener = platform::windows::HotkeyListener::start(
                state.native_bindings(),
                state.settings.lock().hotkeys.suppress_copilot,
                tx.clone(),
            );
            *state.hotkeys.lock() = Some(listener);
        }
        let enabled = state.settings.lock().history.clipboard_capture;
        let monitor = platform::windows::ClipboardMonitor::start(tx.clone(), enabled);
        *state.clipboard_monitor.lock() = Some(monitor);
    }

    let app = app.clone();
    std::thread::Builder::new()
        .name("parle-dispatch".into())
        .spawn(move || {
            for event in rx {
                state.on_platform_event(&app, event);
            }
        })
        .expect("spawn dispatcher");
}

/// Chord-style shortcuts (e.g. Cmd+Shift+V palette) via the portable plugin.
/// Native-only keys (Fn, bare modifiers, Copilot) are handled by the platform
/// listener instead. Safe to call repeatedly: clears previous registrations
/// first so settings changes apply without a restart.
/// Arm or disarm the cancel key as a global shortcut.
///
/// Called from `set_recording_flag`, so the key is only claimed for the
/// duration of a recording. It is a genuine global shortcut (Carbon
/// `RegisterEventHotKey` under the plugin) rather than an event tap, which
/// means it still fires when Accessibility has not been granted, and that is
/// the case where the tap-based path silently did nothing at all.
///
/// The tap path is deliberately left in place as well. It is the faster of the
/// two and it swallows the key properly; this is the belt to its braces. Both
/// firing is harmless: the second cancel arrives when there is no longer a
/// recording to cancel and is a no-op.
///
/// EVERY call into the global-shortcut plugin in this file goes through
/// `on_main_thread`, and NO handler registered with it ever calls back into
/// the plugin or into the pipeline directly. Both rules exist because of the
/// same fact about the plugin: it invokes a shortcut's handler while HOLDING its
/// own (non-reentrant) `shortcuts` mutex, on the main thread. So a handler that
/// reached `pipeline_start` -> `set_recording_flag` -> `on_shortcut` re-took
/// that mutex on the same thread and the whole app froze: UI, overlay, tray.
/// Reachable with a chord dictation key and "Esc cancels" on, or with Esc
/// pressed during a recording while Accessibility is not granted. And a
/// plugin call from any OTHER thread holds that mutex across a round trip to
/// the main thread, so a shortcut firing in that window deadlocks the other
/// way round. Handlers therefore only forward an event to the dispatcher
/// channel, and plugin calls run where the lock lives.
pub(crate) fn set_cancel_shortcut_armed(app: &AppHandle, state: &AppState, armed: bool) {
    let binding = state.settings.lock().hotkeys.cancel.clone();
    if !binding.enabled || binding.key.is_empty() {
        return;
    }
    // "Escape" is the stored name; the shortcut parser wants "Esc".
    let spelled = match binding.key.as_str() {
        "Escape" => "Esc".to_string(),
        other => other.to_string(),
    };
    let Ok(shortcut) = spelled.parse::<tauri_plugin_global_shortcut::Shortcut>() else {
        tracing::warn!("cancel key '{}' is not a registrable shortcut", binding.key);
        return;
    };
    let key_name = binding.key.clone();
    let tx = state.platform_tx.lock().clone();
    on_main_thread(app, move |app| {
        use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
        if !armed {
            let _ = app.global_shortcut().unregister(shortcut);
            return;
        }
        let result = app.global_shortcut().on_shortcut(shortcut, move |_a, _sc, event| {
            if matches!(event.state(), ShortcutState::Pressed) {
                // Forwarded, never handled here: see the note above.
                if let Some(tx) = tx.as_ref() {
                    let _ = tx.send(platform::PlatformEvent::Hotkey {
                        id: platform::HotkeyId::Cancel,
                        phase: hotkey_logic::KeyPhase::Down,
                        mods: platform::Mods::default(),
                    });
                }
            }
        });
        if let Err(e) = result {
            tracing::warn!("could not arm the cancel shortcut '{key_name}': {e}");
        }
    });
}

/// Run `f` on the main thread, where the global-shortcut plugin's lock lives.
/// From the main thread itself Tauri still queues it, which is harmless: we
/// hold no lock while queuing.
fn on_main_thread(app: &AppHandle, f: impl FnOnce(&AppHandle) + Send + 'static) {
    let handle = app.clone();
    if let Err(e) = app.run_on_main_thread(move || f(&handle)) {
        tracing::warn!("could not reach the main thread for a shortcut change: {e}");
    }
}

pub(crate) fn register_chord_shortcuts(app: &AppHandle, state: &Arc<AppState>) {
    let bindings = {
        let s = state.settings.lock();
        let mut v = vec![
            (platform::HotkeyId::Dictation, s.hotkeys.dictation.clone()),
            (platform::HotkeyId::DictationAlt, s.hotkeys.dictation_alt.clone()),
            (platform::HotkeyId::Palette, s.hotkeys.history_palette.clone()),
        ];
        // Only when the user asked for a separate key: same rule as the
        // native bindings, through the same predicate.
        if state::refine_uses_own_key(&s) {
            v.push((platform::HotkeyId::Refine, s.hotkeys.refine.clone()));
        }
        v
    };
    let tx = state.platform_tx.lock().clone();
    let recording = state.is_recording_flag_set();
    let state = state.clone();

    // Plugin calls on the main thread only, and handlers that only forward.
    // See `set_cancel_shortcut_armed` for why both halves are load-bearing.
    on_main_thread(app, move |app| {
        use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

        let _ = app.global_shortcut().unregister_all();

        for (id, binding) in bindings {
            if !binding.enabled || binding.key.is_empty() {
                continue;
            }
            // Skip keys owned by the native listener.
            if platform::NativeKey::parse(&binding.key).is_some() {
                continue;
            }
            let Ok(shortcut) = binding.key.parse::<tauri_plugin_global_shortcut::Shortcut>() else {
                tracing::warn!("unparseable shortcut '{}'", binding.key);
                continue;
            };
            let tx = tx.clone();
            let result = app.global_shortcut().on_shortcut(shortcut, move |_app, _sc, event| {
                let phase = match event.state() {
                    ShortcutState::Pressed => hotkey_logic::KeyPhase::Down,
                    ShortcutState::Released => hotkey_logic::KeyPhase::Up,
                };
                // Through the dispatcher, like every native key. Handling it
                // inline here re-entered the plugin from inside its own lock.
                if let Some(tx) = tx.as_ref() {
                    // No modifiers reported: a chord binding already HAS its
                    // modifiers in the chord, and the plugin does not tell us
                    // what else was held. A modifier Refine trigger therefore
                    // works with the native keys people dictate with (Fn, a
                    // bare modifier, the Copilot key) and not with a chord
                    // dictation key, which Settings says.
                    let _ = tx.send(platform::PlatformEvent::Hotkey {
                        id,
                        phase,
                        mods: platform::Mods::default(),
                    });
                }
            });
            if let Err(e) = result {
                tracing::warn!("failed to register shortcut '{}': {e}", binding.key);
                let _ = app.emit("shortcut-error", format!("{}: {e}", binding.key));
            }
        }
        // `unregister_all` also dropped the cancel key armed for a recording in
        // progress. Put it back, or Esc does nothing until the next take.
        if recording {
            set_cancel_shortcut_armed(app, &state, true);
        }
    });
}

#[cfg(test)]
mod instance_lock_tests {
    //! The lock has to hold ACROSS processes, which a unit test cannot spawn
    //! cheaply without the whole app; so this pins the two properties that can
    //! be checked in one: the first acquisition succeeds, and the lock file is
    //! created where the log lives rather than somewhere else.
    #[test]
    fn the_first_acquisition_succeeds_and_creates_the_file() {
        let dir = std::env::temp_dir().join(format!("parle-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("parle.lock");
        assert!(super::acquire_instance_lock(&lock));
        assert!(lock.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_second_process_holding_the_lock_is_detected() {
        // A child `sh` takes the lock with flock(1)-style semantics via
        // python, holds it for a moment, and we must see it as taken.
        let dir = std::env::temp_dir().join(format!("parle-lock-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("parle.lock");
        let holder = std::process::Command::new("python3")
            .arg("-c")
            .arg("import fcntl,sys,time; f=open(sys.argv[1],'w'); fcntl.flock(f, fcntl.LOCK_EX); print('held', flush=True); time.sleep(3)")
            .arg(&lock)
            .stdout(std::process::Stdio::piped())
            .spawn();
        let Ok(mut holder) = holder else {
            return; // no python3 here; the property is exercised on machines that have it
        };
        // Wait for the child to say it holds the lock.
        {
            use std::io::Read;
            let mut out = holder.stdout.take().unwrap();
            let mut b = [0u8; 4];
            let _ = out.read_exact(&mut b);
        }
        assert!(!super::acquire_instance_lock(&lock), "the lock must read as taken while another process holds it");
        let _ = holder.kill();
        let _ = holder.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod shortcut_defaults_tests {
    //! The guard that was missing.
    //!
    //! `parle-core` cannot check its own shortcut defaults, because the thing
    //! that decides whether a chord is real is the global-shortcut plugin's
    //! parser, which only this crate depends on. So the default was free to be
    //! a string nobody could register, and it was: `Fn+Shift+V` logged
    //! "unparseable shortcut" at every launch for as long as it was set, while
    //! Settings displayed it as though it worked.

    use tauri_plugin_global_shortcut::Shortcut;

    /// Every shortcut default must survive the parser that actually registers it.
    #[test]
    fn every_default_chord_parses_with_the_real_parser() {
        let s = parle_core::settings::Settings::default();
        for (what, binding) in [
            ("history_palette", &s.hotkeys.history_palette),
            ("dictation", &s.hotkeys.dictation),
            ("dictation_alt", &s.hotkeys.dictation_alt),
            ("refine", &s.hotkeys.refine),
            ("cancel", &s.hotkeys.cancel),
        ] {
            if binding.key.is_empty() {
                continue;
            }
            // Keys owned by the native listener never reach this parser, and
            // `Fn` deliberately cannot parse here. Skip exactly those, the way
            // `register_chord_shortcuts` does.
            if crate::platform::NativeKey::parse(&binding.key).is_some() {
                continue;
            }
            assert!(
                binding.key.parse::<Shortcut>().is_ok(),
                "the default for {what} is '{}', which the global-shortcut parser rejects, \
                 so it would silently never fire",
                binding.key
            );
        }
    }

    /// The Refine suggestion must land on exactly one of the two listeners:
    /// either the native tap knows it, or the chord parser does. A key neither
    /// recognises would be a setting the app displays and does not have, the
    /// same class as the `Fn+Shift+V` palette chord.
    #[test]
    fn the_refine_default_is_owned_by_exactly_one_listener() {
        let s = parle_core::settings::Settings::default();
        let key = &s.hotkeys.refine.key;
        let native = crate::platform::NativeKey::parse(key).is_some();
        let chord = key.parse::<Shortcut>().is_ok();
        assert!(native ^ chord, "'{key}': native={native} chord={chord}");
    }

    /// The specific string this test was written for.
    #[test]
    fn an_fn_chord_really_is_rejected_by_the_parser() {
        // Proves the assertion above can fail, rather than passing because
        // everything parses. Without this, the test is theatre.
        assert!(
            "Fn+Shift+V".parse::<Shortcut>().is_err(),
            "if Fn chords ever start parsing, the migration in parle-core can be removed"
        );
    }
}
