//! Window management for the recording HUD and the history palette.
//!
//! The HUD must NEVER take focus (the load-bearing detail of paste-at-cursor):
//! macOS = NSPanel with NonactivatingPanel via tauri-nspanel; Windows =
//! WS_EX_NOACTIVATE|TOPMOST|TOOLWINDOW applied to the raw HWND.

use crate::pipeline::PipelineState;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Epoch-ms until which the HUD stays visible after going Idle, so outcome
/// messages (no speech, errors, "press paste yourself") are actually seen —
/// the main window is usually hidden in tray use.
static HOLD_UNTIL: AtomicU64 = AtomicU64::new(0);

pub fn hold_visible(ms: u64) {
    let until = now_ms() + ms;
    HOLD_UNTIL.fetch_max(until, Ordering::SeqCst);
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub const HUD_LABEL: &str = "hud";
pub const MAIN_LABEL: &str = "main";

// Window is deliberately larger than the pill: the transparent margin gives
// the drop shadow room to render instead of clipping at the window edge.
pub const HUD_WIDTH: f64 = 424.0;
pub const HUD_HEIGHT: f64 = 152.0;

#[cfg(target_os = "macos")]
mod panel {
    #[allow(unused_imports)]
    use tauri::Manager;
    use tauri_nspanel::{tauri_panel, CollectionBehavior, PanelLevel, StyleMask, WebviewWindowExt};

    tauri_panel! {
        panel!(EchoHudPanel {
            config: {
                can_become_key_window: false,
                can_become_main_window: false,
                is_floating_panel: true
            }
        })
    }

    pub fn convert_to_panel(window: &tauri::WebviewWindow) -> tauri::Result<()> {
        let panel = window.to_panel::<EchoHudPanel>()?;
        panel.set_style_mask(StyleMask::empty().borderless().nonactivating_panel().value());
        panel.set_level(PanelLevel::Status.value());
        panel.set_collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary()
                .stationary()
                .ignores_cycle()
                .value(),
        );
        // NSPanel hides when the app deactivates by default — and a
        // non-activating overlay app is effectively always deactivated.
        panel.set_hides_on_deactivate(false);
        Ok(())
    }
}

/// Create the HUD window (hidden). Called once at startup.
pub fn create_hud(app: &AppHandle) -> tauri::Result<()> {
    if app.get_webview_window(HUD_LABEL).is_some() {
        return Ok(());
    }
    let monitor = app.primary_monitor()?.map(|m| m.size().clone());
    let (screen_w, screen_h, scale) = match (app.primary_monitor()?, monitor) {
        (Some(m), Some(size)) => (size.width as f64, size.height as f64, m.scale_factor()),
        _ => (1440.0, 900.0, 1.0),
    };
    let x = (screen_w / scale - HUD_WIDTH) / 2.0;
    let y = screen_h / scale - HUD_HEIGHT - 56.0;

    let window = WebviewWindowBuilder::new(app, HUD_LABEL, WebviewUrl::App("index.html#/hud".into()))
        .title("Parle HUD")
        .inner_size(HUD_WIDTH, HUD_HEIGHT)
        .position(x, y)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .accept_first_mouse(true)
        .build()?;

    #[cfg(target_os = "macos")]
    panel::convert_to_panel(&window)?;

    #[cfg(target_os = "windows")]
    crate::platform::windows::harden_overlay(&window);

    let _ = window;
    Ok(())
}

/// Show/hide the HUD in lockstep with pipeline state, never taking focus.
pub fn sync_hud(app: &AppHandle, state: PipelineState) {
    // Tray badge follows capture only: once we are transcribing the mic is no
    // longer live, and a dot that lingers would misreport that.
    if let Some(tray) = app.tray_by_id("parle-tray") {
        let recording = matches!(state, PipelineState::Recording);
        let style = crate::tray_style_of(app);
        let style = style.as_str();
        let _ = tray.set_icon(Some(crate::tray_icon_for(style, recording)));
        // set_icon resets the template flag on macOS; re-assert it, but only
        // for styles that actually are templates — a coloured asset tinted as a
        // template would come out as a silhouette.
        #[cfg(target_os = "macos")]
        let _ = tray.set_icon_as_template(crate::tray_is_template(style));
    }
    // "hidden": no overlay at all. The tray dot above is the whole indicator.
    //
    // Checked AFTER the tray update, deliberately: this mode leans on that dot
    // entirely, so it must keep running even when nothing else is drawn.
    if crate::overlay_style_of(app) == "hidden" {
        if let Some(hud) = app.get_webview_window(HUD_LABEL) {
            let _ = hud.hide();
        }
        return;
    }
    let Some(hud) = app.get_webview_window(HUD_LABEL) else {
        return;
    };
    match state {
        PipelineState::Recording | PipelineState::Transcribing => {
            let _ = hud.show();
        }
        PipelineState::Idle => {
            let hold = HOLD_UNTIL.load(Ordering::SeqCst);
            let now = now_ms();
            if hold > now {
                let app = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(hold - now));
                    // Re-check: a new hold or a new recording may have started.
                    if HOLD_UNTIL.load(Ordering::SeqCst) <= now_ms() {
                        if let Some(hud) = app.get_webview_window(HUD_LABEL) {
                            let _ = hud.hide();
                        }
                    }
                });
            } else {
                let _ = hud.hide();
            }
        }
    }
}

/// Remember whichever app is frontmost right now (unless it's us) as the
/// paste-back target.
fn capture_previous_app(app: &AppHandle) {
    let state = app.state::<std::sync::Arc<crate::state::AppState>>();
    let (bundle_id, _) = crate::platform::imp::frontmost_app();
    if let Some(id) = bundle_id {
        if id != "com.novaire.parle" {
            *state.previous_app.lock() = Some(id);
        }
    }
}

/// While the main window is visible the app behaves like a normal app
/// (Dock tile, Cmd+Tab entry); hidden again = menu-bar-only.
/// MUST run on the main thread: AppKit wedges window ordering when the
/// activation policy is flipped from a background thread (the app stays
/// alive but every window becomes unshowable).
fn set_regular_on_main(app: &AppHandle, regular: bool) {
    #[cfg(target_os = "macos")]
    {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let policy = if regular {
                tauri::ActivationPolicy::Regular
            } else {
                tauri::ActivationPolicy::Accessory
            };
            let _ = handle.set_activation_policy(policy);
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, regular);
}

/// Hide the main window and return to menu-bar-only, safely from ANY thread.
pub fn hide_main_to_tray(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(main) = handle.get_webview_window(MAIN_LABEL) {
            let _ = main.hide();
        }
        #[cfg(target_os = "macos")]
        let _ = handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
    });
}

/// The history palette is the main window pre-focused on search.
pub fn toggle_palette(app: &AppHandle) {
    if let Some(main) = app.get_webview_window(MAIN_LABEL) {
        if main.is_visible().unwrap_or(false) && main.is_focused().unwrap_or(false) {
            hide_main_to_tray(app);
        } else {
            capture_previous_app(app);
            set_regular_on_main(app, true);
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(main) = handle.get_webview_window(MAIN_LABEL) {
                    let _ = main.show();
                    let _ = main.set_focus();
                    let _ = main.emit("focus-palette", ());
                }
            });
        }
    }
}

pub fn show_main(app: &AppHandle) {
    capture_previous_app(app);
    set_regular_on_main(app, true);
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(main) = handle.get_webview_window(MAIN_LABEL) {
            let _ = main.show();
            let _ = main.set_focus();
        }
    });
}

use tauri::Emitter;
