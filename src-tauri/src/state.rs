//! Shared application state and the platform-event dispatcher.

use crate::hotkey_logic::{GestureAction, GestureMachine, KeyPhase};
use crate::pipeline::{DictationMode, PendingDictation, Pipeline, PipelineEvent};
use crate::platform::{self, HotkeyId, Mods, NativeBindings, NativeKey, PlatformEvent};
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
    /// The Refine key's own gesture machine. Same modes as the other two; what
    /// differs is the `DictationMode` it starts the pipeline in.
    pub gesture_refine: Mutex<GestureMachine>,
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
    /// The recording is detached from the pipeline BEFORE it is queued, so the
    /// job carries its own audio, marks and latched app. Queuing a bare "go and
    /// stop whatever is recording" meant the job read shared slots that the
    /// next dictation had already overwritten.
    StopAndProcess(PendingDictation),
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
            let mut s = open_store_or_recover(&history_db_path());
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
            if let PipelineEvent::StateChanged { state, .. } = &event {
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
                            Work::StopAndProcess(pending) => pipeline.process(pending),
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
        let mode_refine = settings.lock().hotkeys.refine.mode;

        Arc::new(Self {
            settings,
            store,
            engine,
            pipeline,
            downloads: Mutex::new(HashMap::new()),
            gesture: Mutex::new(GestureMachine::new(mode, latch)),
            gesture_alt: Mutex::new(GestureMachine::new(mode_alt, latch)),
            gesture_refine: Mutex::new(GestureMachine::new(mode_refine, latch)),
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

    pub fn pipeline_start(&self, mode: DictationMode) {
        // Flag only reflects reality: set AFTER the recorder actually starts,
        // and reset the gesture machines when it fails so Toggle/Hybrid can't
        // strand in an active state (that combination once swallowed Escape
        // system-wide).
        let ok = self.pipeline.start(mode);
        self.set_recording_flag(ok);
        if !ok {
            self.reset_gestures();
        }
    }

    /// Every gesture machine back to Idle.
    fn reset_gestures(&self) {
        self.gesture.lock().reset();
        self.gesture_alt.lock().reset();
        self.gesture_refine.lock().reset();
    }

    /// Every gesture machine EXCEPT the one that just acted back to Idle.
    ///
    /// Three keys can each start or stop the same single recording. When one of
    /// them stops it, a machine that another key had latched would otherwise
    /// stay in its recording state, so that key's next press read as "stop"
    /// against an idle pipeline and did nothing: a dead keypress the user has
    /// to repeat. The machine that most recently acted is the one that owns
    /// the take; the others follow it.
    fn reset_other_gestures(&self, keep: HotkeyId) {
        if keep != HotkeyId::Dictation {
            self.gesture.lock().reset();
        }
        if keep != HotkeyId::DictationAlt {
            self.gesture_alt.lock().reset();
        }
        if keep != HotkeyId::Refine {
            self.gesture_refine.lock().reset();
        }
    }

    pub fn pipeline_stop(&self) {
        self.set_recording_flag(false);
        // Detached HERE, on the caller's thread, not when the worker reaches the
        // job: the microphone has to stop when the user says stop, and the
        // recorder slot has to be free before the next `pipeline_start`. See
        // `Pipeline::take_pending`. Cheap by design (three mutex takes and an
        // event), the thread joins stay on the worker.
        //
        // Nothing queued when nothing was recording, so a stray stop cannot
        // leave a phantom job holding the HUD out of Idle.
        let Some(pending) = self.pipeline.take_pending() else {
            return;
        };
        if let Err(crossbeam_channel::SendError(job)) =
            self.work_tx.send(Work::StopAndProcess(pending))
        {
            // The worker is gone, so nothing will ever transcribe this. Hand the
            // take back instead of swallowing the error: the count it raised
            // would otherwise hold the HUD on "Transcribing" for the rest of the
            // session, telling the user their dictation is on its way.
            tracing::error!("dictation worker is gone; this take cannot be transcribed");
            if let Work::StopAndProcess(pending) = job {
                self.pipeline.abandon_pending(pending);
            }
        }
    }

    /// Stop initiated outside the hotkey path (HUD click, tray, UI button).
    /// Must also sync the gesture machines or they desynchronise from the
    /// pipeline (latched machine + idle pipeline = dead hotkey press).
    pub fn external_stop(&self) {
        self.reset_gestures();
        self.pipeline_stop();
    }

    /// Cancel from any source: always clears flag and gestures, even when the
    /// pipeline is already idle (stale-flag recovery).
    pub fn external_cancel(&self) {
        self.reset_gestures();
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

    /// Is a recording in progress, as far as the hotkey layer is concerned?
    pub fn is_recording_flag_set(&self) -> bool {
        self.recording_flag.load(Ordering::SeqCst)
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
        {
            let mut g = self.gesture_refine.lock();
            if !g.is_active() {
                g.set_mode(s.hotkeys.refine.mode, s.hotkeys.latch_ms);
            }
        }
        // The program lookup is cached; a changed path or provider must not
        // keep pointing at the old executable.
        crate::refine::forget_resolved();
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
            PlatformEvent::Hotkey { id, phase, mods } => self.on_hotkey(app, id, phase, mods),
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
        let holding = self.gesture.lock().in_hold_phase()
            || self.gesture_alt.lock().in_hold_phase()
            || self.gesture_refine.lock().in_hold_phase();
        if holding {
            self.external_cancel();
        }
    }

    fn on_hotkey(self: &Arc<Self>, app: &AppHandle, id: HotkeyId, phase: KeyPhase, mods: Mods) {
        let now = now_ms();
        match id {
            HotkeyId::Dictation | HotkeyId::DictationAlt | HotkeyId::Refine => {
                let action = match id {
                    HotkeyId::Dictation => self.gesture.lock().on_key(phase, now),
                    HotkeyId::DictationAlt => self.gesture_alt.lock().on_key(phase, now),
                    _ => self.gesture_refine.lock().on_key(phase, now),
                };
                tracing::info!("hotkey {id:?} {phase:?} -> {action:?}");
                // The mode is decided from the modifiers that came WITH this
                // event, so it describes the press the user actually made
                // rather than the state of the keyboard by the time this
                // thread got round to asking.
                let mode = if id == HotkeyId::Refine {
                    DictationMode::Refine
                } else {
                    mode_for_dictation(&self.settings.lock(), mods)
                };
                match action {
                    GestureAction::StartRecording if self.pipeline.is_recording() => {
                        // A "start" while something is already recording, from
                        // a key whose machine was Idle.
                        //
                        // Only the REFINE key may switch the live take, and it
                        // switches without disturbing the machine that holds
                        // the recording, so releasing or re-tapping that key
                        // still stops it. Any other key here means "stop": it
                        // is the key the user started with and habit brings
                        // it back, and turning it into a switch back to
                        // Standard (what an earlier version did, after
                        // resetting the holder) silently un-refined the take
                        // and needed a second press to stop at all.
                        if id == HotkeyId::Refine {
                            self.pipeline_start(mode);
                        } else {
                            self.reset_gestures();
                            self.pipeline_stop();
                        }
                    }
                    GestureAction::StartRecording => self.pipeline_start(mode),
                    GestureAction::StopRecording => {
                        // Whichever key stopped the take owns the outcome; the
                        // other machines are released so their next press
                        // starts afresh instead of "stopping" a recording they
                        // no longer hold.
                        self.reset_other_gestures(id);
                        self.pipeline_stop();
                    }
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

/// Open the history store, and if the file is unreadable, move it aside and
/// start a fresh one rather than dying on the spot.
///
/// This was `.expect("history store")`: a locked, corrupt or half-migrated
/// database crashed the app at launch with no window, no message and nothing
/// in the log, which the user experiences as "Parle stopped opening". The old
/// file is KEPT under a dated name, never deleted, so nothing is lost and it
/// can be inspected or restored by hand.
fn open_store_or_recover(path: &std::path::Path) -> Store {
    match Store::open(path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("history database could not be opened ({e}); moving it aside and starting fresh");
            let stamp = now_ms() / 1000;
            for suffix in ["", "-wal", "-shm"] {
                let from = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
                if from.exists() {
                    let to = std::path::PathBuf::from(format!("{}.unreadable-{stamp}{suffix}", path.display()));
                    if let Err(e) = std::fs::rename(&from, &to) {
                        tracing::error!("could not move {} aside: {e}", from.display());
                    }
                }
            }
            match Store::open(path) {
                Ok(s) => s,
                Err(e2) => {
                    // Last resort: in memory. The app runs; nothing persists
                    // this session, and the log says so.
                    tracing::error!("a fresh history database could not be created either ({e2}); running with an in-memory history");
                    Store::open_in_memory().expect("in-memory history store")
                }
            }
        }
    }
}

/// Does the Refine feature want its own separate binding?
///
/// One place, because three of them ask: the native listener, the chord
/// registration and the Settings panel. Two of the three disagreeing would
/// leave a key armed for a trigger that never fires, or a trigger with no key.
pub fn refine_uses_own_key(s: &parle_core::settings::Settings) -> bool {
    s.refine.enabled && s.refine.trigger == parle_core::settings::RefineTrigger::OwnKey
}

/// Which mode a press of the ORDINARY dictation key starts.
///
/// Pure, and takes the modifiers as a value, so the whole rule can be tested
/// without a keyboard, an app or a running pipeline.
pub fn mode_for_dictation(s: &parle_core::settings::Settings, mods: Mods) -> DictationMode {
    use parle_core::settings::{RefineModifier, RefineTrigger};
    if !s.refine.enabled || s.refine.trigger != RefineTrigger::Modifier {
        return DictationMode::Standard;
    }
    // A dictation key that IS the trigger's modifier can never satisfy the
    // trigger: the platform layer strips the pressed key's own bit, so holding
    // the OTHER Shift would be needed, which is a distinction this feature
    // deliberately does not make. Rather than let that read as an ordinary
    // dictation that silently never refines, it is refused here as well, and
    // Settings shows the clash.
    if RefineModifier::of_native_key(&s.hotkeys.dictation.key) == Some(s.refine.modifier) {
        return DictationMode::Standard;
    }
    if mods.has(s.refine.modifier) {
        DictationMode::Refine
    } else {
        DictationMode::Standard
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
        // Armed only while the feature is on AND the user asked for a separate
        // key. A key that swallows system-wide must not exist for a feature
        // that is off, nor for a trigger that does not use it: the default
        // trigger is a modifier on the dictation key and claims nothing.
        refine: if refine_uses_own_key(s) { parse(&s.hotkeys.refine) } else { None },
        cancel: if s.hotkeys.cancel.enabled {
            NativeKey::parse(&s.hotkeys.cancel.key)
        } else {
            None
        },
    }
}

#[cfg(test)]
mod refine_trigger_tests {
    //! The modifier trigger, pinned from both sides.
    //!
    //! Every rule here has two opposite ways to be wrong: a Refine take that
    //! does not go to the AI, and an ordinary dictation that does. The second
    //! is the one that costs, because the user did not ask for it and the text
    //! leaves the machine, so each test asserts the negative case as well.

    use super::{mode_for_dictation, refine_uses_own_key};
    use crate::pipeline::DictationMode;
    use crate::platform::{Mods, NativeKey};
    use parle_core::settings::{RefineModifier, RefineTrigger, Settings};

    fn shift() -> Mods {
        Mods { shift: true, ..Default::default() }
    }

    /// Refine on, modifier trigger, dictation on the platform default key.
    fn armed() -> Settings {
        let mut s = Settings::default();
        s.refine.enabled = true;
        s.refine.trigger = RefineTrigger::Modifier;
        s.refine.modifier = RefineModifier::Shift;
        s
    }

    #[test]
    fn the_modifier_makes_it_a_refine_take_and_its_absence_does_not() {
        let s = armed();
        assert_eq!(mode_for_dictation(&s, shift()), DictationMode::Refine);
        assert_eq!(mode_for_dictation(&s, Mods::default()), DictationMode::Standard);
    }

    #[test]
    fn a_different_modifier_held_is_not_the_trigger() {
        let s = armed();
        let ctrl = Mods { ctrl: true, ..Default::default() };
        assert_eq!(mode_for_dictation(&s, ctrl), DictationMode::Standard);
        // And the trigger follows the setting rather than being hard-wired.
        //
        // The dictation key is pinned to one that carries no modifier family,
        // instead of being left at the platform default. On Windows that
        // default IS "RightCtrl", which genuinely clashes with a Ctrl trigger
        // and is refused by the rule above — so this assertion passed on macOS
        // (default "Fn", no family) and failed here. Ctrl plus the Copilot key
        // is the combination `the_keys_people_dictate_with_carry_no_modifier_family`
        // describes, and the clash itself is covered by its own test.
        let mut s2 = s.clone();
        s2.hotkeys.dictation.key = "CopilotKey".into();
        s2.refine.modifier = RefineModifier::Ctrl;
        assert_eq!(mode_for_dictation(&s2, ctrl), DictationMode::Refine);
        assert_eq!(mode_for_dictation(&s2, shift()), DictationMode::Standard);
    }

    #[test]
    fn extra_modifiers_alongside_the_trigger_still_count() {
        // People hold Shift with a hand that is also on Cmd. The trigger asks
        // whether ITS modifier is down, not whether it is the only one.
        let s = armed();
        let both = Mods { shift: true, cmd: true, ..Default::default() };
        assert_eq!(mode_for_dictation(&s, both), DictationMode::Refine);
    }

    #[test]
    fn nothing_is_refined_while_the_feature_is_off() {
        let mut s = armed();
        s.refine.enabled = false;
        assert_eq!(mode_for_dictation(&s, shift()), DictationMode::Standard);
    }

    #[test]
    fn the_separate_key_trigger_leaves_the_dictation_key_alone() {
        let mut s = armed();
        s.refine.trigger = RefineTrigger::OwnKey;
        // Holding Shift with the dictation key must do nothing in this mode,
        // or a user who chose a separate key would find Shift secretly
        // sending their dictations to the AI as well.
        assert_eq!(mode_for_dictation(&s, shift()), DictationMode::Standard);
    }

    #[test]
    fn a_dictation_key_that_is_the_trigger_modifier_never_refines() {
        // Right Shift dictates AND Shift is the trigger: the key's own bit is
        // stripped by the platform layer, so this could only ever be
        // satisfied by the other Shift, a left/right distinction this feature
        // does not make. Refused outright rather than left as a trigger that
        // appears to be set and never fires.
        let mut s = armed();
        s.hotkeys.dictation.key = "RightShift".into();
        assert_eq!(mode_for_dictation(&s, shift()), DictationMode::Standard);
        // A different family on the same key is fine.
        s.refine.modifier = RefineModifier::Ctrl;
        let ctrl = Mods { ctrl: true, ..Default::default() };
        assert_eq!(mode_for_dictation(&s, ctrl), DictationMode::Refine);
    }

    #[test]
    fn the_pressed_keys_own_modifier_bit_is_stripped_before_the_rule_sees_it() {
        // The platform half of the rule above. `dispatch_key` strips the bound
        // key's family; this is the same operation, asserted directly.
        let key = NativeKey::parse("RightOption").unwrap();
        assert_eq!(key.mod_family(), Some(RefineModifier::Alt));
        let held = Mods { alt: true, shift: true, ..Default::default() };
        let stripped = held.without(key.mod_family().unwrap());
        assert!(!stripped.alt, "the key's own Option must not count as a held Option");
        assert!(stripped.shift, "an unrelated modifier is untouched");
    }

    #[test]
    fn the_keys_people_dictate_with_carry_no_modifier_family() {
        // Fn and the Copilot key are not modifiers, so nothing is stripped for
        // them and every modifier the user holds is theirs. This is what makes
        // Shift plus a double tapped Globe, and Ctrl plus the Copilot key, work.
        for k in ["Fn", "CopilotKey"] {
            assert_eq!(NativeKey::parse(k).unwrap().mod_family(), None, "{k}");
        }
    }

    #[test]
    fn only_the_own_key_trigger_arms_a_second_binding() {
        let mut s = armed();
        assert!(!refine_uses_own_key(&s), "the modifier trigger must claim no second key");
        s.refine.trigger = RefineTrigger::OwnKey;
        assert!(refine_uses_own_key(&s));
        s.refine.enabled = false;
        assert!(!refine_uses_own_key(&s), "a disabled feature arms nothing at all");
    }

    #[test]
    fn modifier_bits_survive_the_windows_wire() {
        // The helper packs these into one byte of the event frame; a bit that
        // does not survive the round trip is a trigger that never fires.
        for m in [
            Mods { shift: true, ..Default::default() },
            Mods { ctrl: true, ..Default::default() },
            Mods { alt: true, ..Default::default() },
            Mods { cmd: true, ..Default::default() },
            Mods { shift: true, ctrl: true, alt: true, cmd: true },
            Mods::default(),
        ] {
            assert_eq!(Mods::from_bits(m.to_bits()), m, "{m:?}");
        }
        // And the byte an older helper sends (zero padding) reads as nothing.
        assert_eq!(Mods::from_bits(0), Mods::default());
    }
}
