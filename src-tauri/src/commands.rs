//! Tauri IPC commands: everything the React UI can ask the core to do.

use crate::platform;
use crate::state::AppState;
use echokey_asr::download::{self, CancelToken, DownloadProgress};
use echokey_asr::registry;
use echokey_core::dictionary::Dictionary;
use echokey_core::history::Store;
use echokey_core::settings::{models_dir, settings_path, Settings};
use echokey_core::types::{HistoryItem, HistoryKind};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

type Result<T> = std::result::Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// -- Settings ----------------------------------------------------------------

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Settings {
    state.settings.lock().clone()
}

#[tauri::command]
pub async fn set_settings(state: State<'_, Arc<AppState>>, app: AppHandle, settings: Settings) -> Result<()> {
    {
        let mut guard = state.settings.lock();
        *guard = settings.clone();
        guard.save(&settings_path()).map_err(err)?;
    }
    // apply_settings queues engine reconfiguration on the serial worker, so
    // this never blocks behind an in-flight transcription.
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.apply_settings(&app);
        // Chord shortcuts (palette etc.) re-register so changes apply live.
        crate::register_chord_shortcuts(&app, &state);
    })
    .await
    .map_err(err)
}

// -- History -----------------------------------------------------------------

#[tauri::command]
pub fn search_history(
    state: State<'_, Arc<AppState>>,
    query: String,
    kind: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<HistoryItem>> {
    let kind = match kind.as_deref() {
        Some("transcription") => Some(HistoryKind::Transcription),
        Some("clipboard") => Some(HistoryKind::Clipboard),
        _ => None,
    };
    state
        .store
        .lock()
        .search(&query, kind, limit.unwrap_or(60))
        .map_err(err)
}

#[tauri::command]
pub fn pin_item(state: State<'_, Arc<AppState>>, id: i64, pinned: bool) -> Result<()> {
    state.store.lock().set_pinned(id, pinned).map_err(err)
}

#[tauri::command]
pub fn delete_item(state: State<'_, Arc<AppState>>, id: i64) -> Result<()> {
    state.store.lock().delete(id).map_err(err)
}

#[tauri::command]
pub fn clear_history(state: State<'_, Arc<AppState>>, kind: Option<String>) -> Result<usize> {
    let kind = match kind.as_deref() {
        Some("transcription") => Some(HistoryKind::Transcription),
        Some("clipboard") => Some(HistoryKind::Clipboard),
        _ => None,
    };
    state.store.lock().clear(kind).map_err(err)
}

/// Edit an item's text; feeds the auto-learn dictionary when enabled.
/// `learn=false` for restore-raw actions: restoring must never teach the
/// dictionary a reversed correction pair.
#[tauri::command]
pub fn update_item_text(state: State<'_, Arc<AppState>>, id: i64, text: String, learn: Option<bool>) -> Result<()> {
    let changed = state.store.lock().update_text(id, &text).map_err(err)?;
    if let Some((old, new)) = changed {
        let auto_learn = state.settings.lock().dictionary.auto_learn && learn.unwrap_or(true);
        if auto_learn {
            learn_from_edit(&state.store, &old, &new);
        }
    }
    Ok(())
}

/// Copy a history item back to the clipboard (palette Enter action).
#[tauri::command]
pub fn copy_item(state: State<'_, Arc<AppState>>, id: i64) -> Result<()> {
    let item = state.store.lock().get(id).map_err(err)?.ok_or("not found")?;
    platform::imp::write_clipboard(&item.text);
    Ok(())
}

/// Paste a history item into the PREVIOUS app: hide our window first so focus
/// returns to it, then inject.
#[tauri::command]
pub async fn paste_item(state: State<'_, Arc<AppState>>, app: AppHandle, id: i64) -> Result<platform::InjectionOutcome> {
    let item = state.store.lock().get(id).map_err(err)?.ok_or("not found")?;
    let s = state.settings.lock().clone();
    if let Some(main) = app.get_webview_window(crate::hud::MAIN_LABEL) {
        let _ = main.hide();
    }
    // Give the OS a beat to hand focus back to the previous app.
    tokio_sleep(220).await;
    Ok(platform::imp::inject_text(
        &item.text,
        s.paste.prefer_ax_insert,
        s.paste.restore_delay_ms,
        s.paste.copy_to_clipboard,
        s.paste.restore_clipboard,
    ))
}

async fn tokio_sleep(ms: u64) {
    let (tx, rx) = tauri::async_runtime::channel::<()>(1);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        let _ = tx.blocking_send(());
    });
    let mut rx = rx;
    let _ = rx.recv().await;
}

/// Single-word diffs between old and new become correction pairs.
fn learn_from_edit(store: &parking_lot::Mutex<Store>, old: &str, new: &str) {
    let old_words: Vec<&str> = old.split_whitespace().collect();
    let new_words: Vec<&str> = new.split_whitespace().collect();
    if old_words.len() != new_words.len() {
        return; // structural edit, not a word correction
    }
    let diffs: Vec<(usize, &&str, &&str)> = old_words
        .iter()
        .zip(new_words.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| (i, a, b))
        .collect();
    // Only learn small, focused corrections.
    if diffs.len() == 1 {
        let (_, from, to) = diffs[0];
        let clean = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
        let (from, to) = (clean(from), clean(to));
        if from.len() > 2 && to.len() > 2 && from.to_lowercase() != to.to_lowercase() {
            let _ = store.lock().dict_upsert(&to, &[from], true);
        }
    }
}

// -- Models -------------------------------------------------------------------

#[derive(Serialize)]
pub struct ModelRow {
    pub id: String,
    pub display_name: String,
    pub size_bytes: u64,
    pub speed: u8,
    pub accuracy: u8,
    pub multilingual: bool,
    pub downloaded: bool,
    pub active: bool,
}

#[tauri::command]
pub fn list_models(state: State<'_, Arc<AppState>>) -> Vec<ModelRow> {
    let dir = models_dir();
    let active = state.settings.lock().models.active_model.clone();
    registry::MODELS
        .iter()
        .map(|m| ModelRow {
            id: m.id.to_string(),
            display_name: m.display_name.to_string(),
            size_bytes: m.size_bytes,
            speed: m.speed,
            accuracy: m.accuracy,
            multilingual: m.multilingual,
            downloaded: download::is_downloaded(&dir, m),
            active: m.id == active,
        })
        .collect()
}

#[tauri::command]
pub fn download_model(state: State<'_, Arc<AppState>>, app: AppHandle, model_id: String) -> Result<()> {
    let info = registry::by_id(&model_id).ok_or("unknown model")?;
    let token = CancelToken::default();
    state.downloads.lock().insert(model_id.clone(), token.clone());
    let dir = models_dir();
    std::thread::spawn(move || {
        let emit = |p: &DownloadProgress| {
            let _ = app.emit("model-download-progress", p);
        };
        match download::download(&dir, info, &token, |p| emit(&p)) {
            Ok(_) => {
                let _ = app.emit("model-download-complete", &model_id);
            }
            Err(e) => {
                let _ = app.emit("model-download-error", format!("{model_id}: {e}"));
            }
        }
        let state = app.state::<Arc<AppState>>();
        state.downloads.lock().remove(&model_id);
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_download(state: State<'_, Arc<AppState>>, model_id: String) -> Result<()> {
    if let Some(t) = state.downloads.lock().get(&model_id) {
        t.cancel();
    }
    Ok(())
}

#[tauri::command]
pub fn delete_model(state: State<'_, Arc<AppState>>, model_id: String) -> Result<()> {
    let info = registry::by_id(&model_id).ok_or("unknown model")?;
    let active = state.settings.lock().models.active_model.clone();
    if active == model_id {
        return Err("Cannot delete the active model. Switch models first.".into());
    }
    download::delete(&models_dir(), info).map_err(err)
}

#[tauri::command]
pub async fn select_model(state: State<'_, Arc<AppState>>, app: AppHandle, model_id: String) -> Result<()> {
    registry::by_id(&model_id).ok_or("unknown model")?;
    {
        let mut s = state.settings.lock();
        s.models.active_model = model_id.clone();
        s.save(&settings_path()).map_err(err)?;
    }
    let state2 = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state2.reload_engine();
        let _ = app.emit("engine-status", state2.engine_status());
    })
    .await
    .map_err(err)
}

#[tauri::command]
pub fn engine_status(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    // Never block the UI behind an in-flight transcription.
    match state.engine.try_lock() {
        Some(e) => serde_json::to_value(e.status()).unwrap_or_default(),
        None => serde_json::json!({ "loaded_model": null, "warm": true, "busy": true }),
    }
}

#[tauri::command]
pub fn machine_profile() -> registry::MachineProfile {
    registry::detect_machine()
}

// -- Dictionary ---------------------------------------------------------------

#[tauri::command]
pub fn dict_list(state: State<'_, Arc<AppState>>) -> Result<Vec<echokey_core::dictionary::DictEntry>> {
    state.store.lock().dict_entries().map_err(err)
}

#[derive(Serialize)]
pub struct DictAddResult {
    pub id: i64,
    pub warning: Option<String>,
}

#[tauri::command]
pub fn dict_add(
    state: State<'_, Arc<AppState>>,
    term: String,
    corrections: Vec<String>,
) -> Result<DictAddResult> {
    let term = term.trim().to_string();
    if term.is_empty() {
        return Err("Term cannot be empty".into());
    }
    let id = state.store.lock().dict_upsert(&term, &corrections, false).map_err(err)?;
    Ok(DictAddResult { id, warning: Dictionary::false_match_warning(&term) })
}

#[tauri::command]
pub fn dict_set_enabled(state: State<'_, Arc<AppState>>, id: i64, enabled: bool) -> Result<()> {
    state.store.lock().dict_set_enabled(id, enabled).map_err(err)
}

#[tauri::command]
pub fn dict_delete(state: State<'_, Arc<AppState>>, id: i64) -> Result<()> {
    state.store.lock().dict_delete(id).map_err(err)
}

// -- Recording control (UI buttons mirror the hotkeys) -------------------------

#[tauri::command]
pub async fn start_recording(state: State<'_, Arc<AppState>>) -> Result<()> {
    // Recorder init can block (CoreAudio cold open) — keep it off the main thread.
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.pipeline_start())
        .await
        .map_err(err)
}

#[tauri::command]
pub fn stop_recording(state: State<'_, Arc<AppState>>) {
    state.external_stop();
}

#[tauri::command]
pub fn cancel_recording(state: State<'_, Arc<AppState>>) {
    state.external_cancel();
}

// -- Permissions / onboarding ---------------------------------------------------

#[tauri::command]
pub fn permission_status() -> platform::PermissionStatus {
    platform::imp::permission_status()
}

/// Trigger the OS microphone permission prompt (macOS; no-op elsewhere —
/// Windows 11 gates mic per-app in Settings without a runtime prompt API here).
#[tauri::command]
pub fn request_microphone() {
    #[cfg(target_os = "macos")]
    platform::imp::request_microphone_access();
}

#[tauri::command]
pub fn open_permission_settings(which: String) {
    #[cfg(target_os = "macos")]
    match which.as_str() {
        "microphone" => platform::imp::open_microphone_settings(),
        _ => platform::imp::open_accessibility_settings(),
    }
    #[cfg(not(target_os = "macos"))]
    let _ = which;
}

#[tauri::command]
pub fn list_audio_devices() -> Vec<String> {
    echokey_audio::capture::input_devices()
}

/// First-launch model recommendation for the onboarding flow.
#[tauri::command]
pub fn recommended_setup() -> serde_json::Value {
    let profile = registry::detect_machine();
    let (model, chain) = registry::recommend(&profile);
    serde_json::json!({
        "profile": profile,
        "model": model,
        "fallback_chain": chain,
    })
}

#[tauri::command]
pub fn complete_onboarding(state: State<'_, Arc<AppState>>, app: AppHandle) -> Result<()> {
    {
        let mut s = state.settings.lock();
        s.onboarding_complete = true;
        if s.models.active_model.is_empty() {
            let profile = registry::detect_machine();
            let (model, chain) = registry::recommend(&profile);
            s.models.active_model = model.to_string();
            s.models.fallback_chain = chain.iter().map(|s| s.to_string()).collect();
        }
        s.save(&settings_path()).map_err(err)?;
    }
    state.apply_settings(&app);
    state.prewarm_async(app.clone());
    Ok(())
}
