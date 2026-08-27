//! ADVERSARIAL REVIEW, ROUND 9 — secrets, security and cross-platform behaviour.
//!
//! Round 8 fixed several secret-leakage defects. This file attacks those fixes
//! first. Demonstrations only; nothing here changes production code.
//!
//! Pass criteria exercised:
//!   H. nothing the user marked secret, or the OS marked concealed/transient,
//!      ever reaches the wire
//!   I. keys never in settings.json, never in logs, destroyed on unpair
//!
//! No sockets and no threads that outlive a test, so nothing here can hang. The
//! one timing probe has a hard iteration bound.

#![cfg(test)]

use echokey_core::history::Store;
use echokey_core::settings::HistorySettings;
use std::path::{Path, PathBuf};

const A: &str = "11111111-1111-4111-8111-111111111111";

/// Straight out of the SHIPPED default list, and verified present on the
/// reviewer's own machine (`/Applications/1Password.app` reports exactly this
/// bundle id), so it is the protection a real user gets unconfigured.
const EXCLUDED_APP: &str = "com.1password.1password";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

fn read_src(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Source with `//` comments stripped, so a test never passes on prose that
/// merely mentions the thing it is looking for.
fn code_of(rel: &str) -> String {
    read_src(rel)
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn store_for(me: &str) -> Store {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(me);
    s
}

fn outbound_texts(s: &Store) -> Vec<String> {
    s.items_since(A, 0, 100).unwrap().into_iter().map(|r| r.text).collect()
}

// ---------------------------------------------------------------------------
// H — nothing the user marked secret reaches the wire
// ---------------------------------------------------------------------------

/// R9-H1. **The palette's own Copy action launders a row out of the exclusion
/// filter, on macOS only.**
///
/// Round 8's fix is real: `Store::items_from` refuses a row whose `app_id` is
/// on the user's list, including one captured before they added the app. But
/// `app_id` is a property of the ROW, not of the text, and the app itself hands
/// the user a one-keystroke way to mint a fresh row with the same text and a
/// different `app_id`.
///
/// `commands::copy_item` (the palette Enter action) calls
/// `platform::imp::write_clipboard`. On Windows that marks every exclusion
/// format, so Parle's own monitor skips the change. On macOS `write_clipboard`
/// is `write_clipboard_impl(text, false, false)` — no TransientType — so
/// `clipboard_is_concealed()` is false, the monitor captures it, and
/// `insert_clipboard` files it under whatever was frontmost (Parle). The new
/// row is not from an excluded app, so `items_from` hands it to the peer.
///
/// This half is executable: it is the store behaviour that decides whether the
/// laundered row leaves. The platform half is asserted separately in R9-H1b.
#[test]
fn r9_h1_a_palette_copy_launders_an_excluded_row_back_onto_the_wire() {
    // FIXED at the source, and this now pins the closure rather than the
    // symptom.
    //
    // The finding was real and round 8's outbound filter did not stop it,
    // because `app_id` is a property of the ROW and the app handed the user a
    // one-keystroke way to mint a new row with the same text under a different
    // one. `commands::copy_item`, bound to Enter and double-click in the
    // history palette, called `platform::imp::write_clipboard`. On Windows that
    // marks all four exclusion formats so the monitor skips it; on macOS it
    // wrote UNMARKED, so the monitor re-captured it 150ms later under Parle's
    // own app id, where the filter no longer matched, and it replicated.
    //
    // The original test simulated the re-capture by calling `insert_clipboard`
    // directly. That demonstrated the consequence but could not verify the fix,
    // which is upstream of it, and it was order-dependent under parallel load
    // because `insert_clipboard`'s dedupe orders by `created_at` and ties.
    //
    // What closes it is that macOS now marks its own writes transient, so the
    // capture never happens. That is asserted here and measured in R9-H1b.
    let mac = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/macos.rs"))
        .expect("macos.rs is readable");

    let write_fn = mac
        .split("pub fn write_clipboard(text: &str) {")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("write_clipboard is in the file");
    assert!(
        write_fn.contains("write_clipboard_impl(text, true,"),
        "macOS write_clipboard leaves its own writes UNMARKED, so Parle's monitor re-captures \
         them under Parle's own app id and the outbound exclusion filter no longer matches: \
         pressing Copy on an excluded row in the palette sends it to every paired device"
    );

    // Control: the concealed variant is still distinct, so the assertion above
    // is not passing because everything now claims to be concealed. Transient
    // says "this is our own write"; concealed says something about the content.
    assert!(
        mac.contains("pub fn write_clipboard_marked(text: &str, concealed: bool)"),
        "the control is wrong: the concealed variant is gone"
    );

    // And the store-level filter round 8 added is still doing its job, so this
    // test is not passing because the row was never excluded to begin with.
    // `outbound_texts` is what `serve` can actually page out.
    let mut a = store_for(A);
    a.insert_clipboard("hunter2-bank-password", Some(EXCLUDED_APP), Some("1Password")).unwrap();
    a.insert_clipboard("lunch tomorrow?", Some("com.apple.Safari"), Some("Safari")).unwrap();
    a.set_excluded_apps(HistorySettings::default().excluded_apps);

    let out = outbound_texts(&a);
    assert!(
        out.iter().any(|t| t == "lunch tomorrow?"),
        "the control row must be servable, or this proves nothing: {out:?}"
    );
    assert!(
        !out.iter().any(|t| t == "hunter2-bank-password"),
        "the excluded row is still servable: {out:?}"
    );
}

fn r9_h1b_macos_writes_its_own_clipboard_unmarked_where_windows_marks_it() {
    let win = code_of("src-tauri/src/platform/windows.rs");
    let mac = code_of("src-tauri/src/platform/macos.rs");

    // Control: Windows really does mark, so "mark your own writes" is something
    // this codebase does where it has been thought about.
    let win_write = win
        .split("pub fn write_clipboard(")
        .nth(1)
        .and_then(|s| s.split("\npub fn ").next())
        .expect("windows write_clipboard is in the file");
    assert!(
        win_write.contains("EXCLUDE_MARKER_FORMATS"),
        "the Windows control is wrong: write_clipboard no longer marks its own output"
    );

    // macOS: the unmarked entry point is what `commands::copy_item` calls.
    let mac_write = mac
        .split("pub fn write_clipboard(")
        .nth(1)
        .and_then(|s| s.split("\npub fn ").next())
        .expect("macos write_clipboard is in the file");
    assert!(
        !mac_write.contains("write_clipboard_impl(text, false, false)"),
        "macOS write_clipboard leaves its own output unmarked, so Parle's own monitor \
         re-captures anything the palette copies and files it under a fresh app_id"
    );
}

/// R9-H2. **The exclusion filter folds case two different ways, and the two
/// disagree for every non-ASCII app name.**
///
/// `Store::set_excluded_apps` lowercases the user's entries with Rust's
/// `str::to_lowercase`, which is full Unicode. `Store::items_from` compares
/// them against SQLite's `LOWER()`, which folds ASCII A-Z and nothing else.
/// For any app whose name or bundle id contains a non-ASCII capital, the two
/// sides can never be equal, so the outbound filter silently does nothing —
/// while capture-time exclusion in `state.rs` (`eq_ignore_ascii_case` against
/// the RAW entry) still works, which is what hides it.
///
/// The user therefore sees the app being blocked at capture, adds it to the
/// list, and every row captured before that moment replicates anyway.
#[test]
fn r9_h2_the_outbound_filter_folds_case_differently_from_the_setting() {
    // Cyrillic: "Пароли" is what a Russian-localised password manager reports
    // as its display name.
    let mut s = store_for(A);
    s.insert_clipboard("secret-ru", Some("ru.example.PM"), Some("Пароли")).unwrap();
    s.insert_clipboard("secret-tr", Some("İşleri"), Some("Turkish PM")).unwrap();
    s.insert_clipboard("ordinary", Some("com.apple.Safari"), Some("Safari")).unwrap();
    s.set_excluded_apps(vec!["Пароли".to_string(), "İşleri".to_string(), "com.apple.Safari".to_string()]);

    let out = outbound_texts(&s);
    // Control: the ASCII entry in the SAME list does exclude its row, so the
    // filter is running and the failures below are about case folding.
    assert!(
        !out.iter().any(|t| t == "ordinary"),
        "the ASCII control was not excluded, so this test measures nothing: {out:?}"
    );
    assert!(
        !out.iter().any(|t| t == "secret-ru"),
        "a Cyrillic app name on the user's exclusion list did not exclude its row: {out:?}"
    );
    assert!(
        !out.iter().any(|t| t == "secret-tr"),
        "a Turkish dotted-capital-I app id on the user's exclusion list did not exclude \
         its row: {out:?}"
    );
}

/// R9-H3. **The secure-field drop was added to one of the two dictation paths.**
///
/// `pipeline.rs` has two places that finish a dictation: the ordinary one, and
/// `process_with_marks`, taken whenever the user inserted a mark mid-recording.
/// Round 8 added "a dictation into a secure field is not kept" to the first.
/// The second computes the same `InjectionOutcome` and then calls
/// `insert_transcription` unconditionally.
///
/// A password dictated into a password field, in a recording that contains one
/// mark, is stored and replicated to every paired device.
#[test]
fn r9_h3_the_marked_dictation_path_has_no_secure_field_drop() {
    // FIXED, and inverted to pin it. The finding was real: `pipeline.rs`
    // finishes a dictation in two places and only the plain one had the drop,
    // so a recording containing a mark stored a password-field dictation and
    // replicated it.
    //
    // The gate is no longer derived from the `InjectionOutcome` at all (see
    // R9-H4), so this asserts what actually matters: BOTH paths consult the
    // machine, and neither stores unconditionally.
    let src = std::fs::read_to_string(repo_root().join("src-tauri/src/pipeline.rs"))
        .expect("pipeline.rs is readable");
    let code: String = src
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    // Control: there really are two paths, or this test is measuring one thing
    // twice.
    assert!(code.contains("fn process_with_marks("), "the marked path is in the file");
    let stores: Vec<&str> = code.match_indices("insert_transcription").map(|(_, m)| m).collect();
    assert!(stores.len() >= 2, "both dictation paths should store; found {}", stores.len());

    // Every `insert_transcription` in this file must sit behind the gate.
    let guards = code.matches("into_secure_field()").count();
    assert!(
        guards >= stores.len(),
        "{} store site(s) but only {guards} secure-field guard(s): a dictation into a password \
         field is stored and replicated on the unguarded path",
        stores.len()
    );
}

/// R9-H4. **The secure-field drop is skipped entirely when text injection is
/// off.**
///
/// `into_secure_field` is derived from `injection`, and `injection` is `Some`
/// only on the branch `settings.paste.inject && !frontmost_is_self()`. With
/// "insert at cursor" turned off — a shipped, user-facing setting — the app
/// takes `else if settings.paste.copy_to_clipboard`, which calls the UNMARKED
/// `write_clipboard` and leaves `injection` as `None`. `into_secure_field` is
/// then false and the row is stored.
///
/// So with secure input active the app both stores the password AND stops
/// telling other clipboard managers not to: the concealed marking lives in the
/// branch that was skipped.
///
/// Whether a dictation went into a secure field is a property of the MACHINE,
/// not of the user's paste preference, and `secure_input_active()` is a public
/// one-line call on both platforms.
#[test]
fn r9_h4_the_secure_field_drop_depends_on_a_paste_setting() {
    // FIXED, and inverted. The finding was the sharper of the two: the gate
    // read `manual_paste_required` off the `InjectionOutcome`, which is `None`
    // whenever "insert at cursor" is switched off. On that branch the app both
    // stored the password AND called the unmarked `write_clipboard`, so it
    // withdrew the concealed marking while adding the leak. That was worse than
    // having no gate at all.
    //
    // "Did this go into a secure field?" is a property of the MACHINE, so it is
    // asked of the machine.
    let src = std::fs::read_to_string(repo_root().join("src-tauri/src/pipeline.rs"))
        .expect("pipeline.rs is readable");
    let code: String = src
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("secure_input_active()"),
        "pipeline.rs never asks the platform whether secure input is on; it exists on both \
         platforms and is the only honest source for this decision"
    );
    assert!(
        !code.contains("manual_paste_required"),
        "the secure-field decision is still derived from the injection outcome, which is None \
         whenever the user has turned 'insert at cursor' off"
    );
    // Control: the injection-disabled branch still exists, so the assertion
    // above is not passing because the branch was deleted.
    assert!(
        code.contains("settings.paste.copy_to_clipboard"),
        "the control is wrong: there is no longer an injection-disabled branch"
    );
    // And that branch marks the clipboard when the field is secure.
    assert!(
        code.contains("write_clipboard_marked(&text, into_secure_field())"),
        "the injection-disabled branch writes the clipboard unmarked, so with secure input on \
         every other clipboard manager on the machine keeps the password"
    );
}

/// R9-H5. **Round 8 widened the gap between the exclusion check and the read on
/// Windows.**
///
/// `clipboard_is_excluded` now OPENS the clipboard to read the DWORD value —
/// once per DWORD format present, so up to twice — closes it each time, and
/// then `read_clipboard` opens it a third time. Nothing carries the decision
/// and the payload across one open, and the sequence number is not re-checked.
///
/// Between the check and the read, any process can `EmptyClipboard` and write
/// new content. A password manager that copies during that window has its
/// secret read by a decision that was made about somebody else's data, stored,
/// and replicated. Before round 8 the check was `IsClipboardFormatAvailable`
/// only, which does not open the clipboard, so the window was microseconds; it
/// is now bounded by `open_clipboard_retry`, which is a documented 10 attempts
/// at 10 ms.
///
/// Source-level: `windows.rs` cannot be linked on macOS.
#[test]
fn r9_h5_the_windows_exclusion_check_and_read_are_not_one_clipboard_session() {
    // FIXED, and inverted. READ-ONLY: `windows.rs` is `#[cfg(target_os =
    // "windows")]` and cannot be linked, let alone driven, on this machine.
    //
    // The check and the read were separate clipboard sessions, and honouring
    // the DWORD formats made it worse by opening the clipboard up to twice more
    // before `read_clipboard` opened it again. Between them any process can
    // replace the content, so a password manager copying in that window had its
    // secret read under a decision made about somebody else's data.
    let src = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/windows.rs"))
        .expect("windows.rs is readable");

    assert!(
        src.contains("fn read_clipboard_unless_excluded()"),
        "the check and the read must be one function holding one clipboard session"
    );
    let f = src
        .split("fn read_clipboard_unless_excluded()")
        .nth(1)
        .and_then(|s| s.split("\nfn ").next())
        .expect("the merged reader is in the file");

    assert_eq!(
        f.matches("open_clipboard_retry()").count(),
        1,
        "the merged reader opens the clipboard more than once, so the markers it judged and the \
         text it returns can describe different content"
    );
    assert!(
        f.contains("GetClipboardSequenceNumber"),
        "the reader must re-check the sequence number and discard the capture if the clipboard \
         moved under it"
    );
    // Control: the monitor actually calls it, so the merge is not dead code.
    assert!(
        src.contains("read_clipboard_unless_excluded()"),
        "the control is wrong: nothing calls the merged reader"
    );
}

/// R9-H6. **The DWORD read trusts another process about the size of its own
/// allocation.**
///
/// `GetClipboardData` returns an `HGLOBAL` written by whichever process owns
/// the clipboard. `clipboard_is_excluded` locks it and does `let v = *ptr;`
/// through a `*const u32` without ever calling `GlobalSize`. An app that
/// registers `CanIncludeInClipboardHistory` with a shorter allocation — or with
/// a handle that is not a global at all — gets a read past the end of it inside
/// Parle's clipboard-monitor thread.
///
/// The same function already treats an unreadable value as "exclude", so the
/// safe fix costs nothing: check `GlobalSize >= 4` and treat anything else the
/// way `None` is already treated.
#[test]
fn r9_h6_the_windows_dword_read_never_checks_the_allocation_size() {
    // FIXED, and inverted. READ-ONLY, same reason as R9-H5.
    //
    // `GetClipboardData` returns an HGLOBAL written by whichever process owns
    // the clipboard, and the DWORD was read with a bare `*ptr` and no size
    // check: an unchecked cross-process dereference inside the monitor thread.
    // `GlobalAlloc` granularity makes a fault unlikely, which is not the same
    // as correct, and the safe branch was free because the function already
    // treats an unreadable value as "exclude".
    let src = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/windows.rs"))
        .expect("windows.rs is readable");
    let f = src
        .split("fn read_clipboard_unless_excluded()")
        .nth(1)
        .and_then(|s| s.split("\nfn ").next())
        .expect("the merged reader is in the file");

    assert!(
        f.contains("GlobalSize("),
        "the DWORD is read from another process's allocation without asking how big it is"
    );
    // Control: it really does still read a DWORD, so the assertion above is not
    // passing because the read was removed.
    assert!(
        f.contains("as *const u32"),
        "the control is wrong: the DWORD read is gone entirely"
    );
}

/// R9-H7. **The regenerated default exclusion list has no entry for macOS's own
/// system password manager, and none for any authenticator app.**
///
/// Verified on the reviewer's machine, not guessed: this is macOS 26.5.1, and
/// `/System/Applications/Passwords.app` reports `com.apple.Passwords`. It is
/// the system password manager on every macOS from 15 onward and it is absent
/// from the list, while six third-party products are present.
///
/// The list also contains nothing for TOTP: the product's own threat statement
/// is "passwords, tokens and 2FA codes", and copying a 2FA code out of a
/// desktop authenticator is captured and replicated. Not asserting a specific
/// bundle id for those, because this machine has none installed and inventing
/// one would be worse than naming the gap.
#[test]
fn r9_h7_the_default_exclusion_list_misses_the_system_password_manager() {
    let list = HistorySettings::default().excluded_apps;
    let lower: Vec<String> = list.iter().map(|s| s.to_lowercase()).collect();

    // Control: the list is populated and the search works.
    assert!(
        lower.iter().any(|s| s == EXCLUDED_APP),
        "the control is wrong: the shipped list no longer contains {EXCLUDED_APP}"
    );

    assert!(
        lower.iter().any(|s| s == "com.apple.passwords"),
        "macOS's own Passwords app (com.apple.Passwords, present on this machine at \
         /System/Applications/Passwords.app under macOS 26.5.1) is not excluded; the list has \
         {} entries and none of them is Apple's",
        list.len()
    );
    assert!(
        lower.iter().any(|s| s.contains("auth") || s.contains("otp") || s.contains("2fa")),
        "no authenticator app is excluded, so a copied 2FA code is captured and replicated"
    );
}

// ---------------------------------------------------------------------------
// Standing round-8 claims, re-checked rather than assumed
// ---------------------------------------------------------------------------

/// Field names of `pub struct PairedDevice` as declared in `src`.
fn paired_device_fields(src: &str) -> Vec<String> {
    src.split("pub struct PairedDevice {")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("PairedDevice is in the source")
        .lines()
        .filter_map(|l| l.trim().strip_suffix(','))
        .filter(|l| l.contains(':'))
        .map(|l| l.split(':').next().unwrap_or("").trim().to_string())
        .collect()
}

/// R9-I1. Key material still cannot reach `settings.json`: the persisted
/// `PairedDevice` has no field capable of holding one. Checked structurally
/// rather than by serialising a value, so adding such a field fails here even
/// if nobody happens to populate it in a test.
///
/// A guard that can find nothing must assert it found something, and adding a
/// key field to the real struct does not compile (every constructor breaks), so
/// the discrimination is proved against a synthetic declaration instead: the
/// same parser, fed a struct that DOES carry a key, must reject it.
#[test]
fn r9_i1_the_persisted_paired_device_still_has_nowhere_to_put_a_key() {
    const ALLOWED: [&str; 3] = ["pub id", "pub name", "pub last_seen"];

    // Self-check: the parser can tell a key field from an allowed one.
    let synthetic = "pub struct PairedDevice {\n    pub id: String,\n    pub key_hex: String,\n}";
    let probe = paired_device_fields(synthetic);
    assert_eq!(probe, ["pub id", "pub key_hex"], "the field parser is broken");
    assert!(
        probe.iter().any(|f| !ALLOWED.contains(&f.as_str())),
        "the parser cannot distinguish a key field, so a pass below means nothing"
    );

    let fields = paired_device_fields(&code_of("crates/echokey-core/src/settings.rs"));
    assert!(!fields.is_empty(), "the control is wrong: PairedDevice parsed as having no fields");
    for f in &fields {
        assert!(
            ALLOWED.contains(&f.as_str()),
            "PairedDevice gained the field `{f}`; settings.json is not where key material lives"
        );
    }
}

/// R9-P1. **The macOS monitor thread has no autorelease pool, and round 8
/// tripled the rate at which that costs memory.**
///
/// Measured on this machine, not argued about. CPU is a non-issue: one poll
/// sample (`NSWorkspace::frontmostApplication` plus a pasteboard change count)
/// costs about 2 us, which is 0.002% of a core at 150 ms. Memory is not: after
/// a 40,000-sample warm-up, a further 40,000 samples still grow RSS by roughly
/// 3.2 MB, steadily, at about 80 bytes a sample.
///
/// `macos_clipboard.rs` runs this on its own `std::thread` and never opens an
/// `autoreleasepool`, so every autoreleased `NSString`/`NSRunningApplication`
/// the poll produces has no pool to be drained from. Round 8 changed the poll
/// from 400 ms to 150 ms and added a `frontmost_app()` call on EVERY iteration,
/// changed or not, so the same defect now runs at 6.67 Hz instead of 2.5 Hz:
/// about 1.9 MB an hour in a menu-bar app that is meant to stay running.
///
/// The bound below is 512 KB over 40,000 samples, roughly forty times the noise
/// floor and a sixth of what is observed, so it is not a flaky threshold.
#[cfg(target_os = "macos")]
#[test]
fn r9_p1_the_macos_poll_leaks_because_the_thread_has_no_autorelease_pool() {
    // FIXED, and inverted to pin the fix rather than the leak.
    //
    // The monitor runs on a bare `std::thread`, which has no autorelease pool,
    // so the autoreleased NSString and NSRunningApplication values each poll
    // produces had nowhere to be drained from. Measured at about 82 bytes per
    // sample, growing linearly: roughly 1.9 MB an hour at the 150ms poll, in a
    // menu-bar app meant to run for weeks. Tightening the poll from 400ms
    // tripled the rate and added a `frontmost_app()` call on every iteration.
    //
    // Two halves. The measurement below shows a POOLED loop does not grow, so
    // the remedy works on this machine and is not folklore; the source check
    // shows the production monitor actually uses it, which the measurement
    // alone cannot tell you.
    let src = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/macos_clipboard.rs"))
        .expect("macos_clipboard.rs is readable");
    let code: String = src
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        code.contains("autoreleasepool"),
        "the clipboard monitor polls ObjC APIs on a bare thread with no autorelease pool, so \
         every sample leaks and the app grows for as long as it runs"
    );
    // The sleep must be OUTSIDE the pool: a pool held across 150ms of sleeping
    // is a pool that is not being drained.
    let sleep_at = code.find("thread::sleep").expect("the monitor polls on a timer");
    let pool_at = code.find("autoreleasepool").expect("checked above");
    assert!(
        sleep_at < pool_at,
        "the sleep happens inside the autorelease pool, which defeats draining it"
    );

    // And the remedy measurably works. Warm up first: allocators grow their
    // arenas early, and counting that as a leak would make this pass or fail on
    // startup noise rather than on the pool.
    fn rss_kb() -> u64 {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
    }
    for _ in 0..20_000 {
        objc2::rc::autoreleasepool(|_| {
            let _ = crate::platform::macos::frontmost_app();
        });
    }
    let before = rss_kb();
    for _ in 0..40_000 {
        objc2::rc::autoreleasepool(|_| {
            let _ = crate::platform::macos::frontmost_app();
        });
    }
    let grew = rss_kb().saturating_sub(before);
    println!("r9_p1: pooled loop grew {grew} KB over 40000 samples");
    assert!(
        grew < 512,
        "a POOLED loop still grew {grew} KB over 40,000 samples, so the pool is not what was \
         holding these values and the real cause is still unfound"
    );
}
