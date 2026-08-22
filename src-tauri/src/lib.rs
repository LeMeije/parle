//! EchoKey app entry: plugins, tray, windows, platform listeners, dispatcher.

mod commands;
mod hotkey_logic;
mod hud;
mod icons;
mod pipeline;
mod platform;
mod state;

use state::AppState;
use std::sync::Arc;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "echokey=info,echokey_lib=info,echokey_core=info,echokey_audio=info,echokey_asr=info".into()),
        )
        .init();

    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    builder
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Second launch: surface the main window.
            hud::show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::set_settings,
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
        .setup(|app| {
            let handle = app.handle().clone();
            let state = AppState::new(&handle);
            app.manage(state.clone());

            if let Err(e) = hud::create_hud(&handle) {
                tracing::error!("HUD creation failed (continuing without overlay): {e}");
            }
            setup_tray(&handle)?;
            spawn_platform(&handle, state.clone());
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
                    .name("echokey-perm-watch".into())
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

            // Onboarded launches start quietly in the menu bar (no window, no
            // Dock tile). The Dock/Cmd-Tab presence follows the main window:
            // Regular while it's visible, Accessory when hidden (see hud.rs).
            if state.settings.lock().onboarding_complete {
                if let Some(main) = handle.get_webview_window(hud::MAIN_LABEL) {
                    let _ = main.hide();
                }
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
                // whisper.cpp's ggml-metal static device destructor calls
                // ggml_abort during atexit (upstream teardown bug), turning
                // every normal quit into a "quit unexpectedly" dialog. All our
                // state is already durable (settings write atomically, SQLite
                // is WAL); skip C/C++ atexit handlers entirely.
                unsafe { libc::_exit(0) };
            }
        });
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

    TrayIconBuilder::with_id("echokey-tray")
        // Menu-bar template glyph (alpha-only); the full-colour app icon reads
        // as a solid white square in template mode.
        .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .icon_as_template(true)
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
fn spawn_platform(app: &AppHandle, state: Arc<AppState>) {
    let (tx, rx) = crossbeam_channel::unbounded::<platform::PlatformEvent>();

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
        if state.settings.lock().onboarding_complete {
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
        .name("echokey-dispatch".into())
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
