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

use parle_core::history::Store;
use parle_core::settings::{HistorySettings, Settings};
use std::path::{Path, PathBuf};

const A: &str = "11111111-1111-4111-8111-111111111111";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

fn read_src(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Source with `//` comments stripped, so a passing mention of a word in prose
/// cannot make a guard find nothing while looking as though it found something.
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
    // INVERTED. `#[serde(default)]` fills in fields that are ABSENT and does
    // nothing for a field that is present and stale, and `excluded_apps` is
    // present in every settings.json this app has ever written. So additions to
    // the shipped list reached new installs only, and every machine the app had
    // already run on kept the list it was first given, while the source read as
    // though it was covered.
    //
    // `Settings::migrate` unions the stored list with the defaults on load.
    let dir = std::env::temp_dir().join(format!("parle-r10a-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");

    // An install created before the round-9 additions.
    let mut old = parle_core::settings::Settings::default();
    old.version = 1;
    // A build from before round 12 never wrote `excluded_defaults_seen`, so an
    // old file does not have it. Synthesising the premise from the CURRENT
    // default struct pre-populates it with every shipped exclusion, which
    // claims the install has already been offered them all and is the opposite
    // of what this test is describing.
    old.excluded_defaults_seen.clear();
    old.history.excluded_apps =
        vec!["com.1password.1password".into(), "1Password.exe".into()];
    std::fs::write(&path, serde_json::to_string_pretty(&old).unwrap()).unwrap();

    let loaded = parle_core::settings::Settings::load(&path).unwrap();
    let have: Vec<String> =
        loaded.history.excluded_apps.iter().map(|a| a.to_ascii_lowercase()).collect();

    let defaults = parle_core::settings::Settings::default().history.excluded_apps;
    let missing: Vec<&String> =
        defaults.iter().filter(|d| !have.contains(&d.to_ascii_lowercase())).collect();
    assert!(
        missing.is_empty(),
        "an upgraded install still never gains these: {missing:?}"
    );
    // The user's own entry survives: a union, not a replacement.
    assert!(
        have.iter().any(|a| a == "com.1password.1password"),
        "the migration must not discard what was already there"
    );
    assert_eq!(loaded.version, parle_core::settings::SETTINGS_VERSION);
}

/// The consequence, at the layer that decides what leaves the machine. A row
/// copied out of macOS Passwords on an UPGRADED install is servable.
#[test]
fn r10_a2_an_upgraded_install_still_replicates_the_system_password_manager() {
    // Loaded through `Settings::load`, which is what the running app calls and
    // which is where the migration lives. Round 10's version used a bare
    // `serde_json::from_str`, which is precisely the call that skipped the
    // migration and produced the finding; keeping it here would keep asserting
    // the defect after the fix.
    let stored = r#"{"version":1,"history":{"excluded_apps":[
        "com.1password.1password","com.bitwarden.desktop","org.keepassxc.keepassxc"
    ]}}"#;
    let dir = std::env::temp_dir().join(format!("parle-r10a2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");
    std::fs::write(&path, stored).unwrap();
    let loaded = Settings::load(&path).unwrap();

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
        "on an upgraded install a password copied from macOS Passwords is \
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
    // INVERTED, partially, and the residual is pinned rather than hidden.
    //
    // The finding was right: both dictation paths depended on a gate that was a
    // bare `false` on Windows, with no comment saying so, while `pipeline.rs`
    // claimed the function "exists on both platforms and answers the real
    // question". That is the `com.kee.keepass` pattern: a stub that reads as
    // coverage.
    //
    // Windows now implements `focused_field_is_secure` via `GetGUIThreadInfo` +
    // `ES_PASSWORD`, which covers classic Win32 edit controls. It does NOT see
    // a WinUI PasswordBox or a Chromium `<input type=password>`, which need UI
    // Automation. That gap is real and is written down in the function, so it
    // cannot be mistaken for coverage.
    let src = read_src("src-tauri/src/platform/windows.rs");

    assert!(
        src.contains("pub fn focused_field_is_secure()"),
        "Windows has no focused-field check at all, so a password dictated on the PC is stored \
         and replicated"
    );
    let f = src
        .split("pub fn focused_field_is_secure()")
        .nth(1)
        .and_then(|s| s.split("\npub fn ").next())
        .expect("the function is in the file");
    assert!(
        f.contains("ES_PASSWORD"),
        "the Windows check does not actually inspect the focused control's style"
    );
    assert!(
        f.contains("Option<bool>") || src.contains("focused_field_is_secure() -> Option<bool>"),
        "it must distinguish 'not a password field' from 'could not tell'"
    );
    // The known gap is stated where the code is.
    assert!(
        src.contains("UI Automation") || src.contains("KNOWN GAP"),
        "the WinUI and Chromium gap must be written down, or the next reader takes this for \
         full coverage exactly as they took the constant `false`"
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

    // Round 11 replaced the free function with a three-state `FieldSecrecy`,
    // because a boolean could not tell "this is a password field" apart from
    // "secure input is on and I cannot see the field", and the two deserve
    // different treatment. The premise below tracks the decision, not its shape.
    assert!(
        code.contains("enum FieldSecrecy") && code.contains("fn sample_field_secrecy()"),
        "premise: the secure-field decision is still made in pipeline.rs"
    );
    assert!(
        code.contains("fn store_transcription("),
        "premise: both dictation paths still route storage through one helper"
    );

    // R10-B2's finding: when the decision withholds a dictation from History,
    // the user must be TOLD. `secure_input_active()` is process-global and
    // system-wide on macOS, and one crashed app or Terminal's Secure Keyboard
    // Entry can leave it stuck on. While it is stuck, every dictation is
    // withheld. The only report used to be a `tracing::info!` the user never
    // sees, so the failure was indistinguishable from the app being broken.
    let helper = code
        .split("fn store_transcription(")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .expect("store_transcription is in the file");
    let withholds =
        helper.matches("drop_entirely()").count() + helper.matches("keep_local_only()").count();
    assert!(withholds >= 2, "premise: both withholding branches are in the helper");
    assert_eq!(
        helper.matches("Some(").count(),
        withholds,
        "R10-B2: a branch that withholds a dictation from History returns no message for the \
         user. When `secure_input_active()` is stuck on, EVERY dictation is withheld and the \
         only report is a `tracing::info!` nobody sees: no event, no warning, no way to notice."
    );
    assert!(
        code.contains("PipelineEvent::Empty { reason }"),
        "R10-B2: the message the helper returns is never emitted, so it reaches nobody"
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
    // INVERTED. `clearContents` is what advances the change count, so setting
    // the payload first and the markers afterwards left a window in which the
    // pasteboard had changed and carried no marker yet. A monitor polling in
    // that window saw unmarked content. Microseconds against a 150ms poll, so
    // it was a race rather than a routine leak, but it is closed by declaring
    // every type before any value is set.
    let code = code_of("src-tauri/src/platform/macos.rs");
    let f = code
        .split("fn write_clipboard_impl(")
        .nth(1)
        .and_then(|s| s.split("\nfn ").next())
        .expect("write_clipboard_impl is in the file");

    assert!(
        f.contains("declareTypes_owner"),
        "the markers are still attached after the payload, so there is a window in which the \
         pasteboard has changed and carries no marker"
    );
    let declare = f.find("declareTypes_owner").expect("checked above");
    let set_value = f.find("setString_forType(&value").expect("the payload is still written");
    assert!(
        declare < set_value,
        "the types must be declared BEFORE the value is set, or the window is still open"
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

    // The premise: both read paths scan for a NUL terminator. Matched on the
    // dereference alone, NOT on the loop condition, so adding a bound changes
    // the verdict rather than voiding the premise.
    let scans: Vec<&str> = code.match_indices("*ptr.add(len) != 0").map(|(i, _)| &code[..i]).collect();
    assert_eq!(
        scans.len(),
        2,
        "premise: the NUL scan appears in `read_clipboard` and in \
         `read_clipboard_unless_excluded`, found {}",
        scans.len()
    );
    // The fixed DWORD read is the control: the file knows how to do this.
    assert!(
        code.contains("if GlobalSize(h) < 4 {"),
        "control: the DWORD read really is bounded, so the omission below is an omission"
    );

    // Every scan must carry a size bound taken from the SAME handle, near it.
    let unbounded = scans
        .iter()
        .filter(|before| {
            let tail = &before[before.len().saturating_sub(400)..];
            !(tail.contains("GlobalSize") && tail.contains("len <"))
        })
        .count();
    assert_eq!(
        unbounded, 0,
        "R10-D: {unbounded} of 2 CF_UNICODETEXT reads walk another process's \
         allocation looking for a NUL with no `GlobalSize` bound, the exact defect \
         round 9 fixed one branch above in the same function"
    );
}

// ---------------------------------------------------------------------------
// R10-E. Attacked and HELD. Recorded so the next round starts elsewhere.
// ---------------------------------------------------------------------------

fn collect_field_names(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![v];
    while let Some(node) = stack.pop() {
        match node {
            serde_json::Value::Object(m) => {
                for (k, child) in m {
                    out.push(k.clone());
                    stack.push(child);
                }
            }
            serde_json::Value::Array(a) => stack.extend(a.iter()),
            _ => {}
        }
    }
    out
}

/// I. No paired key, and nothing key-shaped, is serialisable into settings.json.
#[test]
fn r10_e1_settings_carry_no_key_material() {
    let mut s = Settings::default();
    s.sync.device_id = A.to_string();
    s.sync.device_name = "Ben's Mac".into();
    s.sync.paired.push(parle_core::settings::PairedDevice {
        id: "22222222-2222-4222-8222-222222222222".into(),
        name: "Ben's G14".into(),
        last_seen: Some(1),
    });
    let json = serde_json::to_string(&s).unwrap();
    // Field NAMES only. The first version of this matched raw substrings and
    // fired on `"hotkeys"` and `"key":"Fn"`, which is a hotkey binding and not
    // a secret: a guard that flags the theme is a guard nobody will read.
    let names = collect_field_names(&serde_json::from_str(&json).unwrap());
    // `key` is deliberately NOT here: `hotkeys.dictation.key` is a keyboard
    // binding, and a guard that fires on the hotkey settings is a guard that
    // gets muted. The 64-hex shape check below is what actually catches a key.
    for forbidden in ["secret", "psk", "noise", "spake", "paired_key", "shared_secret"] {
        let hit: Vec<&String> = names
            .iter()
            .filter(|n| n.to_ascii_lowercase() == forbidden)
            .collect();
        assert!(hit.is_empty(), "settings.json carries a {forbidden} field");
    }
    // And nothing in it is 64 hex characters, which is how a PairedKey is
    // written by `keystore::to_hex`. That is the shape, not the name.
    assert!(
        !json.split('"').any(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())),
        "settings.json carries a 64-hex-character string, the exact shape keystore writes"
    );
    // And the struct that DOES hold peers has exactly three fields, none secret.
    let one = serde_json::to_value(&s.sync.paired[0]).unwrap();
    assert_eq!(
        one.as_object().unwrap().len(),
        3,
        "PairedDevice grew a field; check it carries no secret"
    );
}

/// R10-F. **The gate is ON right now, on this machine, with no password field
/// anywhere.**
///
/// `IsSecureEventInputEnabled()` is not a question about the focused field. It
/// is a SYSTEM-WIDE, process-global flag that any application can raise and
/// that stays raised until that application lowers it or exits. 1Password
/// raises it. Terminal's Secure Keyboard Entry raises it. An app that crashes
/// while holding it leaves it raised.
///
/// Round 9 made every dictation depend on it, on both paths, unconditionally.
/// So while it is up: every dictation returns `item_id = -1` and is never
/// stored, and every `copy_to_clipboard` write is marked ConcealedType so the
/// user's other clipboard managers drop it too. The only report is a
/// `tracing::info!`. The app looks like it is working and silently keeps
/// nothing. Round 8's version, derived from `InjectionOutcome`, could not do
/// this on the `copy_to_clipboard` branch at all.
///
/// Verified outside Rust as well, so this is not an FFI artefact: a three-line
/// C program linked against Carbon prints `raw byte = 1` on this machine,
/// repeatedly, over several seconds, with 1Password running and Terminal's
/// `SecureKeyboardEntry` default reading 0.
#[cfg(target_os = "macos")]
#[test]
fn r10_f_secure_input_is_globally_on_so_every_dictation_is_silently_dropped() {
    // INVERTED. The finding was the most consequential of the round and it was
    // confirmed on this hardware: `IsSecureEventInputEnabled()` reads TRUE
    // continuously with a password manager merely RUNNING and no password field
    // focused. Round 9 keyed both dictation paths off it, so the app threw away
    // every dictation, all day, reporting nothing but a log line.
    //
    // The gate asks the FOCUSED ELEMENT now. The global flag still decides
    // whether to mark the clipboard concealed, where over-marking costs the
    // user nothing visible, but it must not decide whether to keep the row.
    let code = code_of("src-tauri/src/pipeline.rs");

    assert!(
        code.contains("focused_field_is_secure()"),
        "the dictation gate does not ask which element has focus, so it cannot tell a password \
         field from a password manager being open"
    );
    assert!(
        !code.contains("fn into_secure_field() -> bool {\n    platform::imp::secure_input_active()"),
        "the gate is still the bare system-wide flag"
    );
    // The concealed decision may still consult it, and should.
    assert!(
        code.contains("secure_input_active()"),
        "the clipboard marking should still widen to the global flag: marking an ordinary \
         transcript concealed costs nothing the user sees, failing to mark a password does"
    );

    // And the global flag really is on right now, which is what made this a
    // finding rather than a theory. If it is ever false here the test still
    // holds, it just stops being a live demonstration.
    let globally_on = crate::platform::macos::secure_input_active();
    eprintln!("R10-F: IsSecureEventInputEnabled() is currently {globally_on}");
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

    // FIVE exits now, every one carrying a fresh reading.
    //
    // It was four, matching the pre-round-9 loop. Round 10 added a fifth: an
    // early return when the change was one PARLE made, identified by change
    // count rather than by marking our own writes TransientType (which told
    // every other clipboard manager to bin the row the user had just
    // deliberately copied).
    //
    // The COUNT is not the invariant, the property is: every path out of the
    // pool must carry a fresh reading, or the next capture is attributed to a
    // stale app and the exclusion list is matched against the wrong one. The
    // count is how that property is checked, so it moves when a branch is
    // legitimately added.
    let exits = body.matches("macos::frontmost_app()").count();
    assert_eq!(
        exits, 5,
        "a branch stopped refreshing prev_app; the next capture is attributed to a \
         stale app and the exclusion list is matched against the wrong one"
    );
    // Named individually so a future edit cannot satisfy the count by moving
    // one call and dropping another.
    for branch in [
        "if !enabled.load(Ordering::SeqCst) {",
        "if now == last {",
        "if macos::clipboard_is_concealed() {",
    ] {
        let after = body.split(branch).nth(1).unwrap_or_else(|| panic!("{branch} is gone"));
        assert!(
            after[..after.len().min(200)].contains("macos::frontmost_app()"),
            "the {branch:?} branch stopped refreshing prev_app"
        );
    }
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
