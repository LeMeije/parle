//! Parle app entry: plugins, tray, windows, platform listeners, dispatcher.

mod commands;
mod hotkey_logic;
mod hud;
mod icons;
mod pipeline;
mod platform;
mod sync;
mod state;

use state::AppState;
use std::sync::Arc;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Where the app writes its log. Same directory as settings/history.
fn log_path() -> Option<std::path::PathBuf> {
    // `data_dir()`, NOT a second hand-rolled copy of the same path.
    //
    // This used to build the path itself and `create_dir_all` it, and it runs
    // FIRST at startup. That defeated the EchoKey-to-Parle migration outright:
    // by the time `data_dir()` looked, the new directory already existed, so it
    // skipped the rename and the app started on an empty history beside two
    // gigabytes of models it could no longer see. Observed on the first launch
    // after the rename, on the author's own machine.
    let dir = parle_core::settings::data_dir();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("parle.log"))
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
    // app has no diagnostics at all on an installed machine. Truncated per run.
    match log_path().and_then(|p| std::fs::File::create(p).ok()) {
        Some(f) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .init(),
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
            register_chord_shortcuts(&handle, &state);

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

            // macOS: onboarded launches start quietly in the menu bar (no
            // window, no Dock tile). The Dock/Cmd-Tab presence follows the main
            // window: Regular while visible, Accessory when hidden (see hud.rs).
            //
            // Windows has no equivalent convention, and hiding a window that the
            // config already created visible produces a show-then-hide flash on
            // every launch. Leave it up; closing it sends the app to the tray.
            // Windows DOES have the login case, though. Autostart registers
            // with `--hidden` and nothing read it, so a tray app popped a
            // 980x700 window at every boot while macOS started silently. A
            // launch the USER asked for still shows the window, so the
            // show-then-hide flash the comment above describes never happens.
            #[cfg(not(target_os = "macos"))]
            if state.settings.lock().onboarding_complete
                && std::env::args().any(|a| a == "--hidden")
            {
                if let Some(main) = handle.get_webview_window(hud::MAIN_LABEL) {
                    let _ = main.hide();
                }
            }

            #[cfg(target_os = "macos")]
            if state.settings.lock().onboarding_complete {
                if let Some(main) = handle.get_webview_window(hud::MAIN_LABEL) {
                    let _ = main.hide();
                }
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
                        state.pipeline_start();
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
pub(crate) fn register_chord_shortcuts(app: &AppHandle, state: &Arc<AppState>) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let _ = app.global_shortcut().unregister_all();

    let bindings = {
        let s = state.settings.lock();
        vec![
            (platform::HotkeyId::Dictation, s.hotkeys.dictation.clone()),
            (platform::HotkeyId::DictationAlt, s.hotkeys.dictation_alt.clone()),
            (platform::HotkeyId::Palette, s.hotkeys.history_palette.clone()),
        ]
    };

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
        let state = state.clone();
        let result = app.global_shortcut().on_shortcut(shortcut, move |app, _sc, event| {
            let phase = match event.state() {
                ShortcutState::Pressed => hotkey_logic::KeyPhase::Down,
                ShortcutState::Released => hotkey_logic::KeyPhase::Up,
            };
            state.on_platform_event(app, platform::PlatformEvent::Hotkey { id, phase });
        });
        if let Err(e) = result {
            tracing::warn!("failed to register shortcut '{}': {e}", binding.key);
            let _ = app.emit("shortcut-error", format!("{}: {e}", binding.key));
        }
    }
}
