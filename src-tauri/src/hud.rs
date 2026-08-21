//! Window management for the recording HUD and the history palette.
//!
//! The HUD must NEVER take focus (the load-bearing detail of paste-at-cursor):
//! macOS = NSPanel with NonactivatingPanel via tauri-nspanel; Windows =
//! WS_EX_NOACTIVATE|TOPMOST|TOOLWINDOW applied to the raw HWND.

use crate::pipeline::PipelineState;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const HUD_LABEL: &str = "hud";
pub const MAIN_LABEL: &str = "main";

pub const HUD_WIDTH: f64 = 320.0;
pub const HUD_HEIGHT: f64 = 72.0;

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
    let y = screen_h / scale - HUD_HEIGHT - 96.0;

    let window = WebviewWindowBuilder::new(app, HUD_LABEL, WebviewUrl::App("index.html#/hud".into()))
        .title("EchoKey HUD")
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
    let Some(hud) = app.get_webview_window(HUD_LABEL) else {
        return;
    };
    match state {
        PipelineState::Recording | PipelineState::Transcribing => {
            let _ = hud.show();
        }
        PipelineState::Idle => {
            let _ = hud.hide();
        }
    }
}

/// The history palette is the main window pre-focused on search.
pub fn toggle_palette(app: &AppHandle) {
    if let Some(main) = app.get_webview_window(MAIN_LABEL) {
        if main.is_visible().unwrap_or(false) && main.is_focused().unwrap_or(false) {
            let _ = main.hide();
        } else {
            let _ = main.show();
            let _ = main.set_focus();
            let _ = main.emit("focus-palette", ());
        }
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(main) = app.get_webview_window(MAIN_LABEL) {
        let _ = main.show();
        let _ = main.set_focus();
    }
}

use tauri::Emitter;
