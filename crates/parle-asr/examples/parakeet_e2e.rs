//! Parakeet end-to-end: download+extract if missing, load, transcribe fixtures.
//! Run: cargo run --release --example parakeet_e2e -p parle-asr --features "metal,parakeet"

#[cfg(feature = "parakeet")]
fn main() {
    use parle_asr::parakeet::ParakeetEngine;
    use parle_asr::{download, registry, AsrEngine, TranscribeOptions};

    let models_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join("Library/Application Support/Parle/models");
    let info = registry::by_id("parakeet-tdt-v3-int8").expect("registry entry");

    if !download::is_downloaded(&models_dir, info) {
        eprintln!("downloading {} ({} MB)…", info.id, info.size_bytes / 1_000_000);
        let token = download::CancelToken::default();
        let mut last = 0u64;
        download::download(&models_dir, info, &token, |p| {
            if p.downloaded / 50_000_000 > last {
                last = p.downloaded / 50_000_000;
                eprintln!("  {} / {} MB", p.downloaded / 1_000_000, p.total / 1_000_000);
            }
        })
        .expect("download+extract");
    }

    let dir = models_dir.join(registry::extracted_dir(info).unwrap());
    eprintln!("loading from {}…", dir.display());
    let t = std::time::Instant::now();
    let mut engine = ParakeetEngine::load(&dir, info.id).expect("load");
    eprintln!("loaded in {:?}", t.elapsed());
    engine.warmup();

    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/fixtures");
    for f in ["hello-16k.wav", "meeting-16k.wav", "email-16k.wav"] {
        let data = std::fs::read(fixtures.join(f)).expect("fixture");
        let pos = data.windows(4).position(|w| w == b"data").unwrap();
        let samples: Vec<f32> = data[pos + 8..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect();
        let audio_s = samples.len() as f64 / 16_000.0;
        let t = std::time::Instant::now();
        let out = engine.transcribe(&samples, &TranscribeOptions::default(), None).expect("transcribe");
        let ms = t.elapsed().as_millis();
        let text: Vec<String> = out.segments.iter().map(|s| s.text.clone()).collect();
        println!("{f}: {:.1}s audio in {ms}ms ({:.1}x RT)", audio_s, audio_s * 1000.0 / ms as f64);
        println!("  {}", text.join(" "));
    }
}

#[cfg(not(feature = "parakeet"))]
fn main() {
    eprintln!("build with --features parakeet");
}
