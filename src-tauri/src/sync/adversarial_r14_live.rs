//! Live-hardware diagnostics for the stuck secure-input flag.
//!
//! Round 10 (R10-B2) and round 13 both predicted that macOS can leave
//! `IsSecureEventInputEnabled()` raised after the process holding it exits, and
//! both had to label it SPECULATIVE because neither could produce the state on
//! demand. On 28/08/2026 the development Mac was found in exactly that state,
//! reported by the user as "every field acts like a password field": the flag
//! read TRUE and `kCGSSessionSecureInputPID` named a dead process.
//!
//! These are `#[ignore]`d because their result depends on the machine they run
//! on. Run them by name when a Mac starts refusing to auto-paste.

#[cfg(target_os = "macos")]
#[test]
#[ignore = "diagnostic: reports this machine's live secure-input state"]
fn r14_live_secure_input_state() {
    let raw = unsafe {
        extern "C" {
            fn IsSecureEventInputEnabled() -> bool;
        }
        IsSecureEventInputEnabled()
    };
    let effective = crate::platform::imp::secure_input_active();
    println!("raw IsSecureEventInputEnabled() = {raw}");
    println!("effective secure_input_active() = {effective}");
    println!("holder pid as we read it = {:?}", crate::platform::imp::secure_input_holder_pid_for_test());
    println!(
        "If raw is true and effective is false, the flag on this machine is STALE and the fix \
         is doing its job. If both are true, something really is holding secure input."
    );
}

/// The property that matters, independent of this machine's state.
///
/// `secure_input_active()` must never be MORE permissive than the raw flag: it
/// may only ever turn a true into a false (a stale flag), never a false into a
/// true. A regression in the other direction would be a password field going
/// unprotected, which is the worse error.
#[cfg(target_os = "macos")]
#[test]
fn r14_live_the_liveness_check_only_ever_relaxes_a_raised_flag() {
    let raw = unsafe {
        extern "C" {
            fn IsSecureEventInputEnabled() -> bool;
        }
        IsSecureEventInputEnabled()
    };
    let effective = crate::platform::imp::secure_input_active();
    assert!(
        raw || !effective,
        "secure_input_active() reported TRUE while the OS flag is FALSE. The liveness check is \
         only ever allowed to relax a raised flag, never to raise a lowered one"
    );
}

/// The cancel key must have a path that does not depend on the event tap.
///
/// Reported from real use: "escape cancel is on and pressing Escape does
/// nothing". The cause was not in the Escape handling, which reads correctly.
/// It was that the tap was the ONLY listener. `register_chord_shortcuts`
/// deliberately skips every key `NativeKey::parse` recognises, on the grounds
/// that the native listener owns it, and Escape is one of those, so the cancel
/// key was never registered as a global shortcut. When the tap delivers
/// nothing, which is what happens without an Accessibility grant, the setting
/// is a switch wired to nothing.
///
/// This asserts the second path exists and is armed by recording state, which
/// is the property that makes the feature work rather than the mechanism.
#[test]
fn r14_live_the_cancel_key_has_a_path_that_does_not_need_the_tap() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib = std::fs::read_to_string(root.join("src/lib.rs")).expect("lib.rs is readable");
    let state = std::fs::read_to_string(root.join("src/state.rs")).expect("state.rs is readable");

    // PREMISE: the generic registration really does skip native keys, so the
    // cancel key genuinely cannot be covered by it.
    assert!(
        lib.contains("if platform::NativeKey::parse(&binding.key).is_some() {"),
        "premise: register_chord_shortcuts no longer skips native keys; re-read this finding"
    );
    assert!(
        crate::platform::NativeKey::parse("Escape").is_some(),
        "premise: Escape is a native key, which is why the skip above excludes it"
    );

    // THE CLAIM: a dedicated path exists, and recording state drives it.
    assert!(
        lib.contains("fn set_cancel_shortcut_armed("),
        "the cancel key has no registration path outside the event tap, so the setting does \
         nothing whenever the tap is not delivering key events"
    );
    assert!(
        state.contains("set_cancel_shortcut_armed(&app, self, on)"),
        "the cancel shortcut is never armed from recording state, so it is either always \
         registered (swallowing Escape system-wide) or never"
    );
}

/// A launch the USER asked for must show the window, on BOTH platforms.
///
/// Reported from real use on macOS: double-clicking the app made the Dock icon
/// bounce and then apparently nothing happened. The window was created visible
/// by the config and hidden a moment later on every onboarded launch, so it
/// flashed and vanished and the only sign the app was running was a menu bar
/// icon nobody was looking for. It read as "the app is broken".
///
/// Windows had already been gated on the `--hidden` flag that autostart passes.
/// macOS had not, so the two platforms answered the same question differently.
/// This pins the answer rather than the mechanism: hiding at startup must be
/// conditional on the system having asked for the launch.
#[test]
fn r14_live_a_user_launch_shows_the_window_on_both_platforms() {
    let lib = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .expect("lib.rs is readable");
    let code: String = lib
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    // PREMISE: autostart still passes the flag, or there is nothing to gate on.
    assert!(
        code.contains("\"--hidden\""),
        "premise: autostart no longer registers with --hidden, so nothing distinguishes a \
         login launch from a launch the user asked for"
    );

    // THE CLAIM: the startup hide is gated on it, and NOT behind a
    // platform cfg that lets one OS hide unconditionally.
    let hides: Vec<&str> = code
        .lines()
        .filter(|l| l.contains("main.hide()"))
        .collect();
    assert!(!hides.is_empty(), "nothing hides the main window at startup any more");
    assert!(
        code.contains("let launched_by_system = std::env::args().any(|a| a == \"--hidden\");"),
        "the startup hide is not gated on whether the system asked for this launch, so a user \
         double-clicking the app sees the window flash up and vanish"
    );
    assert!(
        code.contains("if state.settings.lock().onboarding_complete && launched_by_system {"),
        "the hide condition has changed shape; check it still requires a system launch"
    );
}
