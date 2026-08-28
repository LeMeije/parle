//! ADVERSARIAL REVIEW, ROUND 11 — secrets, security and cross-platform.
//!
//! Round 10's fixes are attacked first, per the handover's own instruction.
//!
//! Nothing here changes production code. Nothing here opens a socket, spawns a
//! thread that outlives a test, sleeps, writes to the real clipboard or touches
//! the real keychain. The two `#[ignore]`d entries are DIAGNOSTICS that read a
//! process-global OS flag and time one accessibility round trip; they are not
//! part of the suite and a failure there is information.
//!
//! Pass criteria exercised:
//!   E. size limits bound allocation
//!   H. nothing marked secret reaches the wire
//!   I. keys never in settings.json
//!   J. the product still works

#![cfg(test)]

use parle_core::history::Store;
use parle_core::settings::Settings;
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
/// The file verbatim, comments included, for assertions about doc comments.
fn read_src(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn code_of(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tmp_settings(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("parle-r11-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("settings.json");
    std::fs::write(&p, body).unwrap();
    p
}

// ---------------------------------------------------------------------------
// R11-A. Does `Settings::migrate` actually fire for a settings.json that
// exists on a real machine today?
//
// The exact shape below is the one on this development Mac at the time of
// review: `"version": 1`, ten exclusions, no `com.apple.Passwords`. If the
// union does not reach it, round 10's headline secret fix reaches nobody.
// ---------------------------------------------------------------------------

const REAL_SHAPED_V1: &str = r#"{
  "version": 1,
  "history": {
    "clipboard_capture": true,
    "excluded_apps": [
      "com.1password.1password",
      "com.agilebits.onepassword7",
      "1Password.exe",
      "com.bitwarden.desktop",
      "Bitwarden.exe",
      "com.lastpass.LastPass",
      "LastPass.exe",
      "org.keepassxc.keepassxc",
      "KeePassXC.exe",
      "com.dashlane.Dashlane"
    ]
  }
}"#;

#[test]
fn r11_migrate_reaches_a_settings_file_that_already_exists() {
    let p = tmp_settings("v1", REAL_SHAPED_V1);
    let loaded = Settings::load(&p).unwrap();
    let list = &loaded.history.excluded_apps;
    for must in ["com.apple.Passwords", "com.apple.keychainaccess", "KeePass.exe"] {
        assert!(
            list.iter().any(|a| a == must),
            "{must} did not reach an existing install"
        );
    }
    // The user's own entries survive: a union, not a replacement.
    assert!(list.iter().any(|a| a == "com.dashlane.Dashlane"));
    // And no duplicates were appended for the ones already present.
    let ones = list.iter().filter(|a| a.as_str() == "com.1password.1password").count();
    assert_eq!(ones, 1, "the union must not duplicate what is already there");
    assert_eq!(loaded.version, parle_core::settings::SETTINGS_VERSION);
}

/// A user who deliberately removes an entry must get it back ONCE, not on
/// every launch. That turns on the migrated version being persisted, so this
/// pins the round trip: load (migrate) -> save -> load again.
#[test]
fn r11_a_deliberate_removal_survives_the_second_launch() {
    let p = tmp_settings("removal", REAL_SHAPED_V1);
    let mut first = Settings::load(&p).unwrap();
    first.history.excluded_apps.retain(|a| a != "com.apple.Passwords");
    first.save(&p).unwrap();

    let second = Settings::load(&p).unwrap();
    assert!(
        !second.history.excluded_apps.iter().any(|a| a == "com.apple.Passwords"),
        "a deliberate removal was undone on the next launch"
    );
}

/// What happens when `version` is ABSENT.
///
/// `#[serde(default)]` sits on the CONTAINER, so an absent `version` takes
/// `Settings::default().version`, which is `SETTINGS_VERSION` — the newest, not
/// the oldest. A file with no version therefore used to skip the migration
/// entirely, and this test pinned that, noting it was unreachable in practice
/// and that "absent means oldest" is the intuition a later round would bring.
///
/// ROUND 12 INVERTED IT. The union no longer consults `version` at all: it
/// records which shipped defaults an install has already been OFFERED. That
/// closes this hole as a side effect, because a file with no record is offered
/// them regardless of what its version field says or does not say.
#[test]
fn r11_absent_version_does_not_decide_who_gets_protected() {
    let p = tmp_settings("nover", r#"{"history":{"excluded_apps":["com.mine"]}}"#);
    let loaded = Settings::load(&p).unwrap();
    assert!(
        loaded.history.excluded_apps.iter().any(|x| x == "com.mine"),
        "the user's own entry was dropped by the migration"
    );
    assert!(
        loaded.history.excluded_apps.len() > 1,
        "a settings file with no version field is still skipped by the exclusion union, so \
         whether a machine protects its password managers depends on a field the user never \
         sees and the app cannot tell apart from 'already current'"
    );
}

// ---------------------------------------------------------------------------
// R11-B. Criterion I, verified independently: no key in settings.json.
// ---------------------------------------------------------------------------

#[test]
fn r11_no_key_material_can_be_serialised_into_settings() {
    let mut s = Settings::default();
    s.sync.device_id = A.into();
    s.sync.device_name = "Ben's MacBook Pro".into();
    s.sync.paired.push(parle_core::settings::PairedDevice {
        id: "22222222-2222-4222-8222-222222222222".into(),
        name: "G14".into(),
        last_seen: Some(1_700_000_000_000),
    });
    s.sync.resend_owed.push(parle_core::settings::ResendDebt {
        device_id: "22222222-2222-4222-8222-222222222222".into(),
        from: 0,
    });
    let json = serde_json::to_string(&s).unwrap();

    // Field names, checked over the SYNC subtree. The whole document carries a
    // hotkey binding whose field is literally `key`, which is not key material;
    // scoping the check is the difference between a guard and a tripwire.
    let sync_json = serde_json::to_string(&s.sync).unwrap().to_ascii_lowercase();
    for banned in ["key", "secret", "psk", "password", "shared"] {
        assert!(
            !sync_json.contains(&format!("\"{banned}")),
            "sync settings carry a field named {banned:?}"
        );
    }
    // No 64-hex run anywhere: that is the shape `keystore::to_hex` writes.
    let bytes = json.as_bytes();
    let mut run = 0usize;
    for b in bytes {
        if b.is_ascii_hexdigit() {
            run += 1;
            assert!(run < 64, "a 64-hex-digit run appeared in settings.json");
        } else {
            run = 0;
        }
    }
    // The guard found something to look at.
    assert!(json.contains("paired"), "the roster must actually be serialised");
}

/// Unpair must DESTROY the key, not merely hide the device. Pinned against the
/// source because the real keystore is out of bounds here.
#[test]
fn r11_unpair_deletes_the_key_before_it_reports_success() {
    let code = code_of("src-tauri/src/sync/manager.rs");
    let at = code.find("pub fn unpair").expect("unpair exists");
    let body = &code[at..at + 700];
    let del = body.find("keystore::delete").expect("unpair must delete the key");
    let retain = body.find("paired.retain").expect("unpair must drop the roster entry");
    assert!(del < retain, "the key must be destroyed before the roster is edited");
    assert!(
        body[del..].contains("map_err") || body[del..].contains('?'),
        "a failed delete must not be reported as a successful unpair"
    );
}

// ---------------------------------------------------------------------------
// R11-C. Criterion H, verified independently: the outbound exclusion filter
// applies to rows captured BEFORE the app was excluded, and folds case the
// same way SQLite does.
// ---------------------------------------------------------------------------

#[test]
fn r11_excluding_an_app_stops_rows_already_captured_from_it() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(A);
    s.insert_clipboard("hunter2", Some("com.1Password.1Password"), Some("1Password"))
        .unwrap();
    s.insert_clipboard("ordinary note", Some("com.apple.Notes"), Some("Notes"))
        .unwrap();

    // Before: both rows are servable, so the guard is not vacuous.
    let before: Vec<String> =
        s.items_since(A, 0, 100).unwrap().into_iter().map(|r| r.text).collect();
    assert_eq!(before.len(), 2, "both rows must be servable before exclusion");

    // The list is stored with a DIFFERENT case from the captured app id, which
    // is the case-folding agreement the three call sites have to share.
    s.set_excluded_apps(vec!["COM.1PASSWORD.1PASSWORD".into()]);
    let after: Vec<String> =
        s.items_since(A, 0, 100).unwrap().into_iter().map(|r| r.text).collect();
    assert_eq!(after, vec!["ordinary note".to_string()], "the secret still leaves");
}

#[test]
fn r11_excluding_by_app_name_works_too() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(A);
    s.insert_clipboard("totp 194 552", None, Some("Authy Desktop")).unwrap();
    assert_eq!(s.items_since(A, 0, 100).unwrap().len(), 1);
    s.set_excluded_apps(vec!["authy desktop".into()]);
    assert!(
        s.items_since(A, 0, 100).unwrap().is_empty(),
        "a row with no app id but an excluded app NAME still leaves"
    );
}

// ---------------------------------------------------------------------------
// R11-D. The secure-field gate is sampled AFTER the text has been injected.
//
// `Pipeline::finish` calls `platform::imp::inject_text(...)` — which pastes,
// optionally sleeps 160ms and posts Return — and only then calls
// `into_secure_field()` to decide whether to store the row. Pressing Return in
// a password field is what submits a login, and submitting moves focus off the
// secure element. The same file latches the frontmost app at recording START
// for exactly this reason ("focus may move later"); the secure decision was not
// given the same treatment.
//
// This is a source-order guard because the ordering IS the defect and the
// alternative needs a live focused password field. It asserts it found both
// call sites first, so it cannot pass by finding nothing.
// ---------------------------------------------------------------------------

#[test]
fn r11_the_secure_gate_is_decided_after_the_paste_not_before() {
    // INVERTED. The finding was subtle and correct: `inject_text` pastes,
    // waits, and posts Return when "press enter after paste" is on, and
    // submitting a login is exactly what moves focus off the password field.
    // Asking the accessibility tree afterwards got an ordinary element back and
    // stored the password.
    //
    // The same file already latches the frontmost app at recording START for
    // this reason. The secrecy sample now gets the same treatment.
    let code = code_of("src-tauri/src/pipeline.rs");
    let sample = code.find("sample_field_secrecy()").expect("the sample is taken");
    let inject = code.find("imp::inject_text(").expect("injection happens");
    assert!(
        sample < inject,
        "the secure-field question is still asked after the paste, by which time pressing \
         Return has moved focus off the password field"
    );
    // And it is sampled ONCE and carried, not re-asked.
    assert!(
        code.contains("secrecy.conceal_clipboard()") && code.contains("secrecy,"),
        "the sampled value must be carried to both the clipboard marking and the store, or the \
         two can disagree about the same dictation"
    );
    // Round 13: carrying the VARIANT was not enough, because the predicates
    // read the global flag live, so two calls in one dictation could disagree.
    // The flag is sampled with the probe and carried in the variant.
    assert!(
        code.contains("FieldSecrecy::Unknown { secure_input: platform::imp::secure_input_active() }"),
        "the secure-input flag is read live inside the predicates rather than sampled once, so \
         the same dictation can be classified two different ways"
    );
}

/// The injection path still asks the process-global flag that round 10 proved
/// answers the wrong question. With a password manager merely running it takes
/// the ClipboardOnly branch for EVERY dictation, so paste-at-cursor never
/// happens and every transcript comes back "manual paste required".
#[test]
fn r11_injection_still_keys_off_the_discredited_global_flag() {
    // INVERTED. The finding was right and was the same mistake round 10 fixed
    // in the sibling function: the storage gate stopped keying off the
    // process-global flag and the INJECTION gate kept it as its first line, so
    // "insert at cursor" never fired while a password manager was running.
    //
    // The fix separates two questions that branch had conflated:
    //
    //   * "Is this a password field?" is `focused_field_is_secure()`, and the
    //     answer decides whether to hand the text over on the clipboard.
    //   * "Will the OS swallow a synthetic keystroke?" IS the global flag, and
    //     that is a legitimate use of it: secure event input really does
    //     suppress the Cmd-V this path would post.
    //
    // The second no longer means "give up". Accessibility insertion is not a
    // synthetic keystroke and is not suppressed, so it is tried first when the
    // flag is up, regardless of the user's preference: the alternative on that
    // path is not "paste normally", it is "do nothing".
    let code = code_of("src-tauri/src/platform/macos.rs");
    let f = code
        .split("pub fn inject_text(")
        .nth(1)
        .and_then(|s| s.split("\nfn ").next())
        .expect("inject_text is in the file");

    // Round 13 hoisted the probe to `let field = focused_field_is_secure();`
    // so it is read ONCE per injection. The gate is the comparison.
    assert!(
        f.contains("field == Some(true)") || f.contains("focused_field_is_secure() == Some(true)"),
        "the injection gate does not ask whether this is actually a password field"
    );
    // Round 14 moved the probe out of the platform layer entirely: the
    // pipeline samples once and passes the answer down in `FieldView`. That is
    // strictly stronger than probing once here, because the platform and the
    // pipeline can no longer form two opinions about one dictation.
    assert!(
        f.contains("let field = view.is_secure;"),
        "the injection path forms its own opinion of the field instead of using the one the \
         pipeline already sampled, so the two can disagree about the same dictation"
    );
    // The FIELD answer is settled before the flag is consulted. It arrives
    // pre-sampled now, so the ordering is between using it and reading the
    // flag, rather than between two probes.
    let secure_at = f.find("let field = view.is_secure;").expect("checked above");
    let flag_at = f
        .find("let keystrokes_blocked = secure_input_active();")
        .expect("the flag is still consulted");
    assert!(
        secure_at < flag_at,
        "the global flag is still consulted before the focused-field check, so it decides the \
         outcome for every dictation while any app has secure input raised"
    );
    assert!(
        f.contains("keystrokes_blocked") && f.contains("ax_insert_text(text)"),
        "when keystrokes are blocked the app must still try accessibility insertion, which is \
         not suppressed, rather than making the user paste"
    );
}

/// The same evidence is read two ways in the same file.
///
/// `should_conceal_clipboard()` = `into_secure_field() || secure_input_active()`,
/// so when the global flag is TRUE and the accessibility probe answers `None`,
/// Parle marks the clipboard write CONCEALED — "this may be a password, no
/// clipboard manager should keep it" — and in the very next statement stores the
/// same text in a history that replicates to every paired device.
///
/// Measured on this machine (`r11_diag_*`): the flag is TRUE and the probe
/// returns `None` with an Electron app frontmost. So this is the live state, not
/// a corner. The missing option is the middle one: keep the row LOCAL.
#[test]
fn r11_conceal_and_store_disagree_about_the_same_evidence() {
    // INVERTED, and this was the sharpest finding of the round.
    //
    // The probe has THREE answers and the code had somewhere to put two. When
    // it could not tell (Chromium and Electron do not expose their
    // accessibility tree until a client sets `AXManualAccessibility`, so a web
    // password field answers nothing even with permission granted) the app
    // marked the clipboard CONCEALED, meaning "this may be a password", and in
    // the next statement wrote the same text to a database that replicates. The
    // same evidence read two ways in one function.
    //
    // The third answer is now representable: keep it, do not send it.
    let code = code_of("src-tauri/src/pipeline.rs");

    assert!(
        code.contains("enum FieldSecrecy") && code.contains("Unknown"),
        "the secure-field question is still a boolean, so 'cannot tell' has to be mapped onto \
         'yes' (discard the user's dictation) or 'no' (replicate a possible password)"
    );
    assert!(
        code.contains("keep_local_only"),
        "there is no third outcome, so an unanswerable probe still has to guess"
    );

    // And the store can actually honour it: the outbound door filters on it.
    let store = code_of("crates/parle-core/src/history.rs");
    assert!(
        store.contains("local_only = 0"),
        "`items_from` does not exclude local-only rows, so the third outcome is a label with \
         no effect"
    );
    assert!(
        store.contains("insert_transcription_local_only"),
        "nothing can write a local-only row"
    );
}

// ---------------------------------------------------------------------------
// R11-E. The macOS restore path puts the user's own clipboard back marked
// TransientType.
//
// `write_clipboard_marked(&prev, was_concealed)` is `write_clipboard_impl(_,
// transient = true, _)`. Round 10 removed exactly that marking from
// `write_clipboard` because TransientType tells every clipboard manager on the
// machine to bin the row, and left it on the path that restores content Parle
// did not author. `clipboard_is_concealed()` also counts TransientType, so the
// next chained dictation reads the restored row back as concealed.
// ---------------------------------------------------------------------------

#[test]
fn r11_restore_does_not_mark_the_users_own_clipboard_transient() {
    // INVERTED. `write_clipboard_marked` hard-coded `transient = true` for
    // every caller, so the transcript the user asked to keep arrived marked
    // "nobody should keep this" and every other clipboard manager binned it.
    //
    // The restore path compounded it: `clipboard_is_concealed()` counts
    // TransientType, so restoring the user's ordinary clipboard marked Transient
    // made the NEXT dictation read it back as concealed and restore it marked
    // ConcealedType. Two dictations laundered an ordinary clipboard entry into
    // "the OS says this is a secret".
    //
    // The marker is a claim about CONTENT. Our own writes are identified by
    // change count instead.
    let code = code_of("src-tauri/src/platform/macos.rs");
    let f = code
        .split("pub fn write_clipboard_marked(")
        .nth(1)
        .and_then(|s| s.split("\nfn ").next())
        .expect("write_clipboard_marked is in the file");
    assert!(
        f.contains("write_clipboard_impl(text, false, concealed)"),
        "every marked write is still TransientType, including the transcript the user asked to \
         keep"
    );
    // Self-capture is still suppressed, by identity.
    assert!(
        code.contains("OUR_LAST_WRITE") && code.contains("we_wrote_change("),
        "dropping the transient marker must not reintroduce self-capture"
    );
}

/// The transient marker is still hard-coded on for EVERY marked write, and the
/// shipped defaults (`copy_to_clipboard: true`) send the dictation the user
/// asked to keep straight through it. Round 10 removed exactly this marking
/// from `write_clipboard` on the ground that TransientType tells Alfred,
/// Raycast, Maccy and the rest to bin the row; the reason it was kept here —
/// "so clipboard managers (including our own monitor) skip them" — no longer
/// holds, because `we_wrote_change` is what suppresses our own monitor now.
#[test]
fn r11_the_kept_transcript_is_not_marked_do_not_keep() {
    let code = code_of("src-tauri/src/platform/macos.rs");
    assert!(
        code.contains("fn write_clipboard_impl"),
        "PREMISE GONE: the writer was renamed"
    );
    assert!(
        !code.contains("write_clipboard_impl(text, true, concealed)"),
        "write_clipboard_marked forces transient=true, so with the shipped \
         defaults every dictation lands on the clipboard marked \
         org.nspasteboard.TransientType and every other clipboard manager on \
         the machine discards it"
    );
}

/// The self-write marker is stored AFTER the payload is set, and
/// `declareTypes_owner` is what advances the change count. A monitor polling in
/// that window sees a change whose count does not yet match `OUR_LAST_WRITE`.
/// For an unmarked write (`write_clipboard`, which the palette's Copy uses)
/// nothing else catches it, so Parle re-captures its own write under its own
/// app id.
#[test]
fn r11_our_last_write_is_stored_before_the_change_becomes_visible() {
    let code = code_of("src-tauri/src/platform/macos.rs");
    let at = code.find("fn write_clipboard_impl").expect("the writer");
    let body = &code[at..at + 1400];
    let declare = body.find("declareTypes_owner").expect("types are declared");
    let store = body.find("OUR_LAST_WRITE.store").expect("the marker is stored");
    let set = body.find("setString_forType").expect("the payload is set");
    assert!(declare < set, "PREMISE GONE: types are no longer declared first");
    assert!(
        store < set,
        "OUR_LAST_WRITE is stored after the payload, leaving a window in which \
         the pasteboard has changed and the monitor cannot tell it was ours"
    );
}

// ---------------------------------------------------------------------------
// R11-F. Windows: bit 0x0020 is interpreted as ES_PASSWORD without checking
// the window class. That bit means something different for every other control
// class, so an ordinary focused control can be reported SECURE and the
// dictation is silently dropped.
// ---------------------------------------------------------------------------

#[test]
fn r11_windows_password_check_does_not_check_the_window_class() {
    // INVERTED. `ES_PASSWORD` (0x0020) is only meaningful for windows of the
    // EDIT class, and the class was never checked. Every common control gives
    // bit 5 its own meaning, so a focused tree view, owner-draw list or
    // left-text checkbox reported SECURE and the dictation was silently
    // dropped: round 9's headline failure through a different mechanism, on the
    // platform the branch is named after.
    //
    // `EM_GETPASSWORDCHAR` is meaningless outside an edit control, so a window
    // that is not one simply does not answer and a style bit cannot spoof it.
    let src = read_src("src-tauri/src/platform/windows.rs");
    let f = src
        .split("pub fn focused_field_is_secure()")
        .nth(1)
        .and_then(|s| s.split("\npub fn ").next())
        .expect("the function is in the file");

    assert!(
        !f.contains("GWL_STYLE"),
        "the check still reads a class-specific style bit without checking the class"
    );
    assert!(
        f.contains("EM_GETPASSWORDCHAR"),
        "the check must ask something only an edit control answers"
    );
    assert!(
        f.contains("SMTO_ABORTIFHUNG"),
        "this runs on the dictation path, so a hung foreground app must not stall it"
    );
}

/// Windows still marks EVERY write it makes with the do-not-keep formats,
/// including the user's own palette Copy. That is the marking round 10 removed
/// from macOS `write_clipboard`, on the ground that it tells Win+V and every
/// other clipboard manager to bin the row the user deliberately copied.
#[test]
fn r11_windows_palette_copy_does_not_tell_win_v_to_discard_it() {
    // INVERTED. macOS stopped marking its own writes and Windows was not
    // mirrored, so pressing Copy in Parle's own palette produced content Win+V
    // would not show and Cloud Clipboard would not roam. Self-capture is now
    // suppressed by sequence number on both platforms.
    let src = read_src("src-tauri/src/platform/windows.rs");

    assert!(
        src.contains("OUR_LAST_WRITE") && src.contains("fn we_wrote_change("),
        "Windows has no identity-based suppression, so its own writes can only be skipped by \
         relabelling the user's data"
    );
    let plain = src
        .split("pub fn write_clipboard(text: &str) {")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("write_clipboard is in the file");
    assert!(
        plain.contains("write_clipboard_inner(text, false)"),
        "the user's own Copy is still declared excluded from Win+V and Cloud Clipboard"
    );
    // The concealed variant still exists and still excludes.
    assert!(
        src.contains("pub fn write_clipboard_excluded("),
        "the excluded variant must remain for content we believe is a secret"
    );
}

// ---------------------------------------------------------------------------
// DIAGNOSTICS. Not part of the suite; they read this machine.
// ---------------------------------------------------------------------------

/// What does the process-global secure-input flag actually say right now?
///
/// Round 10 measured TRUE with a password manager merely running. If it is TRUE
/// here, `inject_text` is taking the clipboard-only branch for every dictation
/// on this machine.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "diagnostic: reads a live OS flag; run deliberately"]
fn r11_diag_secure_input_flag_on_this_machine() {
    let v = crate::platform::macos::secure_input_active();
    println!("IsSecureEventInputEnabled() = {v}");
    println!(
        "=> inject_text would take the {} branch for every dictation",
        if v { "CLIPBOARD-ONLY (manual paste required)" } else { "normal paste" }
    );
}

/// How long does the accessibility round trip on the dictation path cost?
///
/// `focused_field_is_secure` is up to three cross-process AX calls, and
/// `Pipeline::finish` can evaluate it twice. No `AXUIElementSetMessagingTimeout`
/// is set anywhere in this codebase, so a busy target app decides how long this
/// blocks.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "diagnostic: talks to the accessibility server; run deliberately"]
fn r11_diag_secure_field_probe_latency() {
    let trusted = crate::platform::macos::accessibility_trusted();
    println!("AXIsProcessTrusted() = {trusted}");
    let mut worst = std::time::Duration::ZERO;
    for _ in 0..20 {
        let t = std::time::Instant::now();
        let answer = crate::platform::macos::focused_field_is_secure();
        let d = t.elapsed();
        worst = worst.max(d);
        std::hint::black_box(answer);
    }
    println!("worst of 20 focused_field_is_secure() = {worst:?}");
    println!(
        "frontmost = {:?}, answer for the focused element = {:?}",
        crate::platform::macos::frontmost_app(),
        crate::platform::macos::focused_field_is_secure()
    );
    if !trusted {
        println!(
            "NOTE: not trusted, so this returned None at the first line and the \
             number above measures nothing on the real path"
        );
    }
}
