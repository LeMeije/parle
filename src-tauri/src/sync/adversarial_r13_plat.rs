//! ADVERSARIAL REVIEW, ROUND 13 — platform layer and secrets.
//!
//! Target: everything round 12 (`a1ceaf7`) rewrote in `platform/windows.rs`,
//! `platform/macos.rs` and `pipeline.rs`, plus the consumers of the
//! `PipelineEvent::Completed { withheld }` field that round 12 introduced.
//!
//! Why several tests read SOURCE instead of calling the function, the same
//! reason `adversarial_r12_sec` gives and no other:
//!
//! - `platform::windows` is `#![cfg(target_os = "windows")]` and does not
//!   compile on this host at all, so no test anywhere in this repo can call it.
//! - `FieldSecrecy`, `sample_field_secrecy` and `store_transcription` are
//!   private to `crate::pipeline`, which is not an ancestor of this module.
//! - `platform::macos`'s clipboard writers touch the REAL system pasteboard of
//!   a machine that is running a copy of Parle with its clipboard monitor
//!   armed. Calling them would inject rows into the user's own history.
//! - The `Completed` consumers are TypeScript.
//!
//! Every source-reading test therefore obeys the handover's rule for guards
//! that can find nothing: it FIRST asserts that it found the construct it is
//! measuring, and where possible it asserts that some OTHER site in the same
//! codebase satisfies the rule, so the rule is provably satisfiable and the
//! parser is provably working. `r13_plat_completed_text_reaches_two_unguarded_surfaces`
//! is the clearest example: three of the five consumers must pass before the
//! other two are allowed to fail.
//!
//! Nothing here writes to a clipboard, opens a socket, spawns a thread, sleeps,
//! or touches the user's data.

#![cfg(test)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

/// Source with `//` comments stripped, so a comment cannot satisfy a guard that
/// is looking for code. Round 12 left long explanatory comments beside every
/// line this file examines, and several of them quote the very tokens the
/// assertions below search for.
fn code_of(rel: &str) -> String {
    let text = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"));
    assert!(!text.trim().is_empty(), "{rel} is empty; the search below would be vacuous");
    text.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The body of a function, comments stripped, from `decl` to the next
/// top-level `fn`/`pub fn`. Panics if `decl` is absent, which is the
/// found-something control for every caller.
fn body_of(rel: &str, decl: &str) -> String {
    let code = code_of(rel);
    let after = code
        .split(decl)
        .nth(1)
        .unwrap_or_else(|| panic!("{decl} is not in {rel}; this test is measuring nothing"));
    let end = [after.find("\nfn "), after.find("\npub fn ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(after.len());
    after[..end].to_string()
}

/// Byte index of `needle` in `hay`, or a failing assertion naming both.
fn index_of(hay: &str, needle: &str, what: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("{what}: `{needle}` is not present, so this test found nothing"))
}

// ---------------------------------------------------------------------------
// 1. The secure-field early return throws the user's clipboard away.
// ---------------------------------------------------------------------------

/// No `inject_text` may overwrite the clipboard before it has captured what was
/// on it.
///
/// The invariant, stated without reference to any particular branch: the FIRST
/// clipboard-mutating call in `inject_text` must come after the snapshot of the
/// previous contents. Anything else destroys data the function's own `restore`
/// parameter promises to put back.
///
/// Round 12 added a `focused_field_is_secure() == Some(true)` gate to the top of
/// the Windows `inject_text` that writes and returns. macOS has had the same
/// shape since round 11, plus a second one for `keystrokes_blocked`. On both
/// platforms the snapshot (`let previous = read_clipboard()`) sits BELOW those
/// gates, so the one dictation the app has decided is a secret is also the one
/// dictation that silently eats whatever the user had copied, with no restore
/// scheduled at any delay and regardless of the `restore` argument.
///
/// The control is the pair of `assert!` calls inside `index_of`: both a read
/// and a write must exist in the function, or the ordering claim is vacuous.
#[test]
fn r13_plat_inject_text_writes_the_clipboard_before_reading_it() {
    for (rel, read_call, write_call) in [
        ("src-tauri/src/platform/windows.rs", "read_clipboard()", "write_clipboard"),
        ("src-tauri/src/platform/macos.rs", "read_clipboard()", "write_clipboard"),
    ] {
        let body = body_of(rel, "pub fn inject_text(");
        let first_read = index_of(&body, read_call, rel);
        let first_write = index_of(&body, write_call, rel);
        // NOT "snapshot first". Round 13 proposed hoisting the snapshot above
        // the early returns, and that fix is wrong: those branches return
        // `manual_paste_required: true`, so the dictation has to STAY on the
        // clipboard for the user to paste it. A restore scheduled
        // `restore_delay_ms` later would take it away before they used it.
        //
        // The loss is inherent while the text must sit there. What must be true
        // is that every write above the snapshot belongs to a branch that hands
        // the clipboard to the user deliberately, and that the user is told.
        assert!(
            first_read < first_write || body[..first_read].contains("manual_paste_required: true"),
            "{rel}: inject_text mutates the clipboard at byte {first_write}, does not capture \
             the previous contents until byte {first_read}, and the branches in between do \
             not hand the clipboard to the user: they destroy it with no restore and no word"
        );
    }
}

/// The same invariant, enumerated, so the report can name every offending site
/// rather than only the first one.
///
/// macOS has TWO early returns above the snapshot, not one: the secure-field
/// gate, and the `keystrokes_blocked` fallback taken when accessibility
/// insertion does not land. That second one is the common case, not the exotic
/// one: the flag reads TRUE with a password manager merely running (the file
/// says so, measured), and accessibility insertion fails routinely in Chromium
/// and Electron applications, which is where most people type. So on a Mac with
/// 1Password open, dictating into a Slack or VS Code field can destroy the
/// user's clipboard on every single dictation, with `restore_clipboard` on.
///
/// CONTROL: the snapshot must be found, and at least one write must be found
/// AFTER it, so the ordering test cannot pass by finding no writes at all.
#[test]
fn r13_plat_every_clipboard_write_follows_the_snapshot() {
    let mut offenders: Vec<String> = vec![];
    let mut excused = 0usize;
    for rel in [
        "src-tauri/src/platform/windows.rs",
        "src-tauri/src/platform/macos.rs",
    ] {
        let body = body_of(rel, "pub fn inject_text(");
        let read_at = index_of(&body, "read_clipboard()", rel);
        let writes: Vec<usize> = body
            .match_indices("write_clipboard")
            .map(|(i, _)| i)
            .filter(|i| !body[*i..].starts_with("write_clipboard_inner"))
            .collect();
        // CONTROL: at least one write must sit AFTER the snapshot, or the
        // ordering claim below could be satisfied by finding no writes at all.
        assert!(
            writes.iter().any(|w| *w > read_at),
            "{rel}: no clipboard write follows the snapshot at all; this test is measuring \
             the wrong function"
        );
        for w in writes.into_iter().filter(|w| *w < read_at) {
            // Not an offender: the caller asked for the transcript to STAY on
            // the clipboard. Replacing the previous contents is then the whole
            // point of the setting, not data loss. Matched on the BRANCH, not
            // on the parameter name, and only within the twenty lines above the
            // write, so the function signature cannot excuse anything.
            let lookback: String = body[..w]
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .join("\n");
            if lookback.contains("if keep_on_clipboard {") {
                excused += 1;
                continue;
            }
            // Also not an offender: a branch that returns
            // `manual_paste_required: true` has handed the clipboard to the
            // user on purpose. Restoring over it would remove the dictation
            // before it could be pasted. Looked for in the forty lines AFTER
            // the write, so it describes this branch's own return.
            let lookahead: String =
                body[w..].lines().take(40).collect::<Vec<_>>().join("\n");
            if lookahead.contains("manual_paste_required: true") {
                excused += 1;
                continue;
            }
            let line = body[..w].lines().count();
            offenders.push(format!("{rel} (line {line} of inject_text)"));
        }
    }
    // CONTROL: the exclusion rule has a real target, so it is a narrowing and
    // not a hole. macOS has exactly one `if keep_on_clipboard {` branch that
    // writes, and the rule must never excuse more sites than that.
    let mac = body_of("src-tauri/src/platform/macos.rs", "pub fn inject_text(");
    let keep_at = index_of(&mac, "if keep_on_clipboard {", "macos.rs inject_text");
    assert!(
        mac[keep_at..].starts_with("if keep_on_clipboard {")
            && mac[keep_at..].contains("write_clipboard_marked(text,"),
        "macos.rs: the keep_on_clipboard branch no longer writes the clipboard; the \
         exclusion rule in this test has no target and is silently widening it"
    );
    // Four: the one `keep_on_clipboard` branch, plus the three manual-paste
    // gates (macOS secure field, macOS keystrokes-blocked fallback, Windows
    // secure field). Kept as an exact count so a FIFTH pre-snapshot write
    // cannot slip in under either exclusion without this test saying so.
    assert_eq!(
        excused, 4,
        "the exclusions excused {excused} writes, not the four known branches; either a new \
         pre-snapshot write appeared or a rule is matching what it was not written for"
    );
    assert!(
        offenders.is_empty(),
        "{} clipboard write(s) run before inject_text has captured the previous contents: \
         {offenders:#?}. Each overwrites the user's clipboard and schedules no restore, \
         without either handing it to the user deliberately (`keep_on_clipboard`) or \
         requiring a manual paste. One of those two has to be true, or the write is data \
         loss the user is never told about.",
        offenders.len()
    );
}

// ---------------------------------------------------------------------------
// 2. The Windows marking probe is a second, unguarded clipboard session.
// ---------------------------------------------------------------------------

/// `read_clipboard()` then `clipboard_is_excluded()` are two separate
/// `OpenClipboard`/`CloseClipboard` sessions. Any process may `EmptyClipboard`
/// and write between them, so the marking the restore re-declares may describe
/// content the restore is not putting back.
///
/// The leaking direction is the reachable one: `read_clipboard` reads the
/// payload with no regard for markers at all, so it will happily capture a
/// password manager's MARKED secret; if that entry is then replaced by ordinary
/// unmarked content before `clipboard_is_excluded()` opens its own session, the
/// probe answers `false` and the restore republishes the secret with the
/// owner's `CanUploadToCloudClipboard = 0` deleted. That is precisely the leak
/// round 12's commit message says this code was added to close.
///
/// The technique that fixes it already exists in this same file:
/// `read_clipboard_unless_excluded` samples `GetClipboardSequenceNumber` before
/// and after and discards the capture if it moved. The control below asserts
/// that recheck is really there, which proves both that the search works and
/// that the fix is a five-line copy of a neighbour.
#[test]
fn r13_plat_windows_marking_probe_has_no_sequence_recheck() {
    let rel = "src-tauri/src/platform/windows.rs";

    // CONTROL: the file does know how to do this, forty lines away.
    let reader = body_of(rel, "fn read_clipboard_unless_excluded()");
    assert!(
        reader.contains("let before = GetClipboardSequenceNumber();"),
        "{rel}: read_clipboard_unless_excluded no longer takes a `before` sequence number; \
         this test's control is gone and its claim is unanchored"
    );
    assert!(
        reader.contains("GetClipboardSequenceNumber() != before"),
        "{rel}: read_clipboard_unless_excluded no longer rechecks the sequence number; \
         control gone"
    );

    // THE CLAIM: the injection path does the same two-phase read with no guard.
    //
    // Measured as: the snapshot pair must be BRACKETED by sequence-number
    // reads, one before the text is taken and one after the marking is probed,
    // so a change under either session can be detected. Two reads, therefore,
    // before the transcript is written over the top.
    let body = body_of(rel, "pub fn inject_text(");
    let read_at = index_of(&body, "let previous = read_clipboard();", rel);
    let probe_at = index_of(&body, "clipboard_is_excluded()", rel);
    let write_at = index_of(&body, "write_clipboard(text)", rel);
    assert!(
        read_at < probe_at && probe_at < write_at,
        "{rel}: snapshot, probe and write are not in the order this test assumes \
         ({read_at}, {probe_at}, {write_at})"
    );
    let bracket = &body[..write_at];
    let reads = bracket.matches("GetClipboardSequenceNumber").count();
    assert!(
        reads >= 2,
        "{rel}: the text and its marking are captured in two separate clipboard sessions \
         and only {reads} sequence-number read(s) happen before the transcript overwrites \
         them, so a change in the window is undetectable. The restore then puts back \
         content A carrying content B's marking, and the reachable direction of that is a \
         MARKED secret republished UNMARKED, which Cloud Clipboard uploads. \
         read_clipboard_unless_excluded, forty lines away, already brackets its own session \
         exactly this way."
    );
}

/// The failure policy of `clipboard_is_excluded()` is the safe one, and this
/// test says so out loud so a later round does not "fix" it the wrong way.
///
/// It returns `true` when `OpenClipboard` fails. The two outcomes are: over-mark
/// an ordinary clipboard, and the user's own copy is missing from Win+V; or
/// under-mark a secret, and it is uploaded to Microsoft and pushed to their
/// other machines. Confidentiality outranks Win+V, so `true` is correct.
///
/// This test PASSES today. It is here as a pin, not a finding.
#[test]
fn r13_plat_windows_marking_probe_fails_closed() {
    let rel = "src-tauri/src/platform/windows.rs";
    let body = body_of(rel, "fn clipboard_is_excluded()");
    let open_at = index_of(&body, "if !open_clipboard_retry()", rel);
    let tail = &body[open_at..];
    let brace = index_of(tail, "}", rel);
    let on_failure = &tail[..brace];
    assert!(
        on_failure.contains("return true"),
        "{rel}: clipboard_is_excluded now fails OPEN. A clipboard it could not read is a \
         clipboard whose marking it does not know, and dropping the marking uploads the \
         content. Failure branch was:\n{on_failure}"
    );
}

// ---------------------------------------------------------------------------
// 3. Windows self-capture suppression and the restore guard use two different
//    sequence numbers.
// ---------------------------------------------------------------------------

/// `write_clipboard_inner` records `OUR_LAST_WRITE` INSIDE the clipboard
/// session (round 12 moved the store above `CloseClipboard`). `inject_text`
/// then reads `GetClipboardSequenceNumber()` again, AFTER `write_clipboard`
/// has returned and therefore after `CloseClipboard`, and keeps that as
/// `seq_after_write`.
///
/// Those two reads straddle `CloseClipboard`. They are equal only if the
/// Windows clipboard sequence number is unaffected by closing the clipboard,
/// which is not documented in this repo and which the handover records as never
/// having been checked on hardware ("Win+V exclusion unverified on hardware").
/// If it is affected, then `we_wrote_change()` returns false for Parle's own
/// dictation and the 400 ms monitor captures it as a clipboard row and
/// replicates it: the exact failure round 12's commit message says the move was
/// made to prevent.
///
/// The point of this test is that the code need not depend on the answer at
/// all. `inject_text` can read the number Parle actually recorded instead of
/// asking the OS a second time, and then the restore guard and the self-capture
/// suppression name the same change by construction.
#[test]
fn r13_plat_windows_restore_guard_and_self_capture_use_one_number() {
    let rel = "src-tauri/src/platform/windows.rs";

    // CONTROL: the store really is inside the session.
    let writer = body_of(rel, "fn write_clipboard_inner(");
    let store_at = index_of(&writer, "OUR_LAST_WRITE.store(", rel);
    let close_at = index_of(&writer, "CloseClipboard()", rel);
    assert!(
        store_at < close_at,
        "{rel}: write_clipboard_inner no longer records OUR_LAST_WRITE before \
         CloseClipboard; this test's premise is gone"
    );

    // CONTROL: the accessor that would make the two agree exists.
    assert!(
        code_of(rel).contains("pub fn we_wrote_change(seq: u32)"),
        "{rel}: we_wrote_change is gone; the suggested fix has no target"
    );

    // THE CLAIM.
    let body = body_of(rel, "pub fn inject_text(");
    assert!(
        !body.contains("let seq_after_write = unsafe { GetClipboardSequenceNumber() };"),
        "{rel}: inject_text re-reads GetClipboardSequenceNumber AFTER write_clipboard has \
         closed the clipboard, while write_clipboard_inner recorded OUR_LAST_WRITE BEFORE \
         closing it. The restore guard and the self-capture suppression are then comparing \
         two numbers that are equal only if CloseClipboard does not move the counter, which \
         is nowhere established. Read OUR_LAST_WRITE instead and the question stops mattering."
    );
}

// ---------------------------------------------------------------------------
// 4. macOS conceals the user's deliberate copy on an ORDINARY field.
// ---------------------------------------------------------------------------

/// `pipeline::FieldSecrecy::conceal_clipboard()` is the app's stated policy for
/// this exact question, and it says: an ORDINARY field is never concealed, no
/// matter what the process-global secure-input flag reads.
///
/// `platform::macos::inject_text` decides it again, differently. In the
/// `keep_on_clipboard` branch round 12 changed `false` to `keystrokes_blocked`,
/// which is `secure_input_active()` and nothing else. The secure-field gate at
/// the top of the same function has already returned for `Some(true)`, so this
/// branch runs with the field known NOT to be a password field, and conceals
/// anyway whenever a password manager happens to be running.
///
/// The file's own documentation says why that matters: `write_clipboard_marked`
/// carries a comment explaining that marking the transcript the user asked to
/// keep meant "every other clipboard manager on the machine binned it", and
/// `clipboard_is_concealed()` counts ConcealedType, so the next dictation's
/// restore reads Parle's own mark back as the OS calling the row a secret.
/// `keep_on_clipboard` is the shipped `copy_to_clipboard` setting: this is the
/// user pressing "also copy", and on a machine with 1Password merely running
/// Alfred, Raycast and Maccy will now drop every one of them.
///
/// CONTROL: the pipeline policy is read out of the real source first, and the
/// test asserts it contains the Ordinary exemption. If that ever changes, this
/// test says so instead of quietly asserting a stale rule.
#[test]
fn r13_plat_macos_conceals_a_copy_the_pipeline_says_to_leave_alone() {
    let pipeline = "src-tauri/src/pipeline.rs";
    let policy = body_of(pipeline, "fn conceal_clipboard(self)");
    // The rule is unchanged; round 13 expressed it through the pure predicate
    // rather than re-reading the global. Ordinary is still exempt, because
    // `keep_local_only` is false for it by construction.
    assert!(
        policy.contains("FieldSecrecy::Secure") && policy.contains("keep_local_only()"),
        "{pipeline}: conceal_clipboard is no longer 'Secure, or Unknown with the flag up'; \
         this test's reference policy is gone. Body was:\n{policy}"
    );

    let rel = "src-tauri/src/platform/macos.rs";
    let body = body_of(rel, "pub fn inject_text(");
    // The branch: `if keep_on_clipboard {` ... `write_clipboard_marked(text, ARG);`
    let keep_at = index_of(&body, "if keep_on_clipboard {", rel);
    let tail = &body[keep_at..];
    let call_at = index_of(tail, "write_clipboard_marked(text,", rel);
    let arg_start = call_at + "write_clipboard_marked(text,".len();
    let arg_end = arg_start + index_of(&tail[arg_start..], ")", rel);
    let arg = tail[arg_start..arg_end].trim().to_string();
    assert!(!arg.is_empty(), "{rel}: could not read the concealment argument; found nothing");

    assert!(
        // `is_none` without the parens: the extractor above stops at the first
        // `)`, which is the one that closes `is_none()` itself.
        arg.contains("field.is_none"),
        "{rel}: the keep_on_clipboard branch conceals on `{arg}` alone, which is \
         secure_input_active(). The secure-field gate above has already returned for \
         Some(true), so this branch conceals the user's deliberate 'also copy to clipboard' \
         transcript on an ORDINARY field whenever any app holds secure input up. \
         pipeline::FieldSecrecy::conceal_clipboard() says Ordinary is never concealed. The \
         two decisions must not disagree: hoist `focused_field_is_secure()` once at the top \
         of inject_text and conceal on `field.is_none() && keystrokes_blocked`."
    );
}

// ---------------------------------------------------------------------------
// 5. `withheld` is recomputed from a mutable OS global after the fact.
// ---------------------------------------------------------------------------

/// A decision taken ABOUT a row must not be recomputed from a global that can
/// change before the row is reported.
///
/// `FieldSecrecy::keep_local_only()` is not a function of `FieldSecrecy`. It
/// reads `platform::imp::secure_input_active()`, a process-global,
/// system-wide, any-app-may-raise-or-lower flag, live, at every call. It is
/// called once inside `store_transcription` to decide where the row goes, and
/// again in the `Completed` event to decide what the user is told. Two reads,
/// two answers available:
///
/// - flag TRUE at store, FALSE at report: the row is stored LOCAL ONLY and
///   `withheld` is false, so `App.tsx` renders the first 42 characters of the
///   transcript as a toast. That is round 11's headline defect, reintroduced
///   through a race by round 12's own fix for it.
/// - flag FALSE at store, TRUE at report: the row is stored ORDINARY and
///   replicates to every paired device, and `withheld` is true, so App.tsx and
///   Hud.tsx both `return` and the user is shown NOTHING AT ALL. There is no
///   notice either, because `store_transcription` returned `None`.
///
/// `conceal_clipboard()` reads the same flag a third time, earlier still, so
/// the clipboard marking and the storage decision can already be taken under
/// different world states.
///
/// The fix is to sample the flag once, where the field is sampled, and make the
/// three predicates pure.
#[test]
fn r13_plat_withheld_is_recomputed_from_a_live_global() {
    let rel = "src-tauri/src/pipeline.rs";
    let code = code_of(rel);

    // CONTROL: keep_local_only really does read the live global.
    // INVERTED. The flag is sampled once, with the field probe, and carried in
    // the variant, so the predicate is a function of its own value.
    let pred = body_of(rel, "fn keep_local_only(self)");
    assert!(
        !pred.contains("platform::imp::secure_input_active()"),
        "{rel}: keep_local_only reads the process-global flag live, so two calls in one \
         dictation can disagree about the same text. Body was:\n{pred}"
    );
    assert!(
        code.contains("FieldSecrecy::Unknown { secure_input: platform::imp::secure_input_active() }"),
        "{rel}: the flag is not sampled alongside the field probe, so nothing latches it"
    );

    // CONTROL: the storage decision is one of the callers.
    let store = body_of(rel, "fn store_transcription(");
    assert!(
        store.contains("secrecy.keep_local_only()"),
        "{rel}: store_transcription no longer consults keep_local_only; premise gone"
    );

    // CONTROL: and there is more than one `Completed` site to get wrong.
    let completed_sites = code.matches("PipelineEvent::Completed {").count();
    assert!(
        completed_sites >= 2,
        "{rel}: expected at least two Completed emitters, found {completed_sites}"
    );

    // THE CLAIM.
    let recomputed = code.matches("withheld: item_id < 0 || secrecy.keep_local_only()").count();
    assert_eq!(
        recomputed, 0,
        "{rel}: {recomputed} of the {completed_sites} Completed emitters recompute \
         `withheld` by calling keep_local_only() a second time, re-reading \
         secure_input_active() after store_transcription already read it. The flag is \
         system-wide and any application may lower it at any moment, so the two reads can \
         disagree and both directions are wrong: a local-only row reported as ordinary \
         (round 11's password toast, back again) or a replicated row reported as withheld \
         and therefore shown to the user not at all. Latch the flag in \
         sample_field_secrecy() and return the decision from store_transcription."
    );
}

// ---------------------------------------------------------------------------
// 6. `withheld` is honoured by three of the five surfaces that consume it.
// ---------------------------------------------------------------------------

/// Every consumer of `PipelineEvent::Completed` that renders `e.text` must
/// consult `e.withheld` first.
///
/// Round 12 added the field precisely because "every consumer that forgot to
/// check the convention rendered the transcript instead", and then patched two
/// of the four consumers that exist. `Compose.tsx` and `Onboarding.tsx` still
/// call `setResult(e.text)` unconditionally, and `Compose.tsx` renders the
/// result into the page WITH a "Copy result" button and the caption "Also
/// inserted at your cursor and saved to History", which for a withheld row is
/// false twice over.
///
/// `state.rs` emits with `app.emit(...)`, which broadcasts to every window, so
/// the main window sitting on the Compose tab in the background receives a
/// password-field dictation aimed at another application and paints it.
///
/// CONTROL, and it is the strong kind: three of the five files must PASS the
/// rule before the other two are allowed to fail. That proves the parser finds
/// `completed` handlers, finds `e.text` inside them, finds `e.withheld`, and
/// that the rule is satisfiable by real code in this repo.
#[test]
fn r13_plat_completed_text_reaches_two_unguarded_surfaces() {
    const CONSUMERS: [&str; 5] = [
        "src/App.tsx",
        "src/Hud.tsx",
        "src/views/History.tsx",
        "src/views/Compose.tsx",
        "src/views/Onboarding.tsx",
    ];

    let mut renders_and_guards: Vec<&str> = vec![];
    let mut renders_unguarded: Vec<&str> = vec![];
    let mut no_render: Vec<&str> = vec![];

    for rel in CONSUMERS {
        let text = std::fs::read_to_string(repo_root().join(rel))
            .unwrap_or_else(|e| panic!("{rel}: {e}"));
        let lines: Vec<&str> = text.lines().collect();
        let starts: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("e.kind === 'completed'"))
            .map(|(i, _)| i)
            .collect();
        assert!(
            !starts.is_empty(),
            "{rel}: no `e.kind === 'completed'` handler found, so this file contributes \
             nothing and the control is weakened"
        );
        let mut file_renders = false;
        let mut file_guarded = false;
        for &i in &starts {
            // The handler runs to the next `e.kind ===` test, capped so a
            // missing terminator cannot swallow the rest of the file.
            let end = lines
                .iter()
                .enumerate()
                .skip(i + 1)
                .find(|(_, l)| l.contains("e.kind === '"))
                .map(|(j, _)| j)
                .unwrap_or(lines.len())
                .min(i + 40);
            let region: String = lines[i..end].join("\n");
            let region_code: String = region
                .lines()
                .map(|l| l.split("//").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n");
            if region_code.contains("e.text") {
                file_renders = true;
                if region_code.contains("withheld") {
                    file_guarded = true;
                }
            } else if region_code.contains("withheld") {
                // A handler that consults `withheld` and never renders the text
                // is correct by construction (Hud.tsx's two).
                file_guarded = true;
            }
        }
        match (file_renders, file_guarded) {
            (true, true) => renders_and_guards.push(rel),
            (true, false) => renders_unguarded.push(rel),
            (false, _) => no_render.push(rel),
        }
    }

    // CONTROL 1: the parser can find a handler that renders the transcript AND
    // guards it, so a passing result is reachable.
    assert!(
        renders_and_guards.contains(&"src/App.tsx"),
        "control failed: src/App.tsx renders e.text behind an e.withheld guard and the \
         parser did not classify it that way. Classification was: guarded={renders_and_guards:?} \
         unguarded={renders_unguarded:?} no_render={no_render:?}"
    );
    // CONTROL 2: the parser can also find handlers that never render the text.
    assert!(
        no_render.len() + renders_and_guards.len() >= 3,
        "control failed: expected at least three of the five consumers to be safe, got \
         guarded={renders_and_guards:?} no_render={no_render:?}"
    );

    // THE CLAIM.
    assert!(
        renders_unguarded.is_empty(),
        "these Completed consumers render `e.text` without ever consulting `e.withheld`: \
         {renders_unguarded:?}. A dictation into a password field is dropped from History, \
         announced as withheld, and then painted in full into the main window anyway. \
         Compose.tsx additionally offers a Copy result button for it and captions it \
         'Also inserted at your cursor and saved to History'. Add `if (e.withheld) return;` \
         to each, as App.tsx already has."
    );
}

// ---------------------------------------------------------------------------
// 7. The widened secrecy window: what actually runs in it.
// ---------------------------------------------------------------------------

/// Round 12 moved `sample_field_secrecy()` from just below the
/// empty-after-cleanup branch to just above it, so more code now runs between
/// the sample and `inject_text`.
///
/// This test enumerates that code and pins it. On the path that reaches
/// injection the additional work is exactly `collect_low_confidence`, a pure
/// function over an in-memory transcript, plus the pre-existing
/// `frontmost_is_self()` read. The empty-after-cleanup branch does store and
/// emit, but it `return`s and never reaches an injection, so it cannot widen
/// the injection window at all.
///
/// So the widening is real but it is microseconds of arithmetic with no I/O, no
/// sleep, no window activation and no synthetic event. The sample is still
/// taken before anything Parle does could move the focus, which is the property
/// the comment claims.
///
/// This test PASSES today, and it FAILS the moment anything that can move focus
/// or block is introduced into the window. It is the pin, not a finding.
#[test]
fn r13_plat_nothing_in_the_secrecy_window_can_move_focus() {
    let rel = "src-tauri/src/pipeline.rs";
    let code = code_of(rel);
    let sample_at = index_of(&code, "let secrecy = sample_field_secrecy();", rel);
    let tail = &code[sample_at..];
    let inject_at = index_of(tail, "platform::imp::inject_text(", rel);
    let window = &tail[..inject_at];

    // CONTROL: the window is the one this test means, and it is not empty.
    assert!(
        window.contains("collect_low_confidence"),
        "{rel}: the window between the secrecy sample and inject_text does not contain \
         collect_low_confidence; this test is looking at the wrong region:\n{window}"
    );
    assert!(
        window.contains("if text.is_empty()"),
        "{rel}: the empty-after-cleanup branch is no longer inside the window; the sample \
         has moved again and this test's premise needs re-checking"
    );
    // CONTROL: the empty branch leaves rather than falling through to injection.
    assert!(
        window.matches("return;").count() >= 2,
        "{rel}: the empty-after-cleanup branch no longer returns, so it now runs BEFORE an \
         injection instead of instead of one"
    );

    // THE PIN: nothing in the window can move the focused element.
    for forbidden in [
        "sleep(",
        "activate_app(",
        "synth_return(",
        "synth_cmd_v(",
        "synth_ctrl_v(",
        "set_focus",
        "show()",
    ] {
        assert!(
            !window.contains(forbidden),
            "{rel}: `{forbidden}` now runs between sample_field_secrecy() and inject_text. \
             The sample describes the field as it was BEFORE that call, and the injection \
             will land wherever the focus ended up."
        );
    }
}
