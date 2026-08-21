//! The dictation pipeline controller: owns the ASR engine manager, drives the
//! recording state machine, and on stop runs cleanup -> dictionary -> inject +
//! clipboard + history, emitting events to the UI throughout.
//!
//! The recorded audio is never discarded until a transcription succeeds or the
//! user cancels. On total ASR failure the WAV is written for recovery.

use crate::platform;
use echokey_asr::manager::EngineManager;
use echokey_asr::{is_silence, TranscribeOptions};
use echokey_audio::recorder::{LevelUpdate, Recorder};
use echokey_core::dictionary::Dictionary;
use echokey_core::formatter;
use echokey_core::history::Store;
use echokey_core::settings::{data_dir, Settings};
use echokey_core::types::{LowConfidenceSpan, Segment, TranscriptionResult};
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineState {
    Idle,
    Recording,
    Transcribing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PipelineEvent {
    StateChanged { state: PipelineState },
    Level(LevelUpdate),
    Partial { text: String },
    Completed {
        item_id: i64,
        text: String,
        duration_ms: u64,
        transcribe_ms: u64,
        model_id: String,
        injection: Option<platform::InjectionOutcome>,
        low_confidence_count: usize,
    },
    Empty { reason: String },
    Error { message: String },
}

/// Callback the app uses to push events to the correct windows.
pub type EventSink = Arc<dyn Fn(PipelineEvent) + Send + Sync>;

pub struct Pipeline {
    engine: Arc<Mutex<EngineManager>>,
    store: Arc<Mutex<Store>>,
    settings: Arc<Mutex<Settings>>,
    recorder: Mutex<Option<Recorder>>,
    sink: EventSink,
    /// Latched app id/name captured at recording START (focus may move later).
    start_app: Mutex<(Option<String>, Option<String>)>,
}

impl Pipeline {
    pub fn new(
        engine: Arc<Mutex<EngineManager>>,
        store: Arc<Mutex<Store>>,
        settings: Arc<Mutex<Settings>>,
        sink: EventSink,
    ) -> Self {
        Self {
            engine,
            store,
            settings,
            recorder: Mutex::new(None),
            sink,
            start_app: Mutex::new((None, None)),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.lock().is_some()
    }

    pub fn start(&self) {
        if self.recorder.lock().is_some() {
            return;
        }
        let device = self.settings.lock().audio.input_device.clone();
        *self.start_app.lock() = platform::imp::frontmost_app();

        let sink = self.sink.clone();
        let on_level = move |u: LevelUpdate| sink(PipelineEvent::Level(u));
        match Recorder::start(&device, on_level) {
            Ok(rec) => {
                *self.recorder.lock() = Some(rec);
                (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Recording });
            }
            Err(e) => {
                (self.sink)(PipelineEvent::Error {
                    message: format!("Could not start microphone: {e}"),
                });
            }
        }
    }

    pub fn cancel(&self) {
        if let Some(rec) = self.recorder.lock().take() {
            rec.cancel();
        }
        (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Idle });
    }

    /// Stop recording and run the full pipeline (blocking; call on a worker).
    pub fn stop_and_process(&self) {
        let Some(rec) = self.recorder.lock().take() else {
            return;
        };
        (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Transcribing });
        let recording = rec.stop();

        let settings = self.settings.lock().clone();
        if recording.duration_ms < settings.audio.min_duration_ms {
            (self.sink)(PipelineEvent::Empty { reason: "Too short".into() });
            (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Idle });
            return;
        }
        if is_silence(&recording.samples) {
            (self.sink)(PipelineEvent::Empty { reason: "No speech detected".into() });
            (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Idle });
            return;
        }
        if recording.dropped_chunks > 0 {
            tracing::warn!("dropped {} audio chunks during capture", recording.dropped_chunks);
        }

        // Dictionary bias prompt.
        let dict_entries = self.store.lock().dict_entries().unwrap_or_default();
        let dict = Dictionary::new(dict_entries);
        let bias = if settings.dictionary.enabled && settings.dictionary.bias_recognition {
            dict.bias_prompt(24)
        } else {
            String::new()
        };

        let opts = TranscribeOptions {
            language: settings.language.language.clone(),
            translate_to_english: settings.language.translate_to_english,
            initial_prompt: bias,
            threads: None,
        };

        // Streaming partials to the HUD.
        let sink = self.sink.clone();
        let on_partial: echokey_asr::PartialCallback = Box::new(move |t: &str| {
            sink(PipelineEvent::Partial { text: t.to_string() });
        });

        let result = self.engine.lock().transcribe(&recording.samples, &opts, Some(on_partial));
        let (asr, model_id) = match result {
            Ok(v) => v,
            Err(e) => {
                // Total failure: preserve the audio for recovery.
                let path = data_dir().join("recovered").join(format!(
                    "recording-{}.wav",
                    now_stamp()
                ));
                let _ = std::fs::create_dir_all(path.parent().unwrap());
                let _ = echokey_audio::wav::write_wav_16k_mono(&path, &recording.samples);
                (self.sink)(PipelineEvent::Error {
                    message: format!(
                        "Transcription failed ({e}). Your recording was saved to {}.",
                        path.display()
                    ),
                });
                (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Idle });
                return;
            }
        };

        let raw_text: String = asr
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();

        // Tier-1 deterministic cleanup.
        let segments: Vec<Segment> = asr
            .segments
            .iter()
            .map(|s| Segment {
                text: s.text.clone(),
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                confidence: s.confidence,
            })
            .collect();
        let formatted = formatter::format(
            &raw_text,
            &segments,
            &settings.cleanup,
            &settings.language.locale,
        );

        // Dictionary post-correction.
        let (mut text, _corrections) = if settings.dictionary.enabled {
            dict.apply(&formatted.text, settings.dictionary.fuzzy_correct)
        } else {
            (formatted.text.clone(), vec![])
        };
        text = text.trim().to_string();

        if text.is_empty() {
            (self.sink)(PipelineEvent::Empty { reason: "No speech detected".into() });
            (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Idle });
            return;
        }

        // Low-confidence surfacing (word-level, threshold 0.55).
        let low_confidence = collect_low_confidence(&asr, &text);

        // Output: inject + clipboard + history, simultaneously in intent.
        let injection = if settings.paste.inject {
            Some(platform::imp::inject_text(
                &text,
                settings.paste.prefer_ax_insert,
                settings.paste.restore_delay_ms,
            ))
        } else if settings.paste.copy_to_clipboard {
            platform::imp::write_clipboard(&text);
            None
        } else {
            None
        };
        // If injection used the clipboard we've already set it; otherwise honour
        // copy_to_clipboard explicitly.
        if injection.is_none() && settings.paste.copy_to_clipboard && !settings.paste.inject {
            // already written above
        }

        let (app_id, app_name) = self.start_app.lock().clone();
        let tr = TranscriptionResult {
            raw_text: raw_text.clone(),
            text: text.clone(),
            language: asr.detected_language.clone(),
            model_id: model_id.clone(),
            duration_ms: recording.duration_ms,
            transcribe_ms: asr.transcribe_ms,
            segments,
            trimmed: formatted.trimmed,
            low_confidence: low_confidence.clone(),
            cleanup_tier: if settings.cleanup.enabled { 1 } else { 0 },
        };
        let item_id = self
            .store
            .lock()
            .insert_transcription(&tr, app_id.as_deref(), app_name.as_deref())
            .unwrap_or(-1);

        (self.sink)(PipelineEvent::Completed {
            item_id,
            text,
            duration_ms: recording.duration_ms,
            transcribe_ms: asr.transcribe_ms,
            model_id,
            injection,
            low_confidence_count: low_confidence.len(),
        });
        (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Idle });
    }
}

fn collect_low_confidence(asr: &echokey_asr::AsrOutput, final_text: &str) -> Vec<LowConfidenceSpan> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for seg in &asr.segments {
        for (word, conf) in &seg.words {
            if *conf < 0.55 && word.len() > 1 {
                if let Some(pos) = final_text[cursor..].find(word.as_str()) {
                    let start = cursor + pos;
                    let end = start + word.len();
                    out.push(LowConfidenceSpan {
                        start,
                        end,
                        word: word.clone(),
                        confidence: *conf,
                    });
                    cursor = end;
                }
            }
        }
    }
    out
}

fn now_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
