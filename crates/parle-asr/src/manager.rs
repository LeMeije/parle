//! Owns the loaded engine and implements the failure ladder: if the active
//! model can't load or transcribe, walk the fallback chain. The caller's audio
//! buffer is never consumed — on total failure it is still intact for recovery.

use crate::registry::{self, ModelInfo};
use crate::{AsrEngine, AsrError, AsrOutput, PartialCallback, TranscribeOptions};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct EngineManager {
    models_dir: PathBuf,
    use_gpu: bool,
    engine: Option<Box<dyn AsrEngine>>,
    active_model: String,
    fallback_chain: Vec<String>,
    /// Models the user supplied from their own disk, checked BEFORE the
    /// registry so a custom id can never be shadowed by one.
    custom: Vec<CustomModelSpec>,
    /// When the loaded engine last did any work: its load, the warm-up, or a
    /// transcription pass. The app frees the model once this is old enough.
    last_used: Option<Instant>,
    /// True from the moment `release` freed a model until the next one loads,
    /// so the UI can say "resting" instead of "not loaded yet".
    resting: bool,
}

/// A user-supplied model, owned rather than `&'static` like `ModelInfo`.
#[derive(Debug, Clone)]
pub struct CustomModelSpec {
    pub id: String,
    pub path: PathBuf,
    pub multilingual: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EngineStatus {
    pub loaded_model: Option<String>,
    pub warm: bool,
    /// Freed to give the memory back, and loads again when a recording starts.
    pub resting: bool,
}

impl EngineManager {
    pub fn new(models_dir: PathBuf, use_gpu: bool) -> Self {
        Self {
            models_dir,
            use_gpu,
            engine: None,
            active_model: String::new(),
            fallback_chain: Vec::new(),
            custom: Vec::new(),
            last_used: None,
            resting: false,
        }
    }

    pub fn configure(&mut self, active_model: &str, fallback_chain: &[String]) {
        if active_model != self.active_model {
            self.engine = None; // model switch -> reload on next use
        }
        self.active_model = active_model.to_string();
        self.fallback_chain = fallback_chain.to_vec();
    }

    /// The user's own model files. Replacing the list drops a loaded custom
    /// engine, because the path behind it may have just changed.
    pub fn set_custom(&mut self, custom: Vec<CustomModelSpec>) {
        let changed = custom.len() != self.custom.len()
            || custom.iter().zip(self.custom.iter()).any(|(a, b)| {
                a.id != b.id || a.path != b.path || a.multilingual != b.multilingual
            });
        if changed {
            if self.active_model.starts_with("custom:") {
                self.engine = None;
            }
            self.custom = custom;
        }
    }

    pub fn status(&self) -> EngineStatus {
        EngineStatus {
            loaded_model: self.engine.as_ref().map(|e| e.model_id().to_string()),
            warm: self.engine.is_some(),
            resting: self.resting && self.engine.is_none(),
        }
    }

    /// Load + warm the active model (startup pre-warm). Falls back down the
    /// chain; returns the model id actually loaded.
    pub fn prewarm(&mut self) -> Result<String, AsrError> {
        self.ensure_loaded()?;
        if let Some(e) = self.engine.as_mut() {
            e.warmup();
            self.last_used = Some(Instant::now());
            Ok(e.model_id().to_string())
        } else {
            Err(AsrError::LoadFailed("no engine after ensure_loaded".into()))
        }
    }

    /// Load the active model, down the fallback chain, if none is in memory.
    /// No warm-up pass: ggml-metal keeps its device, shader library and
    /// compiled pipelines in a process-wide singleton, so they survive a
    /// release. Measured: the first pass after a reload was within the
    /// run-to-run noise of a warm pass.
    ///
    /// Called the moment a recording starts, so the load overlaps the user's
    /// own speech instead of adding to the wait after it. Returns true when
    /// this call did the loading.
    pub fn preload(&mut self) -> Result<bool, AsrError> {
        if self.engine.is_some() {
            return Ok(false);
        }
        self.ensure_loaded()?;
        Ok(true)
    }

    /// Free the loaded model and give its memory back to the OS (whisper.cpp
    /// allocates the weights with vm_allocate and returns them with
    /// vm_deallocate, so this is not just a hand-back to malloc). The next
    /// `preload`, `prewarm` or `transcribe` loads it again. Returns the id that
    /// was freed, None when nothing was loaded.
    pub fn release(&mut self) -> Option<String> {
        let engine = self.engine.take()?;
        let id = engine.model_id().to_string();
        drop(engine);
        self.last_used = None;
        self.resting = true;
        Some(id)
    }

    /// How long the loaded model has gone without work; None when nothing is
    /// loaded.
    pub fn idle_for(&self) -> Option<Duration> {
        self.engine.as_ref()?;
        self.last_used.map(|t| t.elapsed())
    }

    fn candidates(&self) -> Vec<String> {
        let mut v = vec![self.active_model.clone()];
        for f in &self.fallback_chain {
            if !v.contains(f) {
                v.push(f.clone());
            }
        }
        v
    }

    fn ensure_loaded(&mut self) -> Result<(), AsrError> {
        if self.engine.is_some() {
            return Ok(());
        }
        let mut last_err = AsrError::LoadFailed("no models configured".into());
        for id in self.candidates() {
            // The user's own files first: a custom id is namespaced with
            // `custom:` so it cannot collide, and checking here means a missing
            // FILE reports as a missing model rather than an unknown id.
            if let Some(spec) = self.custom.iter().find(|c| c.id == id).cloned() {
                if !spec.path.exists() {
                    last_err = AsrError::ModelMissing(format!(
                        "{} (the file is no longer at {})",
                        id,
                        spec.path.display()
                    ));
                    continue;
                }
                match self.load_custom(&spec) {
                    Ok(engine) => {
                        self.set_loaded(engine);
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::error!("custom model {id} failed to load: {e}");
                        last_err = e;
                        continue;
                    }
                }
            }
            let Some(info) = registry::by_id(&id) else {
                last_err = AsrError::LoadFailed(format!("unknown model id {id}"));
                continue;
            };
            if !crate::download::is_downloaded(&self.models_dir, info) {
                last_err = AsrError::ModelMissing(id.clone());
                continue;
            }
            match self.load_model(info) {
                Ok(engine) => {
                    if id != self.active_model {
                        tracing::warn!("fell back from {} to {}", self.active_model, id);
                    }
                    self.set_loaded(engine);
                    return Ok(());
                }
                Err(e) => {
                    tracing::error!("model {id} failed to load: {e}");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    /// The one place a freshly loaded engine is installed, so the idle clock
    /// and the resting flag can never disagree with what is in memory.
    fn set_loaded(&mut self, engine: Box<dyn AsrEngine>) {
        self.engine = Some(engine);
        self.last_used = Some(Instant::now());
        self.resting = false;
    }

    /// Load a user-supplied whisper.cpp model file.
    fn load_custom(&self, spec: &CustomModelSpec) -> Result<Box<dyn AsrEngine>, AsrError> {
        #[cfg(feature = "whisper")]
        {
            let engine = crate::whisper::WhisperEngine::load(
                &spec.path,
                &spec.id,
                spec.multilingual,
                self.use_gpu,
            )?;
            Ok(Box::new(engine))
        }
        #[cfg(not(feature = "whisper"))]
        {
            let _ = spec;
            Err(AsrError::EngineUnavailable("whisper feature disabled".into()))
        }
    }

    fn load_model(&self, info: &ModelInfo) -> Result<Box<dyn AsrEngine>, AsrError> {
        match info.engine {
            registry::EngineKind::Whisper => {
                #[cfg(feature = "whisper")]
                {
                    let path = crate::download::model_path(&self.models_dir, info);
                    let engine = crate::whisper::WhisperEngine::load(
                        &path,
                        info.id,
                        info.multilingual,
                        self.use_gpu,
                    )?;
                    Ok(Box::new(engine))
                }
                #[cfg(not(feature = "whisper"))]
                Err(AsrError::EngineUnavailable("whisper feature disabled".into()))
            }
            registry::EngineKind::Parakeet => {
                #[cfg(feature = "parakeet")]
                {
                    let dir = self
                        .models_dir
                        .join(registry::extracted_dir(info).unwrap_or(info.file_name));
                    let engine = crate::parakeet::ParakeetEngine::load(&dir, info.id)?;
                    Ok(Box::new(engine))
                }
                #[cfg(not(feature = "parakeet"))]
                Err(AsrError::EngineUnavailable("parakeet backend not built (enable the `parakeet` feature)".into()))
            }
        }
    }

    /// Transcribe with the failure ladder. On a transcription error the engine
    /// is dropped and the next rung is tried against the SAME samples.
    pub fn transcribe(
        &mut self,
        samples: &[f32],
        opts: &TranscribeOptions,
        mut on_partial: Option<PartialCallback>,
    ) -> Result<(AsrOutput, String), AsrError> {
        let mut last_err: Option<AsrError> = None;
        let mut retried: Option<String> = None;
        // +1 attempt budget for the single same-model retry.
        for _attempt in 0..(self.candidates().len() + 1).max(1) {
            if let Err(e) = self.ensure_loaded() {
                return Err(last_err.unwrap_or(e));
            }
            let engine = self.engine.as_mut().unwrap();
            let model_id = engine.model_id().to_string();
            match engine.transcribe(samples, opts, on_partial.take()) {
                Ok(out) => {
                    // Measured from the END of the pass: a 30 s transcription
                    // must not count as 30 s of idleness.
                    self.last_used = Some(Instant::now());
                    return Ok((out, model_id));
                }
                Err(e) => {
                    // Transient failures (momentary GPU pressure) deserve one
                    // same-model retry before the model is demoted for the session.
                    if retried.as_deref() != Some(model_id.as_str()) {
                        tracing::warn!("transcribe failed on {model_id}: {e}; retrying same model once");
                        retried = Some(model_id.clone());
                        self.engine = None;
                        last_err = Some(e);
                        continue;
                    }
                    tracing::error!("transcribe failed on {model_id} twice: {e}; trying next rung");
                    // Drop the broken engine and demote the failed model so
                    // ensure_loaded picks the next candidate.
                    self.engine = None;
                    self.fallback_chain.retain(|m| m != &model_id);
                    if self.active_model == model_id {
                        self.active_model = self
                            .fallback_chain
                            .first()
                            .cloned()
                            .unwrap_or_else(|| self.active_model.clone());
                    }
                    last_err = Some(e);
                    if self.candidates().iter().all(|c| c == &model_id) {
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| AsrError::TranscribeFailed("no engine available".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    struct FakeEngine {
        id: String,
        fail: bool,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl AsrEngine for FakeEngine {
        fn model_id(&self) -> &str {
            &self.id
        }
        fn transcribe(
            &mut self,
            _samples: &[f32],
            _opts: &TranscribeOptions,
            _on_partial: Option<PartialCallback>,
        ) -> Result<AsrOutput, AsrError> {
            self.calls.lock().push(self.id.clone());
            if self.fail {
                Err(AsrError::TranscribeFailed("boom".into()))
            } else {
                Ok(AsrOutput {
                    segments: vec![crate::AsrSegment {
                        text: "ok".into(),
                        start_ms: 0,
                        end_ms: 100,
                        confidence: 0.9,
                        words: vec![],
                    }],
                    detected_language: Some("en".into()),
                    transcribe_ms: 1,
                })
            }
        }
    }

    /// Manager variant whose loader is injectable, for ladder testing.
    struct TestManager {
        inner: EngineManager,
        loads: Arc<Mutex<Vec<String>>>,
        fail_models: Vec<String>,
    }

    impl TestManager {
        fn load(&self, id: &str) -> Box<dyn AsrEngine> {
            Box::new(FakeEngine {
                id: id.to_string(),
                fail: self.fail_models.contains(&id.to_string()),
                calls: self.loads.clone(),
            })
        }
    }

    #[test]
    fn ladder_falls_through_to_working_model() {
        // Exercise the ladder logic directly (loading real models needs files).
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut tm = TestManager {
            inner: EngineManager::new(std::env::temp_dir(), false),
            loads: calls.clone(),
            fail_models: vec!["big".into()],
        };
        tm.inner.configure("big", &["small".to_string()]);

        // Simulate: big loads but fails to transcribe; manager should demote it
        // and try small.
        tm.inner.engine = Some(tm.load("big"));
        let engine = tm.inner.engine.as_mut().unwrap();
        let r = engine.transcribe(&[0.0; 100], &TranscribeOptions::default(), None);
        assert!(r.is_err());
        tm.inner.engine = None;
        tm.inner.fallback_chain.retain(|m| m != "big");
        tm.inner.active_model = "small".into();

        tm.inner.engine = Some(tm.load("small"));
        let engine = tm.inner.engine.as_mut().unwrap();
        let r = engine.transcribe(&[0.0; 100], &TranscribeOptions::default(), None);
        assert!(r.is_ok());
        assert_eq!(*calls.lock(), vec!["big".to_string(), "small".to_string()]);
    }

    #[test]
    fn missing_models_error_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = EngineManager::new(dir.path().to_path_buf(), false);
        m.configure("whisper-small-q5_1", &["whisper-base-q5_1".to_string()]);
        let err = m.prewarm().unwrap_err();
        // `matches!` alone returns a bool nobody read, so this test could not
        // fail. It asserts now.
        assert!(matches!(err, AsrError::ModelMissing(_)), "{err:?}");
    }

    fn fake(id: &str) -> Box<dyn AsrEngine> {
        Box::new(FakeEngine { id: id.into(), fail: false, calls: Arc::new(Mutex::new(Vec::new())) })
    }

    #[test]
    fn release_frees_the_engine_and_reports_resting() {
        let mut m = EngineManager::new(std::env::temp_dir(), false);
        m.configure("a", &[]);
        m.set_loaded(fake("a"));
        assert!(m.idle_for().is_some(), "a loaded engine has an idle clock");
        assert!(!m.status().resting);

        assert_eq!(m.release().as_deref(), Some("a"));
        assert!(m.engine.is_none(), "the engine itself must be dropped, not just flagged");
        assert!(m.idle_for().is_none(), "nothing loaded, nothing idle");
        let st = m.status();
        assert!(st.resting && !st.warm && st.loaded_model.is_none(), "{st:?}");
        assert_eq!(m.release(), None, "a second release has nothing to free");
    }

    #[test]
    fn a_transcription_pass_resets_the_idle_clock() {
        let mut m = EngineManager::new(std::env::temp_dir(), false);
        m.configure("a", &[]);
        m.set_loaded(fake("a"));
        let Some(ten_minutes_ago) = Instant::now().checked_sub(Duration::from_secs(600)) else {
            return; // uptime under ten minutes: the clock cannot be backdated
        };
        m.last_used = Some(ten_minutes_ago);
        assert!(m.idle_for().unwrap() >= Duration::from_secs(600));
        m.transcribe(&[0.0; 100], &TranscribeOptions::default(), None).unwrap();
        assert!(m.idle_for().unwrap() < Duration::from_secs(5), "a pass is use");
    }

    #[test]
    fn preload_leaves_a_loaded_engine_alone() {
        let mut m = EngineManager::new(std::env::temp_dir(), false);
        m.configure("a", &[]);
        m.set_loaded(fake("a"));
        assert!(!m.preload().unwrap(), "already loaded: nothing to do");
        assert_eq!(m.status().loaded_model.as_deref(), Some("a"));
    }

    #[test]
    fn preload_reports_a_missing_model_instead_of_pretending() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = EngineManager::new(dir.path().to_path_buf(), false);
        m.configure("whisper-small-q5_1", &[]);
        let err = m.preload().unwrap_err();
        assert!(matches!(err, AsrError::ModelMissing(_)), "{err:?}");
        assert!(!m.status().warm);
    }

    #[test]
    fn loading_again_ends_the_rest() {
        let mut m = EngineManager::new(std::env::temp_dir(), false);
        m.configure("a", &[]);
        m.set_loaded(fake("a"));
        m.release();
        assert!(m.status().resting);
        m.set_loaded(fake("a"));
        let st = m.status();
        assert!(st.warm && !st.resting, "{st:?}");
    }

    #[test]
    fn candidates_dedupe() {
        let mut m = EngineManager::new(std::env::temp_dir(), false);
        m.configure("a", &["a".to_string(), "b".to_string(), "b".to_string()]);
        assert_eq!(m.candidates(), vec!["a".to_string(), "b".to_string()]);
    }
}
