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
        // The whole Settings blob comes back from the UI on every write, so any
        // subtree the UI does not own would be overwritten with whatever it last
        // cached. `sync` is server-owned: it carries the device identity, which
        // must survive forever, and the paired-kind flags, which the dedicated
        // sync_* commands mutate. A stale (or, since the field is optional in
        // the frontend type, an ABSENT) sync subtree would deserialise to
        // defaults and silently wipe device_id — orphaning every history row
        // stamped with it and invalidating every pairing.
        let owned_sync = guard.sync.clone();
        *guard = settings.clone();
        guard.sync = owned_sync;
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

/// Paste a history item into the PREVIOUS app: hide our window, explicitly
/// re-activate the app that was frontmost before us, wait until it actually
/// holds focus, then inject. Runs entirely off the main thread.
#[tauri::command]
pub async fn paste_item(state: State<'_, Arc<AppState>>, app: AppHandle, id: i64) -> Result<platform::InjectionOutcome> {
    let item = state.store.lock().get(id).map_err(err)?.ok_or("not found")?;
    let s = state.settings.lock().clone();
    let target = state.previous_app.lock().clone();
    crate::hud::hide_main_to_tray(&app);
    tauri::async_runtime::spawn_blocking(move || {
        // Hand focus back explicitly, then wait for it to actually land.
        #[cfg(target_os = "macos")]
        if let Some(ref bundle) = target {
            platform::imp::activate_app(bundle);
        }
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let (front, _) = platform::imp::frontmost_app();
            match front.as_deref() {
                Some("com.novaire.echokey") | None => continue,
                Some(f) => {
                    if target.as_deref().map(|t| t == f).unwrap_or(true) {
                        break; // the right app (or at least not us) is frontmost
                    }
                    break;
                }
            }
        }
        platform::imp::inject_text(
            &item.text,
            s.paste.prefer_ax_insert,
            s.paste.restore_delay_ms,
            s.paste.copy_to_clipboard,
            s.paste.restore_clipboard,
            s.paste.press_enter,
        )
    })
    .await
    .map_err(err)
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
    /// Where inference runs for this model in THIS build: "Metal GPU",
    /// "CUDA GPU" or "CPU".
    pub backend: String,
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
            backend: match m.engine {
                registry::EngineKind::Whisper => {
                    if cfg!(target_os = "macos") {
                        "Metal GPU".to_string()
                    } else if cfg!(feature = "cuda") {
                        "CUDA GPU".to_string()
                    } else {
                        "CPU".to_string()
                    }
                }
                registry::EngineKind::Parakeet => "CPU".to_string(),
            },
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

/// Register this binary in the Accessibility list and show the grant prompt.
/// Fixes the stale-entry trap after rebuilds (toggle looks on, does nothing).
#[tauri::command]
pub fn request_accessibility() {
    #[cfg(target_os = "macos")]
    platform::imp::request_accessibility_access();
}

/// Nuclear option for the rebuild-orphaned grant: drop OUR stale TCC entry
/// (tccutil is per-bundle and needs no sudo) and re-register this binary so
/// the System Settings toggle points at something real again.
#[tauri::command]
pub fn repair_accessibility() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("tccutil")
            .args(["reset", "Accessibility", "com.novaire.echokey"])
            .status();
        std::thread::sleep(std::time::Duration::from_millis(400));
        platform::imp::request_accessibility_access();
    }
}

/// Switch the app icon: persists the choice, applies it to the running app
/// where the OS allows, and swaps the bundle's icon.icns when writable.
/// Returns true when a restart is needed for the change to fully land.
#[tauri::command]
pub async fn set_app_icon(state: State<'_, Arc<AppState>>, app: AppHandle, icon_id: String) -> Result<bool> {
    const VALID: [&str; 5] = ["default", "keycap", "waveform", "echo-rings", "cassette"];
    if !VALID.contains(&icon_id.as_str()) {
        return Err("unknown icon id".into());
    }
    {
        let mut s = state.settings.lock();
        s.appearance.app_icon = icon_id.clone();
        s.save(&settings_path()).map_err(err)?;
    }
    crate::icons::apply_app_icon(&app, &icon_id)
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

/// Pin pasted/typed content to the current moment of the active recording.
#[tauri::command]
pub fn insert_mark(state: State<'_, Arc<AppState>>, text: String) -> Result<u64> {
    state.pipeline.add_mark(&text)
}

/// Current pipeline state, for views that mount mid-recording (a hotkey can
/// start dictation before the Compose screen exists).
#[tauri::command]
pub fn pipeline_state(state: State<'_, Arc<AppState>>) -> &'static str {
    if state.pipeline.is_recording() {
        "recording"
    } else {
        "idle"
    }
}

#[tauri::command]
pub fn open_permission_settings(which: String) {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    match which.as_str() {
        "microphone" => platform::imp::open_microphone_settings(),
        // Without this arm "local-network" fell through to Accessibility, so
        // the one button offered to a user whose mDNS is being filtered sent
        // them to the wrong settings pane entirely.
        "local-network" => platform::imp::open_local_network_settings(),
        _ => platform::imp::open_accessibility_settings(),
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
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

// -- Sync --------------------------------------------------------------------
//
// Every one of these is async + spawn_blocking. A non-async Tauri command runs
// on the MAIN thread, and these bind sockets, start the mDNS daemon, talk to the
// OS credential store and write settings.json. This app has already deadlocked
// its UI thread once on blocking I/O; none of that belongs there.

#[tauri::command]
pub fn sync_status(state: State<'_, Arc<AppState>>) -> Result<crate::sync::manager::SyncStatus> {
    // Only takes the state mutex, which is never held across I/O.
    Ok(state.sync.status())
}

#[tauri::command]
pub async fn sync_set_enabled(state: State<'_, Arc<AppState>>, enabled: bool) -> Result<()> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // A failure here has already rolled `enabled` back and recorded why, so
        // the switch cannot sit on while nothing is listening.
        let r = st.sync.set_enabled(enabled);
        persist_sync(&st);
        r
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn sync_set_device_name(state: State<'_, Arc<AppState>>, name: String) -> Result<()> {
    // Sanitised HERE, against the same rule the wire enforces, so the settings
    // layer can never hold a name that makes sync unsendable. Taking 64 chars
    // was not the same rule: the wire counts bytes and refuses '=' (the name
    // rides in an mDNS TXT key=value pair), so "Ben=Work" and any longish
    // non-Latin name were stored and then killed every exchange and discovery
    // with it, which the UI showed as a network fault.
    let Some(name) = echokey_sync::sanitise_device_name(&name) else {
        return Err("Give this device a name so you can recognise it when pairing".into());
    };
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        st.sync.set_device_name(&name);
        persist_sync(&st);
    })
    .await
    .map_err(err)
}

#[tauri::command]
pub async fn sync_set_kinds(
    state: State<'_, Arc<AppState>>,
    dictations: bool,
    clipboard: bool,
) -> Result<()> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        st.sync.set_kinds(dictations, clipboard);
        persist_sync(&st);
    })
    .await
    .map_err(err)
}

#[derive(serde::Serialize)]
pub struct StartPairing {
    pub code: String,
    /// Epoch ms.
    pub expires_at: i64,
}

#[tauri::command]
pub async fn sync_start_pairing(state: State<'_, Arc<AppState>>) -> Result<StartPairing> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        st.sync
            .start_pairing()
            .map(|(code, expires_at)| StartPairing { code, expires_at })
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub fn sync_cancel_pairing(state: State<'_, Arc<AppState>>) -> Result<()> {
    state.sync.cancel_pairing();
    Ok(())
}

#[tauri::command]
pub async fn sync_pair_with(
    state: State<'_, Arc<AppState>>,
    peer_id: String,
    code: String,
) -> Result<()> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let r = st.sync.pair_with(&peer_id, &code);
        if r.is_ok() {
            persist_sync(&st);
        }
        r
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn sync_unpair(state: State<'_, Arc<AppState>>, device_id: String) -> Result<()> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Credential-store RPC; can block for a surprising while.
        let r = st.sync.unpair(&device_id);
        if r.is_ok() {
            persist_sync(&st);
        }
        r
    })
    .await
    .map_err(err)?
}

/// Mirror the manager's state into settings.json.
///
/// The manager owns this now, because it also changes pairing state on paths no
/// command ever sees — an inbound pairing completes on a listener thread.
fn persist_sync(state: &Arc<AppState>) {
    state.sync.persist();
}
