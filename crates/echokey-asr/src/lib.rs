//! ASR engines behind one trait, a static model registry, a resumable
//! downloader, and a manager that owns the loaded engine + fallback chain.

pub mod download;
pub mod manager;
pub mod registry;
#[cfg(feature = "whisper")]
pub mod whisper;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    #[error("model file missing: {0}")]
    ModelMissing(String),
    #[error("model load failed: {0}")]
    LoadFailed(String),
    #[error("transcription failed: {0}")]
    TranscribeFailed(String),
    #[error("engine not available in this build: {0}")]
    EngineUnavailable(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsrSegment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Mean token probability, 0.0..=1.0. Engines without token probs report 1.0.
    pub confidence: f32,
    /// Per-word confidences where available: (word, confidence).
    #[serde(default)]
    pub words: Vec<(String, f32)>,
}

#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    /// ISO 639-1 or "auto".
    pub language: String,
    pub translate_to_english: bool,
    /// Bias prompt (dictionary glossary); empty = none.
    pub initial_prompt: String,
    pub threads: Option<i32>,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            translate_to_english: false,
            initial_prompt: String::new(),
            threads: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AsrOutput {
    pub segments: Vec<AsrSegment>,
    pub detected_language: Option<String>,
    pub transcribe_ms: u64,
}

/// A partial-transcript callback: (segment_text). Called from the ASR thread
/// in strict segment order.
pub type PartialCallback = Box<dyn FnMut(&str) + Send>;

pub trait AsrEngine: Send {
    fn model_id(&self) -> &str;

    /// Transcribe a complete 16 kHz mono f32 buffer.
    fn transcribe(
        &mut self,
        samples: &[f32],
        opts: &TranscribeOptions,
        on_partial: Option<PartialCallback>,
    ) -> Result<AsrOutput, AsrError>;

    /// Prime caches so the first real dictation is fast (1 s of silence).
    fn warmup(&mut self) {
        let silence = vec![0.0f32; 16_000];
        let _ = self.transcribe(&silence, &TranscribeOptions::default(), None);
    }
}

/// Cheap energy gate: true when the buffer is effectively silence.
/// "Silence in -> nothing injected" is a product guarantee.
pub fn is_silence(samples: &[f32]) -> bool {
    if samples.is_empty() {
        return true;
    }
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak < 0.015 {
        return true;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    rms < 0.004
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_detected() {
        assert!(is_silence(&vec![0.0; 16000]));
        assert!(is_silence(&vec![0.001; 16000]));
        let speechy: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.05).sin() * 0.2).collect();
        assert!(!is_silence(&speechy));
    }
}
