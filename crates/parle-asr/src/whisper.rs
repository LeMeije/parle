//! whisper.cpp backend via whisper-rs (Metal on macOS, CUDA on Windows via
//! cargo features; CPU otherwise).

use crate::{AsrEngine, AsrError, AsrOutput, AsrSegment, PartialCallback, TranscribeOptions};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperEngine {
    ctx: WhisperContext,
    model_id: String,
    multilingual: bool,
}

/// How many threads to give whisper.cpp.
///
/// PERFORMANCE CORES ONLY on Apple Silicon, not the total core count.
///
/// This was `min(available_parallelism(), 8)`. On an Apple M4 that is 10 total
/// cores but only FOUR performance cores and six efficiency cores, so it asked
/// for eight threads and four of them landed on E-cores. whisper.cpp splits the
/// work evenly across its threads and then waits for all of them, so the whole
/// transcription proceeds at efficiency-core speed: the fast cores finish their
/// share and idle while the slow ones grind. The effect is worst exactly when
/// the machine is already busy, because the E-cores are then contended too.
///
/// Thread count does not affect the output, only how long it takes, so this
/// costs no accuracy.
fn default_threads() -> i32 {
    #[cfg(target_os = "macos")]
    {
        // `hw.perflevel0.logicalcpu` is the performance-core count. It is
        // absent on Intel Macs, where every core is the same and the total is
        // the right answer.
        if let Some(p) = std::process::Command::new("sysctl")
            .args(["-n", "hw.perflevel0.logicalcpu"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            if p >= 1 {
                return p.min(8);
            }
        }
    }
    (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as i32).min(8)
}

impl WhisperEngine {
    pub fn load(model_path: &Path, model_id: &str, multilingual: bool, use_gpu: bool) -> Result<Self, AsrError> {
        if !model_path.exists() {
            return Err(AsrError::ModelMissing(model_path.display().to_string()));
        }
        let mut params = WhisperContextParameters::default();
        params.use_gpu(use_gpu);
        let ctx = WhisperContext::new_with_params(
            model_path.to_str().ok_or_else(|| AsrError::LoadFailed("non-utf8 path".into()))?,
            params,
        )
        .map_err(|e| AsrError::LoadFailed(e.to_string()))?;
        Ok(Self { ctx, model_id: model_id.to_string(), multilingual })
    }
}

impl AsrEngine for WhisperEngine {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn transcribe(
        &mut self,
        samples: &[f32],
        opts: &TranscribeOptions,
        on_partial: Option<PartialCallback>,
    ) -> Result<AsrOutput, AsrError> {
        let started = std::time::Instant::now();
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| AsrError::TranscribeFailed(format!("create_state: {e}")))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_token_timestamps(true);
        params.set_no_context(true);

        let threads = opts.threads.unwrap_or_else(default_threads);
        params.set_n_threads(threads);

        // Language: whisper wants a static-ish &str; map through known codes.
        let lang = if self.multilingual {
            match opts.language.as_str() {
                "auto" | "" => None,
                code => Some(code),
            }
        } else {
            Some("en")
        };
        if let Some(l) = lang {
            params.set_language(Some(l));
        } else {
            params.set_language(None); // auto-detect
        }
        if opts.translate_to_english && self.multilingual {
            params.set_translate(true);
        }
        if !opts.initial_prompt.is_empty() {
            params.set_initial_prompt(&opts.initial_prompt);
        }

        if let Some(mut cb) = on_partial {
            params.set_segment_callback_safe(move |data: whisper_rs::SegmentCallbackData| {
                cb(&data.text);
            });
        }

        // whisper.cpp requires at least ~1s of audio; pad short utterances.
        let mut audio;
        let samples = if samples.len() < 16_320 {
            audio = samples.to_vec();
            audio.resize(16_320, 0.0);
            &audio[..]
        } else {
            samples
        };

        state
            .full(params, samples)
            .map_err(|e| AsrError::TranscribeFailed(e.to_string()))?;

        let n = state.full_n_segments();
        let mut segments = Vec::with_capacity(n as usize);
        for i in 0..n {
            let Some(seg) = state.get_segment(i) else { continue };
            let text = seg.to_str_lossy().map(|s| s.to_string()).unwrap_or_default();
            let t0 = seg.start_timestamp();
            let t1 = seg.end_timestamp();
            let n_tokens = seg.n_tokens();
            let mut words: Vec<(String, f32)> = Vec::new();
            let mut prob_sum = 0.0f32;
            let mut prob_count = 0u32;
            for j in 0..n_tokens {
                let Some(tok) = seg.get_token(j) else { continue };
                let tok_text = tok.to_str_lossy().map(|s| s.to_string()).unwrap_or_default();
                let p = tok.token_probability();
                if tok_text.starts_with("[_") || tok_text.starts_with("<|") {
                    continue; // special tokens
                }
                prob_sum += p;
                prob_count += 1;
                // Aggregate BPE pieces into words on leading-space boundaries.
                if tok_text.starts_with(' ') || words.is_empty() {
                    words.push((tok_text.trim_start().to_string(), p));
                } else if let Some(last) = words.last_mut() {
                    last.0.push_str(&tok_text);
                    last.1 = last.1.min(p);
                }
            }
            let confidence = if prob_count > 0 { prob_sum / prob_count as f32 } else { 1.0 };
            let text = text.trim().to_string();
            if text.is_empty() {
                continue;
            }
            segments.push(AsrSegment {
                text,
                start_ms: (t0.max(0) as u64) * 10, // centiseconds -> ms
                end_ms: (t1.max(0) as u64) * 10,
                confidence,
                words,
            });
        }

        // Detected language (multilingual models only).
        let detected_language = if self.multilingual {
            let id = state.full_lang_id_from_state();
            if id >= 0 {
                whisper_rs::get_lang_str(id).map(|s| s.to_string())
            } else {
                None
            }
        } else {
            Some("en".into())
        };

        Ok(AsrOutput {
            segments,
            detected_language,
            transcribe_ms: started.elapsed().as_millis() as u64,
        })
    }
}
