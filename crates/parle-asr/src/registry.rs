//! Static model registry. URLs and sizes verified 21/08/2026
//! (docs/research/ASR.md). Sizes are used for download progress and the
//! post-download sanity check.

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Whisper,
    Parakeet,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: &'static str,
    pub display_name: &'static str,
    pub engine: EngineKind,
    pub file_name: &'static str,
    /// Full download URL when the model lives outside ggerganov/whisper.cpp.
    pub url_override: Option<&'static str>,
    pub size_bytes: u64,
    /// 1 (slowest) .. 5 (fastest) on the reference machines.
    pub speed: u8,
    /// 1 (roughest) .. 5 (best) transcription quality.
    pub accuracy: u8,
    pub multilingual: bool,
    /// Approximate resident memory when loaded (MB), for auto-selection.
    pub ram_mb: u32,
}

const HF: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        id: "whisper-tiny-q5_1",
        display_name: "Whisper Tiny (fastest)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-tiny-q5_1.bin",
        size_bytes: 33_770_000,
        speed: 5,
        accuracy: 1,
        multilingual: true,
        ram_mb: 120,
    },
    ModelInfo {
        id: "whisper-base-q5_1",
        display_name: "Whisper Base (fast)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-base-q5_1.bin",
        size_bytes: 62_600_000,
        speed: 5,
        accuracy: 2,
        multilingual: true,
        ram_mb: 210,
    },
    ModelInfo {
        id: "whisper-base-en-q5_1",
        display_name: "Whisper Base English (fast)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-base.en-q5_1.bin",
        size_bytes: 62_600_000,
        speed: 5,
        accuracy: 2,
        multilingual: false,
        ram_mb: 210,
    },
    ModelInfo {
        id: "whisper-small-q5_1",
        display_name: "Whisper Small (balanced)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-small-q5_1.bin",
        size_bytes: 199_200_000,
        speed: 4,
        accuracy: 3,
        multilingual: true,
        ram_mb: 550,
    },
    ModelInfo {
        id: "whisper-small-en-q5_1",
        display_name: "Whisper Small English (balanced)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-small.en-q5_1.bin",
        size_bytes: 199_200_000,
        speed: 4,
        accuracy: 3,
        multilingual: false,
        ram_mb: 550,
    },
    ModelInfo {
        id: "whisper-medium-q5_0",
        display_name: "Whisper Medium (accurate)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-medium-q5_0.bin",
        size_bytes: 565_200_000,
        speed: 2,
        accuracy: 4,
        multilingual: true,
        ram_mb: 1400,
    },
    ModelInfo {
        id: "whisper-large-v3-turbo-q5_0",
        display_name: "Whisper Large v3 Turbo (best, compact)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-large-v3-turbo-q5_0.bin",
        size_bytes: 601_900_000,
        speed: 3,
        accuracy: 5,
        multilingual: true,
        ram_mb: 1600,
    },
    ModelInfo {
        id: "whisper-tiny-en-q5_1",
        display_name: "Whisper Tiny English (instant)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-tiny.en-q5_1.bin",
        size_bytes: 32_166_155,
        speed: 5,
        accuracy: 1,
        multilingual: false,
        ram_mb: 110,
    },
    ModelInfo {
        id: "whisper-medium-en-q5_0",
        display_name: "Whisper Medium English (accurate)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-medium.en-q5_0.bin",
        size_bytes: 539_225_533,
        speed: 2,
        accuracy: 4,
        multilingual: false,
        ram_mb: 1400,
    },
    ModelInfo {
        id: "whisper-large-v3-q5_0",
        display_name: "Whisper Large v3 (maximum quality)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-large-v3-q5_0.bin",
        size_bytes: 1_081_140_203,
        speed: 1,
        accuracy: 5,
        multilingual: true,
        ram_mb: 2600,
    },
    ModelInfo {
        id: "distil-large-v3.5",
        display_name: "Distil-Whisper Large v3.5 (English, fast+accurate)",
        engine: EngineKind::Whisper,
        url_override: Some("https://huggingface.co/distil-whisper/distil-large-v3.5-ggml/resolve/main/ggml-model.bin"),
        file_name: "ggml-distil-large-v3.5.bin",
        size_bytes: 1_519_521_155,
        speed: 3,
        accuracy: 5,
        multilingual: false,
        ram_mb: 3200,
    },
    ModelInfo {
        id: "whisper-large-v3-turbo-q8_0",
        display_name: "Whisper Large v3 Turbo (best)",
        engine: EngineKind::Whisper,
        url_override: None,
        file_name: "ggml-large-v3-turbo-q8_0.bin",
        size_bytes: 916_500_000,
        speed: 3,
        accuracy: 5,
        multilingual: true,
        ram_mb: 2100,
    },
    ModelInfo {
        id: "parakeet-tdt-v3-int8",
        display_name: "Parakeet TDT v3 (fastest, 25 languages)",
        engine: EngineKind::Parakeet,
        url_override: Some("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2"),
        file_name: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
        size_bytes: 487_170_055,
        speed: 5,
        accuracy: 4,
        multilingual: true,
        ram_mb: 2000,
    },
];

/// The directory an archive model extracts into (archives only).
pub fn extracted_dir(model: &ModelInfo) -> Option<&'static str> {
    model
        .file_name
        .strip_suffix(".tar.bz2")
}

/// Real download URL for a model (const tables can't concat strings).
pub fn url_for(model: &ModelInfo) -> String {
    match model.url_override {
        Some(u) => u.to_string(),
        None => format!("{HF}/{}", model.file_name),
    }
}

pub fn by_id(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

/// Machine profile used for first-launch auto-selection.
#[derive(Debug, Clone, Serialize)]
pub struct MachineProfile {
    pub os: &'static str,
    pub total_ram_mb: u64,
    pub gpu: &'static str, // "metal" | "cuda" | "none"
}

pub fn detect_machine() -> MachineProfile {
    let total_ram_mb = total_ram_mb();
    #[cfg(target_os = "macos")]
    let (os, gpu) = ("macos", "metal");
    #[cfg(target_os = "windows")]
    let (os, gpu) = ("windows", if cfg!(feature = "cuda") { "cuda" } else { "none" });
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let (os, gpu) = ("linux", "none");
    MachineProfile { os, total_ram_mb, gpu }
}

/// First-launch recommendation: (default model, fallback chain).
/// The chain always ends in the smallest model so the failure ladder can't
/// bottom out.
pub fn recommend(profile: &MachineProfile) -> (&'static str, Vec<&'static str>) {
    // Measured on an M2 (docs/BENCHMARKS.md): turbo has a ~2.6 s fixed cost per
    // utterance on Metal — too slow for short dictations — while small-q5_1 is
    // 8-24x realtime with full accuracy on the fixtures. Small is the Metal
    // default at any RAM size; turbo stays available as the quality option.
    let default = match (profile.gpu, profile.total_ram_mb) {
        ("cuda", _) => "whisper-large-v3-turbo-q8_0",
        ("metal", _) => "whisper-small-q5_1",
        (_, ram) if ram >= 16_000 => "whisper-small-q5_1",
        _ => "whisper-base-q5_1",
    };
    let chain: Vec<&'static str> = ["whisper-large-v3-turbo-q5_0", "whisper-small-q5_1", "whisper-base-q5_1", "whisper-tiny-q5_1"]
        .into_iter()
        .filter(|id| *id != default)
        .collect();
    (default, chain)
}

/// Real RAM, published by the app at startup on platforms where detecting it
/// needs OS APIs this crate deliberately doesn't depend on (Windows). Zero
/// means "not published yet"; the per-platform fallback below is used instead.
static DETECTED_RAM_MB: AtomicU64 = AtomicU64::new(0);

/// Publish the true installed RAM. Call once, early, before `detect_machine()`.
pub fn set_total_ram_mb(mb: u64) {
    DETECTED_RAM_MB.store(mb, Ordering::Relaxed);
}

fn total_ram_mb() -> u64 {
    let published = DETECTED_RAM_MB.load(Ordering::Relaxed);
    if published > 0 {
        return published;
    }
    #[cfg(target_os = "macos")]
    {
        // sysctl hw.memsize
        use std::process::Command;
        if let Ok(out) = Command::new("sysctl").args(["-n", "hw.memsize"]).output() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                if let Ok(bytes) = s.trim().parse::<u64>() {
                    return bytes / 1_048_576;
                }
            }
        }
        8192
    }
    #[cfg(target_os = "windows")]
    {
        // GlobalMemoryStatusEx via the windows crate lives in the app crate; a
        // conservative default here keeps this crate dependency-light. The app
        // publishes the real value through set_total_ram_mb() at startup.
        16_384
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        8192
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_unique() {
        let mut ids: Vec<_> = MODELS.iter().map(|m| m.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), MODELS.len());
    }

    #[test]
    fn urls_are_https_from_known_hosts() {
        for m in MODELS {
            let url = url_for(m);
            let known = url.starts_with("https://huggingface.co/")
                || url.starts_with("https://github.com/k2-fsa/sherpa-onnx/releases/download/");
            assert!(known, "{url}");
        }
    }

    #[test]
    fn published_ram_overrides_platform_fallback() {
        // The Windows fallback is a hardcoded 16 GB guess; publishing the real
        // value must win, so small-RAM machines aren't handed a model too big
        // for them.
        set_total_ram_mb(8_192);
        assert_eq!(detect_machine().total_ram_mb, 8_192);
        set_total_ram_mb(0); // reset: the static is process-global
    }

    #[test]
    fn recommendation_ladder() {
        let mac16 = MachineProfile { os: "macos", total_ram_mb: 24_576, gpu: "metal" };
        let (d, chain) = recommend(&mac16);
        assert_eq!(d, "whisper-small-q5_1");
        assert!(!chain.contains(&d));
        assert_eq!(*chain.last().unwrap(), "whisper-tiny-q5_1");

        let mac8 = MachineProfile { os: "macos", total_ram_mb: 8_192, gpu: "metal" };
        assert_eq!(recommend(&mac8).0, "whisper-small-q5_1");

        let win = MachineProfile { os: "windows", total_ram_mb: 65_536, gpu: "cuda" };
        assert_eq!(recommend(&win).0, "whisper-large-v3-turbo-q8_0");
    }

    #[test]
    fn all_models_have_positive_metadata() {
        for m in MODELS {
            assert!(m.size_bytes > 10_000_000);
            assert!((1..=5).contains(&m.speed));
            assert!((1..=5).contains(&m.accuracy));
            assert!(by_id(m.id).is_some());
        }
    }
}
