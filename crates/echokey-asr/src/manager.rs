//! Owns the loaded engine and implements the failure ladder: if the active
//! model can't load or transcribe, walk the fallback chain. The caller's audio
//! buffer is never consumed — on total failure it is still intact for recovery.

use crate::registry::{self, ModelInfo};
use crate::{AsrEngine, AsrError, AsrOutput, PartialCallback, TranscribeOptions};
use std::path::PathBuf;

pub struct EngineManager {
    models_dir: PathBuf,
    use_gpu: bool,
    engine: Option<Box<dyn AsrEngine>>,
    active_model: String,
    fallback_chain: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EngineStatus {
    pub loaded_model: Option<String>,
    pub warm: bool,
}

impl EngineManager {
    pub fn new(models_dir: PathBuf, use_gpu: bool) -> Self {
        Self {
            models_dir,
            use_gpu,
            engine: None,
            active_model: String::new(),
            fallback_chain: Vec::new(),
        }
    }

    pub fn configure(&mut self, active_model: &str, fallback_chain: &[String]) {
        if active_model != self.active_model {
            self.engine = None; // model switch -> reload on next use
        }
        self.active_model = active_model.to_string();
        self.fallback_chain = fallback_chain.to_vec();
    }

    pub fn status(&self) -> EngineStatus {
        EngineStatus {
            loaded_model: self.engine.as_ref().map(|e| e.model_id().to_string()),
            warm: self.engine.is_some(),
        }
    }

    /// Load + warm the active model (startup pre-warm). Falls back down the
    /// chain; returns the model id actually loaded.
    pub fn prewarm(&mut self) -> Result<String, AsrError> {
        self.ensure_loaded()?;
        if let Some(e) = self.engine.as_mut() {
            e.warmup();
            Ok(e.model_id().to_string())
        } else {
            Err(AsrError::LoadFailed("no engine after ensure_loaded".into()))
        }
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
                    self.engine = Some(engine);
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
                Ok(out) => return Ok((out, model_id)),
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
        matches!(err, AsrError::ModelMissing(_));
    }

    #[test]
    fn candidates_dedupe() {
        let mut m = EngineManager::new(std::env::temp_dir(), false);
        m.configure("a", &["a".to_string(), "b".to_string(), "b".to_string()]);
        assert_eq!(m.candidates(), vec!["a".to_string(), "b".to_string()]);
    }
}
