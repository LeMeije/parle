//! The dictation pipeline controller: owns the ASR engine manager, drives the
//! recording state machine, and on stop runs cleanup -> dictionary -> inject +
//! clipboard + history, emitting events to the UI throughout.
//!
//! The recorded audio is never discarded until a transcription succeeds or the
//! user cancels. On total ASR failure the WAV is written for recovery.

use crate::platform;
use parle_asr::manager::EngineManager;
use parle_asr::{is_silence, TranscribeOptions};
use parle_audio::recorder::{LevelUpdate, Recorder};
use parle_core::dictionary::Dictionary;
use parle_core::formatter;
use parle_core::history::Store;
use parle_core::settings::{data_dir, Settings};
use parle_core::types::{LowConfidenceSpan, Segment, TranscriptionResult};
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
        /// The row did NOT go into a syncing history, and the user needs to
        /// know that rather than being told "Copied".
        ///
        /// An explicit field rather than an `item_id < 0` convention, because
        /// every consumer that forgot to check the convention rendered the
        /// transcript instead. Round 11 emitted a notice for this case and the
        /// very next statement emitted `Completed`, whose handlers overwrote
        /// the notice with the first 42 characters of the password.
        withheld: bool,
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

/// What the accessibility tree says about the field the user is typing into.
///
/// THREE answers, and the third is the important one.
///
/// `Some(true)` is a password field. `Some(false)` is an ordinary one.
/// `None` means the probe could not tell, and that is not rare: Chromium and
/// Electron applications do not expose their accessibility tree until a client
/// sets `AXManualAccessibility`, so a web password field answers nothing even
/// with permission granted, and a `sudo` prompt in Terminal has no secure text
/// field at all.
///
/// Both previous rounds collapsed this into a boolean and each picked a
/// different wrong side. Round 9 treated the process-global secure-input flag
/// as the answer and discarded every dictation on a machine with a password
/// manager running. Round 10 mapped "cannot tell" to "not secure" and stored a
/// web password field's contents in a history that replicates.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldSecrecy {
    Secure,
    Ordinary,
    Unknown {
        /// Whether SOME application had secure input raised at the moment we
        /// looked. Carried rather than re-read, so every decision in one
        /// dictation is made from one observation.
        secure_input: bool,
    },
}

/// Sampled ONCE, before anything is injected.
///
/// It has to be taken before the paste, because `inject_text` pastes, waits,
/// and then posts Return when "press enter after paste" is on, and submitting a
/// login is precisely what moves focus off the password field. Asking
/// afterwards got an ordinary element back and stored the password. The
/// frontmost app is already latched at recording start for the same reason.
fn sample_field_secrecy() -> FieldSecrecy {
    match platform::imp::focused_field_is_secure() {
        Some(true) => FieldSecrecy::Secure,
        Some(false) => FieldSecrecy::Ordinary,
        // The global flag is a hint here and nothing more: it says some
        // application has raised secure input, which is evidence that a
        // password is somewhere on screen, but not that it is under THIS
        // caret. Combined with a probe that could not answer, it is enough to
        // keep the row off the wire and not enough to throw it away.
        // The flag is SAMPLED HERE, once, and carried.
        //
        // It used to be read live inside `keep_local_only` and
        // `conceal_clipboard`, which made those predicates impure: any
        // application may lower the flag at any moment, so two calls in the
        // same dictation could disagree. `store_transcription` called it to
        // decide where the row goes and the `Completed` literal called it again
        // to decide what the user is told, and a flag that dropped in between
        // stored the row local-only while reporting it as ordinary, so the
        // toast rendered the first 42 characters of a suspected password. That
        // is the defect round 12 added `withheld` to fix, reintroduced by the
        // fix itself.
        None => FieldSecrecy::Unknown { secure_input: platform::imp::secure_input_active() },
    }
}

impl FieldSecrecy {
    /// Never store it: this is a password field and we know it.
    fn drop_entirely(self) -> bool {
        self == FieldSecrecy::Secure
    }

    /// Keep it, but never let it leave this machine.
    fn keep_local_only(self) -> bool {
        matches!(self, FieldSecrecy::Unknown { secure_input: true })
    }

    /// Mark the clipboard write concealed.
    ///
    /// Wider than the other two, deliberately: marking an ordinary transcript
    /// concealed loses the user nothing they can see, while failing to mark a
    /// password tells every other clipboard manager on the machine to keep it.
    fn conceal_clipboard(self) -> bool {
        self == FieldSecrecy::Secure || self.keep_local_only()
    }

    /// The sampled answer, in the shape the platform layer needs.
    ///
    /// The platform used to re-probe both globals itself. One sample, one
    /// decision, carried the whole way down.
    fn view(self) -> platform::FieldView {
        platform::FieldView {
            is_secure: match self {
                FieldSecrecy::Secure => Some(true),
                FieldSecrecy::Ordinary => Some(false),
                FieldSecrecy::Unknown { .. } => None,
            },
            conceal: self.conceal_clipboard(),
        }
    }
}

/// Store a finished dictation according to what we could learn about the field.
///
/// One place, so the two dictation paths cannot drift: the secure-field drop
/// was added to one of them and missed on the other once already.
fn store_transcription(
    store: &std::sync::Arc<parking_lot::Mutex<parle_core::history::Store>>,
    tr: &parle_core::types::TranscriptionResult,
    app_id: &Option<String>,
    app_name: &Option<String>,
    secrecy: FieldSecrecy,
    // Did the text actually reach the clipboard on this path? The
    // empty-after-cleanup branch never injects and never copies, and it was
    // telling the user "Password field: copied, not saved to History". Half of
    // that sentence was false, and it is the half that reassures.
    copied: bool,
) -> (i64, Option<String>) {
    // The second half of the pair is what the USER is told.
    //
    // Both of these outcomes are surprising and both used to be a
    // `tracing::info!` and nothing else: the dictation is not in History and
    // the user has no way to know why, or that it happened at all. A silent
    // drop is the failure mode that made a stuck secure-input flag invisible
    // for a whole round.
    if secrecy.drop_entirely() {
        tracing::info!("dictation went to a password field; not storing it");
        return (
            -1,
            Some(if copied {
                // The user's previous clipboard is gone, and saying so is the
                // only warning they get: the text has to stay there to be
                // pasted, so it cannot be restored on a delay without taking
                // the dictation away before they use it.
                "Password field: replaced your clipboard, not saved to History".into()
            } else {
                "Password field: not saved to History".to_string()
            }),
        );
    }
    let g = store.lock();
    if secrecy.keep_local_only() {
        tracing::info!(
            "could not tell whether this went to a password field and secure input is on; \
             keeping it on this device only"
        );
        let id = g
            .insert_transcription_local_only(tr, app_id.as_deref(), app_name.as_deref())
            .unwrap_or(-1);
        // No notice when the write FAILED. The notice was returned on the same
        // statement that swallows the error into `-1`, so one dictation emitted
        // "Saved on this device only", then "Could not save that to History",
        // then Completed. The caller's `item_id < 0` arm owns the failure.
        if id < 0 {
            return (id, None);
        }
        return (id, Some("Saved on this device only: this may be a password field".into()));
    }
    (
        g.insert_transcription(tr, app_id.as_deref(), app_name.as_deref()).unwrap_or(-1),
        None,
    )
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
            .name("parle-partials".into())
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
    ) -> Result<(parle_asr::AsrOutput, String), parle_asr::AsrError> {
        let mut segments = Vec::new();
        let mut langs: Vec<String> = Vec::new();
        let mut transcribe_ms = 0u64;
        let mut model_id = String::new();
        for r in chunks {
            let offset_ms = (r.start as u64 * 1000) / parle_audio::ASR_SAMPLE_RATE as u64;
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
            parle_asr::AsrOutput { segments, detected_language, transcribe_ms },
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
        let on_partial: parle_asr::PartialCallback = Box::new(move |t: &str| {
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
                parle_asr::split_on_speech(samples, parle_audio::ASR_SAMPLE_RATE)
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
                let _ = parle_audio::wav::write_wav_16k_mono(&path, &recording.samples);
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

        // Sampled ABOVE the empty-after-cleanup branch, not below it.
        //
        // Round 11's commit message says there are two dictation paths and one
        // gate. There are three. The branch below stores the RAW transcript
        // when cleanup empties the cleaned one, and it ran before
        // `sample_field_secrecy()` was ever called, with no secrecy argument
        // and no local-only flag. A dictation into a password field whose
        // cleaned text collapses to nothing was therefore stored in full and
        // replicated to every paired device, on the one branch whose own
        // comment says "Nothing is ever lost".
        //
        // The sample is still taken before any injection, which is the property
        // that matters: nothing below this line has moved focus.
        let secrecy = sample_field_secrecy();

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
                let (_, notice) =
                    store_transcription(&self.store, &tr, &app_id, &app_name, secrecy, false);
                if let Some(reason) = notice {
                    (self.sink)(PipelineEvent::Empty { reason });
                    self.emit_idle_if_quiescent();
                    return;
                }
            }
            (self.sink)(PipelineEvent::Empty { reason: if raw_text.is_empty() {
                    "Nothing to save".into()
                } else {
                    // Only claim to have kept something when we did.
                    "Nothing left after cleanup (kept raw in history)".to_string()
                } });
            self.emit_idle_if_quiescent();
            return;
        }

        // Low-confidence surfacing (word-level, threshold 0.55).
        let low_confidence = collect_low_confidence(&asr, &text);

        // Output: inject + clipboard + history, simultaneously in intent.
        // `secrecy` was sampled above, before the empty-after-cleanup branch and
        // before any injection.
        // Tracks the CLIPBOARD, which is what the message is about.
        //
        // `copied` was derived from `injection.is_some()`, which is false on
        // the one branch that copies without injecting: with "insert at cursor"
        // off, a password-field dictation reported "not saved to History" while
        // the clipboard had silently been replaced, with no snapshot and no
        // restore on that path.
        let mut copied_to_clipboard = false;
        let injection = if settings.paste.inject && !frontmost_is_self() {
            Some(platform::imp::inject_text(
                &text,
                settings.paste.prefer_ax_insert,
                settings.paste.restore_delay_ms,
                settings.paste.copy_to_clipboard,
                settings.paste.restore_clipboard,
                settings.paste.press_enter,
                secrecy.view(),
            ))
        } else if settings.paste.copy_to_clipboard {
            // CONCEALED when this is a password field. The unmarked call here
            // meant that with "insert at cursor" switched off, a dictation into
            // a secure field was put on the clipboard with no marking at all,
            // so every other clipboard manager on the machine kept it. The
            // injection path had always marked it; this branch never did.
            platform::imp::write_clipboard_marked(&text, secrecy.conceal_clipboard());
            copied_to_clipboard = true;
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
        // A dictation into a SECURE FIELD is not kept.
        //
        // `inject_text` reports `manual_paste_required` when macOS secure event
        // input is on, which is what a password field turns on. On that path the
        // platform layer deliberately marks the clipboard write CONCEALED so no
        // other clipboard manager keeps it, and then this line stored it anyway
        // and replication carried it to every paired device. Parle was telling
        // every other app on the machine not to hold the user's banking
        // password while holding it itself and posting it to their work PC.
        //
        // The row is dropped rather than flagged: there is no view in which a
        // password typed into a password field belongs in a history that syncs.
        // The text is still on the clipboard for the user to paste, which is
        // the whole point of that branch.
        // The injecting path writes the clipboard too: a ClipboardOnly outcome
        // IS the clipboard write, and `copy_to_clipboard` asks for one
        // alongside the insert.
        if matches!(
            injection.as_ref().map(|o| &o.method),
            Some(platform::InjectionMethod::ClipboardPaste)
                | Some(platform::InjectionMethod::ClipboardOnly)
        ) || (injection.is_some() && settings.paste.copy_to_clipboard)
        {
            copied_to_clipboard = true;
        }
        let (item_id, notice) =
            store_transcription(&self.store, &tr, &app_id, &app_name, secrecy, copied_to_clipboard);
        if let Some(reason) = notice {
            (self.sink)(PipelineEvent::Empty { reason });
        }

        // A FAILED WRITE is an error, not a withholding.
        //
        // `withheld` was `item_id < 0 || ...`, which folded the two together.
        // Both handlers return early on `withheld`, so a store that failed
        // (locked, disk full, key gone) produced no toast, no HUD line and no
        // notice: the text had been injected and nothing anywhere said the
        // history write had not happened.
        if item_id < 0 && !secrecy.drop_entirely() {
            (self.sink)(PipelineEvent::Error {
                // The sentence is chosen from what actually happened. It said
                // "The text was still inserted" unconditionally, and with
                // injection off, copy off, or Parle's own window frontmost,
                // nothing was: the dictation was lost outright and the one
                // message told the user it was in their document.
                message: if injection.is_some() {
                    "Could not save that to History. The text was still inserted.".into()
                } else {
                    "Could not save that to History, and it was not inserted anywhere. \
                     That dictation is lost."
                        .to_string()
                },
            });
        }
        (self.sink)(PipelineEvent::Completed {
            item_id,
            withheld: secrecy.drop_entirely() || secrecy.keep_local_only(),
            // BLANKED at the source. `withheld` protected the render and not
            // the transmission, so the password still reached every webview
            // heap and the only thing between it and a screen was five React
            // files each remembering a convention. That mechanism has failed in
            // both rounds it has existed. Nothing on the frontend needs the
            // text of a row it must not show.
            text: if secrecy.drop_entirely() || secrecy.keep_local_only() {
                String::new()
            } else {
                text
            },
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
        recording: parle_audio::recorder::Recording,
        mut marks: Vec<(u64, String)>,
        opts: &TranscribeOptions,
        settings: &Settings,
        dict: &Dictionary,
    ) {
        use crate::pipeline::PipelineState;
        marks.sort_by_key(|(ms, _)| *ms);
        let sr = parle_audio::ASR_SAMPLE_RATE as u64;
        let total = recording.samples.len();

        // Piece boundaries in samples.
        let mut boundaries: Vec<usize> = vec![0];
        for (ms, _) in &marks {
            boundaries.push(((ms * sr / 1000) as usize).min(total));
        }
        boundaries.push(total);

        // (text, starts_lowercase_in_raw) for speech, or a verbatim mark.
        enum Part {
            Speech { text: String, raw_lower_start: bool },
            Mark(String),
        }
        let mut parts: Vec<Part> = Vec::new();
        let mut raw_parts: Vec<String> = Vec::new();
        let mut all_segments: Vec<Segment> = Vec::new();
        let mut langs: Vec<String> = Vec::new();
        let mut transcribe_ms = 0u64;
        let mut model_id = String::new();

        for i in 0..boundaries.len() - 1 {
            let range = boundaries[i]..boundaries[i + 1];
            let is_last_piece = i == boundaries.len() - 2;
            if range.len() > (sr as usize / 4) && !is_silence(&recording.samples[range.clone()]) {
                let piece = &recording.samples[range.clone()];
                let chunks = if settings.language.language == "auto" {
                    parle_asr::split_on_speech(piece, parle_audio::ASR_SAMPLE_RATE)
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
                        // Pieces that run into a mark must not be closed with an
                        // artificial full stop: the pasted link continues the sentence.
                        let mut piece_cfg = settings.cleanup.clone();
                        if !is_last_piece {
                            piece_cfg.ensure_terminal_punctuation = false;
                        }
                        let formatted = formatter::format(&raw, &segs, &piece_cfg, &settings.language.locale);
                        let (text, _) = if settings.dictionary.enabled {
                            dict.apply(&formatted.text, settings.dictionary.fuzzy_correct)
                        } else {
                            (formatted.text.clone(), vec![])
                        };
                        all_segments.extend(segs);
                        if !text.trim().is_empty() {
                            let raw_lower_start = raw
                                .chars()
                                .find(|c| c.is_alphabetic())
                                .map(|c| c.is_lowercase())
                                .unwrap_or(false);
                            parts.push(Part::Speech { text: text.trim().to_string(), raw_lower_start });
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
                parts.push(Part::Mark(mark_text.clone()));
                raw_parts.push(mark_text.clone());
            }
        }

        let text = join_marked_parts(
            parts
                .iter()
                .map(|p| match p {
                    Part::Speech { text, raw_lower_start } => JoinPart::Speech {
                        text: text.clone(),
                        raw_lower_start: *raw_lower_start,
                    },
                    Part::Mark(m) => JoinPart::Mark(m.clone()),
                })
                .collect(),
        );
        let raw_text = raw_parts.join(" ").trim().to_string();
        if text.is_empty() {
            (self.sink)(PipelineEvent::Empty { reason: "No speech detected".into() });
            self.emit_idle_if_quiescent();
            return;
        }

        // Sampled BEFORE `inject_text`, which pastes and may post Return.
        let secrecy = sample_field_secrecy();
        // Tracks the CLIPBOARD, which is what the message is about.
        //
        // `copied` was derived from `injection.is_some()`, which is false on
        // the one branch that copies without injecting: with "insert at cursor"
        // off, a password-field dictation reported "not saved to History" while
        // the clipboard had silently been replaced, with no snapshot and no
        // restore on that path.
        let mut copied_to_clipboard = false;
        let injection = if settings.paste.inject && !frontmost_is_self() {
            Some(platform::imp::inject_text(
                &text,
                settings.paste.prefer_ax_insert,
                settings.paste.restore_delay_ms,
                settings.paste.copy_to_clipboard,
                settings.paste.restore_clipboard,
                settings.paste.press_enter,
                secrecy.view(),
            ))
        } else if settings.paste.copy_to_clipboard {
            // CONCEALED when this is a password field. The unmarked call here
            // meant that with "insert at cursor" switched off, a dictation into
            // a secure field was put on the clipboard with no marking at all,
            // so every other clipboard manager on the machine kept it. The
            // injection path had always marked it; this branch never did.
            platform::imp::write_clipboard_marked(&text, secrecy.conceal_clipboard());
            copied_to_clipboard = true;
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
        // The SAME secure-field gate as the plain path. This one was missed,
        // so a dictation into a password field that happened to contain a mark
        // was stored and replicated while the other path correctly dropped it.
        // The injecting path writes the clipboard too: a ClipboardOnly outcome
        // IS the clipboard write, and `copy_to_clipboard` asks for one
        // alongside the insert.
        if matches!(
            injection.as_ref().map(|o| &o.method),
            Some(platform::InjectionMethod::ClipboardPaste)
                | Some(platform::InjectionMethod::ClipboardOnly)
        ) || (injection.is_some() && settings.paste.copy_to_clipboard)
        {
            copied_to_clipboard = true;
        }
        let (item_id, notice) =
            store_transcription(&self.store, &tr, &app_id, &app_name, secrecy, copied_to_clipboard);
        if let Some(reason) = notice {
            (self.sink)(PipelineEvent::Empty { reason });
        }

        if item_id < 0 && !secrecy.drop_entirely() {
            (self.sink)(PipelineEvent::Error {
                // The sentence is chosen from what actually happened. It said
                // "The text was still inserted" unconditionally, and with
                // injection off, copy off, or Parle's own window frontmost,
                // nothing was: the dictation was lost outright and the one
                // message told the user it was in their document.
                message: if injection.is_some() {
                    "Could not save that to History. The text was still inserted.".into()
                } else {
                    "Could not save that to History, and it was not inserted anywhere. \
                     That dictation is lost."
                        .to_string()
                },
            });
        }
        (self.sink)(PipelineEvent::Completed {
            item_id,
            withheld: secrecy.drop_entirely() || secrecy.keep_local_only(),
            // BLANKED at the source. `withheld` protected the render and not
            // the transmission, so the password still reached every webview
            // heap and the only thing between it and a screen was five React
            // files each remembering a convention. That mechanism has failed in
            // both rounds it has existed. Nothing on the frontend needs the
            // text of a row it must not show.
            text: if secrecy.drop_entirely() || secrecy.keep_local_only() {
                String::new()
            } else {
                text
            },
            duration_ms: recording.duration_ms,
            transcribe_ms,
            model_id,
            injection,
            low_confidence_count: 0,
        });
        self.emit_idle_if_quiescent();
    }
}

fn collect_low_confidence(asr: &parle_asr::AsrOutput, final_text: &str) -> Vec<LowConfidenceSpan> {
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

/// True when Parle itself is the frontmost app: pasting the result into our
/// own Compose window would be absurd — the result panel and clipboard cover it.
fn frontmost_is_self() -> bool {
    let (app_id, _) = platform::imp::frontmost_app();
    platform::is_self(app_id.as_deref())
}

/// A speech stretch or a verbatim inserted mark, in speaking order.
pub enum JoinPart {
    Speech { text: String, raw_lower_start: bool },
    Mark(String),
}

/// Sentence-continuity joining: a pause taken to paste a link must not break
/// the sentence. Before a mark, soft trailing punctuation (. , ; :) is
/// stripped (question/exclamation marks are kept — they're intentional).
/// After a mark, a continuation that the ASR heard starting lowercase gets its
/// artificial capitalisation undone (except the pronoun "I").
pub fn join_marked_parts(parts: Vec<JoinPart>) -> String {
    let mut out = String::new();
    let mut prev_was_mark = false;
    for part in parts {
        match part {
            JoinPart::Mark(m) => {
                while out.ends_with(' ') {
                    out.pop();
                }
                while out.ends_with(['.', ',', ';', ':']) {
                    out.pop();
                }
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&m);
                prev_was_mark = true;
            }
            JoinPart::Speech { mut text, raw_lower_start } => {
                if prev_was_mark && raw_lower_start && !out.ends_with(['.', '!', '?']) {
                    let keep_capital = text == "I" || text.starts_with("I ") || text.starts_with("I'");
                    if !keep_capital {
                        let mut cs = text.chars();
                        if let Some(f) = cs.next() {
                            text = f.to_lowercase().collect::<String>() + cs.as_str();
                        }
                    }
                }
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&text);
                prev_was_mark = false;
            }
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod join_tests {
    use super::{join_marked_parts, JoinPart};

    fn speech(t: &str, lower: bool) -> JoinPart {
        JoinPart::Speech { text: t.to_string(), raw_lower_start: lower }
    }

    #[test]
    fn link_flows_inside_sentence() {
        let out = join_marked_parts(vec![
            speech("You can check this link.", false),
            JoinPart::Mark("https://example.com".into()),
            speech("And it explains everything.", true),
        ]);
        assert_eq!(out, "You can check this link https://example.com and it explains everything.");
    }

    #[test]
    fn question_before_mark_is_kept() {
        let out = join_marked_parts(vec![
            speech("Have you seen this?", false),
            JoinPart::Mark("https://example.com".into()),
        ]);
        assert_eq!(out, "Have you seen this? https://example.com");
    }

    #[test]
    fn pronoun_i_stays_capital() {
        let out = join_marked_parts(vec![
            speech("Check this out.", false),
            JoinPart::Mark("https://example.com".into()),
            speech("I think it's great.", true),
        ]);
        assert_eq!(out, "Check this out https://example.com I think it's great.");
    }

    #[test]
    fn genuine_new_sentence_after_mark_keeps_capital() {
        // ASR heard a capital start (real sentence boundary): keep it.
        let out = join_marked_parts(vec![
            speech("Here's the report.", false),
            JoinPart::Mark("https://example.com".into()),
            speech("Next topic is the budget.", false),
        ]);
        assert_eq!(out, "Here's the report https://example.com Next topic is the budget.");
    }

    #[test]
    fn mark_only() {
        let out = join_marked_parts(vec![JoinPart::Mark("https://example.com".into())]);
        assert_eq!(out, "https://example.com");
    }
}

fn now_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
