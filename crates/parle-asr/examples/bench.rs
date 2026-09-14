//! Latency + accuracy benchmark across downloaded models on this machine.
//! Produces the numbers for docs/BENCHMARKS.md. Run:
//!   cargo run --release --example bench -p parle-asr --features metal   (macOS)
//!   cargo run --release --example bench -p parle-asr --features cuda    (Windows)

use parle_asr::registry;
use parle_asr::whisper::WhisperEngine;
use parle_asr::{AsrEngine, TranscribeOptions};
use std::time::Instant;

struct Fixture {
    name: &'static str,
    file: &'static str,
    expect_words: &'static [&'static str],
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "hello (6.1s)",
        file: "hello-16k.wav",
        expect_words: &["quick", "brown", "fox", "lazy", "dog", "dictation"],
    },
    Fixture {
        name: "meeting (5.3s)",
        file: "meeting-16k.wav",
        expect_words: &["thursday", "wednesday", "lunch", "quarterly"],
    },
    Fixture {
        name: "email (30s)",
        file: "email-16k.wav",
        expect_words: &["proposal", "budget", "deadline", "melbourne", "september", "feedback", "revenue"],
    },
];

fn main() {
    let models_dir = default_models_dir();
    let fixtures_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/fixtures");
    let runs: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);

    println!("# Parle ASR benchmark");
    println!("machine: {:?}", registry::detect_machine());
    println!("runs per fixture: {runs} (after 1 warmup)\n");
    println!("| model | fixture | audio_s | median_ms | p95_ms | xRT | words_ok |");
    println!("|---|---|---|---|---|---|---|");

    for info in registry::MODELS {
        let path = models_dir.join(info.file_name);
        if !path.exists() {
            continue;
        }
        let Ok(mut engine) = WhisperEngine::load(&path, info.id, info.multilingual, true) else {
            println!("| {} | LOAD FAILED | | | | | |", info.id);
            continue;
        };
        engine.warmup();

        for fx in FIXTURES {
            let samples = read_wav_16k(&fixtures_dir.join(fx.file));
            let audio_s = samples.len() as f64 / 16_000.0;
            let mut times = Vec::with_capacity(runs);
            let mut last_text = String::new();
            for _ in 0..runs {
                let t = Instant::now();
                let out = engine
                    .transcribe(&samples, &TranscribeOptions::default(), None)
                    .expect("transcribe");
                times.push(t.elapsed().as_millis() as u64);
                last_text = out
                    .segments
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
            }
            times.sort();
            let median = times[times.len() / 2];
            let p95_idx = ((times.len() as f64 * 0.95) as usize).min(times.len() - 1);
            let p95 = times[p95_idx];
            let xrt = audio_s / (median as f64 / 1000.0);
            let hits = fx.expect_words.iter().filter(|w| last_text.contains(**w)).count();
            println!(
                "| {} | {} | {:.1} | {} | {} | {:.1}x | {}/{} |",
                info.id,
                fx.name,
                audio_s,
                median,
                p95,
                xrt,
                hits,
                fx.expect_words.len()
            );
        }
    }
}

fn default_models_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::path::PathBuf::from(std::env::var("HOME").unwrap())
            .join("Library/Application Support/Parle/models")
    }
    #[cfg(target_os = "windows")]
    {
        std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap()).join("Parle/models")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/Parle/models")
    }
}

fn read_wav_16k(path: &std::path::Path) -> Vec<f32> {
    let data = std::fs::read(path).expect("fixture missing");
    let pos = data.windows(4).position(|w| w == b"data").expect("no data chunk");
    data[pos + 8..]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect()
}
