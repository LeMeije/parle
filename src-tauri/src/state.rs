//! Shared application state and the platform-event dispatcher.

use crate::hotkey_logic::{GestureAction, GestureMachine, KeyPhase};
use crate::pipeline::{Pipeline, PipelineEvent};
use crate::platform::{self, HotkeyId, NativeBindings, NativeKey, PlatformEvent};
use parle_asr::download::CancelToken;
use parle_asr::manager::EngineManager;
use parle_core::history::Store;
use parle_core::settings::{history_db_path, models_dir, Settings};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

pub struct AppState {
    pub settings: Arc<Mutex<Settings>>,
    pub store: Arc<Mutex<Store>>,
    pub sync: Arc<crate::sync::manager::SyncManager>,
    pub engine: Arc<Mutex<EngineManager>>,
    pub pipeline: Arc<Pipeline>,
    pub downloads: Mutex<HashMap<String, CancelToken>>,
    pub gesture: Mutex<GestureMachine>,
    pub gesture_alt: Mutex<GestureMachine>,
    recording_flag: Arc<AtomicBool>,
    /// Kept so the cancel shortcut can be armed while recording. See
    /// `set_recording_flag`.
    app: parking_lot::Mutex<Option<AppHandle>>,
    /// Worker that runs stop_and_process off the event thread, strictly serial.
    work_tx: crossbeam_channel::Sender<Work>,
    /// Channel into the dispatcher, kept so listeners can be armed later
    /// (the native listener only starts after onboarding completes).
    pub platform_tx: Mutex<Option<crossbeam_channel::Sender<PlatformEvent>>>,
    /// The app that was frontmost before OUR window took focus — the paste
    /// target for "paste into previous app".
    pub previous_app: Mutex<Option<String>>,
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
    /// Arbitrary job on the serial worker (engine reconfiguration etc.) so it
    /// queues after any in-flight transcription instead of blocking the caller.
    Run(Box<dyn FnOnce() + Send>),
}

impl AppState {
    pub fn new(app: &AppHandle) -> Arc<Self> {
        let (mut loaded, migrated) = Settings::load_migrated(&parle_core::settings::settings_path())
            .unwrap_or_else(|_| (Settings::default(), false));
        // Assign this install's identity on first run and persist it straight
        // away: a device id that changed between launches would orphan every
        // row already stamped with the old one.
        //
        // `migrated` joins it because the two share the only write that happens
        // at startup. Without that, a migration ran in memory and was thrown
        // away at exit, so it repeated its work and its log line at every
        // launch and the file on disk never caught up.
        let assigned_identity = loaded.ensure_device_identity();
        if assigned_identity || migrated {
            let path = parle_core::settings::settings_path();
            // Say which of the two caused the write. One log line covering both
            // would report a device identity being assigned on every launch
            // that merely migrated something, which is exactly the kind of
            // false record that sends you looking in the wrong place later.
            let why = match (assigned_identity, migrated) {
                (true, true) => "assigned a device identity and migrated settings",
                (true, false) => "assigned a device identity",
                _ => "migrated settings",
            };
            match loaded.save(&path) {
                Ok(()) => tracing::info!("{why}; saved (device {})", loaded.sync.device_id),
                Err(e) => tracing::warn!("{why} but could not save settings: {e}"),
            }
        }
        let device_id = loaded.sync.device_id.clone();
        // Taken here, before `loaded` moves into the mutex. The store must know
        // the exclusion list from the first exchange, not from the first
        // settings write: a listener can be serving within milliseconds of
        // launch, and a row from an excluded app must not leave in that window.
        let excluded_apps = loaded.history.excluded_apps.clone();
        let settings = Arc::new(Mutex::new(loaded));
        let store = Arc::new(Mutex::new({
            let mut s = Store::open(&history_db_path()).expect("history store");
            s.set_device_id(&device_id);
            s.set_excluded_apps(excluded_apps);
            s
        }));
        // Both snapshots are taken and the guard DROPPED before SyncManager::new
        // runs. Written inline as `&settings.lock().sync.clone()`, the temporary
        // guard lives to the end of the statement — that is, across
        // TcpListener::bind, the mDNS daemon start and two thread spawns. The
        // module header claims no lock is ever held across a blocking network
        // call, and on this path it was not true. Contention-free today only
        // because nothing else is running yet, which makes it a trap for the
        // next person rather than a bug you can see.
        let (sync_settings, retention_days, max_items) = {
            let s = settings.lock();
            (s.sync.clone(), s.history.retention_days, s.history.max_items)
        };
        // Retention goes in through the constructor: it starts the listener,
        // so a setter afterwards leaves a window in which an inbound session
        // enforces no retention at all.
        let sync = crate::sync::manager::SyncManager::new(
            app.clone(),
            &sync_settings,
            store.clone(),
            settings.clone(),
            retention_days,
        );
        sync.set_max_items(max_items);

        let use_gpu = cfg!(any(target_os = "macos", feature = "cuda"));
        let engine = Arc::new(Mutex::new(EngineManager::new(models_dir(), use_gpu)));
        {
            let s = settings.lock();
            let mut g = engine.lock();
            // The user's own model files are handed over at STARTUP too, not
            // only when they add one, or a restart would forget them and the
            // active model would fall back to the registry chain.
            g.set_custom(
                s.models
                    .custom
                    .iter()
                    .map(|c| parle_asr::manager::CustomModelSpec {
                        id: c.id.clone(),
                        path: std::path::PathBuf::from(&c.path),
                        multilingual: c.multilingual,
                    })
                    .collect(),
            );
            g.configure(&s.models.active_model, &s.models.fallback_chain);
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
            // Outcome messages must survive the Idle transition: the main
            // window is usually hidden, so the HUD is the only visible surface.
            match &event {
                PipelineEvent::Empty { .. } => crate::hud::hold_visible(2200),
                PipelineEvent::Error { .. } => crate::hud::hold_visible(4000),
                PipelineEvent::Completed { injection, .. } => {
                    if injection.as_ref().map(|i| i.manual_paste_required).unwrap_or(false) {
                        crate::hud::hold_visible(3500);
                    }
                }
                _ => {}
            }
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
                .name("parle-worker".into())
                .spawn(move || {
                    // ONE worker: transcription jobs are strictly ordered.
                    for job in work_rx {
                        match job {
                            Work::StopAndProcess => pipeline.stop_and_process(),
                            Work::Run(job) => job(),
                            Work::Prewarm(app) => {
                                let state = app.state::<Arc<AppState>>();
                                // Never at the expense of the hotkey hook.
                                #[cfg(target_os = "windows")]
                                crate::platform::windows::set_background_priority(true);
                                let loaded = state.engine.lock().prewarm();
                                #[cfg(target_os = "windows")]
                                crate::platform::windows::set_background_priority(false);
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
            sync,
            recording_flag: Arc::new(AtomicBool::new(false)),
            app: parking_lot::Mutex::new(None),
            work_tx,
            platform_tx: Mutex::new(None),
            previous_app: Mutex::new(None),
            hotkeys: Mutex::new(None),
            clipboard_monitor: Mutex::new(None),
        })
    }

    pub fn engine_status(&self) -> serde_json::Value {
        let status = self.engine.lock().status();
        serde_json::to_value(status).unwrap_or_default()
    }

    /// Queue engine reconfiguration on the serial worker: it must never block
    /// a caller (UI thread) behind an in-flight transcription.
    pub fn reload_engine(&self) {
        let engine = self.engine.clone();
        let (active, chain) = {
            let s = self.settings.lock();
            (s.models.active_model.clone(), s.models.fallback_chain.clone())
        };
        let _ = self.work_tx.send(Work::Run(Box::new(move || {
            engine.lock().configure(&active, &chain);
        })));
    }

    pub fn prewarm_async(&self, app: AppHandle) {
        let prewarm = self.settings.lock().models.prewarm;
        if prewarm {
            let _ = self.work_tx.send(Work::Prewarm(app));
        }
    }

    pub fn pipeline_start(&self) {
        // Flag only reflects reality: set AFTER the recorder actually starts,
        // and reset the gesture machines when it fails so Toggle/Hybrid can't
        // strand in an active state (that combination once swallowed Escape
        // system-wide).
        let ok = self.pipeline.start();
        self.set_recording_flag(ok);
        if !ok {
            self.gesture.lock().reset();
            self.gesture_alt.lock().reset();
        }
    }

    pub fn pipeline_stop(&self) {
        self.set_recording_flag(false);
        let _ = self.work_tx.send(Work::StopAndProcess);
    }

    /// Stop initiated outside the hotkey path (HUD click, tray, UI button).
    /// Must also sync the gesture machines or they desynchronise from the
    /// pipeline (latched machine + idle pipeline = dead hotkey press).
    pub fn external_stop(&self) {
        self.gesture.lock().reset();
        self.gesture_alt.lock().reset();
        self.pipeline_stop();
    }

    /// Cancel from any source: always clears flag and gestures, even when the
    /// pipeline is already idle (stale-flag recovery).
    pub fn external_cancel(&self) {
        self.gesture.lock().reset();
        self.gesture_alt.lock().reset();
        self.pipeline.cancel();
        self.set_recording_flag(false);
    }

    pub fn set_recording_flag(&self, on: bool) {
        self.recording_flag.store(on, Ordering::SeqCst);
        if let Some(h) = self.hotkeys.lock().as_ref() {
            h.set_recording(on);
        }
        // Arm the cancel key as a REAL global shortcut for as long as we are
        // recording, and disarm it the moment we stop.
        //
        // The event tap was the only thing listening for it, and the tap does
        // not receive ordinary key events when Accessibility has not been
        // granted, which made "press Escape to cancel" a switch that did
        // nothing with no way to tell. `register_chord_shortcuts` could not
        // help: it skips any key `NativeKey::parse` recognises on the grounds
        // that the native listener owns it, and Escape is one of those.
        //
        // Registered only WHILE RECORDING, because a permanently registered
        // Escape would swallow the key system-wide, which is exactly why the
        // setting is off by default in the first place.
        if let Some(app) = self.app.lock().clone() {
            crate::set_cancel_shortcut_armed(&app, self, on);
        }
    }

    /// Remember the handle so `set_recording_flag` can reach the shortcut API.
    pub fn set_app_handle(&self, app: AppHandle) {
        *self.app.lock() = Some(app);
    }

    /// Build native bindings from settings (only keys the native layer owns).
    /// DoubleTap bindings are watch-only: the key keeps its normal system
    /// behaviour (that's the whole point of the mode).
    pub fn native_bindings(&self) -> NativeBindings {
        bindings_from(&self.settings.lock())
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
    /// Push model configuration to the engine without touching anything else.
    ///
    /// The add/remove custom-model commands change what the engine can load and
    /// nothing about the window, tray or shortcuts, so they do not need the full
    /// `apply_settings` and should not pay for it mid-dictation.
    pub fn apply_settings_engine_only(&self) {
        let s = self.settings.lock().clone();
        let specs: Vec<parle_asr::manager::CustomModelSpec> = s
            .models
            .custom
            .iter()
            .map(|c| parle_asr::manager::CustomModelSpec {
                id: c.id.clone(),
                path: std::path::PathBuf::from(&c.path),
                multilingual: c.multilingual,
            })
            .collect();
        let engine = self.engine.clone();
        std::thread::spawn(move || {
            let mut g = engine.lock();
            g.set_custom(specs);
            g.configure(&s.models.active_model, &s.models.fallback_chain);
        });
    }

    pub fn apply_settings(&self, _app: &AppHandle) {
        let s = self.settings.lock().clone();
        // Tray style is a live setting: repaint it now rather than at next launch.
        {
            use tauri::Manager;
            if let Some(tray) = _app.tray_by_id("parle-tray") {
                let style = s.appearance.tray_style.as_str();
                let recording = self.recording_flag.load(Ordering::SeqCst);
                let _ = tray.set_icon(Some(crate::tray_icon_for(style, recording)));
                #[cfg(target_os = "macos")]
                let _ = tray.set_icon_as_template(crate::tray_is_template(style));
            }
        }
        // Never reset an ACTIVE gesture (a settings write mid-hold would orphan
        // the recording: the release event would arrive in Idle and do nothing).
        {
            let mut g = self.gesture.lock();
            if !g.is_active() {
                g.set_mode(s.hotkeys.dictation.mode, s.hotkeys.latch_ms);
            }
        }
        {
            let mut g = self.gesture_alt.lock();
            if !g.is_active() {
                g.set_mode(s.hotkeys.dictation_alt.mode, s.hotkeys.latch_ms);
            }
        }
        if let Some(h) = self.hotkeys.lock().as_ref() {
            h.update_bindings(self.native_bindings());
            #[cfg(target_os = "windows")]
            h.set_suppress_copilot(s.hotkeys.suppress_copilot);
        }
        // Replication must never hand out a row from an app the user excluded,
        // including one captured BEFORE they added it to the list. The store is
        // where that is enforced, so it has to be told on every settings write.
        self.store.lock().set_excluded_apps(s.history.excluded_apps.clone());
        // Replication refuses rows older than we keep, so it has to know.
        self.sync.set_retention_days(s.history.retention_days);
        // Mirrored alongside retention, not just at launch. Missing it meant
        // the post-exchange prune kept enforcing the cap the app started with,
        // so changing it in the UI had no effect on sync until a restart.
        self.sync.set_max_items(s.history.max_items);
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
            PlatformEvent::AbortGesture => self.on_abort_gesture(),
            PlatformEvent::ClipboardChanged { text, app_id, app_name } => {
                let s = self.settings.lock();
                if !s.history.clipboard_capture {
                    return;
                }
                let excluded = s.history.excluded_apps.iter().any(|x| {
                    app_id.as_deref().map(|a| x.eq_ignore_ascii_case(a)).unwrap_or(false)
                        || app_name.as_deref().map(|a| x.eq_ignore_ascii_case(a)).unwrap_or(false)
                });
                if excluded {
                    return;
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

    /// Another key was pressed while a dictation modifier was held (Fn+C for
    /// emoji, Fn+arrow for paging): the user is using the key as a modifier,
    /// not dictating. Abort the just-started hold gesture and its recording.
    pub fn on_abort_gesture(self: &Arc<Self>) {
        let holding = self.gesture.lock().in_hold_phase() || self.gesture_alt.lock().in_hold_phase();
        if holding {
            self.external_cancel();
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
                tracing::info!("hotkey {id:?} {phase:?} -> {action:?}");
                match action {
                    GestureAction::StartRecording => self.pipeline_start(),
                    GestureAction::StopRecording => self.pipeline_stop(),
                    GestureAction::Nothing => {}
                }
            }
            HotkeyId::Cancel => {
                if phase == KeyPhase::Down {
                    self.external_cancel();
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

/// Native bindings straight from settings. Free-standing so the hotkey hook can
/// be armed at process start, before AppState (and its window/DB setup) exists.
pub fn bindings_from(s: &parle_core::settings::Settings) -> NativeBindings {
    use parle_core::settings::HotkeyMode;
    let parse = |b: &parle_core::settings::HotkeyBinding| {
        if b.enabled {
            NativeKey::parse(&b.key).map(|key| platform::WatchedKey {
                key,
                swallow: b.mode != HotkeyMode::DoubleTap,
            })
        } else {
            None
        }
    };
    NativeBindings {
        dictation: parse(&s.hotkeys.dictation),
        dictation_alt: parse(&s.hotkeys.dictation_alt),
        cancel: if s.hotkeys.cancel.enabled {
            NativeKey::parse(&s.hotkeys.cancel.key)
        } else {
            None
        },
    }
}
