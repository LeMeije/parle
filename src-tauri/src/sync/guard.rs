//! Pairing attempt guard: expiry, single-flight, and rate limiting.
//!
//! SPAKE2 gives an attacker exactly one guess per protocol run, so the maths of
//! the 6-digit code is only as good as the number of runs we allow. Unlimited
//! attempts turn a 10^6 keyspace into minutes of grinding on a LAN. The sync
//! crate deliberately has no timer and no cross-run memory — that state has to
//! live somewhere that outlives a single pairing, which is here.
//!
//! The policy: a code lives two minutes, only one is live at a time, and the
//! budget is counted BOTH per source address (3 guesses, with 1s/2s/4s backoff
//! between them) and per code (12 in total).
//!
//! Both halves are load-bearing, and neither works alone.
//!
//! The per-code total is the crypto bound: it is what keeps a 10^6 keyspace
//! meaningful against online guessing.
//!
//! The per-source limit is what keeps pairing USABLE while someone is
//! attacking it. Two earlier designs failed here. Burning the code after five
//! failures meant anyone on the LAN could kill every fresh code with a few
//! bytes of well-formed junk, faster than a human can read six digits off one
//! screen and type them into another — pairing simply never worked while they
//! were present. Global backoff failed the same way for the same reason: an
//! automated attacker always wins the race to the next open slot. Keyed by
//! source, the honest device dials from its own address and finds its own
//! allowance untouched.
//!
//! Residual, stated plainly: an attacker with many addresses on the same
//! segment can still spend a code's 12 guesses and retire it. They cannot stop
//! the user showing another, and a fresh code is independently random, so
//! nothing carries over — the cost is a retry, not a lockout.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// How long a displayed code stays valid.
pub const CODE_TTL: Duration = Duration::from_secs(120);
/// The most guesses ONE source address may spend against a single code.
///
/// The per-source part is what keeps pairing usable while under attack. A
/// global counter alone could not: an automated attacker always wins the race
/// against a human reading six digits off a screen, so whatever the budget, it
/// was spent before the honest device connected. The honest device dials from
/// its own address and therefore has its own untouched allowance.
pub const MAX_PER_SOURCE: u32 = 3;
/// The most guesses a single code will ever absorb, across all sources.
///
/// This is the crypto bound: it is what keeps a 10^6 keyspace meaningful. An
/// attacker with many addresses can exhaust it, which retires that code — the
/// user can always ask for another, and a fresh code is independently random,
/// so nothing is learned. What they cannot do is make pairing impossible.
pub const MAX_PER_CODE: u32 = 12;
/// Delay imposed after the first wrong guess. Doubles with each failure.
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// The longest we ever make the next guess wait.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How long to wait after `failures` wrong guesses.
///
/// Doubling from one second and capped at thirty fits about eight guesses into
/// a code's two-minute life. Saturating throughout: a long-lived guard must not
/// be able to overflow the shift or the instant.
fn backoff_for(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let shift = (failures - 1).min(16);
    let secs = BACKOFF_BASE.as_secs().saturating_mul(1u64 << shift);
    Duration::from_secs(secs).min(BACKOFF_MAX)
}

#[derive(Debug, PartialEq, Eq)]
pub enum GuardError {
    /// No code is being shown right now.
    NotPairing,
    /// The code was shown too long ago.
    Expired,
    /// Wrong codes are arriving too fast; the next guess has to wait.
    LockedOut { retry_in: Duration },
    /// This code has absorbed all the guesses it ever will. Ask for a new one.
    CodeExhausted,
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
            Self::CodeExhausted => {
                write!(f, "too many incorrect codes for that one; show a new code")
            }
        }
    }
}

struct Active {
    code: String,
    started: Instant,
    /// Guesses spent against this code, in total and per source address.
    spent: u32,
    per_source: HashMap<IpAddr, Source>,
}

#[derive(Default)]
struct Source {
    failures: u32,
    next_allowed: Option<Instant>,
}

/// Guards the pairing window. One per app.
pub struct PairingGuard {
    active: Option<Active>,
}

impl Default for PairingGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingGuard {
    pub fn new() -> Self {
        Self { active: None }
    }

    /// Begin showing `code`. Replaces any code already on screen, so a user who
    /// cancels and restarts cannot leave an older code quietly still valid.
    ///
    /// Backoff does NOT block this, and the failure count resets with the new
    /// code: a fresh code is independently random, so guesses against the old
    /// one carry no information about it.
    pub fn begin(&mut self, code: String, now: Instant) -> Result<(), GuardError> {
        self.active = Some(Active {
            code,
            started: now,
            spent: 0,
            per_source: HashMap::new(),
        });
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
    pub fn reserve(&mut self, now: Instant, from: IpAddr) -> Result<String, GuardError> {
        let Some(active) = self.active.as_mut() else {
            return Err(GuardError::NotPairing);
        };
        if now.saturating_duration_since(active.started) >= CODE_TTL {
            self.active = None;
            return Err(GuardError::Expired);
        }
        if active.spent >= MAX_PER_CODE {
            return Err(GuardError::CodeExhausted);
        }

        let src = active.per_source.entry(from).or_default();
        if let Some(until) = src.next_allowed {
            if now < until {
                return Err(GuardError::LockedOut { retry_in: until - now });
            }
        }
        if src.failures >= MAX_PER_SOURCE {
            return Err(GuardError::CodeExhausted);
        }

        src.failures += 1;
        src.next_allowed = now.checked_add(backoff_for(src.failures));
        active.spent += 1;
        // The code stays live. Burning it was what let an attacker kill every
        // fresh code before a human could type one.
        Ok(active.code.clone())
    }

    /// The reserved guess was correct: clear the window and refund the budget.
    pub fn succeed(&mut self) {
        self.reset();
    }

    fn reset(&mut self) {
        self.active = None;
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// The attacker's address in these tests.
    fn evil() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 66))
    }

    /// The user's own second device.
    fn honest() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7))
    }

    fn t0() -> Instant {
        Instant::now()
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
            if g.reserve(now, evil()).is_ok() {
                granted += 1;
            }
        }
        assert_eq!(
            granted, 1,
            "a thousand simultaneous attempts must not buy a thousand guesses"
        );
    }

    #[test]
    fn a_reserved_guess_that_succeeds_refunds_the_budget() {
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();
        let code = g.reserve(now, evil()).unwrap();
        assert_eq!(code, "123456");
        g.succeed();
        // Budget restored and the window closed.
        g.begin("654321".into(), now).unwrap();
        assert!(g.reserve(now, evil()).is_ok(), "a fresh code starts with no backoff owing");
    }

    #[test]
    fn reserve_refuses_when_no_code_is_live_or_it_expired() {
        let mut g = PairingGuard::new();
        let now = t0();
        assert_eq!(g.reserve(now, evil()), Err(GuardError::NotPairing));
        g.begin("123456".into(), now).unwrap();
        let late = now + CODE_TTL + Duration::from_secs(1);
        assert_eq!(g.reserve(late, evil()), Err(GuardError::Expired));
    }

    #[test]
    fn a_reserved_code_expires() {
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();
        let late = now + CODE_TTL + Duration::from_secs(1);
        assert_eq!(g.reserve(late, evil()), Err(GuardError::Expired));
        assert!(g.code(late).is_none());
    }

    #[test]
    fn grinding_slows_down_but_never_kills_the_live_code() {
        // Backoff, not a lockout, and the code is NOT burnt.
        //
        // Burning it after five failures handed anyone on the LAN a permanent
        // denial of service: a well-formed junk message costs an attacker
        // nothing, five arrive in milliseconds, and a human needs seconds to
        // read six digits off one screen and type them into another. Every
        // fresh code died before it could be used.
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();

        assert!(g.reserve(now, evil()).is_ok(), "the first guess is free");
        match g.reserve(now, evil()) {
            Err(GuardError::LockedOut { retry_in }) => {
                assert!(retry_in <= BACKOFF_BASE, "first backoff is one step: {retry_in:?}");
            }
            other => panic!("expected backoff, got {other:?}"),
        }

        // Waiting it out returns the SAME code: the honest user is delayed,
        // never locked out of their own pairing.
        let later = now + BACKOFF_BASE + Duration::from_millis(1);
        assert_eq!(g.reserve(later, evil()).as_deref(), Ok("123456"));
    }

    #[test]
    fn backoff_bounds_an_attacker_to_single_digits_of_guesses_per_code() {
        // The security property that replaced the lockout. Grinding as fast as
        // the backoff allows, for the whole life of one code.
        let mut g = PairingGuard::new();
        let start = t0();
        g.begin("123456".into(), start).unwrap();

        let mut now = start;
        let mut guesses = 0;
        // Step in 100ms slices so the attacker takes every opening the instant
        // it appears.
        while now.saturating_duration_since(start) < CODE_TTL {
            if g.reserve(now, evil()).is_ok() {
                guesses += 1;
            }
            now += Duration::from_millis(100);
        }
        assert!(
            (1..=10).contains(&guesses),
            "an attacker got {guesses} guesses against one code in its lifetime"
        );
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff_for(0), Duration::ZERO);
        assert_eq!(backoff_for(1), BACKOFF_BASE);
        assert_eq!(backoff_for(2), BACKOFF_BASE * 2);
        assert_eq!(backoff_for(3), BACKOFF_BASE * 4);
        assert_eq!(backoff_for(60), BACKOFF_MAX, "capped, not overflowed");
        assert_eq!(backoff_for(u32::MAX), BACKOFF_MAX, "and cannot shift-overflow");
    }


    #[test]
    fn backoff_governs_the_live_code_but_never_blocks_a_fresh_one() {
        // Asking for a new code always works, even mid-backoff. It is safe
        // because guesses against a discarded code say nothing about a freshly
        // random replacement.
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();
        let _ = g.reserve(now, evil());
        assert!(matches!(g.reserve(now, evil()), Err(GuardError::LockedOut { .. })));

        // The user can still ask for another one, immediately.
        g.begin("654321".into(), now).expect("a fresh code must always be available");

        // And it starts clean rather than inheriting the old backoff.
        assert_eq!(g.reserve(now, evil()).as_deref(), Ok("654321"));
        assert!(matches!(g.reserve(now, evil()), Err(GuardError::LockedOut { .. })));
    }


    #[test]
    fn codes_that_simply_lapse_do_not_count_as_guesses() {
        let mut g = PairingGuard::new();
        let mut now = t0();
        // A user who walks away is not an attacker and must not be locked out.
        for _ in 0..MAX_PER_SOURCE + 2 {
            g.begin("123456".into(), now).unwrap();
            now += CODE_TTL + Duration::from_secs(1);
            assert_eq!(g.reserve(now, evil()), Err(GuardError::Expired));
        }
        g.begin("123456".into(), now).unwrap();
        assert!(g.reserve(now, evil()).is_ok());
    }
}
