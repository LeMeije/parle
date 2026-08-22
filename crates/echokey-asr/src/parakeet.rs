//! NVIDIA Parakeet TDT via sherpa-onnx (official crate). CPU int8 on purpose:
//! the CoreML EP has been measured SLOWER than CPU for these models
//! (k2-fsa#2910), and 4 threads beats 8 (docs/research/ASR.md).

use crate::{AsrEngine, AsrError, AsrOutput, AsrSegment, PartialCallback, TranscribeOptions};
use sherpa_onnx::{OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig};
use std::path::Path;

/// The exported encoder's relative-position table caps inputs at ~400 s;
/// stay well under it and stitch.
const MAX_CHUNK_SECS: usize = 300;

pub struct ParakeetEngine {
    recognizer: OfflineRecognizer,
    model_id: String,
}

// The recognizer wraps a C pointer with no thread affinity; it is only ever
// used behind the EngineManager's mutex.
unsafe impl Send for ParakeetEngine {}

impl ParakeetEngine {
    pub fn load(model_dir: &Path, model_id: &str) -> Result<Self, AsrError> {
        let file = |name: &str| -> Result<String, AsrError> {
            let p = model_dir.join(name);
            if p.exists() {
                Ok(p.to_string_lossy().into_owned())
            } else {
                Err(AsrError::ModelMissing(p.display().to_string()))
            }
        };
        let mut config = OfflineRecognizerConfig::default();
        config.feat_config.sample_rate = 16_000;
        // Parakeet TDT uses 128-dim features; the crate default (80) fails.
        config.feat_config.feature_dim = 128;
        config.model_config = OfflineModelConfig {
            transducer: OfflineTransducerModelConfig {
                encoder: Some(file("encoder.int8.onnx")?),
                decoder: Some(file("decoder.int8.onnx")?),
                joiner: Some(file("joiner.int8.onnx")?),
            },
            tokens: Some(file("tokens.txt")?),
            num_threads: 4, // measured sweet spot; 8 is slower
            provider: Some("cpu".into()),
            model_type: Some("nemo_transducer".into()), // mandatory
            ..Default::default()
        };
        config.decoding_method = Some("greedy_search".into());

        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| AsrError::LoadFailed("sherpa OfflineRecognizer::create returned None".into()))?;
        Ok(Self { recognizer, model_id: model_id.to_string() })
    }

    fn transcribe_chunk(&self, samples: &[f32]) -> Result<(String, u64, u64), AsrError> {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16_000, samples);
        self.recognizer.decode(&stream);
        let result = stream
            .get_result()
            .ok_or_else(|| AsrError::TranscribeFailed("sherpa get_result returned None".into()))?;
        let end_ms = (samples.len() as u64 * 1000) / 16_000;
        Ok((result.text.trim().to_string(), 0, end_ms))
    }
}

impl AsrEngine for ParakeetEngine {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn transcribe(
        &mut self,
        samples: &[f32],
        _opts: &TranscribeOptions,
        mut on_partial: Option<PartialCallback>,
    ) -> Result<AsrOutput, AsrError> {
        let started = std::time::Instant::now();
        let max = MAX_CHUNK_SECS * 16_000;
        let mut segments = Vec::new();
        let mut offset = 0usize;
        while offset < samples.len() {
            let end = (offset + max).min(samples.len());
            let (text, s_ms, e_ms) = self.transcribe_chunk(&samples[offset..end])?;
            let base_ms = (offset as u64 * 1000) / 16_000;
            if !text.is_empty() {
                if let Some(cb) = on_partial.as_mut() {
                    cb(&text);
                }
                segments.push(AsrSegment {
                    text,
                    start_ms: base_ms + s_ms,
                    end_ms: base_ms + e_ms,
                    // The C API exposes no usable confidence for TDT greedy
                    // decoding; report certain so nothing gets flagged falsely.
                    confidence: 1.0,
                    words: vec![],
                });
            }
            offset = end;
        }
        Ok(AsrOutput {
            segments,
            // Parakeet v3 handles 25 European languages but does not report
            // which it heard; leave language undetected.
            detected_language: None,
            transcribe_ms: started.elapsed().as_millis() as u64,
        })
    }
}
