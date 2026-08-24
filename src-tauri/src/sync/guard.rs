//! Pairing attempt guard: expiry, single-flight, and rate limiting.
//!
//! SPAKE2 gives an attacker exactly one guess per protocol run, so the maths of
//! the 6-digit code is only as good as the number of runs we allow. Unlimited
//! attempts turn a 10^6 keyspace into minutes of grinding on a LAN. The sync
//! crate deliberately has no timer and no cross-run memory — that state has to
//! live somewhere that outlives a single pairing, which is here.
//!
//! The policy: a code is valid for two minutes and only one is live at a time;
//! five failures locks pairing out for five minutes and burns the code. An
//! attacker therefore gets 5 guesses per 5 minutes against a keyspace of a
//! million, and every failure is visible to the user who is watching the code
//! on screen.

use std::time::{Duration, Instant};

/// How long a displayed code stays valid.
pub const CODE_TTL: Duration = Duration::from_secs(120);
/// Failures before pairing locks out.
pub const MAX_FAILURES: u32 = 5;
/// How long a lockout lasts.
pub const LOCKOUT: Duration = Duration::from_secs(300);

#[derive(Debug, PartialEq, Eq)]
pub enum GuardError {
    /// No code is being shown right now.
    NotPairing,
    /// The code was shown too long ago.
    Expired,
    /// Too many wrong codes; pairing is closed for a while.
    LockedOut { retry_in: Duration },
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPairing => write!(f, "no pairing is in progress"),
            Self::Expired => write!(f, "that pairing code has expired"),
            Self::LockedOut { retry_in } => write!(
                f,
                "too many incorrect codes; try again in {} seconds",
                retry_in.as_secs()
            ),
        }
    }
}

struct Active {
    code: String,
    started: Instant,
}

/// Guards the pairing window. One per app.
pub struct PairingGuard {
    active: Option<Active>,
    failures: u32,
    locked_until: Option<Instant>,
}

impl Default for PairingGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingGuard {
    pub fn new() -> Self {
        Self { active: None, failures: 0, locked_until: None }
    }

    /// Begin showing `code`. Replaces any code already on screen, so a user who
    /// cancels and restarts cannot leave an older code quietly still valid.
    ///
    /// A lockout does NOT block this, and that is deliberate. The budget exists
    /// to stop online guessing against ONE code, and it does: five wrong
    /// guesses burn that code and it can never be guessed again. Refusing to
    /// issue a NEW one on top of that did not add any security — guesses
    /// against a discarded code tell an attacker nothing about a freshly random
    /// replacement, so the odds stay at five attempts in 10^6 per code — while
    /// it did hand anyone on the LAN a permanent denial of service: 33 bytes of
    /// well-formed junk, five times, and the user could not pair a device at
    /// all for five minutes, repeatable forever.
    ///
    /// Issuing a new code therefore clears the failure count with it. The
    /// lockout still governs guesses against whatever code is currently live.
    pub fn begin(&mut self, code: String, now: Instant) -> Result<(), GuardError> {
        self.failures = 0;
        self.locked_until = None;
        self.active = Some(Active { code, started: now });
        Ok(())
    }

    pub fn cancel(&mut self) {
        self.active = None;
    }

    /// Seconds left on the current code, if one is live.
    pub fn expires_in(&self, now: Instant) -> Option<Duration> {
        let a = self.active.as_ref()?;
        CODE_TTL.checked_sub(now.saturating_duration_since(a.started))
    }

    pub fn code(&self, now: Instant) -> Option<&str> {
        let a = self.active.as_ref()?;
        if now.saturating_duration_since(a.started) >= CODE_TTL {
            return None;
        }
        Some(a.code.as_str())
    }

    /// Reserve one guess BEFORE the network exchange runs, returning the code
    /// to attempt with.
    ///
    /// This ordering is the whole point. Checking the budget only after the
    /// exchange completes is a TOCTOU race: an attacker opening a thousand
    /// concurrent connections would read the same live code a thousand times
    /// and get a thousand guesses before any of them incremented the counter,
    /// which defeats the rate limit completely. Charging the attempt up front,
    /// atomically under the lock, bounds concurrent guessing to the budget.
    ///
    /// The cost is that an exchange interrupted by a dropped network still
    /// spends a guess. That is the right trade at five attempts per five
    /// minutes, and `succeed` refunds the whole budget on the happy path.
    pub fn reserve(&mut self, now: Instant) -> Result<String, GuardError> {
        self.check_lockout(now)?;
        let Some(active) = self.active.as_ref() else {
            return Err(GuardError::NotPairing);
        };
        if now.saturating_duration_since(active.started) >= CODE_TTL {
            self.active = None;
            return Err(GuardError::Expired);
        }
        let code = active.code.clone();
        self.failures += 1;
        if self.failures >= MAX_FAILURES {
            self.locked_until = Some(now + LOCKOUT);
            // Burn the code so waiting out the lockout does not hand the
            // attacker back the same target.
            self.active = None;
            return Err(GuardError::LockedOut { retry_in: LOCKOUT });
        }
        Ok(code)
    }

    /// The reserved guess was correct: clear the window and refund the budget.
    pub fn succeed(&mut self) {
        self.reset();
    }

    fn reset(&mut self) {
        self.active = None;
        self.failures = 0;
        self.locked_until = None;
    }

    fn check_lockout(&mut self, now: Instant) -> Result<(), GuardError> {
        if let Some(until) = self.locked_until {
            if now < until {
                return Err(GuardError::LockedOut { retry_in: until - now });
            }
            self.locked_until = None;
            self.failures = 0;
        }
        Ok(())
    }
}


/// Length-independent, value-constant comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn constant_time_eq_matches_normal_equality() {
        assert!(constant_time_eq(b"123456", b"123456"));
        assert!(!constant_time_eq(b"123456", b"123457"));
        assert!(!constant_time_eq(b"123456", b"12345"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn concurrent_guesses_cannot_exceed_the_budget() {
        // The attack the reserve/succeed split exists to stop: many peers
        // reading the same live code before any of them has been charged.
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();

        let mut granted = 0;
        for _ in 0..1000 {
            if g.reserve(now).is_ok() {
                granted += 1;
            }
        }
        assert_eq!(
            granted,
            (MAX_FAILURES - 1) as usize,
            "a thousand simultaneous attempts must not buy a thousand guesses"
        );
    }

    #[test]
    fn a_reserved_guess_that_succeeds_refunds_the_budget() {
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();
        let code = g.reserve(now).unwrap();
        assert_eq!(code, "123456");
        g.succeed();
        // Budget restored and the window closed.
        g.begin("654321".into(), now).unwrap();
        for _ in 0..MAX_FAILURES - 1 {
            assert!(g.reserve(now).is_ok());
        }
    }

    #[test]
    fn reserve_refuses_when_no_code_is_live_or_it_expired() {
        let mut g = PairingGuard::new();
        let now = t0();
        assert_eq!(g.reserve(now), Err(GuardError::NotPairing));
        g.begin("123456".into(), now).unwrap();
        let late = now + CODE_TTL + Duration::from_secs(1);
        assert_eq!(g.reserve(late), Err(GuardError::Expired));
    }

    #[test]
    fn a_reserved_code_expires() {
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();
        let late = now + CODE_TTL + Duration::from_secs(1);
        assert_eq!(g.reserve(late), Err(GuardError::Expired));
        assert!(g.code(late).is_none());
    }

    #[test]
    fn grinding_locks_pairing_out_and_burns_the_code() {
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();
        for _ in 0..MAX_FAILURES - 1 {
            assert!(g.reserve(now).is_ok(), "within budget");
        }
        match g.reserve(now) {
            Err(GuardError::LockedOut { .. }) => {}
            other => panic!("expected lockout, got {other:?}"),
        }
        // The code is burned, so waiting out the lockout does not hand the
        // attacker back the same target.
        let after = now + LOCKOUT + Duration::from_secs(1);
        assert_eq!(g.reserve(after), Err(GuardError::NotPairing));
    }

    #[test]
    fn a_lockout_governs_the_live_code_but_never_blocks_a_fresh_one() {
        // The lockout used to refuse a NEW code too, which added nothing and
        // handed anyone on the LAN a permanent denial of service: junk of the
        // right shape, five times, and the user could not pair at all.
        //
        // It is safe to hand out a new one because guesses against a discarded
        // code say nothing about a freshly random replacement — the odds stay
        // at MAX_FAILURES attempts in 10^6 per code.
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();
        for _ in 0..MAX_FAILURES {
            let _ = g.reserve(now);
        }
        // Locked out for the code that was on screen.
        assert!(matches!(g.reserve(now), Err(GuardError::LockedOut { .. })));

        // But the user can still ask for another one, immediately.
        g.begin("654321".into(), now).expect("a fresh code must always be available");

        // And that new code gets its own full budget, not a poisoned one.
        let mut granted = 0;
        for _ in 0..MAX_FAILURES + 3 {
            if g.reserve(now).is_ok() {
                granted += 1;
            }
        }
        assert_eq!(
            granted,
            (MAX_FAILURES - 1) as usize,
            "the new code is rate-limited exactly like the first"
        );
    }


    #[test]
    fn codes_that_simply_lapse_do_not_count_as_guesses() {
        let mut g = PairingGuard::new();
        let mut now = t0();
        // A user who walks away is not an attacker and must not be locked out.
        for _ in 0..MAX_FAILURES + 2 {
            g.begin("123456".into(), now).unwrap();
            now += CODE_TTL + Duration::from_secs(1);
            assert_eq!(g.reserve(now), Err(GuardError::Expired));
        }
        g.begin("123456".into(), now).unwrap();
        assert!(g.reserve(now).is_ok());
    }
}
