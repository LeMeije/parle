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

/// Split a recording into speech chunks at natural pauses, so language
/// auto-detection can run PER CHUNK (whisper detects a language once per pass;
/// code-switching between sentences needs one pass per stretch).
///
/// Adaptive: the silence threshold derives from the recording's own noise
/// floor, so quiet mics and noisy rooms both split sensibly. Returns one full
/// range when there's nothing to split.
pub fn split_on_speech(samples: &[f32], sample_rate: u32) -> Vec<std::ops::Range<usize>> {
    let sr = sample_rate as usize;
    let frame = sr / 50; // 20 ms frames
    if samples.len() < sr * 3 || frame == 0 {
        return vec![0..samples.len()];
    }

    // Per-frame RMS.
    let rms: Vec<f32> = samples
        .chunks(frame)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();

    // Adaptive threshold, robust to both speech-dominant recordings (a low
    // percentile can land inside speech, so cap against the median) and
    // silence-dominant ones (absolute floor).
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct10 = sorted[sorted.len() / 10];
    let median = sorted[sorted.len() / 2];
    let threshold = (pct10 * 3.0).min(median * 0.6).max(0.006);

    const MIN_SILENCE_FRAMES: usize = 32; // 640 ms pause splits
    const PAD_FRAMES: usize = 8; // keep 160 ms context each side
    let min_chunk = sr; // merge chunks under 1 s into their neighbour

    // Collect speech runs.
    let mut runs: Vec<(usize, usize)> = Vec::new(); // frame indices [start, end)
    let mut start: Option<usize> = None;
    let mut silence_run = 0usize;
    for (i, &v) in rms.iter().enumerate() {
        if v >= threshold {
            if start.is_none() {
                start = Some(i);
            }
            silence_run = 0;
        } else if let Some(s) = start {
            silence_run += 1;
            if silence_run >= MIN_SILENCE_FRAMES {
                runs.push((s, i + 1 - silence_run));
                start = None;
                silence_run = 0;
            }
        }
    }
    if let Some(s) = start {
        runs.push((s, rms.len()));
    }
    if runs.len() <= 1 {
        return vec![0..samples.len()];
    }

    // Frames -> padded sample ranges, merging tiny chunks into the previous one.
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for (fs, fe) in runs {
        let s = fs.saturating_sub(PAD_FRAMES) * frame;
        let e = ((fe + PAD_FRAMES) * frame).min(samples.len());
        match out.last_mut() {
            Some(prev) if e - s < min_chunk || s < prev.end => prev.end = e,
            _ => out.push(s..e),
        }
    }
    // A stray tiny first chunk merges forward.
    if out.len() >= 2 && out[0].len() < min_chunk {
        let first = out.remove(0);
        out[0].start = first.start;
    }
    // Cost guard: never explode into dozens of passes.
    while out.len() > 8 {
        let merged_end = out[1].end;
        out[0].end = merged_end;
        out.remove(1);
    }
    if out.is_empty() {
        vec![0..samples.len()]
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech(secs: f32) -> Vec<f32> {
        (0..(16_000.0 * secs) as usize)
            .map(|i| ((i as f32 * 0.13).sin() + (i as f32 * 0.031).sin()) * 0.15)
            .collect()
    }

    fn quiet(secs: f32) -> Vec<f32> {
        (0..(16_000.0 * secs) as usize)
            .map(|i| (i as f32 * 0.11).sin() * 0.0015)
            .collect()
    }

    #[test]
    fn split_finds_pause_boundaries() {
        let mut audio = speech(3.0);
        audio.extend(quiet(1.2));
        audio.extend(speech(2.5));
        let chunks = split_on_speech(&audio, 16_000);
        assert_eq!(chunks.len(), 2, "expected 2 chunks, got {chunks:?}");
        // First chunk covers the first speech stretch, second starts after it.
        assert!(chunks[0].start < 16_000);
        assert!(chunks[1].start > chunks[0].end.saturating_sub(16_000));
    }

    #[test]
    fn continuous_speech_stays_whole() {
        let audio = speech(6.0);
        let chunks = split_on_speech(&audio, 16_000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], 0..audio.len());
    }

    #[test]
    fn short_recordings_never_split() {
        let audio = speech(2.0);
        assert_eq!(split_on_speech(&audio, 16_000).len(), 1);
    }

    #[test]
    fn three_language_stretches() {
        let mut audio = speech(2.5);
        audio.extend(quiet(1.0));
        audio.extend(speech(2.0));
        audio.extend(quiet(1.0));
        audio.extend(speech(3.0));
        let chunks = split_on_speech(&audio, 16_000);
        assert_eq!(chunks.len(), 3, "{chunks:?}");
    }

    #[test]
    fn silence_detected() {
        assert!(is_silence(&vec![0.0; 16000]));
        assert!(is_silence(&vec![0.001; 16000]));
        let speechy: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.05).sin() * 0.2).collect();
        assert!(!is_silence(&speechy));
    }
}
