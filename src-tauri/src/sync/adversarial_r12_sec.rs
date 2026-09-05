//! ADVERSARIAL REVIEW, ROUND 12 — secrets, secure fields, clipboard marking.
//!
//! Round 11 (`67ab14c`) is attacked first, per the handover's own instruction:
//! "the newest code is the most dangerous code".
//!
//! Every test asserts the contract the code OUGHT to hold. A failure is a
//! finding. Where an attack was tried and the code held, the test passes and
//! names the line it is pinning, so a later round cannot quietly undo it.
//!
//! Several tests here read SOURCE rather than calling the function. That is not
//! laziness: `platform::windows` does not compile on this host at all, and
//! `FieldSecrecy`, `sample_field_secrecy` and `store_transcription` are private
//! to `crate::pipeline`. Every one of those tests first asserts it FOUND the
//! construct it is measuring, so it cannot pass by finding nothing.
//!
//! Nothing here writes to the real clipboard, opens a socket, spawns a thread
//! that outlives a test, or sleeps.

#![cfg(test)]

use parle_core::history::Store;
use parle_core::types::TranscriptionResult;
use std::path::{Path, PathBuf};

const A: &str = "11111111-1111-4111-8111-111111111111";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

/// Source with `//` comments stripped, so prose cannot satisfy a guard that is
/// looking for code.
fn code_of(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The body of a free function, comments stripped, from `fn NAME(` to the next
/// top-level `fn`. Panics if the function is not there, which is the
/// found-something control for every caller.
fn body_of(rel: &str, decl: &str) -> String {
    let code = code_of(rel);
    code.split(decl)
        .nth(1)
        .and_then(|s| s.split("\nfn ").next())
        .and_then(|s| s.split("\npub fn ").next())
        .unwrap_or_else(|| panic!("{decl} is not in {rel}"))
        .to_string()
}

fn dictation(text: &str) -> TranscriptionResult {
    TranscriptionResult {
        raw_text: text.into(),
        text: text.into(),
        language: None,
        model_id: "test".into(),
        duration_ms: 1000,
        transcribe_ms: 10,
        segments: vec![],
        trimmed: vec![],
        low_confidence: vec![],
        cleanup_tier: 0,
        refine: None,
    }
}

// ---------------------------------------------------------------------------
// R12-A. Windows: a dictation into a confirmed password field is handed to
//        Cloud Clipboard, which is off the machine.
//
// `platform::windows::inject_text` (windows.rs:507) calls `write_clipboard`,
// and round 11 redefined `write_clipboard` to declare NO exclusion formats. It
// never asks `focused_field_is_secure()` at all, unlike its macOS sibling,
// which round 11 gave exactly that gate.
//
// Before round 11 the single `write_clipboard` unconditionally declared
// `CanUploadToCloudClipboard = 0` and `CanIncludeInClipboardHistory = 0`
// (`git show 67ab14c^:src-tauri/src/platform/windows.rs`), so a password-field
// dictation was withheld from Win+V and from Cloud Clipboard. Splitting the
// function fixed a real defect on the palette-Copy path and opened this one on
// the injection path, which is the shipped default.
//
// The pipeline's own verdict on that dictation is `FieldSecrecy::Secure`:
// `drop_entirely()` — never store it, never replicate it. The platform layer
// then uploads it to Microsoft.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_windows_injection_never_asks_whether_it_is_a_password_field() {
    let win = body_of("src-tauri/src/platform/windows.rs", "pub fn inject_text(");
    let mac = body_of("src-tauri/src/platform/macos.rs", "pub fn inject_text(");

    // FOUND-SOMETHING CONTROL. The needle exists and this test can see it: the
    // macOS sibling has the gate. If this ever fails, the assertion below is
    // measuring nothing.
    assert!(
        mac.contains("if field == Some(true)"),
        "the macOS injection gate is gone, so this test can no longer discriminate"
    );
    assert!(
        win.contains("write_clipboard"),
        "the Windows injection path no longer writes the clipboard at all"
    );

    assert!(
        // Round 14: both platforms read the PIPELINE's sample rather than
        // probing again, so the gate is `view.is_secure`, not a live probe.
        win.contains("view.is_secure == Some(true)"),
        "`inject_text` on Windows writes the transcript with `write_clipboard`, which since \
         round 11 declares no exclusion formats. A dictation the pipeline classifies \
         `FieldSecrecy::Secure` and refuses to store is therefore published to Win+V history \
         AND to Cloud Clipboard, which uploads it to Microsoft and syncs it to the user's \
         other Windows machines. Before 67ab14c every write carried \
         CanUploadToCloudClipboard = 0"
    );
}

// ---------------------------------------------------------------------------
// R12-B. Windows: the restore path strips another app's exclusion markers.
//
// `inject_text` snapshots the clipboard with `read_clipboard()` — the reader
// that does NOT honour the exclusion formats — and later re-publishes it with
// bare `write_clipboard(&prev)`, which since round 11 declares none.
//
// So: the user copies a password out of KeePassXC (which sets
// `ExcludeClipboardContentFromMonitorProcessing`), dictates anywhere with the
// shipped "restore clipboard" default on, and Parle puts that password back on
// the clipboard with the owner's own "do not keep this, do not upload this"
// statement removed. Win+V keeps it; Cloud Clipboard uploads it.
//
// macOS carries the marking across the restore (`PENDING_RESTORE_CONCEALED`,
// added in round 11 for precisely this reason). Windows was not mirrored.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_windows_restore_puts_back_what_it_found_with_the_marking_it_found() {
    let win = body_of("src-tauri/src/platform/windows.rs", "pub fn inject_text(");
    let mac = code_of("src-tauri/src/platform/macos.rs");

    // FOUND-SOMETHING CONTROL: the pattern exists on the sibling platform.
    assert!(
        mac.contains("PENDING_RESTORE_CONCEALED")
            && mac.contains("write_clipboard_marked(&prev, was_concealed)"),
        "macOS no longer carries the marking across a restore; this test cannot discriminate"
    );
    assert!(
        win.contains("restore"),
        "the Windows injection path no longer has a restore branch"
    );

    assert!(
        !win.contains("write_clipboard(&prev)"),
        "the Windows restore re-publishes the user's previous clipboard with `write_clipboard`, \
         which declares no exclusion formats. A secret copied from a password manager is put \
         back with the owner's own exclusion markers stripped, so Win+V keeps it and Cloud \
         Clipboard uploads it off the machine. macOS solved exactly this with \
         PENDING_RESTORE_CONCEALED"
    );
    // The fix taken is PARITY WITH macOS, which is the other of the two the
    // finding offered, and it is the one that does not lose the user's data.
    //
    // Refusing to READ an excluded clipboard leaves `previous` empty, and the
    // injection has already overwritten the clipboard, so the user's password
    // is destroyed rather than put back. macOS reads the text, probes the
    // marking separately with `clipboard_is_concealed`, and restores both. The
    // exclusion markers are a statement about what may be PERSISTED and
    // ROAMED, which restoring them honours, not about what may be held in
    // memory for `restore_delay_ms`.
    assert!(
        win.contains("clipboard_is_excluded()"),
        "the Windows restore does not probe the marking at all, so it cannot put back what it \
         found: whatever it restores is unmarked, and the owner's 'do not keep this, do not \
         upload this' statement is deleted in the process"
    );
    // Round 14 gave Windows the chained-restore guard macOS already had, so
    // the marking travels through `PENDING_RESTORE` rather than a local.
    assert!(
        win.contains("*pending = Some((prev, previous_excluded));")
            && win.contains("write_clipboard_inner(&prev, excluded);"),
        "the Windows restore probes the marking and then does not apply it"
    );
}

// ---------------------------------------------------------------------------
// R12-C. macOS: when secure event input is up and the accessibility probe
//        cannot answer, the same text is judged two opposite ways.
//
// `FieldSecrecy::conceal_clipboard()` (pipeline.rs:124) is TRUE for
// `Unknown && secure_input_active()`, and `keep_local_only()` is TRUE for the
// same pair — the row is kept off the wire because it MIGHT be a password.
//
// In that exact state `inject_text` takes the `keystrokes_blocked` path and
// writes `write_clipboard_marked(text, false)`: unconcealed. Alfred, Raycast,
// Maccy and every other clipboard manager on the machine keep the string Parle
// judged too sensitive to send to the user's own second device.
//
// Before round 11 that branch was `if secure_input_active() {
// write_clipboard_marked(text, true); ... }` — concealed. This is a regression
// introduced by the fix for "the global flag broke insert-at-cursor", which was
// itself a correct finding.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_macos_conceals_when_the_pipeline_says_it_would_conceal() {
    let pipeline = code_of("src-tauri/src/pipeline.rs");
    let mac = body_of("src-tauri/src/platform/macos.rs", "pub fn inject_text(");

    // FOUND-SOMETHING CONTROLS.
    assert!(
        pipeline.contains("fn conceal_clipboard") && pipeline.contains("secure_input_active()"),
        "conceal_clipboard no longer counts the global flag; nothing to disagree with"
    );
    assert!(
        mac.contains("write_clipboard_marked(text, true)"),
        "no branch of macOS inject_text conceals anything, so `false` below means nothing"
    );
    let blocked = mac
        .split("let keystrokes_blocked")
        .nth(1)
        .expect("the keystrokes_blocked branch exists")
        .split("let previous = read_clipboard();")
        .next()
        .expect("the branch ends before the Cmd-V path");
    assert!(
        blocked.contains("write_clipboard_marked"),
        "the keystrokes-blocked branch no longer writes the clipboard"
    );

    assert!(
        !blocked.contains("write_clipboard_marked(text, false)"),
        "with secure event input up and the accessibility probe unable to answer, \
         `FieldSecrecy::conceal_clipboard()` is TRUE and `keep_local_only()` is TRUE — the \
         pipeline refuses to replicate the row because it may be a password — while this \
         branch puts the identical text on the pasteboard UNCONCEALED. Two answers from one \
         piece of evidence, in code written in the same commit. Pre-67ab14c this branch \
         concealed"
    );
}

// ---------------------------------------------------------------------------
// R12-D. Windows marks its own write AFTER releasing the clipboard.
//
// Round 11 moved the macOS `OUR_LAST_WRITE.store` to sit immediately after
// `declareTypes_owner` and BEFORE the payload, and wrote out why: "Storing it
// afterwards left a window in which the pasteboard had already changed and the
// atomic still held the previous value, so a monitor poll landing in it saw
// `we_wrote_change` as false and captured Parle's own write."
//
// It then introduced the identical mechanism on Windows in the same commit and
// put the store AFTER `CloseClipboard()`, outside the session, reading a
// sequence number that by then may belong to somebody else's write. Two
// failures from one line: our own dictation gets captured as a clipboard row
// (and replicated), and a real user copy that landed in the window is skipped
// as "ours".
//
// Self-capture is the path that walks a withheld password back into the
// history the pipeline just refused to put it in.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_our_own_write_is_marked_before_the_change_is_visible() {
    let mac = body_of("src-tauri/src/platform/macos.rs", "fn write_clipboard_impl(");
    let win = body_of("src-tauri/src/platform/windows.rs", "fn write_clipboard_inner(");

    // FOUND-SOMETHING CONTROL: macOS holds the property, so the shape is real.
    let m_store = mac.find("OUR_LAST_WRITE.store(").expect("macOS marks its own write");
    let m_payload = mac.find("pb.setString_forType(").expect("macOS writes a payload");
    assert!(
        m_store < m_payload,
        "round 11's macOS ordering fix is gone; this test cannot discriminate"
    );

    let w_store = win.find("OUR_LAST_WRITE.store(").expect("Windows marks its own write");
    let w_close = win.find("CloseClipboard()").expect("Windows closes the clipboard");
    assert!(
        w_store < w_close,
        "`write_clipboard_inner` stores OUR_LAST_WRITE after `CloseClipboard()`, so between \
         releasing the clipboard and recording the sequence number the monitor can see the \
         change, find `we_wrote_change` false and capture Parle's own write into history, \
         where it replicates. It also reads `GetClipboardSequenceNumber()` outside the \
         session, so another process's write can be recorded as ours and its content then \
         skipped. This is the exact window round 11 closed on macOS in the same commit"
    );
}

// ---------------------------------------------------------------------------
// R12-E. There are THREE places that store a dictation, and round 11's single
//        gate covers two of them.
//
// The commit message: "One place, so the two dictation paths cannot drift: the
// secure-field drop was added to one of them and missed on the other once
// already." There is a third, at pipeline.rs:478-500. When cleanup empties the
// text but the raw transcript is not empty, the raw is written straight to the
// store with a bare `insert_transcription`, BEFORE `sample_field_secrecy()` is
// ever called, with no secrecy argument and no local-only flag.
//
// So a dictation into a password field whose cleaned text collapses to nothing
// is stored in full — as `raw_text` — and replicated to every paired device,
// on a branch whose own comment says "Nothing is ever lost".
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_every_dictation_reaches_the_store_through_the_one_gate() {
    let code = code_of("src-tauri/src/pipeline.rs");
    let (before, rest) = code
        .split_once("fn store_transcription(")
        .expect("the single gate exists");
    let (gate, after) = rest.split_once("\nimpl Pipeline {").expect("the gate ends");

    // FOUND-SOMETHING CONTROL: the needle is findable, inside the gate.
    assert!(
        gate.contains("insert_transcription"),
        "the gate no longer inserts anything, so counting call sites proves nothing"
    );
    assert!(
        gate.contains("insert_transcription_local_only"),
        "the gate lost the local-only outcome"
    );

    let stray = before.matches("insert_transcription").count()
        + after.matches("insert_transcription").count();
    assert_eq!(
        stray, 0,
        "{stray} call(s) to `insert_transcription` bypass `store_transcription` entirely. The \
         empty-after-cleanup branch (pipeline.rs, \"kept raw in history\") runs BEFORE \
         `sample_field_secrecy()` and stores the RAW transcript with no secrecy argument, so a \
         password-field dictation that cleanup empties is stored and replicated. Round 11's \
         own commit message claims there are two paths and one gate; there are three paths"
    );
}

// ---------------------------------------------------------------------------
// R12-F. Does the new user-facing message leak? No, and pin it.
//
// The brief's question. Both notices are literal constants: neither
// interpolates the transcript, the app id, or the app name. This test PASSES
// and exists so that a later round adding "…from 1Password" is caught.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_the_withholding_notice_names_no_secret_and_no_app() {
    let code = code_of("src-tauri/src/pipeline.rs");
    let (_, rest) = code.split_once("fn store_transcription(").expect("the gate exists");
    let (gate, _) = rest.split_once("\nimpl Pipeline {").expect("the gate ends");

    // Both notices are string LITERALS, not formatted.
    // Round 13 split this in two, because the empty-after-cleanup branch never
    // copies and the message claimed it did. Both arms are still LITERALS: the
    // branch chooses between them, it does not build one.
    assert!(
        gate.contains(r#""Password field: replaced your clipboard, not saved to History".into()"#)
            && gate.contains(r#""Password field: not saved to History".to_string()"#),
        "the password-field notice is no longer a bare literal; check what it now interpolates"
    );
    assert!(
        gate.contains(r#"Some("Saved on this device only: this may be a password field".into())"#),
        "the local-only notice is no longer a bare literal; check what it now interpolates"
    );
    // FOUND-SOMETHING CONTROL: the gate really does hold the attribution and
    // the transcript, so "they are not in the notice" is a fact about the
    // notice rather than about an empty region.
    assert!(
        gate.contains("app_name.as_deref()") && gate.contains("tr,"),
        "the gate no longer sees the transcript or the app name; this test proves nothing"
    );

    // Nothing in the gate builds a user-visible string at all: no `format!`,
    // and therefore no placeholder into which the transcript, the app id or the
    // app name could be interpolated. Both notices are the literals above.
    assert!(
        !gate.contains("format!"),
        "the gate now formats a string; check whether the transcript or the app reaches the user"
    );
    for literal in [
        "Password field: replaced your clipboard, not saved to History",
        "Password field: not saved to History",
        "Saved on this device only: this may be a password field",
    ] {
        assert!(!literal.contains('{'), "{literal} carries a placeholder");
    }
}

// ---------------------------------------------------------------------------
// R12-G. The notice is emitted and then immediately superseded by the
//        transcript it was withholding.
//
// `store_transcription` returns a notice, the pipeline emits
// `PipelineEvent::Empty { reason }`, and the very next statement emits
// `PipelineEvent::Completed { item_id: -1, text, .. }` carrying the full
// password.
//
// `src/Hud.tsx:45` sets the outcome from `empty`; line 47 overwrites it from
// `completed`. `src/App.tsx:46` shows the reason as a toast; lines 35-45 show a
// toast built from `e.text` — the first 42 characters of the password — and the
// completed handler never looks at `item_id`, which is the only signal in the
// event that the row was withheld.
//
// So round 11's headline UX fix ("the user is told") is overwritten within one
// event, and the surface that overwrites it renders the secret.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_the_completed_event_does_not_render_a_withheld_transcript() {
    let pipeline = code_of("src-tauri/src/pipeline.rs");
    let app = code_of("src/App.tsx");

    // FOUND-SOMETHING CONTROLS.
    let notice = pipeline.find("PipelineEvent::Empty { reason }").expect("the notice is emitted");
    let completed = pipeline[notice..]
        .find("PipelineEvent::Completed")
        .expect("Completed follows the notice");
    assert!(completed > 0, "the two events are emitted from the same block");
    assert!(
        app.contains("e.text.length > 42") && app.contains("showToast"),
        "App.tsx no longer builds a preview toast from the transcript; this test is stale"
    );

    let handler = app
        .split("if (e.kind === 'completed')")
        .nth(1)
        .and_then(|s| s.split("if (e.kind === 'empty')").next())
        .expect("the completed handler exists");
    // The fix taken is the explicit flag the finding offered as its second
    // option, rather than the `item_id < 0` convention. It is the better of the
    // two for exactly the reason this test exists: every consumer that had to
    // remember the convention forgot it, and a field that must be read is
    // harder to forget than a sentinel that carries no name.
    assert!(
        handler.contains("e.withheld") || handler.contains("item_id"),
        "the `completed` handler renders `e.text` without ever consulting `item_id`, which is \
         the only thing in the event that says the row was withheld. A password-field \
         dictation therefore replaces the \"Password field: copied, not saved to History\" \
         toast with `Copied \"<the first 42 characters of the password>\"`, and the HUD \
         replaces it with \"Copied. Press paste to insert (secure field)\". Round 11 added the \
         notice so the user would be told; it survives one event"
    );
}

// ---------------------------------------------------------------------------
// R12-H. Is a local-only row actually withheld? Yes. Pin it properly, from
//        both sides, because the whole third answer rests on one SQL clause.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_a_local_only_row_is_kept_locally_and_never_offered() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(A);
    let ordinary = s.insert_transcription(&dictation("buy milk"), None, None).unwrap();
    let secret = s
        .insert_transcription_local_only(&dictation("correct horse battery"), None, None)
        .unwrap();
    assert!(ordinary > 0 && secret > 0, "both rows must actually be written");

    // NOT VACUOUS: the ordinary row IS offered, so the filter is not simply
    // returning nothing for an unrelated reason.
    let offered: Vec<String> =
        s.items_since(A, 0, 100).unwrap().into_iter().map(|r| r.text).collect();
    assert_eq!(
        offered,
        vec!["buy milk".to_string()],
        "the local-only row was offered to a peer"
    );

    // And it is genuinely kept: the user still has their dictation locally.
    let local: Vec<String> =
        s.recent(None, 100).unwrap().into_iter().map(|i| i.text).collect();
    assert!(
        local.iter().any(|t| t == "correct horse battery"),
        "the local-only row was not kept at all, which is round 9's failure again"
    );
}

/// The other side of the same rule: a local-only row must stay withheld when
/// the peer's cursor is re-based, which is the round-11 recovery path that
/// restarts a source from zero.
#[test]
fn r12_sec_a_local_only_row_stays_withheld_when_a_source_restarts_from_zero() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(A);
    s.insert_transcription_local_only(&dictation("hunter2"), None, None).unwrap();
    s.insert_transcription(&dictation("ordinary"), None, None).unwrap();

    // Found-something control, then the rule, at every cursor a peer can name.
    let all = s.items_from(A, 0, "", 100).unwrap();
    assert_eq!(all.len(), 1, "exactly the ordinary row is servable");
    for cursor in [0i64, 1, i64::MAX / 2] {
        let page = s.items_from(A, cursor, "", 100).unwrap();
        assert!(
            page.iter().all(|r| r.text != "hunter2"),
            "a local-only row was served from cursor {cursor}"
        );
    }
}

// ---------------------------------------------------------------------------
// R12-I. A local-only row is committed replicable and flagged afterwards.
//
// `insert_transcription_local_only` (history.rs:531) runs an INSERT and then a
// separate `UPDATE items SET local_only = 1`. There is no transaction, and
// rusqlite autocommits each statement, so between them the suspected password
// is on disk as an ordinary, servable row. The store mutex keeps a concurrent
// `serve` out, so this is not a race between threads — it is a crash or power
// loss in that window making the row permanently replicable, on the one code
// path whose entire purpose is that the row must never leave the machine.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_a_local_only_row_is_never_on_disk_replicable() {
    let hist = code_of("crates/parle-core/src/history.rs");
    let f = hist
        .split("pub fn insert_transcription_local_only(")
        .nth(1)
        .and_then(|s| s.split("\n    pub fn ").next())
        .expect("the local-only insert exists");

    // FOUND-SOMETHING CONTROL: the file does know how to open a transaction, so
    // "no transaction here" is a choice and not an unavailable API.
    assert!(
        hist.contains("unchecked_transaction()") || hist.contains(".transaction()"),
        "history.rs has no transaction anywhere; this test cannot discriminate"
    );
    assert!(
        f.contains("local_only"),
        "the local-only insert no longer sets the flag"
    );

    let atomic = f.contains("transaction")
        || (f.contains("INSERT") && f.contains("local_only") && !f.contains("UPDATE items SET"));
    assert!(
        atomic,
        "`insert_transcription_local_only` INSERTs the row and only then UPDATEs local_only = 1, \
         with no transaction. Each statement autocommits, so the suspected password exists on \
         disk as an ordinary servable row first. A crash or power loss in that window leaves it \
         replicable for ever"
    );
}

// ---------------------------------------------------------------------------
// R12-J. Attacks that were tried and held. These PASS.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_excluded_apps_are_withheld_by_id_by_name_and_by_case() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(A);
    s.insert_clipboard("hunter2", Some("org.keepassxc.keepassxc"), None).unwrap();
    s.insert_clipboard("totp 194 552", None, Some("Authy Desktop")).unwrap();
    s.insert_clipboard("KeePassXC row", Some("KeePassXC.exe"), None).unwrap();
    s.insert_clipboard("ordinary", Some("com.apple.Notes"), Some("Notes")).unwrap();
    assert_eq!(s.items_since(A, 0, 100).unwrap().len(), 4, "all four start servable");

    // Stored in a DIFFERENT case from every captured value, which is the
    // agreement `set_excluded_apps`, SQLite's LOWER() and the capture gate must
    // share.
    s.set_excluded_apps(vec![
        "ORG.KEEPASSXC.KEEPASSXC".into(),
        "Authy Desktop".into(),
        "keepassxc.EXE".into(),
    ]);
    let out: Vec<String> =
        s.items_since(A, 0, 100).unwrap().into_iter().map(|r| r.text).collect();
    assert_eq!(out, vec!["ordinary".to_string()], "an excluded row still leaves");
}

/// The residual, pinned so it is not mistaken for coverage: a capture with NO
/// attribution at all cannot be excluded by a list of app names, and IS served.
/// `frontmost_app()` returns `(None, None)` when the frontmost application has
/// no bundle id, and `clipboard_owner_app()` falls back to it on Windows when
/// `GetClipboardOwner` fails.
#[test]
fn r12_sec_an_unattributed_capture_is_replicated_and_that_is_the_residual() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(A);
    s.insert_clipboard("hunter2", None, None).unwrap();
    s.set_excluded_apps(vec!["org.keepassxc.keepassxc".into(), "keepassxc.exe".into()]);
    assert_eq!(
        s.items_since(A, 0, 100).unwrap().len(),
        1,
        "PREMISE GONE: unattributed rows are now withheld, which would be the stronger rule"
    );
}

/// Round 10's settings migration still reaches an existing install, and the
/// exclusion list the app ships is still the one the store is given at launch.
#[test]
fn r12_sec_the_shipped_exclusion_list_still_reaches_an_old_install() {
    use parle_core::settings::{Settings, SETTINGS_VERSION};
    let dir = std::env::temp_dir().join(format!("parle-r12-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("settings.json");
    std::fs::write(
        &p,
        r#"{"version":1,"history":{"clipboard_capture":true,"excluded_apps":["com.1password.1password"]}}"#,
    )
    .unwrap();
    let loaded = Settings::load(&p).unwrap();
    assert_eq!(loaded.version, SETTINGS_VERSION);
    for must in ["com.apple.Passwords", "org.keepassxc.keepassxc", "KeePass.exe"] {
        assert!(
            loaded.history.excluded_apps.iter().any(|a| a == must),
            "{must} did not reach an existing install"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
