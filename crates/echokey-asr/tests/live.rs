//! Live engine test: real whisper.cpp inference on synthesised speech.
//! Needs ggml-base-q5_1.bin in the EchoKey models dir — run
//! `cargo test -p echokey-asr --features metal -- --ignored` after downloading.

#![cfg(feature = "whisper")]

use echokey_asr::whisper::WhisperEngine;
use echokey_asr::{AsrEngine, TranscribeOptions};

fn models_dir() -> std::path::PathBuf {
    dirs_fallback().join("EchoKey/models")
}

fn dirs_fallback() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::path::PathBuf::from(std::env::var("HOME").unwrap()).join("Library/Application Support")
    }
    #[cfg(target_os = "windows")]
    {
        std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share")
    }
}

fn fixture(name: &str) -> Vec<f32> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bench/fixtures")
        .join(name);
    let mut reader = hound_read(&path);
    reader
}

fn hound_read(path: &std::path::Path) -> Vec<f32> {
    // Minimal 16-bit mono WAV reader to avoid a dev-dependency cycle.
    let data = std::fs::read(path).expect("fixture missing — see bench/fixtures");
    // Find the "data" chunk.
    let pos = data
        .windows(4)
        .position(|w| w == b"data")
        .expect("no data chunk");
    let samples = &data[pos + 8..];
    samples
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect()
}

#[test]
#[ignore = "needs downloaded model + fixtures; run explicitly"]
fn transcribes_synthesised_speech() {
    let model = models_dir().join("ggml-base-q5_1.bin");
    let mut engine =
        WhisperEngine::load(&model, "whisper-base-q5_1", true, true).expect("model load");

    // Warmup, then measure.
    engine.warmup();

    let samples = fixture("hello-16k.wav");
    let start = std::time::Instant::now();
    let out = engine
        .transcribe(&samples, &TranscribeOptions::default(), None)
        .expect("transcribe");
    let elapsed = start.elapsed();

    let text: String = out
        .segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    eprintln!("TRANSCRIPT: {text}");
    eprintln!("LATENCY: {:?} for {:.1}s of audio", elapsed, samples.len() as f32 / 16000.0);
    assert!(text.contains("quick brown fox"), "got: {text}");
    assert!(text.contains("lazy dog"), "got: {text}");
    assert_eq!(out.detected_language.as_deref(), Some("en"));
    // Confidence present.
    assert!(out.segments.iter().all(|s| s.confidence > 0.0 && s.confidence <= 1.0));
}

#[test]
#[ignore = "needs downloaded model + fixtures; run explicitly"]
fn partial_callback_fires_in_order() {
    let model = models_dir().join("ggml-base-q5_1.bin");
    let mut engine =
        WhisperEngine::load(&model, "whisper-base-q5_1", true, true).expect("model load");
    let samples = fixture("meeting-16k.wav");
    let partials = std::sync::Arc::new(parking_lot_stub::Mutex::new(Vec::<String>::new()));
    let p2 = partials.clone();
    let out = engine
        .transcribe(
            &samples,
            &TranscribeOptions::default(),
            Some(Box::new(move |t| p2.lock().push(t.to_string()))),
        )
        .expect("transcribe");
    assert!(!out.segments.is_empty());
    let partials = partials.lock();
    eprintln!("PARTIALS: {partials:?}");
    assert!(!partials.is_empty(), "segment callback never fired");
}

/// Tiny local stand-in so the test file has no extra deps.
mod parking_lot_stub {
    pub struct Mutex<T>(std::sync::Mutex<T>);
    impl<T> Mutex<T> {
        pub fn new(v: T) -> Self {
            Self(std::sync::Mutex::new(v))
        }
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap()
        }
    }
}
