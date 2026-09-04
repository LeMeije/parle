//! Round 15, dictation flow: what happens when the SECOND dictation starts
//! while the first is still transcribing.
//!
//! The reported symptom was that the first recording looked discarded: stop a
//! long take, watch the overlay say "Transcribing", start talking again, and
//! the overlay flips back to a waveform with no sign the first one survived.
//!
//! The audio always survived. `stop_and_process` took the recorder out of its
//! slot on its first line and held the samples in a local, and one worker
//! thread drained the job channel in order. What did NOT survive was everything
//! ELSE that belonged to that take, because it stayed behind in slots on the
//! shared `Pipeline` that the next `start` overwrote:
//!
//!   1. `start` calls `self.marks.lock().clear()`, and the queued job did not
//!      read the marks until it ran. Start the next dictation first and the
//!      pasted links spliced into the previous one were gone.
//!   2. `start` overwrites `self.start_app`, and the job read it AFTER
//!      transcribing. The first take's history row was filed under the app the
//!      second take was started from.
//!   3. Worst: the recorder slot was only emptied when the WORKER reached the
//!      job. Press stop while the worker is busy (a queued transcription, or a
//!      model prewarm on the same channel) and the slot is still full, so
//!      `start` returns "already recording" without starting anything and the
//!      microphone keeps appending to the FIRST buffer. Two dictations get
//!      transcribed as one blob and the second stop finds an empty slot and
//!      silently does nothing.
//!
//! The fix moves the whole handover to the stop keypress: `take_pending` empties
//! the slot, ends capture, and carries the marks and the latched app out with
//! the audio as one `PendingDictation`. The queue then holds self-contained
//! jobs instead of instructions to go and read shared state later.
//!
//! These are surface guards over flow that needs a real microphone to exercise,
//! so every one of them asserts its anchor exists before it asserts anything
//! about it. A guard that can find nothing must first assert that it found
//! something.

#![cfg(test)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

fn read_src(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The file with `//` line comments stripped, so the prose above cannot satisfy
/// a guard that is looking for code, and with every run of whitespace removed,
/// so a rustfmt line break inside an expression cannot make an anchor miss.
fn squashed(rel: &str) -> String {
    let code: String = read_src(rel)
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

fn anchor(hay: &str, needle: &str, what: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("ANCHOR MISSING in {what}: {needle:?}"))
}

/// The stop keypress detaches the recording; the worker only transcribes it.
///
/// This is the guard for symptom 3. While the worker owned the `take`, the
/// window between "user pressed stop" and "slot is free" was as long as
/// whatever else was queued, and everything spoken in that window went into the
/// previous recording.
#[test]
fn r15_queue_a_the_recording_is_detached_on_the_stop_path_not_on_the_worker() {
    let s = squashed("src-tauri/src/state.rs");
    let p = squashed("src-tauri/src/pipeline.rs");

    // ANCHOR: the job carries a payload rather than being an instruction to go
    // and find whatever is recording when it eventually runs.
    anchor(&s, "enumWork{StopAndProcess(PendingDictation),", "state.rs Work");
    anchor(
        &s,
        "Work::StopAndProcess(pending)=>pipeline.process(pending),",
        "state.rs worker loop",
    );

    // THE CLAIM. `pipeline_stop` takes the recording itself, and only queues
    // when there was one.
    anchor(
        &s,
        "letSome(pending)=self.pipeline.take_pending()else{return;};\
         ifletErr(crossbeam_channel::SendError(job))=\
         self.work_tx.send(Work::StopAndProcess(pending))",
        "state.rs pipeline_stop",
    );

    // The count is raised by `take_pending`, so a take the worker refuses has to
    // be handed back. Swallowed, it holds the HUD on "Transcribing" for the rest
    // of the session, promising a dictation that is already lost.
    anchor(&s, "self.pipeline.abandon_pending(pending);", "state.rs pipeline_stop");
    anchor(
        &p,
        "pubfnabandon_pending(&self,pending:PendingDictation){drop(pending);\
         self.pending.fetch_sub(1,Ordering::SeqCst);",
        "pipeline.rs abandon_pending",
    );

    // THE CLAIM. `take_pending` empties the slot AND ends capture, in that
    // order, before anything else can look at either.
    anchor(
        &p,
        "pubfntake_pending(&self)->Option<PendingDictation>{\
         letrecorder=self.recorder.lock().take()?;recorder.request_stop();",
        "pipeline.rs take_pending",
    );

    // And `request_stop` must genuinely be the non-blocking half: if it grew
    // the joins, `pipeline_stop` would be blocking the hotkey thread on them.
    let r = squashed("crates/parle-audio/src/recorder.rs");
    anchor(
        &r,
        "pubfnrequest_stop(&self){self.stop_flag.store(true,Ordering::SeqCst);}",
        "recorder.rs request_stop",
    );
}

/// Everything belonging to one take travels WITH it.
///
/// The guard for symptoms 1 and 2, stated as the property that actually
/// prevents them rather than as a list of the two slots that bit us: the
/// processing path does not reach back into the live-recording state at all.
#[test]
fn r15_queue_b_processing_never_reads_the_slots_the_next_recording_overwrites() {
    let p = squashed("src-tauri/src/pipeline.rs");

    // ANCHOR: marks and the latched app leave with the audio.
    anchor(
        &p,
        "letmarks=std::mem::take(&mut*self.marks.lock());\
         letstart_app=self.start_app.lock().clone();",
        "pipeline.rs take_pending",
    );
    anchor(&p, "fnprocess_inner(&self,pending:PendingDictation){", "pipeline.rs process_inner");
    anchor(&p, "fncollect_low_confidence(", "pipeline.rs end of impl");

    // THE CLAIM. Nothing from `process_inner` to the end of the impl (which is
    // the whole processing path, `process_with_marks` included) touches the
    // recorder, the marks or the latched app. Those three are the live
    // recording's state, and by the time this code runs the live recording may
    // be a DIFFERENT one.
    let start = anchor(&p, "fnprocess_inner(&self,pending:PendingDictation){", "pipeline.rs");
    let end = anchor(&p, "fncollect_low_confidence(", "pipeline.rs");
    assert!(start < end, "the processing path no longer sits where this guard looks");
    let body = &p[start..end];

    for slot in ["self.recorder", "self.marks", "self.start_app"] {
        assert!(
            !body.contains(slot),
            "{slot} is read on the processing path again. It belongs to whatever is \
             recording NOW, which is not necessarily the take being processed: that is \
             how the previous dictation lost its marks and got filed under the wrong app."
        );
    }
}

/// Idle waits for the LAST take, not the first one to finish.
///
/// Two dictations can be outstanding at once, and both of them end by asking
/// whether things are quiet. Asking only "is anything recording" put the HUD
/// back to Idle the moment the first finished, while the second was still
/// queued behind it on the worker.
#[test]
fn r15_queue_c_idle_is_gated_on_outstanding_takes_not_just_on_the_recorder() {
    let p = squashed("src-tauri/src/pipeline.rs");

    // THE CLAIM. Both conditions, and the count is one of them.
    anchor(
        &p,
        "fnemit_idle_if_quiescent(&self){\
         ifself.recorder.lock().is_none()&&self.pending.load(Ordering::SeqCst)==0{",
        "pipeline.rs emit_idle_if_quiescent",
    );

    // ANCHOR: the count is raised once, when a take is detached, and dropped on
    // both disposal paths (transcribed, or handed back). A gate on a counter
    // nothing moves is not a gate, and one that is only ever raised wedges the
    // HUD on "Transcribing" forever.
    assert_eq!(
        p.matches("self.pending.fetch_add(1,Ordering::SeqCst);").count(),
        1,
        "the pending count is raised somewhere other than `take_pending`"
    );
    assert_eq!(
        p.matches("self.pending.fetch_sub(1,Ordering::SeqCst);").count(),
        2,
        "a detached take has exactly two ends: `process` transcribes it, or \
         `abandon_pending` gives up on it. Both must drop the count."
    );

    // THE CLAIM, and the one that makes the gate mean anything: Idle is emitted
    // from ONE place in the whole file, so there is no path to Idle that skips
    // the check. It shipped with eight scattered emits, one per early return,
    // and each of them was a chance to answer "is everything quiet?" without
    // ever consulting what was still queued.
    assert_eq!(
        p.matches("PipelineEvent::StateChanged{state:PipelineState::Idle}").count(),
        1,
        "Idle is emitted somewhere that is not `emit_idle_if_quiescent`. Every path to \
         Idle has to go through the gate, cancel included."
    );
}

/// A partial-transcript loop cannot outlive its own recording.
///
/// The loops wake every two seconds and read the shared recorder slot. With the
/// slot now freed at the stop keypress, a loop belonging to the finished take
/// can wake to find the NEXT recording sitting there and start transcribing and
/// emitting partials for it, alongside that recording's own loop. Generation
/// numbers make the slot's identity checkable, not just its occupancy.
#[test]
fn r15_queue_d_a_partial_loop_belongs_to_one_recording() {
    let p = squashed("src-tauri/src/pipeline.rs");

    // ANCHOR: every start mints a generation and hands it to its loop.
    anchor(
        &p,
        "letgeneration=self.generation.fetch_add(1,Ordering::SeqCst)+1;",
        "pipeline.rs start",
    );
    anchor(&p, "self.spawn_partial_loop(generation);", "pipeline.rs start");
    anchor(&p, "fnspawn_partial_loop(self:&Arc<Self>,generation:u64){", "pipeline.rs");

    // THE CLAIM. The loop checks identity, not just occupancy, and does it both
    // before it takes a snapshot and after the transcription it was blocked in.
    assert_eq!(
        p.matches("this.generation.load(Ordering::SeqCst)!=generation").count(),
        2,
        "the partial loop no longer checks BOTH before snapshotting and after \
         transcribing. A pass can take seconds; the recording can be replaced inside one."
    );
    anchor(
        &p,
        "ifthis.recorder.lock().is_none()||this.generation.load(Ordering::SeqCst)!=generation{break;}",
        "pipeline.rs partial loop post-transcribe check",
    );
}

/// Cancelling drops the marks with the audio it was going to splice them into.
///
/// Only `start` cleared the marks, so a cancelled recording left its marks in
/// the slot and the NEXT recording spliced them in. Not part of the reported
/// symptom, but the same class: state belonging to one take outliving it.
#[test]
fn r15_queue_e_cancel_drops_the_marks_too() {
    let p = squashed("src-tauri/src/pipeline.rs");
    anchor(
        &p,
        "pubfncancel(&self){ifletSome(rec)=self.recorder.lock().take(){rec.cancel();\
         self.marks.lock().clear();}",
        "pipeline.rs cancel",
    );
}
