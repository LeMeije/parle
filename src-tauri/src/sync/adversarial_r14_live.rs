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
