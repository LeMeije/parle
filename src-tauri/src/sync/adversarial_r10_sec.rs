//! ADVERSARIAL REVIEW, ROUND 10 — secrets, security and cross-platform.
//!
//! Round 9's fixes are attacked first. Nothing here changes production code and
//! nothing here opens a socket, spawns a thread that outlives a test, sleeps,
//! or touches the real clipboard or the real keychain.
//!
//! Pass criteria exercised:
//!   H. nothing the user marked secret, or the OS marked concealed/transient,
//!      ever reaches the wire
//!   I. keys never in settings.json

#![cfg(test)]

use echokey_core::history::Store;
use echokey_core::settings::{HistorySettings, Settings};
use std::path::{Path, PathBuf};

const A: &str = "11111111-1111-4111-8111-111111111111";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
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
// R10-A. The exclusion-list fix reaches new installs only.
// ---------------------------------------------------------------------------

/// Round 9 added macOS Passwords, Keychain Access, MacPass, Strongbox and the
/// authenticators to `default_excluded_apps()`. `HistorySettings` is
/// `#[serde(default)]` and `Settings::load` is a bare `serde_json::from_str`,
/// so a stored `excluded_apps` array is taken VERBATIM. Every machine that has
/// ever saved settings — which is every machine the app has run on, and both
/// field-test machines — keeps the round-8 list.
///
/// This is the `com.kee.keepass` failure mode in a different place: the list
/// reads as coverage in the source and is not what a real user is running.
#[test]
fn r10_a_an_existing_install_never_gains_the_new_exclusions() {
    // Exactly what a round-8 install has on disk. Nothing exotic: this is the
    // shipped default of the previous build, round-tripped through serde.
    let round8_list = r#"[
        "com.1password.1password","com.agilebits.onepassword7","1Password.exe",
        "com.bitwarden.desktop","Bitwarden.exe",
        "com.lastpass.LastPass","LastPass.exe",
        "org.keepassxc.keepassxc","KeePassXC.exe",
        "com.dashlane.Dashlane","Dashlane.exe",
        "com.kee.keepass","KeePass.exe",
        "in.sinew.Enpass-Desktop","Enpass.exe"
    ]"#;
    let stored = format!(r#"{{"history":{{"excluded_apps":{round8_list}}}}}"#);
    let loaded: Settings =
        serde_json::from_str(&stored).expect("an old settings.json still deserialises");

    let fresh = HistorySettings::default().excluded_apps;
    assert!(
        fresh.iter().any(|a| a == "com.apple.Passwords"),
        "premise: the round-9 default really does carry the system password manager"
    );

    let missing: Vec<&String> =
        fresh.iter().filter(|a| !loaded.history.excluded_apps.contains(a)).collect();
    assert!(
        missing.is_empty(),
        "R10-A: an upgraded install keeps the old list; these round-9 additions \
         never reach it: {missing:?}"
    );
}

/// The consequence, at the layer that decides what leaves the machine. A row
/// copied out of macOS Passwords on an UPGRADED install is servable.
#[test]
fn r10_a2_an_upgraded_install_still_replicates_the_system_password_manager() {
    let stored = r#"{"history":{"excluded_apps":[
        "com.1password.1password","com.bitwarden.desktop","org.keepassxc.keepassxc"
    ]}}"#;
    let loaded: Settings = serde_json::from_str(stored).unwrap();

    let mut a = store_for(A);
    a.insert_clipboard("hunter2-bank-password", Some("com.apple.Passwords"), Some("Passwords"))
        .unwrap();
    a.insert_clipboard("lunch tomorrow?", Some("com.apple.Safari"), Some("Safari")).unwrap();
    a.set_excluded_apps(loaded.history.excluded_apps.clone());

    let out = outbound_texts(&a);
    assert!(
        out.iter().any(|t| t == "lunch tomorrow?"),
        "control: an ordinary row must still be servable, or this proves nothing: {out:?}"
    );
    assert!(
        !out.iter().any(|t| t == "hunter2-bank-password"),
        "R10-A2: on an upgraded install a password copied from macOS Passwords is \
         handed to every paired device: {out:?}"
    );
}

/// Control for R10-A: on a FRESH install the round-9 entry does work, capital
/// `P` and all, so the case-folding fix is genuinely holding and R10-A is about
/// migration and nothing else.
#[test]
fn r10_a3_on_a_fresh_install_the_capitalised_entry_really_does_match() {
    let mut a = store_for(A);
    a.insert_clipboard("hunter2-bank-password", Some("com.apple.Passwords"), Some("Passwords"))
        .unwrap();
    a.insert_clipboard("lunch tomorrow?", Some("com.apple.Safari"), Some("Safari")).unwrap();
    a.set_excluded_apps(HistorySettings::default().excluded_apps);

    let out = outbound_texts(&a);
    assert!(out.iter().any(|t| t == "lunch tomorrow?"), "control row servable: {out:?}");
    assert!(
        !out.iter().any(|t| t == "hunter2-bank-password"),
        "the ASCII folding for a capitalised bundle id has regressed: {out:?}"
    );
}

// ---------------------------------------------------------------------------
// R10-B. The secure-field gate on Windows is a hardcoded `false`.
// ---------------------------------------------------------------------------

/// `pipeline::into_secure_field()` is `platform::imp::secure_input_active()`,
/// and the doc comment above it says the function "exists on both platforms and
/// answers the real question directly". On Windows it is:
///
/// ```ignore
/// pub fn secure_input_active() -> bool {
///     false
/// }
/// ```
///
/// So on Windows a dictation into a password field is stored in history and
/// replicated to the Mac. The gate reads as cross-platform coverage in
/// `pipeline.rs` and has no effect on half the supported platforms.
///
/// Source-level, because the reviewer is on macOS and the Windows body is
/// `cfg`-ed out of this build. Comments are stripped so prose cannot satisfy it.
#[test]
fn r10_b_the_windows_secure_field_gate_is_a_constant_false() {
    let win = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/windows.rs"))
        .expect("windows.rs is readable");
    let code: String = win
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    let body = code
        .split("pub fn secure_input_active() -> bool {")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("premise: Windows defines secure_input_active");

    assert_ne!(
        body.trim(),
        "false",
        "R10-B: the secure-field gate both dictation paths now depend on is a \
         constant `false` on Windows, so a password dictated on the PC is stored \
         and replicated. A mitigation exists (UI Automation IsPassword); the \
         finding is its absence, not the absence of an API."
    );
}

/// And the gate is unconditional, so the OPPOSITE failure has no floor: there
/// is no per-app, per-field or user-visible qualification anywhere on the path.
/// Whatever the machine says, both dictation paths obey it silently.
#[test]
fn r10_b2_the_gate_has_no_escape_hatch_and_fails_silently() {
    let pipe = std::fs::read_to_string(repo_root().join("src-tauri/src/pipeline.rs"))
        .expect("pipeline.rs is readable");
    let code: String = pipe
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("fn into_secure_field() -> bool"),
        "premise: the free function round 9 introduced is still here"
    );
    // Both drop sites report through `tracing::info!` only.
    let drops = code.matches("into_secure_field()").count();
    assert!(drops >= 3, "premise: both dictation paths plus both clipboard writes use it");
    assert!(
        code.contains("PipelineEvent::SecureFieldSkipped")
            || code.contains("secure_field_skipped")
            || code.contains("warn!(")
            && code.contains("secure field"),
        "R10-B2: when `secure_input_active()` is stuck on — which one crashed app, \
         or Terminal's Secure Keyboard Entry, does system-wide and process-globally \
         on macOS — EVERY dictation is silently dropped from history and written to \
         the clipboard CONCEALED. The only report is a `tracing::info!` the user \
         never sees. There is no event, no warning and no way to notice."
    );
}

// ---------------------------------------------------------------------------
// R10-C. macOS: what the round-9 transient marking costs, and what it misses.
// ---------------------------------------------------------------------------

/// The clipboard RESTORE path re-writes the user's ORIGINAL clipboard with
/// `concealed = false`, unconditionally. It read that clipboard with a plain
/// `read_clipboard()`, which consults no marker at all.
///
/// So: user copies a password out of a password manager (pasteboard carries
/// `org.nspasteboard.ConcealedType`), then dictates anywhere with "restore
/// clipboard" on. Parle reads the password, holds it in a process-global
/// `PENDING_RESTORE`, and puts it back with the ConcealedType marker GONE. The
/// pasteboard's own statement that this is a secret is destroyed by an app
/// whose entire claim is that it does not do that.
#[test]
fn r10_c_the_restore_path_strips_the_concealed_marker_from_the_users_clipboard() {
    let mac = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/macos.rs"))
        .expect("macos.rs is readable");
    let code: String = mac
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("let previous = read_clipboard();"),
        "premise: the restore path snapshots the clipboard with an unmarked read"
    );
    assert!(
        !code.contains("write_clipboard_marked(&prev, false)"),
        "R10-C: the user's original clipboard is restored with concealed=false, so \
         a password that arrived marked ConcealedType goes back unmarked and every \
         other clipboard manager on the machine is now free to keep it"
    );
}

/// `write_clipboard_impl` sets the PAYLOAD before it sets the markers, and
/// `changeCount` moves on `clearContents()`. The monitor polls `changeCount`
/// and then asks `clipboard_is_concealed()`, so a poll landing between the two
/// `setString_forType` calls sees a changed clipboard with no marker on it.
///
/// On the secure-input path that is a password-field dictation captured into
/// history by the clipboard monitor, which is a completely different path from
/// the `into_secure_field()` gate in `pipeline.rs` and is not covered by it.
#[test]
fn r10_c2_the_marker_is_written_after_the_payload_not_with_it() {
    let mac = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/macos.rs"))
        .expect("macos.rs is readable");
    let body = mac
        .split("fn write_clipboard_impl(text: &str, transient: bool, concealed: bool) {")
        .nth(1)
        .and_then(|s| s.split("\npub fn ").next())
        .expect("premise: write_clipboard_impl is in the file");

    let clear = body.find("clearContents()").expect("it clears first");
    let payload = body.find("setString_forType(&value").expect("then writes the text");
    let marker = body.find("TransientType").expect("then marks it");
    assert!(clear < payload && payload < marker, "premise: this is the order");

    assert!(
        body.contains("declareTypes") || body.contains("declare_types"),
        "R10-C2: the pasteboard is populated payload-first and marked afterwards, so \
         the change is observable through a window in which the content is present \
         and the 'do not capture this' marker is not. `declareTypes:owner:` names \
         every type in one call before any value is set and closes that window."
    );
}

/// The other half of round 9's transient marking: `commands::copy_item` — the
/// palette's Enter and double-click — now writes TRANSIENT. That is the
/// convention for "an app wrote this for itself, do not keep it", so every
/// OTHER clipboard manager on the machine now drops the thing the user
/// deliberately asked to copy out of Parle's history.
#[test]
fn r10_c3_the_palette_copy_is_marked_do_not_keep() {
    let cmds = std::fs::read_to_string(repo_root().join("src-tauri/src/commands.rs"))
        .expect("commands.rs is readable");
    assert!(
        cmds.contains("platform::imp::write_clipboard(&item.text)"),
        "premise: copy_item goes through the plain write"
    );
    let mac = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/macos.rs"))
        .expect("macos.rs is readable");
    let write_fn = mac
        .split("pub fn write_clipboard(text: &str) {")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("write_clipboard is in the file");

    assert!(
        !write_fn.contains("write_clipboard_impl(text, true,"),
        "R10-C3: a user's explicit Copy from Parle's history is marked transient, so \
         Alfred/Raycast/Maccy and friends discard it. Suppressing OUR OWN re-capture \
         is what was wanted; recording the changeCount of our own write and skipping \
         exactly that one does it without telling the rest of the machine to throw \
         the user's copy away."
    );
}

// ---------------------------------------------------------------------------
// R10-D. Windows: the DWORD read was bounded; the TEXT read was not.
// ---------------------------------------------------------------------------

/// Round 9 added `GlobalSize(h) < 4` before dereferencing another process's
/// allocation as a `u32`, and its own comment calls the unchecked version "an
/// unchecked cross-process dereference". The CF_UNICODETEXT read three lines
/// below scans for a NUL terminator with no size check at all, in both
/// `read_clipboard` and `read_clipboard_unless_excluded`.
///
/// An app that publishes a CF_UNICODETEXT handle whose buffer is not
/// NUL-terminated makes that loop walk past the end of the allocation. Best
/// case Parle crashes; worse case it appends whatever follows in the mapped
/// region to the captured text, stores it in history, and replicates it.
#[test]
fn r10_d_the_windows_text_read_has_no_globalsize_bound() {
    let win = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/windows.rs"))
        .expect("windows.rs is readable");
    let code: String = win
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    // The premise: the unbounded scan exists, more than once.
    let scans = code.matches("while *ptr.add(len) != 0 {").count();
    assert!(
        scans >= 2,
        "premise: the NUL scan appears in both read paths, found {scans}"
    );
    // The fixed DWORD read is the control: the file knows how to do this.
    assert!(
        code.contains("if GlobalSize(h) < 4 {"),
        "control: the DWORD read really is bounded, so the omission below is an omission"
    );

    // Every unbounded scan must be preceded by a size bound on the SAME handle.
    let bounded = code
        .split("while *ptr.add(len) != 0 {")
        .take(scans)
        .filter(|before| {
            let tail = &before[before.len().saturating_sub(600)..];
            tail.contains("GlobalSize") && tail.contains("CF_UNICODETEXT")
        })
        .count();
    assert_eq!(
        bounded, scans,
        "R10-D: {} of {scans} CF_UNICODETEXT reads dereference another process's \
         allocation with no `GlobalSize` bound, the exact defect round 9 fixed one \
         branch above",
        scans - bounded
    );
}

// ---------------------------------------------------------------------------
// R10-E. Attacked and HELD. Recorded so the next round starts elsewhere.
// ---------------------------------------------------------------------------

/// I. No paired key, and nothing key-shaped, is serialisable into settings.json.
#[test]
fn r10_e1_settings_carry_no_key_material() {
    let mut s = Settings::default();
    s.sync.device_id = A.to_string();
    s.sync.device_name = "Ben's Mac".into();
    s.sync.paired.push(echokey_core::settings::PairedDevice {
        id: "22222222-2222-4222-8222-222222222222".into(),
        name: "Ben's G14".into(),
        last_seen: Some(1),
    });
    let json = serde_json::to_string(&s).unwrap();
    for forbidden in ["key", "secret", "psk", "noise", "spake"] {
        assert!(
            !json.to_ascii_lowercase().contains(forbidden),
            "settings.json carries a {forbidden}-shaped field: {json}"
        );
    }
    // And the struct that DOES hold peers has exactly three fields, none secret.
    let one = serde_json::to_value(&s.sync.paired[0]).unwrap();
    assert_eq!(
        one.as_object().unwrap().len(),
        3,
        "PairedDevice grew a field; check it carries no secret"
    );
}

/// The macOS gate is not stuck on right now, on this machine, unprompted. If
/// it were, R10-B2's silent-drop failure would already be live here.
#[cfg(target_os = "macos")]
#[test]
fn r10_e2_secure_input_is_not_stuck_on_this_machine() {
    let on = crate::platform::imp::secure_input_active();
    assert!(
        !on,
        "IsSecureEventInputEnabled() is true with no password field in sight; every \
         dictation on this machine is currently being dropped from history"
    );
}

/// The round-9 monitor restructure preserved every branch's bookkeeping. The
/// old loop set `prev_app` on all five exits and `last` on the disabled and
/// changed branches; the closure form must do the same, or a missed `last`
/// re-reads the same clipboard for ever and a missed `prev_app` misattributes
/// the next capture to a stale app — which is what the exclusion list matches.
#[test]
fn r10_e3_every_monitor_branch_still_returns_a_fresh_frontmost_app() {
    let src = std::fs::read_to_string(
        repo_root().join("src-tauri/src/platform/macos_clipboard.rs"),
    )
    .expect("macos_clipboard.rs is readable");
    let body = src
        .split("objc2::rc::autoreleasepool(|_| {")
        .nth(1)
        .and_then(|s| s.split("\n                        if let Some(ev) = event").next())
        .expect("premise: the poll body is inside a pool");

    // Five exits, every one of them carrying a fresh reading.
    let exits = body.matches("macos::frontmost_app()").count();
    assert_eq!(
        exits, 5,
        "a branch stopped refreshing prev_app; the next capture is attributed to a \
         stale app and the exclusion list is matched against the wrong one"
    );
    // `last` is advanced on the disabled branch and on a real change, exactly
    // as before, and nowhere else.
    assert_eq!(
        body.matches("last = macos::pasteboard_change_count();").count(),
        1,
        "the disabled branch must still resynchronise `last`"
    );
    assert!(body.contains("last = now;"), "a real change must still advance `last`");
    // And the sleep stays outside the pool, or the pool is held across it.
    let outer = src.split("objc2::rc::autoreleasepool").next().unwrap();
    assert!(
        outer.contains("std::thread::sleep(std::time::Duration::from_millis(150));"),
        "the sleep moved inside the pool, which defeats draining it"
    );
}
