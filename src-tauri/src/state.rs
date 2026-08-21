//! Shared application state and the platform-event dispatcher.

use crate::hotkey_logic::{GestureAction, GestureMachine, KeyPhase};
use crate::pipeline::{Pipeline, PipelineEvent};
use crate::platform::{self, HotkeyId, NativeBindings, NativeKey, PlatformEvent};
use echokey_asr::download::CancelToken;
use echokey_asr::manager::EngineManager;
use echokey_core::history::Store;
use echokey_core::settings::{history_db_path, models_dir, Settings};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

pub struct AppState {
    pub settings: Arc<Mutex<Settings>>,
    pub store: Arc<Mutex<Store>>,
    pub engine: Arc<Mutex<EngineManager>>,
    pub pipeline: Arc<Pipeline>,
    pub downloads: Mutex<HashMap<String, CancelToken>>,
    pub gesture: Mutex<GestureMachine>,
    pub gesture_alt: Mutex<GestureMachine>,
    recording_flag: Arc<AtomicBool>,
    /// Worker that runs stop_and_process off the event thread, strictly serial.
    work_tx: crossbeam_channel::Sender<Work>,
    /// Channel into the dispatcher, kept so listeners can be armed later
    /// (the native listener only starts after onboarding completes).
    pub platform_tx: Mutex<Option<crossbeam_channel::Sender<PlatformEvent>>>,
    #[cfg(target_os = "macos")]
    pub hotkeys: Mutex<Option<platform::macos::HotkeyListener>>,
    #[cfg(target_os = "macos")]
    pub clipboard_monitor: Mutex<Option<platform::macos_clipboard::ClipboardMonitor>>,
    #[cfg(target_os = "windows")]
    pub hotkeys: Mutex<Option<platform::windows::HotkeyListener>>,
    #[cfg(target_os = "windows")]
    pub clipboard_monitor: Mutex<Option<platform::windows::ClipboardMonitor>>,
}

enum Work {
    StopAndProcess,
    Prewarm(AppHandle),
}

impl AppState {
    pub fn new(app: &AppHandle) -> Arc<Self> {
        let settings = Arc::new(Mutex::new(
            Settings::load(&echokey_core::settings::settings_path()).unwrap_or_default(),
        ));
        let store = Arc::new(Mutex::new(
            Store::open(&history_db_path()).expect("history store"),
        ));

        let use_gpu = cfg!(any(target_os = "macos", feature = "cuda"));
        let engine = Arc::new(Mutex::new(EngineManager::new(models_dir(), use_gpu)));
        {
            let s = settings.lock();
            engine
                .lock()
                .configure(&s.models.active_model, &s.models.fallback_chain);
        }

        // Event sink: route pipeline events to all windows.
        let app_for_sink = app.clone();
        let sink: crate::pipeline::EventSink = Arc::new(move |event: PipelineEvent| {
            let name = match &event {
                PipelineEvent::Level(_) => "pipeline-level",
                PipelineEvent::Partial { .. } => "pipeline-partial",
                _ => "pipeline-event",
            };
            let _ = app_for_sink.emit(name, &event);
            // Keep the HUD visible/hidden in lockstep with state.
            if let PipelineEvent::StateChanged { state } = &event {
                crate::hud::sync_hud(&app_for_sink, *state);
            }
        });

        let pipeline = Arc::new(Pipeline::new(engine.clone(), store.clone(), settings.clone(), sink));

        let (work_tx, work_rx) = crossbeam_channel::unbounded::<Work>();
        {
            let pipeline = pipeline.clone();
            std::thread::Builder::new()
                .name("echokey-worker".into())
                .spawn(move || {
                    // ONE worker: transcription jobs are strictly ordered.
                    for job in work_rx {
                        match job {
                            Work::StopAndProcess => pipeline.stop_and_process(),
                            Work::Prewarm(app) => {
                                let state = app.state::<Arc<AppState>>();
                                let loaded = state.engine.lock().prewarm();
                                match loaded {
                                    Ok(id) => {
                                        tracing::info!("prewarmed model {id}");
                                        let _ = app.emit("engine-status", state.engine_status());
                                    }
                                    Err(e) => {
                                        tracing::warn!("prewarm failed: {e}");
                                        let _ = app.emit("engine-status", state.engine_status());
                                    }
                                }
                            }
                        }
                    }
                })
                .expect("spawn worker");
        }

        let latch = settings.lock().hotkeys.latch_ms;
        let mode = settings.lock().hotkeys.dictation.mode;
        let mode_alt = settings.lock().hotkeys.dictation_alt.mode;

        Arc::new(Self {
            settings,
            store,
            engine,
            pipeline,
            downloads: Mutex::new(HashMap::new()),
            gesture: Mutex::new(GestureMachine::new(mode, latch)),
            gesture_alt: Mutex::new(GestureMachine::new(mode_alt, latch)),
            recording_flag: Arc::new(AtomicBool::new(false)),
            work_tx,
            platform_tx: Mutex::new(None),
            hotkeys: Mutex::new(None),
            clipboard_monitor: Mutex::new(None),
        })
    }

    pub fn engine_status(&self) -> serde_json::Value {
        let status = self.engine.lock().status();
        serde_json::to_value(status).unwrap_or_default()
    }

    pub fn reload_engine(&self) {
        let s = self.settings.lock();
        self.engine
            .lock()
            .configure(&s.models.active_model, &s.models.fallback_chain);
    }

    pub fn prewarm_async(&self, app: AppHandle) {
        let prewarm = self.settings.lock().models.prewarm;
        if prewarm {
            let _ = self.work_tx.send(Work::Prewarm(app));
        }
    }

    pub fn pipeline_start(&self) {
        self.set_recording_flag(true);
        self.pipeline.start();
    }

    pub fn pipeline_stop(&self) {
        self.set_recording_flag(false);
        let _ = self.work_tx.send(Work::StopAndProcess);
    }

    pub fn set_recording_flag(&self, on: bool) {
        self.recording_flag.store(on, Ordering::SeqCst);
        if let Some(h) = self.hotkeys.lock().as_ref() {
            h.set_recording(on);
        }
    }

    /// Build native bindings from settings (only keys the native layer owns).
    pub fn native_bindings(&self) -> NativeBindings {
        let s = self.settings.lock();
        let parse = |b: &echokey_core::settings::HotkeyBinding| {
            if b.enabled {
                NativeKey::parse(&b.key)
            } else {
                None
            }
        };
        NativeBindings {
            dictation: parse(&s.hotkeys.dictation),
            dictation_alt: parse(&s.hotkeys.dictation_alt),
            cancel: parse(&s.hotkeys.cancel),
        }
    }

    /// Arm the native hotkey listener if it isn't running yet (called after
    /// onboarding completes; never before — it swallows keys system-wide).
    pub fn ensure_hotkey_listener(&self) {
        if self.hotkeys.lock().is_some() || !self.settings.lock().onboarding_complete {
            return;
        }
        let Some(tx) = self.platform_tx.lock().clone() else { return };
        #[cfg(target_os = "macos")]
        {
            if platform::macos::accessibility_trusted() {
                let listener = platform::macos::HotkeyListener::start(self.native_bindings(), tx);
                *self.hotkeys.lock() = Some(listener);
                tracing::info!("native hotkey listener armed");
            }
        }
        #[cfg(target_os = "windows")]
        {
            let suppress = self.settings.lock().hotkeys.suppress_copilot;
            let listener = platform::windows::HotkeyListener::start(self.native_bindings(), suppress, tx);
            *self.hotkeys.lock() = Some(listener);
            tracing::info!("native hotkey listener armed");
        }
    }

    /// (Re)apply settings: gestures, native bindings, clipboard monitor.
    pub fn apply_settings(&self, _app: &AppHandle) {
        let s = self.settings.lock().clone();
        self.gesture
            .lock()
            .set_mode(s.hotkeys.dictation.mode, s.hotkeys.latch_ms);
        self.gesture_alt
            .lock()
            .set_mode(s.hotkeys.dictation_alt.mode, s.hotkeys.latch_ms);
        if let Some(h) = self.hotkeys.lock().as_ref() {
            h.update_bindings(self.native_bindings());
        }
        if let Some(m) = self.clipboard_monitor.lock().as_ref() {
            m.set_enabled(s.history.clipboard_capture);
        }
        self.ensure_hotkey_listener();
        self.reload_engine();
    }

    /// Handle a platform event (called from the dispatcher thread).
    pub fn on_platform_event(self: &Arc<Self>, app: &AppHandle, event: PlatformEvent) {
        match event {
            PlatformEvent::Hotkey { id, phase } => self.on_hotkey(app, id, phase),
            PlatformEvent::ClipboardChanged { text, app_id, app_name } => {
                let s = self.settings.lock();
                if !s.history.clipboard_capture {
                    return;
                }
                if let Some(ref aid) = app_id {
                    if s.history.excluded_apps.iter().any(|x| x.eq_ignore_ascii_case(aid)) {
                        return;
                    }
                }
                drop(s);
                let _ = self
                    .store
                    .lock()
                    .insert_clipboard(&text, app_id.as_deref(), app_name.as_deref());
                let _ = app.emit("history-changed", ());
            }
        }
    }

    fn on_hotkey(self: &Arc<Self>, app: &AppHandle, id: HotkeyId, phase: KeyPhase) {
        let now = now_ms();
        match id {
            HotkeyId::Dictation | HotkeyId::DictationAlt => {
                let action = if id == HotkeyId::Dictation {
                    self.gesture.lock().on_key(phase, now)
                } else {
                    self.gesture_alt.lock().on_key(phase, now)
                };
                match action {
                    GestureAction::StartRecording => self.pipeline_start(),
                    GestureAction::StopRecording => self.pipeline_stop(),
                    GestureAction::Nothing => {}
                }
            }
            HotkeyId::Cancel => {
                if phase == KeyPhase::Down && self.pipeline.is_recording() {
                    self.gesture.lock().reset();
                    self.gesture_alt.lock().reset();
                    self.pipeline.cancel();
                    self.set_recording_flag(false);
                }
            }
            HotkeyId::Palette => {
                if phase == KeyPhase::Down {
                    crate::hud::toggle_palette(app);
                }
            }
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
