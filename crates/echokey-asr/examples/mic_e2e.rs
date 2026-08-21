//! Live end-to-end check: mic -> ordered pipeline -> 16kHz -> whisper Metal.
//! Run with speech playing near the mic (we drive `say` from the test script).

use echokey_asr::whisper::WhisperEngine;
use echokey_asr::{AsrEngine, TranscribeOptions};

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(6);
    let home = std::env::var("HOME").unwrap();
    let model = std::path::PathBuf::from(home)
        .join("Library/Application Support/EchoKey/models/ggml-base-q5_1.bin");

    eprintln!("loading model…");
    let mut engine = WhisperEngine::load(&model, "whisper-base-q5_1", true, true).expect("load");
    engine.warmup();
    eprintln!("recording {secs}s from default mic…");

    let levels = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let l2 = levels.clone();
    let rec = echokey_audio::recorder::Recorder::start("", move |u| {
        l2.store((u.envelope * 1000.0) as u32, std::sync::atomic::Ordering::Relaxed);
    })
    .expect("recorder start");

    std::thread::sleep(std::time::Duration::from_secs(secs));
    let recording = rec.stop();
    eprintln!(
        "captured {:.1}s ({} samples, dropped {}) from '{}'",
        recording.duration_ms as f32 / 1000.0,
        recording.samples.len(),
        recording.dropped_chunks,
        recording.device_name
    );
    if echokey_asr::is_silence(&recording.samples) {
        println!("SILENCE");
        return;
    }
    let start = std::time::Instant::now();
    let out = engine
        .transcribe(&recording.samples, &TranscribeOptions::default(), None)
        .expect("transcribe");
    let text: Vec<String> = out.segments.iter().map(|s| s.text.clone()).collect();
    println!("TRANSCRIPT: {}", text.join(" "));
    println!("LATENCY: {:?}", start.elapsed());
}
