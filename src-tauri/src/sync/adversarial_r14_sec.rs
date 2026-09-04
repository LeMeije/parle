//! ADVERSARIAL REVIEW, ROUND 14 — secrets and UI.
//!
//! Target: commit `517bc3d` (round 13). Round 13 rewrote `FieldSecrecy`, the
//! `Completed` event that reports it, the Windows snapshot bracket and four
//! React files. Every finding below lives in code round 13 touched.
//!
//! Nothing here edits production code. Two kinds of test, and the difference is
//! stated rather than blurred, following the convention rounds 12 and 13
//! established:
//!
//!   * RUNTIME tests, which drive real production code.
//!   * SURFACE tests, which read a source file. The user-facing half of this
//!     product is React and `package.json` has no JS test runner;
//!     `FieldSecrecy`, `sample_field_secrecy` and `store_transcription` are
//!     private to `crate::pipeline` and cannot be called from here; and
//!     `platform::windows` does not compile on this machine at all.
//!
//! EVERY surface test asserts its ANCHOR first — that the code it reasons about
//! is present in the shape it expects — before asserting the property. A guard
//! that can find nothing must first assert that it found something. Five
//! guards have already shipped in this repo that could not fail.

#![cfg(test)]

use parle_core::history::Store;
use parle_core::types::TranscriptionResult;
use std::path::{Path, PathBuf};

use crate::pipeline::PipelineEvent;
use crate::platform::{InjectionMethod, InjectionOutcome};

const ME: &str = "11111111-1111-4111-8111-111111111111";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

fn read_src(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The file with `//` line comments stripped, so prose cannot satisfy a guard
/// that is looking for code.
fn code_of(rel: &str) -> String {
    read_src(rel)
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Comment-stripped code with every run of whitespace removed, so a rustfmt or
/// prettier line break inside an expression cannot make an anchor miss.
fn squashed(rel: &str) -> String {
    code_of(rel).chars().filter(|c| !c.is_whitespace()).collect()
}

/// Assert an anchor exists and return its byte offset, so ordering claims can
/// never be made against a needle that is not there.
fn anchor(hay: &str, needle: &str, what: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("ANCHOR MISSING in {what}: {needle:?}"))
}

/// The English text behind an i18n key, read out of `src/i18n/en.ts`.
///
/// THE i18n MOVE. The React UI now runs on a translation layer: every
/// user-facing literal these surface tests used to grep for inside a `.tsx`
/// file lives in `src/i18n/en.ts` under a dot-namespaced key, and the component
/// holds a `t('key')` call. `en` is the fallback dictionary for every other
/// language (`src/i18n/index.ts`), so this file is where the sentence a user
/// actually reads is defined, and it is the honest place to assert on it.
///
/// A surface test that follows a string into the dictionary must anchor BOTH
/// halves: the `t()` call in the component (or the sentence is rendered
/// nowhere) and the value here (or the sentence has changed underneath it).
/// This panics rather than returning empty, so a guard still cannot find
/// nothing and pass.
fn en_string(key: &str) -> String {
    let src = read_src("src/i18n/en.ts");
    let needle = format!("'{key}':");
    let at = src
        .find(&needle)
        .unwrap_or_else(|| panic!("ANCHOR MISSING in src/i18n/en.ts: key {key:?}"));
    let rest = &src[at + needle.len()..];
    let start = rest
        .find(|c| c == '\'' || c == '"')
        .unwrap_or_else(|| panic!("i18n key {key:?} has no string value in src/i18n/en.ts"));
    let quote = rest[start..].chars().next().unwrap();
    let mut out = String::new();
    let mut esc = false;
    for ch in rest[start + quote.len_utf8()..].chars() {
        if esc {
            out.push(ch);
            esc = false;
        } else if ch == '\\' {
            esc = true;
        } else if ch == quote {
            return out;
        } else {
            out.push(ch);
        }
    }
    panic!("i18n key {key:?}: unterminated string literal in src/i18n/en.ts")
}

fn tr(text: &str) -> TranscriptionResult {
    TranscriptionResult {
        raw_text: text.to_string(),
        text: text.to_string(),
        language: Some("en".into()),
        model_id: "whisper-small".into(),
        duration_ms: 1200,
        transcribe_ms: 300,
        segments: Vec::new(),
        trimmed: Vec::new(),
        low_confidence: Vec::new(),
        cleanup_tier: 1,
    }
}

fn completed(text: &str, withheld: bool, manual_paste: Option<bool>) -> PipelineEvent {
    PipelineEvent::Completed {
        item_id: if withheld { -1 } else { 7 },
        withheld,
        text: text.to_string(),
        duration_ms: 1200,
        transcribe_ms: 300,
        model_id: "whisper-small".into(),
        injection: manual_paste.map(|m| InjectionOutcome {
            method: if m { InjectionMethod::ClipboardOnly } else { InjectionMethod::AxInsert },
            manual_paste_required: m,
        }),
        low_confidence_count: 0,
    }
}

// ===========================================================================
// R14-A. `withheld` guards the RENDER. The password is still broadcast.
// ===========================================================================

/// FINDING R14-A1 (HIGH). The transcript of a withheld dictation is serialised
/// into the `pipeline-event` payload and broadcast to every webview.
///
/// `state.rs` emits with `app_for_sink.emit(name, &event)`, which is the
/// broadcast form: it goes to the main window, the HUD window and any window
/// that exists later. Round 13's own commit message names this as the reason
/// `withheld` had to exist ("the event is broadcast to every window, so a
/// password-field dictation aimed at a browser landed there"), and the fix it
/// chose was to guard the five consumers rather than to stop sending the text.
///
/// So the secret still crosses the IPC boundary into every webview's heap on
/// every password-field dictation, and the only thing standing between it and
/// the screen is that five independent React files each remember a convention.
/// Round 12 added the field for exactly that reason and patched two of four
/// consumers; round 13 found the other two still rendering it in full with a
/// Copy button. The mechanism has already failed twice in two rounds.
///
/// RUNTIME. The payload is built from the real `PipelineEvent` and the real
/// derive, and the assertion is that the password is in the bytes.
#[test]
fn r14_sec_a1_a_withheld_completed_event_still_carries_the_transcript_on_the_wire() {
    const SECRET: &str = "correct-horse-battery-staple-9021";

    let payload = serde_json::to_string(&completed(SECRET, true, Some(true)))
        .expect("PipelineEvent serialises");

    // CONTROL 1: this really is the completed event, in the shape the frontend
    // switches on. Without this the test could be measuring anything.
    assert!(
        payload.contains(r#""kind":"completed""#),
        "ANCHOR MISSING: the completed event no longer serialises as kind=completed: {payload}"
    );
    // CONTROL 2: the field the whole mechanism turns on is present and true, so
    // this payload is the withheld case and not an ordinary one.
    assert!(
        payload.contains(r#""withheld":true"#),
        "ANCHOR MISSING: `withheld` is not on the wire at all, so no consumer could honour it"
    );
    // CONTROL 3: the serialiser demonstrably CAN carry text, so a failure of
    // the claim below would mean something, rather than the derive having
    // dropped every string field.
    let ordinary = serde_json::to_string(&completed("an ordinary dictation", false, None))
        .expect("PipelineEvent serialises");
    assert!(
        ordinary.contains("an ordinary dictation"),
        "ANCHOR MISSING: text is not serialised even for an ordinary dictation; this test is \
         measuring the wrong thing"
    );

    // CONTROL 4 (RUNTIME, and it is the load-bearing one): the event type has
    // no protection of its own. Whatever string the pipeline puts in `text`
    // goes out on the wire verbatim, withheld or not.
    assert!(
        payload.contains(SECRET),
        "PipelineEvent no longer serialises `text` for a withheld event; the finding below \
         would already be fixed at the type level: {payload}"
    );

    // THE CLAIM: the pipeline must not put the transcript in the event at all
    // when it has decided to withhold it. Both emitters pass `text` straight
    // through next to the `withheld` flag they just computed.
    let p = squashed("src-tauri/src/pipeline.rs");
    assert_eq!(
        p.matches("withheld:secrecy.drop_entirely()||secrecy.keep_local_only(),text,").count(),
        0,
        "both `Completed` emitters compute `withheld` and then hand the transcript over anyway, \
         and `state.rs` broadcasts it to every webview with `app.emit`. `withheld` protects the \
         RENDER, not the transmission, so a password-field dictation lands in the heap of the \
         main window, the HUD window and every window added later, and the only thing between \
         it and the screen is that five independent React files each remember a convention. \
         Round 12 added the flag because \"every consumer that had to remember a convention \
         forgot it\" and patched two of four; round 13 found the other two rendering it in \
         full with a Copy button. The mechanism has failed in each of the two rounds it has \
         existed. Smallest fix at both sites: `text: if withheld {{ String::new() }} else {{ text }}`. \
         Nothing on the frontend uses `text` for a withheld row."
    );
}

/// FINDING R14-A2 (MEDIUM). Round 13 made Compose and Onboarding report a
/// withheld dictation as "No speech detected", and Onboarding invites the user
/// to say their password again, louder.
///
/// Round 13 stopped both views rendering the transcript, which was right, and
/// mapped the withheld case onto the empty string:
///
/// ```ignore
/// // Compose.tsx
/// if (e.kind === 'completed' && e.withheld) setResult('');
/// // Onboarding.tsx
/// setResult(e.withheld ? '' : e.text);
/// ```
///
/// `''` is already taken. Both views render `{result || '<nothing was heard>'}`,
/// so the empty string is the value that means the microphone picked nothing up.
/// A password-field dictation that was heard, transcribed and then deliberately
/// dropped is therefore reported to the user as a failure of the microphone.
/// In Onboarding the exact words are "No speech detected. Try again a little
/// louder.", which is an instruction to re-dictate the password.
///
/// The true reason exists: the pipeline emits `Empty { reason: "Password
/// field: ..." }` immediately before `Completed`. Neither view consumes
/// `e.reason` for anything except also blanking the result, so on the Compose
/// tab and in Onboarding the explanation never appears.
#[test]
fn r14_sec_a2_a_withheld_dictation_is_reported_as_a_microphone_failure() {
    let c = squashed("src/views/Compose.tsx");
    let o = squashed("src/views/Onboarding.tsx");

    // ANCHOR 1: the round-13 handlers, in the shape they were written.
    anchor(&c, "if(e.kind==='completed'&&e.withheld)setResult(", "Compose.tsx");
    anchor(&o, "setResult(e.withheld?", "Onboarding.tsx");
    // ANCHOR 2: the empty string is the "nothing was heard" sentinel in both.
    //
    // i18n MOVE. Both sentences left the views for `src/i18n/en.ts`
    // (`compose.noSpeech`, `onboarding.test.noSpeech`), so the anchor is the
    // `t()` call in the `result || …` position PLUS the English value. Both
    // halves matter here: the call proves the empty string still falls through
    // to the microphone-failure message, and the value proves that message is
    // still the one that blames the microphone — which is the entire premise
    // of this finding, and the reason `''` must not be the withheld value.
    anchor(&c, "{result||t('compose.noSpeech')}", "Compose.tsx");
    anchor(&o, "{result||t('onboarding.test.noSpeech')}", "Onboarding.tsx");
    assert_eq!(
        en_string("compose.noSpeech"),
        "No speech detected.",
        "ANCHOR MISSING: Compose no longer reports the empty string as a microphone failure"
    );
    assert_eq!(
        en_string("onboarding.test.noSpeech"),
        "No speech detected. Try again a little louder.",
        "ANCHOR MISSING: Onboarding no longer tells the user to try again a little louder"
    );
    // ANCHOR 3: `result === null` is the free value that means "no dictation
    // yet", so a third state was available and was not used.
    anchor(&c, "{result!==null&&(", "Compose.tsx");
    anchor(&o, "{result!==null&&(", "Onboarding.tsx");
    // CONTROL: the pipeline really does have the true reason to hand, on the
    // event immediately before this one.
    assert!(
        read_src("src-tauri/src/pipeline.rs")
            .contains("Password field: not saved to History"),
        "ANCHOR MISSING: the pipeline no longer emits a reason for a dropped dictation"
    );

    // THE CLAIM: withheld must not collapse onto the microphone-failure value.
    //
    // The i18n layer opened a second door onto the same defect: a withheld
    // dictation can now be pointed straight at the microphone-failure KEY
    // without ever mentioning the empty string, so both forms are refused.
    let mut offenders: Vec<&str> = vec![];
    if c.contains("e.withheld)setResult('')") || c.contains("e.withheld)setResult(t('compose.noSpeech')") {
        offenders.push("src/views/Compose.tsx");
    }
    if o.contains("setResult(e.withheld?'':")
        || o.contains("setResult(e.withheld?t('onboarding.test.noSpeech'):")
    {
        offenders.push("src/views/Onboarding.tsx");
    }
    assert!(
        offenders.is_empty(),
        "{offenders:?} map a WITHHELD dictation onto `''`, which both files render as \
         \"No speech detected\". Parle heard the user perfectly and threw the transcript away \
         on purpose, and reports it as a microphone failure; Onboarding then tells them to \
         \"Try again a little louder\", which is an instruction to re-dictate a password. \
         Round 13 introduced both lines while fixing the render leak and did not check what \
         the empty string already meant in these two files. Smallest fix: keep a separate \
         withheld state (or reuse `null`) and show the reason the pipeline already sent on \
         the preceding `Empty` event."
    );
}

// ===========================================================================
// R14-B. `copied` is the injection flag, and the copy-only branch copies.
// ===========================================================================

/// FINDING R14-B1 (HIGH). With "insert at cursor" switched off, a dictation
/// into a password field replaces the user's clipboard and the message says
/// nothing about it.
///
/// Round 13 added the `copied` parameter to `store_transcription` precisely so
/// that a message would stop being half false ("Half of that sentence was
/// false, and it is the half that reassures"), and then derived it from
/// `injection.is_some()`. `injection` is `None` on the branch that copies
/// without injecting:
///
/// ```ignore
/// let injection = if settings.paste.inject && !frontmost_is_self() {
///     Some(platform::imp::inject_text(...))
/// } else if settings.paste.copy_to_clipboard {
///     platform::imp::write_clipboard_marked(&text, secrecy.conceal_clipboard());
///     None                       // <- the clipboard was just replaced
/// } else { None };
/// ...
/// store_transcription(..., secrecy, injection.is_some());   // <- false
/// ```
///
/// So on the one branch reachable with `inject` off, the user is told
/// "Password field: not saved to History" and their previous clipboard is gone
/// with no restore, no snapshot and no word. The comment eight lines above that
/// write states the configuration is real and supported: "with 'insert at
/// cursor' switched off, a dictation into a secure field was put on the
/// clipboard".
///
/// The same branch is worse for `keep_local_only`: there the notice is "Saved
/// on this device only: this may be a password field", which mentions the
/// clipboard on no branch at all.
#[test]
fn r14_sec_b1_the_copy_only_branch_replaces_the_clipboard_and_reports_copied_false() {
    let p = squashed("src-tauri/src/pipeline.rs");

    // ANCHOR 1: the branch exists, writes the clipboard, and yields `None`.
    let branch = "}elseifsettings.paste.copy_to_clipboard{platform::imp::write_clipboard_marked\
                  (&text,secrecy.conceal_clipboard());copied_to_clipboard=true;None}else{None};";
    assert_eq!(
        p.matches(branch).count(),
        2,
        "ANCHOR MISSING: the copy-only branch is not the shape this test reasons about. It \
         must write the clipboard and evaluate to None, on both dictation paths."
    );
    // ANCHOR 2: `copied` is the second half of the store call.
    //
    // `app_id`/`app_name` lost their `&` when the latched app stopped being read
    // back off `Pipeline::start_app` (which the NEXT dictation overwrites) and
    // started travelling with the take as borrows. Only the spelling of those
    // two arguments moved; the claim below, that `copied` is the argument after
    // `secrecy` and is about the clipboard, is untouched.
    assert!(
        p.matches("store_transcription(&self.store,&tr,app_id,app_name,secrecy,").count() >= 2,
        "ANCHOR MISSING: the store call no longer takes a `copied` argument in this position"
    );
    // ANCHOR 3: `copied` is what selects the clipboard sentence.
    anchor(&p, "return(-1,Some(ifcopied{", "pipeline.rs store_transcription");
    assert!(
        code_of("src-tauri/src/pipeline.rs")
            .contains("Password field: replaced your clipboard, not saved to History"),
        "ANCHOR MISSING: the clipboard warning literal is gone"
    );

    // THE CLAIM. `copied` must be a statement about the CLIPBOARD, not about
    // injection: the two differ on exactly the branch above.
    assert!(
        !p.contains("secrecy,injection.is_some());"),
        "`copied` is `injection.is_some()`, which is FALSE on the `else if \
         settings.paste.copy_to_clipboard` branch even though that branch has just called \
         write_clipboard_marked and destroyed the user's clipboard. With `paste.inject` off, \
         a dictation into a password field therefore reports \"Password field: not saved to \
         History\" and never mentions that the clipboard is gone, and there is no snapshot \
         and no restore on that path to get it back. Smallest fix: track the clipboard write \
         itself, e.g. `let mut copied = false;` set to true in the copy-only branch and to \
         `o.method != AxInsert || settings.paste.copy_to_clipboard` from the injection \
         outcome, and pass that."
    );
}

// ===========================================================================
// R14-C. The new Error promises an insertion that need not have happened.
// ===========================================================================

/// FINDING R14-C1 (MEDIUM-HIGH). "The text was still inserted." is emitted on
/// paths where nothing was inserted and nothing was copied.
///
/// Round 13 split a failed write out of `withheld` and gave it an `Error`, which
/// was right. The message it chose asserts a fact the guard never checks:
///
/// ```ignore
/// if item_id < 0 && !secrecy.drop_entirely() {
///     (self.sink)(PipelineEvent::Error {
///         message: "Could not save that to History. The text was still inserted.".into(),
///     });
/// }
/// ```
///
/// `injection` appears nowhere in that condition. With "insert at cursor" off
/// and "also copy to clipboard" off — both are switches, and the third arm of
/// the `if` chain is a bare `else { None }` — a failed store leaves the
/// dictation nowhere: not in history, not on the clipboard, not in the field.
/// The one message the user gets tells them it was inserted, so they will not
/// go looking for it. `frontmost_is_self()` reaches the same arm: dictating with
/// Parle's own window in front never injects either.
#[test]
fn r14_sec_c1_the_failed_write_error_promises_an_insertion_that_may_not_exist() {
    let p = squashed("src-tauri/src/pipeline.rs");
    let raw = code_of("src-tauri/src/pipeline.rs");

    // ANCHOR 1: the guard is there, twice, in the shape round 13 gave it.
    assert_eq!(
        p.matches("ifitem_id<0&&!secrecy.drop_entirely()").count(),
        2,
        "ANCHOR MISSING: the failed-write guard is not the shape this test reasons about"
    );
    // ANCHOR 2: the literal that makes the promise.
    assert!(
        raw.contains("Could not save that to History. The text was still inserted."),
        "ANCHOR MISSING: the failed-write message has changed"
    );
    // ANCHOR 3: a path on which NOTHING reaches the user exists. Both dictation
    // paths end their injection chain with a bare `else { None }`.
    assert_eq!(
        p.matches("None}else{None};").count(),
        2,
        "ANCHOR MISSING: the injection chain no longer has an arm that neither injects nor \
         copies, so the premise of this finding would be gone"
    );

    // THE CLAIM: the guard must consult whether anything actually happened.
    let guard_at = anchor(&p, "ifitem_id<0&&!secrecy.drop_entirely()", "pipeline.rs");
    let guard = &p[guard_at..guard_at + 200];
    assert!(
        guard.contains("injection"),
        "the failed-write Error asserts \"The text was still inserted.\" without consulting \
         `injection`. With `paste.inject` off and `paste.copy_to_clipboard` off — or simply \
         with Parle's own window frontmost, which `frontmost_is_self()` routes to the same \
         arm — the injection chain evaluates to None, nothing was inserted and nothing was \
         copied. A store that then fails loses the dictation completely, and the only message \
         the user sees tells them it is in their document. Smallest fix: choose the sentence \
         from `injection`, e.g. \"The text was still inserted.\" when `injection.is_some()` \
         and \"The text is gone; please dictate it again.\" otherwise.\n\
         guard was: {guard}"
    );
}

// ===========================================================================
// R14-D. A failed local-only insert is announced as a successful save.
// ===========================================================================

/// FINDING R14-D1 (MEDIUM). One dictation, three contradictory messages, and
/// the first one is false.
///
/// `store_transcription` returns the local-only notice unconditionally, on the
/// same statement that swallows the insert error into `-1`:
///
/// ```ignore
/// let id = g.insert_transcription_local_only(...).unwrap_or(-1);
/// return (id, Some("Saved on this device only: this may be a password field".into()));
/// ```
///
/// When that insert fails the pipeline emits, in order: `Empty` carrying "Saved
/// on this device only" (nothing was saved), then round 13's new `Error`
/// carrying "Could not save that to History", then `Completed { withheld: true }`.
/// The HUD paints the first, then the second over the top of it. The user is
/// told the row was kept and then told it was not, in that order, for one
/// dictation. Round 13 introduced the second message and did not notice it
/// contradicts the first.
///
/// RUNTIME half: `-1` really is the error arm, so the two can co-occur.
#[test]
fn r14_sec_d1_a_failed_local_only_insert_is_announced_as_saved_and_then_as_failed() {
    // RUNTIME CONTROL: a successful local-only insert returns a positive id, so
    // `-1` can only ever mean the insert failed.
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(ME);
    let id = s
        .insert_transcription_local_only(&tr("could not tell what field this was"), None, None)
        .expect("a healthy store inserts");
    assert!(
        id > 0,
        "a healthy local-only insert returned {id}; `-1` would then not be the error sentinel \
         and this finding's premise would be wrong"
    );

    let p = squashed("src-tauri/src/pipeline.rs");
    // ANCHOR: the statement that swallows the error and the notice that follows
    // it are one expression, in that order.
    anchor(&p, "insert_transcription_local_only(tr,app_id.as_deref(),app_name.as_deref())", "pipeline.rs");
    anchor(&p, ".unwrap_or(-1);", "pipeline.rs store_transcription");
    assert!(
        read_src("src-tauri/src/pipeline.rs")
            .contains("Saved on this device only: this may be a password field"),
        "ANCHOR MISSING: the local-only notice literal is gone"
    );
    // ANCHOR: the Error that contradicts it exists.
    assert!(
        p.contains("ifitem_id<0&&!secrecy.drop_entirely()"),
        "ANCHOR MISSING: round 13's failed-write Error is gone, so nothing contradicts the \
         notice and this finding is stale"
    );

    // THE CLAIM: the notice must be conditioned on the insert having worked.
    let at = anchor(&p, "ifsecrecy.keep_local_only(){", "pipeline.rs");
    let arm = &p[at..at + 400];
    assert!(
        arm.contains("ifid<0") || arm.contains("ifid>0") || arm.contains("match"),
        "the local-only branch returns \"Saved on this device only: this may be a password \
         field\" on the same statement that maps a failed insert to -1, so a dictation whose \
         local-only write failed is announced as saved and then, one event later, as not \
         saved. Round 13 added the second message without reconciling it with the first. \
         Smallest fix: return `(id, Some(...))` only when `id > 0` and let the caller's \
         `item_id < 0` arm own the failure.\n\
         branch was: {arm}"
    );
}

// ===========================================================================
// R14-E. Two probes, two opinions, one dictation.
// ===========================================================================

/// FINDING R14-E1 (HIGH). The sampled `FieldSecrecy` is thrown away at the
/// platform boundary and the platform forms its own opinion from fresh reads.
///
/// Round 13's headline fix is that the secure-input flag is "SAMPLED HERE,
/// once, and carried", so that "every decision in one dictation is made from
/// one observation". That property holds inside `crate::pipeline` and stops at
/// its edge: `inject_text` takes no secrecy argument, and calls
/// `focused_field_is_secure()` and `secure_input_active()` itself. Two
/// observations per dictation on the shipped default path, and the pipeline's
/// `conceal_clipboard()` is only ever evaluated on the branch where injection
/// did NOT run.
///
/// Both directions of disagreement cost something real:
///
///  * pipeline `Secure`, platform not: the row is dropped and the user is told
///    "Password field", while `inject_text` falls through to the Cmd-V path and
///    calls `write_clipboard_marked(text, false)` — the suspected password goes
///    to every clipboard manager on the machine with no marker, which is the
///    exact leak `conceal_clipboard` exists to prevent.
///  * pipeline `Ordinary`, platform `Secure`: the row is stored in the syncing
///    history and replicated to every paired device, while the platform refuses
///    to type it and hands it over concealed. Parle tells the rest of the
///    machine the string is a secret and posts it to the user's other computer
///    in the same breath, which is verbatim the defect the pipeline comment at
///    line 610 says the secure-field gate was added to stop.
#[test]
fn r14_sec_e1_the_platform_reprobes_the_field_the_pipeline_already_sampled() {
    let p = squashed("src-tauri/src/pipeline.rs");
    let mac = squashed("src-tauri/src/platform/macos.rs");
    let win = squashed("src-tauri/src/platform/windows.rs");

    // ANCHOR 1: the pipeline samples, once, and carries.
    anchor(&p, "letsecrecy=sample_field_secrecy();", "pipeline.rs");
    anchor(
        &p,
        "FieldSecrecy::Unknown{secure_input:platform::imp::secure_input_active()}",
        "pipeline.rs sample_field_secrecy",
    );
    assert!(
        read_src("src-tauri/src/pipeline.rs").contains("The flag is SAMPLED HERE, once, and carried."),
        "ANCHOR MISSING: round 13's sample-once claim is no longer stated in the code"
    );
    // ANCHOR 2, INVERTED: the platform takes the pipeline's answer.
    anchor(&mac, "letfield=view.is_secure;", "macos.rs inject_text");
    anchor(&mac, "letkeystrokes_blocked=secure_input_active();", "macos.rs inject_text");
    anchor(&win, "ifview.is_secure==Some(true){", "windows.rs inject_text");
    // ANCHOR 3: the unconcealed write on the fall-through path really is there.
    anchor(&mac, "write_clipboard_marked(text,view.conceal);", "macos.rs inject_text");
    // ANCHOR 4: the pipeline's own concealment decision is evaluated only on
    // the branch where inject_text did not run. Both occurrences sit AFTER the
    // `inject_text(` call of their own path and inside the `else if` arm.
    assert!(
        p.matches("secrecy.conceal_clipboard()").count() >= 2,
        "ANCHOR MISSING: conceal_clipboard is no longer used by the pipeline at all"
    );
    for (call, conceal) in p.match_indices("platform::imp::inject_text(").zip(
        p.match_indices("write_clipboard_marked(&text,secrecy.conceal_clipboard())"),
    ) {
        assert!(
            conceal.0 > call.0,
            "ANCHOR MISSING: conceal_clipboard is no longer confined to the non-injection arm"
        );
    }

    // THE CLAIM: the sampled value must reach the platform.
    let sig = anchor(&mac, "pubfninject_text(", "macos.rs");
    let signature = &mac[sig..sig + 220];
    assert!(
        signature.contains("view:super::FieldView"),
        "`inject_text` takes no secrecy argument and re-probes `focused_field_is_secure()` and \
         `secure_input_active()` itself, so the shipped default path makes TWO observations of \
         a value round 13 rewrote FieldSecrecy to observe once. The pipeline's own \
         `conceal_clipboard()` is evaluated only on the branch where injection did not run, so \
         on the default path the concealment of the clipboard is decided entirely by the \
         platform's second look. When the two disagree the costs are: a password the pipeline \
         dropped written to the pasteboard UNMARKED by `write_clipboard_marked(text, false)`, \
         or an \"ordinary\" row stored and REPLICATED to every paired device while the \
         platform is concealing the same string from the rest of the machine. Smallest fix: \
         pass the sampled decision in, e.g. `inject_text(text, secrecy.is_secure_field(), \
         secrecy.conceal_clipboard(), ...)`, and delete both probes from the platform layer.\n\
         signature was: {signature}"
    );
}

// ===========================================================================
// R14-F. The Windows bracket fixes the marking and keeps the stale payload.
// ===========================================================================

/// FINDING R14-F1 (MEDIUM-HIGH). When the sequence number moves, round 13
/// corrects the MARKING of the restore and restores the STALE TEXT anyway.
///
/// ```ignore
/// let seq_before = GetClipboardSequenceNumber();
/// let previous = read_clipboard();                    // session 1
/// let mut previous_excluded = clipboard_is_excluded(); // session 2
/// if GetClipboardSequenceNumber() != seq_before { previous_excluded = true; }
/// ```
///
/// The bracket detects that the two sessions saw different content, and then
/// keeps `previous` — the string session 1 read — and schedules it to be
/// republished `restore_delay_ms` later. The restore's own guard is
/// `seq_now == seq_after_write`, which only rules out writes AFTER Parle's, so
/// a turnover between the two reads passes it. Whatever the user copied in that
/// window is destroyed by `write_clipboard`'s `EmptyClipboard` and then
/// overwritten by content from before they copied it.
///
/// The technique the comment says it is copying does not do this. Forty lines
/// away, `read_clipboard_unless_excluded` runs the same recheck and returns
/// `None`: it abandons the capture. The bracket borrowed the mechanism and not
/// the conclusion. Marking is not the only thing that goes stale when the
/// clipboard turns over; the payload does too, and it is the payload that gets
/// republished.
#[test]
fn r14_sec_f1_the_windows_bracket_corrects_the_marking_and_keeps_the_stale_text() {
    let w = squashed("src-tauri/src/platform/windows.rs");

    // ANCHOR 1: the bracket, exactly as round 13 wrote it.
    let bracket = "letseq_before=unsafe{GetClipboardSequenceNumber()};letmutprevious=read_clipboard();\
                   letmutprevious_excluded=clipboard_is_excluded();\
                   ifunsafe{GetClipboardSequenceNumber()}!=seq_before{previous_excluded=true;";
    anchor(&w, bracket, "windows.rs inject_text");
    // ANCHOR 2: `previous` is used, by value, to feed the restore.
    anchor(&w, "ifletSome(prev)=previous{", "windows.rs inject_text");
    anchor(&w, "write_clipboard_inner(&prev,excluded);", "windows.rs restore");
    // CONTROL: the neighbour it cites does abandon the read on the same signal,
    // so the correct handling exists in this file and is a five-line copy.
    anchor(
        &w,
        "ifGetClipboardSequenceNumber()!=before{returnNone;}",
        "windows.rs read_clipboard_unless_excluded",
    );

    // THE CLAIM: the recheck body must drop the payload, not only re-mark it.
    let at = anchor(&w, "ifunsafe{GetClipboardSequenceNumber()}!=seq_before{", "windows.rs");
    let body = &w[at..at + 120];
    assert!(
        body.contains("previous=None") || body.contains("previous=None;"),
        "the bracket sets `previous_excluded = true` and keeps `previous`. Detecting that the \
         text and the marking describe different content, and then republishing the text \
         anyway, restores content from BEFORE whatever the user copied in that window: \
         `write_clipboard` calls EmptyClipboard, and the restore guard only compares against \
         Parle's own write, so a turnover before that write sails through it. \
         `read_clipboard_unless_excluded` in the same file answers the identical signal with \
         `return None`. Smallest fix: `previous = None;` inside this branch as well, which \
         cancels the restore and loses nothing the user can see, since the dictation is on \
         the clipboard where they wanted it.\n\
         branch was: {body}"
    );
}

// ===========================================================================
// R14-G. Windows never got the chained-restore fix macOS has.
// ===========================================================================

/// FINDING R14-G1 (MEDIUM). Two Windows dictations inside the restore window
/// and Parle restores its OWN transcript over the user's clipboard, for ever.
///
/// macOS carries the oldest original forward across chained dictations
/// (`PENDING_RESTORE`) and cancels superseded restores (`RESTORE_GENERATION`),
/// with the reason written above the static: "Chained dictations within the
/// restore window carry the OLDEST original forward instead of restoring a
/// transcript over the user's real clipboard."
///
/// The Windows `inject_text` has neither. Dictation 1 snapshots the user's
/// clipboard A and spawns a restore holding A and `seq_after_write = S1`.
/// Dictation 2, inside `restore_delay_ms`, snapshots what is on the clipboard
/// now — which is dictation 1's transcript T1 — and spawns a restore holding T1
/// and `seq_after_write = S2`. Thread 1 wakes, sees `S2 != S1`, and does
/// nothing. Thread 2 wakes, sees `S2 == S2`, and writes T1. A is never restored
/// and the user's clipboard now holds a Parle transcript that they never copied.
///
/// The `previous_excluded` the restore re-declares was measured against T1 (an
/// unmarked Parle write), so the republished row also loses whatever exclusion
/// A carried, which is the leak the snapshot pair exists to prevent.
#[test]
fn r14_sec_g1_windows_has_no_chained_restore_guard() {
    let mac = squashed("src-tauri/src/platform/macos.rs");
    let win = squashed("src-tauri/src/platform/windows.rs");

    // CONTROL: the fix exists on macOS, in this repo, and is named.
    anchor(&mac, "staticPENDING_RESTORE:Mutex<Option<String>>", "macos.rs");
    anchor(&mac, "ifpending.is_none(){*pending=previous;", "macos.rs inject_text");
    anchor(&mac, "RESTORE_GENERATION.fetch_add(1,", "macos.rs inject_text");
    assert!(
        read_src("src-tauri/src/platform/macos.rs")
            .contains("carry the OLDEST original forward instead of restoring a"),
        "ANCHOR MISSING: the macOS reasoning for chaining is gone, so the control is stale"
    );
    // CONTROL: Windows really does schedule a delayed restore, so the hazard is
    // reachable and this is not a comparison of two different things.
    anchor(&win, "std::thread::spawn(move||{", "windows.rs inject_text");
    anchor(&win, "std::thread::sleep(std::time::Duration::from_millis(restore_delay_ms));", "windows.rs");

    // THE CLAIM.
    assert!(
        win.contains("PENDING_RESTORE") || win.contains("RESTORE_GENERATION"),
        "windows.rs schedules a delayed clipboard restore with no chaining state of any kind. \
         A second dictation inside `restore_delay_ms` snapshots the FIRST dictation's \
         transcript as \"the user's previous clipboard\" and its restore thread, whose \
         `seq_after_write` is the latest write, is the one that fires. The user's real \
         clipboard is lost and one of Parle's own transcripts is put back in its place, \
         re-marked from a probe of Parle's own unmarked write. macOS fixed exactly this and \
         the fix did not cross. Smallest fix: mirror `PENDING_RESTORE` and \
         `RESTORE_GENERATION`, both of which are twenty lines."
    );
}

// ===========================================================================
// R14-H. A local-only dictation is never told where its text went.
// ===========================================================================

/// FINDING R14-H1 (MEDIUM-HIGH). On the commonest macOS configuration, a
/// dictation Parle could not classify is copied to the clipboard, the user is
/// never told to paste it, and the field stays empty.
///
/// `withheld` is `drop_entirely() || keep_local_only()`, and both toast
/// surfaces return early on it BEFORE the manual-paste instruction:
///
/// ```ignore
/// // App.tsx
/// if (e.withheld) return;
/// ... e.injection?.manual_paste_required ? 'Copied. Press paste to insert (secure field)' ...
/// // Hud.tsx
/// if (e.kind === 'completed' && e.withheld) return;
/// ... e.injection?.manual_paste_required && !e.withheld ...
/// ```
///
/// `keep_local_only` is `Unknown { secure_input: true }`: the probe could not
/// answer AND some app has secure input up. That is the state the platform file
/// records as measured with a password manager merely running, and an
/// unanswering probe is the normal case in Chromium and Electron apps. In that
/// state `inject_text` tries accessibility insertion, and when it does not land
/// — routine in those same apps — it returns `manual_paste_required: true` with
/// the text on the clipboard. The only message the user then gets is "Saved on
/// this device only: this may be a password field", which says nothing about
/// pasting, and `state.rs` holds the HUD open for 3500 ms to display it.
///
/// Withholding a transcript from the RENDER and withholding the one instruction
/// that makes the feature work are different things, and one early return does
/// both.
#[test]
fn r14_sec_h1_a_local_only_dictation_is_never_told_to_paste() {
    let app = squashed("src/App.tsx");
    let hud = squashed("src/Hud.tsx");
    let p = squashed("src-tauri/src/pipeline.rs");
    let mac = squashed("src-tauri/src/platform/macos.rs");

    // ANCHOR 1: withheld folds the local-only case in with the dropped one.
    assert_eq!(
        p.matches("withheld:secrecy.drop_entirely()||secrecy.keep_local_only(),").count(),
        2,
        "ANCHOR MISSING: `withheld` no longer includes keep_local_only"
    );
    // ANCHOR 2: the local-only notice says nothing about pasting.
    let raw_p = code_of("src-tauri/src/pipeline.rs");
    assert!(
        raw_p.contains("Saved on this device only: this may be a password field"),
        "ANCHOR MISSING: the local-only notice has changed"
    );
    assert!(
        !raw_p.contains("Saved on this device only: this may be a password field. Press"),
        "ANCHOR MISSING: the notice already carries a paste instruction; finding is stale"
    );
    // ANCHOR 3: the platform can return manual_paste_required with the field
    // UNKNOWN, which is precisely the keep_local_only state.
    let inj = anchor(&mac, "pubfninject_text(", "macos.rs");
    let body = &mac[inj..];
    // INVERTED. The branch this anchored on no longer exists: when
    // accessibility insertion does not take, the paste is ATTEMPTED rather than
    // refused, so a local-only dictation lands in the field like any other and
    // the outcome still reports the raised flag so the user is told the chord.
    assert!(
        !body.contains("ifkeystrokes_blocked{write_clipboard_marked"),
        "inject_text still gives up without trying the paste whenever the global flag is \
         raised, which is every field on the machine while any app holds secure input"
    );
    // The outcome still REPORTS the raised flag, so the user is told the chord
    // in case the attempted paste did not land. It is no longer a refusal to
    // try, only an admission that we cannot confirm it worked.
    anchor(body, "manual_paste_required:keystrokes_blocked,", "macos.rs");

    // CONTROL: the instruction exists and is the only one the product gives.
    //
    // i18n MOVE. The sentence left `App.tsx` for `src/i18n/en.ts` under
    // `app.toast.pasteInstruction`; the toast holds the `t()` call and still
    // passes the platform-derived `PASTE_KEYS` in. Anchor the call in the
    // manual-paste position, then the English value, because an instruction
    // that no longer instructs would leave the finding open while the call
    // site still looked right.
    anchor(
        &app,
        "e.injection?.manual_paste_required?t('app.toast.pasteInstruction',{keys:PASTE_KEYS})",
        "App.tsx",
    );
    assert_eq!(
        en_string("app.toast.pasteInstruction"),
        "Copied. Press {keys} to paste",
        "ANCHOR MISSING: the main window's paste instruction no longer tells the user to paste"
    );
    anchor(&hud, "e.injection?.manual_paste_required", "Hud.tsx");

    // THE CLAIM: the early return must not swallow the paste instruction.
    // INVERTED. Both surfaces now withhold the TRANSCRIPT and keep the
    // INSTRUCTION: the early return is conditional on there being no manual
    // paste to tell the user about.
    let ret = anchor(&app, "if(e.withheld&&!e.injection?.manual_paste_required)return;", "App.tsx");
    let instr = anchor(&app, "e.injection?.manual_paste_required?", "App.tsx");
    assert!(
        ret < instr,
        "App.tsx returns on `withheld` before the only paste instruction Parle ever gives, and \
         Hud.tsx gates the same instruction on `!e.withheld`. `withheld` is true for \
         keep_local_only, which is the state a Mac with a password manager running and a \
         Chromium field focused produces routinely, and on that path `inject_text` has put the \
         text on the clipboard and asked for a manual paste. The user is told only \"Saved on \
         this device only: this may be a password field\", the field stays empty and nothing \
         says the text is waiting on the clipboard. Smallest fix: render the paste instruction \
         whenever `injection.manual_paste_required` is set, withheld or not; it names no \
         transcript, so it leaks nothing."
    );
}

// ===========================================================================
// R14-I. The delete confirmation learns about pairings once and never again.
// ===========================================================================

/// FINDING R14-I1 (LOW). `pairedNames` is fetched on mount and never updated,
/// so a device paired while History is open is not named in the confirmation
/// for an irreversible, travelling delete.
///
/// Round 13 fixed the `[]`-versus-`null` collapse for a FAILED status call and
/// left the STALE case: an empty array that was correct at mount time is
/// indistinguishable from an empty array that is now wrong, and the confirmation
/// says nothing at all rather than "this may also delete it from your paired
/// devices". `sync-status` is pushed as an event for exactly this reason, and
/// `SettingsView` subscribes to it.
#[test]
fn r14_sec_i1_the_delete_confirmation_never_hears_about_a_new_pairing() {
    let h = squashed("src/views/History.tsx");
    let sv = squashed("src/views/SettingsView.tsx");
    let api = squashed("src/api.ts");

    // ANCHOR 1: the round-13 state and its one-shot fetch.
    // Repointed after the roster stopped being its own state: History now holds
    // the whole `SyncStatus`, because the per-device row markers need the local
    // device id as well as the peer names, and the roster is derived from it.
    // Same subscription, same null contract, one source of truth instead of two.
    anchor(&h, "const[syncStatus,setSyncStatus]=useState<SyncStatus|null>(null);", "History.tsx");
    anchor(&h, "onSyncStatus((st)=>setSyncStatus(st))", "History.tsx");
    anchor(&h, ".catch(()=>setSyncStatus(null));", "History.tsx");
    anchor(&h, "constpairedNames=syncStatus?syncStatus.paired.map((d)=>d.name):null;", "History.tsx");
    // ANCHOR 2: an empty array really does silence the warning.
    anchor(&h, "elseif(pairedNames.length>0){", "History.tsx confirmDelete");
    // CONTROL: the push channel exists and a sibling view already uses it, so
    // the fix is available and not hypothetical.
    anchor(&api, "exportfunctiononSyncStatus(", "api.ts");
    anchor(&sv, "constun=onSyncStatus", "SettingsView.tsx");

    // THE CLAIM.
    assert!(
        h.contains("onSyncStatus"),
        "History.tsx reads the paired roster once, on mount, and never subscribes to the \
         `sync-status` event that manager.rs pushes on every change. A device paired after the \
         view mounted leaves `pairedNames` as a stale `[]`, and confirmDelete's `length > 0` \
         test then omits the travel warning entirely from a delete that does travel. Round 13 \
         separated \"we do not know\" from \"nobody\" for a failed call and not for a stale \
         one. Smallest fix: add the `onSyncStatus` subscription SettingsView already has."
    );
}

// ===========================================================================
// R14-J. Defence that holds: the local-only badge has real data behind it.
// ===========================================================================

/// NOT a finding. The "this device only" badge reads `item.local_only`, and the
/// flag survives the insert, the projection and the search, so the badge is not
/// decoration. Recorded because round 14 attacked it and it held: a badge whose
/// field never arrives would render never, silently, and look identical to a
/// working one in every source-level test.
///
/// RUNTIME.
#[test]
fn r14_sec_j1_a_local_only_row_reaches_the_badge_with_its_flag_set() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(ME);
    let ordinary = s.insert_transcription(&tr("an ordinary dictation"), None, None).unwrap();
    let hidden = s
        .insert_transcription_local_only(&tr("a dictation we could not classify"), None, None)
        .unwrap();
    assert!(ordinary > 0 && hidden > 0, "both inserts must produce real rows");

    let rows = s.search("dictation", None, 60).expect("search runs");
    // CONTROL: the search finds BOTH rows, so a false negative below cannot be
    // "the local-only row was filtered out of the list".
    assert_eq!(rows.len(), 2, "expected both rows back, got {}", rows.len());

    let flagged: Vec<bool> = rows.iter().map(|r| r.local_only).collect();
    assert!(
        flagged.contains(&true) && flagged.contains(&false),
        "the local_only flag does not survive the round trip into the row the UI renders, so \
         the History badge could never appear: {flagged:?}"
    );

    // And the badge really does read that field.
    anchor(&squashed("src/views/History.tsx"), "{item.local_only&&(", "History.tsx");
    assert!(
        read_src("src/types.ts").contains("local_only: boolean;"),
        "ANCHOR MISSING: the frontend type no longer carries local_only"
    );
}

// ===========================================================================
// R14-K. Round 13 fixed one of two identical writes, four lines apart.
// ===========================================================================

/// FINDING R14-K1 (MEDIUM-HIGH). The `keystrokes_blocked` fallback still
/// conceals a field it knows is ORDINARY, and the next dictation reads that
/// mark back as the OS calling the user's own text a secret.
///
/// Round 13 narrowed one write in `macos::inject_text` and wrote the rule down:
///
/// ```ignore
/// // `field.is_none() && keystrokes_blocked`, not `keystrokes_blocked` alone.
/// // ... reaching here with `Some(false)` means the field is known ORDINARY,
/// // and `FieldSecrecy::conceal_clipboard` says an ordinary field is never
/// // concealed. ... concealing it tells every clipboard manager on the machine
/// // to bin the copy they just asked for, and `clipboard_is_concealed` then
/// // reads our own mark back as the OS calling the row a secret.
/// write_clipboard_marked(text, field.is_none() && keystrokes_blocked);
/// ```
///
/// Four lines below, the fallback taken when accessibility insertion does not
/// land still writes `write_clipboard_marked(text, true)` with the same `field`
/// in scope and unconsulted. Every word of the reasoning above applies to it.
///
/// That branch is not the exotic one. The file itself records that the flag
/// reads TRUE with a password manager merely running, and accessibility
/// insertion fails routinely in Chromium and Electron applications, so an
/// ordinary Slack or VS Code field on a Mac with 1Password open takes it.
///
/// The laundering the doc comment on `write_clipboard_marked` warns about then
/// completes on the NEXT dictation: `previous_concealed = clipboard_is_concealed()`
/// reads Parle's own ConcealedType back off its own transcript, and
/// `PENDING_RESTORE_CONCEALED` carries it into the restore, so the user's
/// ordinary clipboard is republished marked as a secret. "Two dictations
/// laundered an ordinary clipboard entry into 'the OS says this is a secret'"
/// is that comment's own sentence.
#[test]
fn r14_sec_k1_the_keystrokes_blocked_fallback_conceals_a_known_ordinary_field() {
    let mac = squashed("src-tauri/src/platform/macos.rs");
    let raw = read_src("src-tauri/src/platform/macos.rs");

    // ANCHOR 1: the probe is taken once and bound, so `field` is in scope.
    anchor(&mac, "letfield=view.is_secure;", "macos.rs inject_text");
    // ANCHOR 2: round 13's narrowed write, the one that got the rule.
    anchor(
        &mac,
        "write_clipboard_marked(text,view.conceal);",
        "macos.rs inject_text",
    );
    // ANCHOR 3: the rule is stated in the file, so this is round 13's own rule
    // and not one invented here.
    assert!(
        raw.contains("ordinary field is never concealed"),
        "ANCHOR MISSING: round 13's stated rule is gone from macos.rs"
    );
    // ANCHOR 4: the laundering the mark causes is documented in this file too.
    assert!(
        raw.contains("laundered an ordinary clipboard entry"),
        "ANCHOR MISSING: the laundering warning is gone, so the cost is unevidenced"
    );
    // ANCHOR 5: the sibling branch exists and is reachable below the narrowed
    // one, with the same `field` still in scope.
    let inj = anchor(&mac, "pubfninject_text(", "macos.rs");
    // Round 14 moved the rule OUT of the platform: both writes take the
    // pipeline's `view.conceal`, which is `Secure || keep_local_only()` and so
    // false for a known-ordinary field by construction. There is no longer a
    // second copy of the rule that could disagree with the first, which is
    // stronger than having the two agree.
    // The second write is gone entirely: the fallback that concealed a known
    // ordinary field was the give-up branch, and the give-up branch is what the
    // paste-anyway change removed. Every remaining write takes `view.conceal`.
    let narrowed = anchor(&mac[inj..], "write_clipboard_marked(text,view.conceal);", "macos.rs");
    let _ = narrowed;
    // Exactly ONE hard-coded write survives, and it is the known-password-field
    // gate, where concealing unconditionally is the whole point. Every other
    // write takes the pipeline's decision.
    assert_eq!(
        mac[inj..].matches("write_clipboard_marked(text,true)").count(),
        1,
        "a clipboard write in inject_text hard-codes its concealment outside the \
         known-secure gate, so the platform and the pipeline can disagree about one dictation"
    );
    assert!(
        !mac[inj..].contains("write_clipboard_marked(text,false)"),
        "a clipboard write in inject_text hard-codes NOT concealing, which is the direction \
         that leaks"
    );

    // THE CLAIM: the same rule must reach the second write.
    assert!(
        !mac.contains("ifkeystrokes_blocked{write_clipboard_marked(text,true);"),
        "the `keystrokes_blocked` fallback writes the clipboard CONCEALED without consulting \
         `field`, four lines below the write round 13 narrowed for exactly that reason and \
         with the same variable in scope. With the field known ORDINARY (`Some(false)`), Parle \
         tells every clipboard manager on the machine to bin a transcript the user asked for, \
         and on the next dictation `clipboard_is_concealed()` reads Parle's own mark back and \
         `PENDING_RESTORE_CONCEALED` carries it into the restore, so the user's ordinary \
         clipboard comes back marked as a secret. Both consequences are written down in this \
         file. The branch is the common one: the flag reads TRUE with a password manager \
         merely running, and accessibility insertion fails routinely in Chromium and Electron. \
         Smallest fix: `write_clipboard_marked(text, field.is_none());` — the gate above \
         already returned for a known password field."
    );
}

/// FINDING R14-K2 (LOW). The main window's toast still asserts "(secure field)"
/// for a field Parle knows is ordinary, and still names no key.
///
/// Round 12 fixed both halves of this sentence in `Hud.tsx` and wrote down why:
/// the bare paste chord "is wrong on the half of the product that runs on
/// Windows", and the message "asserted '(secure field)' unconditionally, while
/// the same outcome is returned when the field is ordinary and merely a
/// password manager is running". The identical literal in `App.tsx` was not
/// touched by round 12 or round 13.
#[test]
fn r14_sec_k2_the_main_window_still_claims_a_secure_field_it_cannot_see() {
    let app = squashed("src/App.tsx");
    let hud = squashed("src/Hud.tsx");

    // CONTROL: the sibling was fixed, in this repo, so the correct wording
    // exists and the rule is satisfiable.
    // Round 14 hoisted the chord into types.ts so the two surfaces share one.
    //
    // i18n MOVE. Both surfaces' sentences left their components for
    // `src/i18n/en.ts` (`hud.pasteInstruction`, `app.toast.pasteInstruction`),
    // while the chord stayed a `PASTE_KEYS` interpolation. So the control is
    // now: the HUD renders the key, and the HUD's ENGLISH TEXT is free of the
    // unconditional secure-field claim. Checking only the component would be
    // vacuous, because the component no longer contains any wording at all.
    anchor(&squashed("src/types.ts"), "exportconstPASTE_KEYS=", "types.ts");
    anchor(&hud, "text:t('hud.pasteInstruction',{keys:PASTE_KEYS})", "Hud.tsx");
    let hud_text = en_string("hud.pasteInstruction");
    assert!(
        !hud.contains("(securefield)")
            && !hud_text.contains("secure field")
            && hud_text.contains("{keys}"),
        "ANCHOR MISSING: the HUD has regained the unconditional secure-field claim; the \
         control for this test is stale"
    );
    // ANCHOR: App.tsx is the other surface that renders this outcome, and it
    // renders it through a key.
    anchor(&app, "e.injection?.manual_paste_required?", "App.tsx");
    anchor(&app, "t('app.toast.pasteInstruction',{keys:PASTE_KEYS})", "App.tsx");

    // THE CLAIM. It follows the literal into the dictionary: the sentence the
    // main window shows must not assert a field it cannot see, and must name a
    // key. Both halves of the sentence round 12 fixed in the HUD.
    let app_text = en_string("app.toast.pasteInstruction");
    assert!(
        !app_text.contains("secure field") && app_text.contains("{keys}"),
        "App.tsx tells the user \"Copied. Press paste to insert (secure field)\". \
         `manual_paste_required` is returned on the `keystrokes_blocked` fallback too, where \
         the field may be known ORDINARY and only a password manager is raising the global \
         flag, so the claim is false on the commonest path that reaches it. \"Press paste\" \
         also names no key, which is the other half of the same sentence round 12 fixed in \
         Hud.tsx and never carried across. Smallest fix: reuse the HUD's wording and its \
         `PASTE_KEYS` constant."
    );
}
