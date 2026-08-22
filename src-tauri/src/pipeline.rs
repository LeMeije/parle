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
    /// User pasted/typed content mid-recording; spliced in at this timestamp.
    MarkAdded { at_ms: u64, text: String },
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
    /// (audio position ms, verbatim text) inserted while recording.
    marks: Mutex<Vec<(u64, String)>>,
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
            marks: Mutex::new(Vec::new()),
            sink,
            start_app: Mutex::new((None, None)),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.lock().is_some()
    }

    /// Returns true when recording actually started.
    pub fn start(self: &Arc<Self>) -> bool {
        if self.recorder.lock().is_some() {
            return true; // already recording
        }
        let device = self.settings.lock().audio.input_device.clone();
        *self.start_app.lock() = platform::imp::frontmost_app();

        let sink = self.sink.clone();
        let on_level = move |u: LevelUpdate| sink(PipelineEvent::Level(u));
        self.marks.lock().clear();
        match Recorder::start(&device, on_level) {
            Ok(rec) => {
                *self.recorder.lock() = Some(rec);
                (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Recording });
                if self.settings.lock().overlay.show_partial_text {
                    self.spawn_partial_loop();
                }
                true
            }
            Err(e) => {
                (self.sink)(PipelineEvent::Error {
                    message: format!("Could not start microphone: {e}"),
                });
                false
            }
        }
    }

    /// Live partial transcripts while still speaking: every couple of seconds,
    /// transcribe a snapshot of the audio so far. Best-effort by design —
    /// engine `try_lock` means partials NEVER queue behind (or delay) the final
    /// pass, and the loop exits the moment the recorder is taken for stop.
    fn spawn_partial_loop(self: &Arc<Self>) {
        let this = self.clone();
        std::thread::Builder::new()
            .name("echokey-partials".into())
            .spawn(move || {
                const MAX_PARTIAL_SECS: usize = 30 * 16_000;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(2000));
                    let snapshot = {
                        let guard = this.recorder.lock();
                        match guard.as_ref() {
                            Some(rec) => rec.snapshot(),
                            None => break, // stopped or cancelled
                        }
                    };
                    if snapshot.len() < 24_000 || snapshot.len() > MAX_PARTIAL_SECS {
                        continue; // too short to bother, or too long to keep cheap
                    }
                    if is_silence(&snapshot) {
                        continue;
                    }
                    let Some(mut engine) = this.engine.try_lock() else {
                        continue; // engine busy (prewarm/final pass) — skip this tick
                    };
                    let opts = TranscribeOptions {
                        language: this.settings.lock().language.language.clone(),
                        ..Default::default()
                    };
                    if let Ok((out, _)) = engine.transcribe(&snapshot, &opts, None) {
                        drop(engine);
                        // Recorder may have been taken while we transcribed.
                        if this.recorder.lock().is_none() {
                            break;
                        }
                        let text: String = out
                            .segments
                            .iter()
                            .map(|s| s.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !text.trim().is_empty() {
                            (this.sink)(PipelineEvent::Partial { text: text.trim().to_string() });
                        }
                    }
                }
            })
            .expect("spawn partial loop");
    }

    /// One ASR pass per speech chunk, timestamps re-based, languages collected
    /// in speaking order ("fr+en"). The engine lock is taken per chunk so a
    /// cancel/settings change can interleave.
    fn transcribe_chunked(
        &self,
        samples: &[f32],
        chunks: &[std::ops::Range<usize>],
        opts: &TranscribeOptions,
    ) -> Result<(echokey_asr::AsrOutput, String), echokey_asr::AsrError> {
        let mut segments = Vec::new();
        let mut langs: Vec<String> = Vec::new();
        let mut transcribe_ms = 0u64;
        let mut model_id = String::new();
        for r in chunks {
            let offset_ms = (r.start as u64 * 1000) / echokey_audio::ASR_SAMPLE_RATE as u64;
            let (out, mid) = self.engine.lock().transcribe(&samples[r.clone()], opts, None)?;
            model_id = mid;
            transcribe_ms += out.transcribe_ms;
            if let Some(l) = out.detected_language {
                if langs.last() != Some(&l) {
                    langs.push(l);
                }
            }
            for mut seg in out.segments {
                seg.start_ms += offset_ms;
                seg.end_ms += offset_ms;
                segments.push(seg);
            }
        }
        let detected_language = if langs.is_empty() { None } else { Some(langs.join("+")) };
        Ok((
            echokey_asr::AsrOutput { segments, detected_language, transcribe_ms },
            model_id,
        ))
    }

    /// Insert verbatim content (pasted link, typed text) at the CURRENT moment
    /// of the recording. Returns the audio timestamp it was pinned to.
    pub fn add_mark(&self, text: &str) -> Result<u64, String> {
        let guard = self.recorder.lock();
        let Some(rec) = guard.as_ref() else {
            return Err("Not recording".into());
        };
        let at_ms = rec.elapsed_ms();
        drop(guard);
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("Nothing to insert".into());
        }
        self.marks.lock().push((at_ms, text.clone()));
        (self.sink)(PipelineEvent::MarkAdded { at_ms, text });
        Ok(at_ms)
    }

    pub fn cancel(&self) {
        if let Some(rec) = self.recorder.lock().take() {
            rec.cancel();
        }
        (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Idle });
    }

    /// Emit Idle unless a new recording started while we were processing —
    /// its Recording state must not be clobbered by our late Idle.
    fn emit_idle_if_quiescent(&self) {
        if self.recorder.lock().is_none() {
            (self.sink)(PipelineEvent::StateChanged { state: PipelineState::Idle });
        }
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
            self.emit_idle_if_quiescent();
            return;
        }
        if is_silence(&recording.samples) {
            (self.sink)(PipelineEvent::Empty { reason: "No speech detected".into() });
            self.emit_idle_if_quiescent();
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

        // Insert marks (pasted links/text mid-recording): split the audio at
        // each mark's timestamp so speech on either side is transcribed and
        // cleaned separately, and the mark text is spliced in verbatim.
        let marks = std::mem::take(&mut *self.marks.lock());

        // Code-switching: whisper detects ONE language per pass, so with
        // language=auto we split at natural pauses and let every stretch
        // detect its own language ("parle français", pause, "then English").
        let auto_lang = settings.language.language == "auto";
        let lang_chunks = |samples: &[f32]| {
            if auto_lang {
                echokey_asr::split_on_speech(samples, echokey_audio::ASR_SAMPLE_RATE)
            } else {
                vec![0..samples.len()]
            }
        };

        if !marks.is_empty() {
            self.process_with_marks(recording, marks, &opts, &settings, &dict);
            return;
        }

        let chunks = lang_chunks(&recording.samples);
        let result = if chunks.len() >= 2 {
            self.transcribe_chunked(&recording.samples, &chunks, &opts)
                // Any per-chunk failure falls back to one whole-buffer pass.
                .or_else(|e| {
                    tracing::warn!("chunked transcription failed ({e}); falling back to single pass");
                    self.engine.lock().transcribe(&recording.samples, &opts, None)
                })
        } else {
            self.engine.lock().transcribe(&recording.samples, &opts, Some(on_partial))
        };
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
                self.emit_idle_if_quiescent();
                return;
            }
        };

        use unicode_normalization::UnicodeNormalization;
        let raw_text: String = asr
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .nfc()
            .collect();

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
            // Nothing is ever lost: cleanup may legitimately empty the text
            // ("scratch that", pure filler) — keep the raw transcript in
            // history without injecting anything.
            if !raw_text.is_empty() {
                let (app_id, app_name) = self.start_app.lock().clone();
                let tr = TranscriptionResult {
                    raw_text: raw_text.clone(),
                    text: raw_text.clone(),
                    language: asr.detected_language.clone(),
                    model_id: model_id.clone(),
                    duration_ms: recording.duration_ms,
                    transcribe_ms: asr.transcribe_ms,
                    segments: segments.clone(),
                    trimmed: vec![],
                    low_confidence: vec![],
                    cleanup_tier: 0,
                };
                let _ = self
                    .store
                    .lock()
                    .insert_transcription(&tr, app_id.as_deref(), app_name.as_deref());
            }
            (self.sink)(PipelineEvent::Empty { reason: "Nothing left after cleanup (kept raw in history)".into() });
            self.emit_idle_if_quiescent();
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
                settings.paste.copy_to_clipboard,
                settings.paste.restore_clipboard,
            ))
        } else if settings.paste.copy_to_clipboard {
            platform::imp::write_clipboard(&text);
            None
        } else {
            None
        };

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
        self.emit_idle_if_quiescent();
    }
}

impl Pipeline {
    /// Mark-splice flow: audio split at mark timestamps; each speech piece is
    /// transcribed (with per-piece language auto-detection) and cleaned
    /// independently; mark text goes in verbatim — URLs never touch cleanup.
    fn process_with_marks(
        &self,
        recording: echokey_audio::recorder::Recording,
        mut marks: Vec<(u64, String)>,
        opts: &TranscribeOptions,
        settings: &Settings,
        dict: &Dictionary,
    ) {
        use crate::pipeline::PipelineState;
        marks.sort_by_key(|(ms, _)| *ms);
        let sr = echokey_audio::ASR_SAMPLE_RATE as u64;
        let total = recording.samples.len();

        // Piece boundaries in samples.
        let mut boundaries: Vec<usize> = vec![0];
        for (ms, _) in &marks {
            boundaries.push(((ms * sr / 1000) as usize).min(total));
        }
        boundaries.push(total);

        let mut final_parts: Vec<String> = Vec::new();
        let mut raw_parts: Vec<String> = Vec::new();
        let mut all_segments: Vec<Segment> = Vec::new();
        let mut langs: Vec<String> = Vec::new();
        let mut transcribe_ms = 0u64;
        let mut model_id = String::new();

        for i in 0..boundaries.len() - 1 {
            let range = boundaries[i]..boundaries[i + 1];
            if range.len() > (sr as usize / 4) && !is_silence(&recording.samples[range.clone()]) {
                let piece = &recording.samples[range.clone()];
                let chunks = if settings.language.language == "auto" {
                    echokey_asr::split_on_speech(piece, echokey_audio::ASR_SAMPLE_RATE)
                } else {
                    vec![0..piece.len()]
                };
                let result = if chunks.len() >= 2 {
                    self.transcribe_chunked(piece, &chunks, opts)
                } else {
                    self.engine.lock().transcribe(piece, opts, None)
                };
                match result {
                    Ok((out, mid)) => {
                        model_id = mid;
                        transcribe_ms += out.transcribe_ms;
                        if let Some(l) = &out.detected_language {
                            if langs.last() != Some(l) {
                                langs.push(l.clone());
                            }
                        }
                        use unicode_normalization::UnicodeNormalization;
                        let raw: String = out
                            .segments
                            .iter()
                            .map(|s| s.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                            .trim()
                            .nfc()
                            .collect();
                        let segs: Vec<Segment> = out
                            .segments
                            .iter()
                            .map(|s| Segment {
                                text: s.text.clone(),
                                start_ms: s.start_ms + (range.start as u64 * 1000 / sr),
                                end_ms: s.end_ms + (range.start as u64 * 1000 / sr),
                                confidence: s.confidence,
                            })
                            .collect();
                        let formatted = formatter::format(&raw, &segs, &settings.cleanup, &settings.language.locale);
                        let (text, _) = if settings.dictionary.enabled {
                            dict.apply(&formatted.text, settings.dictionary.fuzzy_correct)
                        } else {
                            (formatted.text.clone(), vec![])
                        };
                        all_segments.extend(segs);
                        if !text.trim().is_empty() {
                            final_parts.push(text.trim().to_string());
                        }
                        if !raw.is_empty() {
                            raw_parts.push(raw);
                        }
                    }
                    Err(e) => {
                        tracing::error!("mark-piece transcription failed: {e}");
                    }
                }
            }
            // The mark that follows this piece (none after the last piece).
            if let Some((_, mark_text)) = marks.get(i) {
                final_parts.push(mark_text.clone());
                raw_parts.push(mark_text.clone());
            }
        }

        let text = final_parts.join(" ").trim().to_string();
        let raw_text = raw_parts.join(" ").trim().to_string();
        if text.is_empty() {
            (self.sink)(PipelineEvent::Empty { reason: "No speech detected".into() });
            self.emit_idle_if_quiescent();
            return;
        }

        let injection = if settings.paste.inject {
            Some(platform::imp::inject_text(
                &text,
                settings.paste.prefer_ax_insert,
                settings.paste.restore_delay_ms,
                settings.paste.copy_to_clipboard,
                settings.paste.restore_clipboard,
            ))
        } else if settings.paste.copy_to_clipboard {
            platform::imp::write_clipboard(&text);
            None
        } else {
            None
        };

        let (app_id, app_name) = self.start_app.lock().clone();
        let detected_language = if langs.is_empty() { None } else { Some(langs.join("+")) };
        let tr = TranscriptionResult {
            raw_text,
            text: text.clone(),
            language: detected_language,
            model_id: model_id.clone(),
            duration_ms: recording.duration_ms,
            transcribe_ms,
            segments: all_segments,
            trimmed: vec![], // per-piece offsets don't map onto the joined raw; deferred
            low_confidence: vec![],
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
            transcribe_ms,
            model_id,
            injection,
            low_confidence_count: 0,
        });
        self.emit_idle_if_quiescent();
    }
}

fn collect_low_confidence(asr: &echokey_asr::AsrOutput, final_text: &str) -> Vec<LowConfidenceSpan> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for seg in &asr.segments {
        for (word, conf) in &seg.words {
            if *conf < 0.55 && word.len() > 1 {
                if let Some(pos) = find_word_boundary(&final_text[cursor..], word) {
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

/// Whole-word find: "an" must not match inside "and".
fn find_word_boundary(haystack: &str, word: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(word) {
        let start = from + pos;
        let end = start + word.len();
        let before_ok = start == 0
            || !haystack[..start].chars().next_back().map(|c| c.is_alphanumeric()).unwrap_or(false);
        let after_ok = end >= haystack.len()
            || !haystack[end..].chars().next().map(|c| c.is_alphanumeric()).unwrap_or(false);
        if before_ok && after_ok {
            return Some(start);
        }
        from = end;
    }
    None
}

fn now_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
