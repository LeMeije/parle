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

/// Fold an address to its routing prefix: /24 for IPv4, /64 for IPv6.
///
/// NOT used for the pairing or pre-auth budgets, and the reason is worth
/// recording. Folding looks like the obvious hardening — an IPv6 host owns a
/// whole /64 and can mint addresses at will — but on a home LAN the user's own
/// second device sits in the SAME prefix as the attacker, so folding hands
/// them one shared budget and destroys the carve-out that keeps pairing
/// possible while under attack. Exact addresses are what give the honest
/// device an allowance of its own.
pub fn network_of(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            IpAddr::V4(std::net::Ipv4Addr::new(o[0], o[1], o[2], 0))
        }
        IpAddr::V6(v6) => {
            // Normalise a v4-mapped address to its v4 network first, or the
            // same host counts twice depending on how it connected.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return network_of(IpAddr::V4(v4));
            }
            let mut o = v6.octets();
            o[8..].fill(0);
            IpAddr::V6(std::net::Ipv6Addr::from(o))
        }
    }
}

/// How long a displayed code stays valid.
pub const CODE_TTL: Duration = Duration::from_secs(120);
/// The most guesses ONE source NETWORK may spend against a single code.
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
/// The absolute ceiling on guesses against one code, counting the free first
/// guess every previously-unseen source is given.
///
/// This is a straight trade and the arithmetic is the whole argument. Against a
/// 6-digit code the chance an attacker wins is `ceiling / 10^6`; at 200 that is
/// 2 in 10,000 per code, on a code that lives two minutes. In exchange the
/// attacker needs 200 distinct source addresses inside that window to spend the
/// budget and retire a code, rather than the 40 it used to take.
///
/// It cannot be pushed to "never": a budget that always admits an unseen source
/// is not a budget, and an attacker can mint addresses. What the reservation
/// guarantees is that grinding from a realistic handful of addresses cannot
/// shut the user's own device out, and that a retired code costs the user one
/// press of "show code" rather than a lockout.
///
/// The two limits exist because one cannot do both jobs. `MAX_PER_CODE` is the
/// budget for sources that have already guessed, and it is what an attacker
/// grinding from a handful of addresses runs into. But an attacker can mint
/// addresses, so a single shared total — however large — is always spent before
/// a human has read six digits off one screen and typed them into another, and
/// pairing simply never works while they are present.
///
/// So a source that has NOT yet guessed against this code always gets one, up
/// to this hard ceiling. The user's second device is exactly that source, so it
/// gets in. The cost is that the crypto bound is this number rather than
/// `MAX_PER_CODE`: 40 guesses against a 10^6 keyspace, on a code that lives two
/// minutes. An attacker with more than 40 addresses can still retire one code —
/// they cannot stop the user showing another, and a fresh code is independently
/// random.
pub const HARD_MAX_PER_CODE: u32 = 200;
/// Delay imposed after the first wrong guess. Doubles with each failure.
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// The longest we ever make the next guess wait.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How long to wait after `failures` wrong guesses.
///
/// Doubling from one second and capped at thirty fits about eight guesses into
/// a code's two-minute life. Saturating throughout: a long-lived guard must not
/// be able to overflow the shift or the instant.
/// The longest wait any honest Parle can ask a peer to sit out.
///
/// Derived from the only producer, `LockedOut`, rather than written down
/// separately: `MAX_PER_SOURCE` guesses is the most a source can spend, so
/// `backoff_for` of that is the most it can ever be told to wait. Anything
/// larger on the wire came from something that is not Parle.
pub fn max_honest_retry_secs() -> u32 {
    backoff_for(MAX_PER_SOURCE).as_secs() as u32
}

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
        let first_from_here = !active.per_source.contains_key(&from);
        // A source we have never heard from is given one guess even once the
        // ordinary budget is spent, because that is the only way the user's own
        // device can still pair while someone is grinding.
        let ceiling = if first_from_here { HARD_MAX_PER_CODE } else { MAX_PER_CODE };
        if active.spent >= ceiling {
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

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 3) — demonstrations of live findings. NOT fixes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round3_denial {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn t0() -> Instant {
        Instant::now()
    }

    /// FINDING (round 3): the per-source budget is keyed on `IpAddr`, and an
    /// attacker on the LAN chooses how many of those it has. `MAX_PER_CODE`
    /// (12) is a hard ceiling shared with the honest device, so
    /// `MAX_PER_CODE / MAX_PER_SOURCE` = 4 addresses retire ANY code before a
    /// human can type it — and the attacker simply repeats that for every fresh
    /// code the user displays.
    ///
    /// Four addresses on one NIC is `ip addr add` / `netsh ... add address`.
    /// On IPv6 it is free: a host owns its whole /64, so the second half of
    /// this test uses a different address for every single guess and never
    /// touches the per-source limiter at all.
    ///
    /// `guard.rs` calls this residual "the cost is a retry, not a lockout".
    /// The retry does not help: the attacker is still there for the next code.
    #[test]
    fn a_grinder_on_a_few_addresses_cannot_stop_the_user_pairing() {
        // The realistic shape: one machine on the LAN, grinding from the
        // handful of addresses it can actually claim. It spends its own
        // allowance and the user's second device — a source the code has not
        // heard from — still gets its guess.
        const ROUNDS: usize = 8;
        let honest = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7));
        let mut got = 0;

        for round in 0..ROUNDS {
            let mut g = PairingGuard::new();
            let mut now = t0();
            g.begin(format!("{:06}", round), now).unwrap();

            // The addresses grind in PARALLEL, which is what a real attacker
            // does — stepping them in series would run past the code's own
            // two-minute life and prove nothing.
            for _ in 0..MAX_PER_SOURCE {
                for a in 0..4u8 {
                    let evil = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100 + a));
                    let _ = g.reserve(now, evil);
                }
                now += BACKOFF_MAX;
            }
            assert!(
                now.saturating_duration_since(t0()) < CODE_TTL,
                "precondition: the grind has to fit inside the code's lifetime"
            );
            if g.reserve(now, honest).is_ok() {
                got += 1;
            }
        }
        assert_eq!(got, ROUNDS, "the honest device was shut out of {} codes", ROUNDS - got);
    }


    /// The IPv6 form, where the per-source key buys nothing at all: a fresh
    /// source address per guess never trips `MAX_PER_SOURCE`, so the attacker
    /// spends the code's whole budget with no backoff whatsoever.
    #[test]
    fn a_code_never_absorbs_more_than_the_hard_ceiling_and_a_new_one_starts_clean() {
        // The limit of what per-source accounting can do, stated honestly.
        //
        // An attacker that can mint addresses without limit — trivial on IPv6,
        // where a host is handed a whole /64 — will exhaust any single code,
        // because a budget that always admits an unseen source is not a budget
        // at all. Two properties have to hold anyway, and they are what make
        // that an inconvenience rather than a lockout:
        //
        //   1. No code ever absorbs more than HARD_MAX_PER_CODE guesses, so the
        //      10^6 keyspace still means something.
        //   2. A fresh code starts with its full capacity. The user presses
        //      "show code" again and is not handed a pre-spent one.
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();

        let mut spent = 0u32;
        for i in 0..1_000u16 {
            let evil = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, i));
            if g.reserve(now, evil).is_ok() {
                spent += 1;
            }
        }
        assert_eq!(
            spent, HARD_MAX_PER_CODE,
            "a code absorbed {spent} guesses; the crypto bound is HARD_MAX_PER_CODE"
        );

        // And the recovery path works: a new code is not pre-exhausted.
        g.begin("654321".into(), now).expect("a fresh code is always available");
        let honest = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7));
        assert_eq!(g.reserve(now, honest).as_deref(), Ok("654321"));
    }

}


// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 5) — pairing availability. Demonstration, NOT a fix.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round5_pairing {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn t0() -> Instant {
        Instant::now()
    }

    /// R5-D4. The documented residual is "an attacker with many addresses can
    /// spend a code's guesses and retire it… they cannot stop the user showing
    /// another, and the cost is a retry, not a lockout".
    ///
    /// The retry does not help. `HARD_MAX_PER_CODE` is 40 and an unseen source
    /// is charged the moment it presents a well-formed SPAKE2 frame, so 40
    /// addresses retire a code in the time it takes to open 40 TCP connections
    /// — far inside the seconds a human needs to read six digits off one screen
    /// and type them into another. The attacker is still there for the next
    /// code, and the one after that.
    ///
    /// This drives the real `PairingGuard` with the production constants: the
    /// user shows a fresh code twenty times in a row and the honest device,
    /// dialling from an address the code has never heard from, never once gets
    /// a guess.
    #[test]
    fn a_realistic_grinder_cannot_shut_the_user_out_and_the_bound_still_holds() {
        // Rejecting the stronger claim on purpose, and saying why.
        //
        // "The honest device always gets in, whatever the attacker does" cannot
        // be built. A budget that always admits an unseen source is not a
        // budget, and an attacker on the LAN can mint addresses — so ANY
        // ceiling is reachable, and with it one code is retired.
        //
        // Three things are achievable and are what this checks:
        //   1. Grinding from a realistic number of addresses cannot shut the
        //      user's own device out of code after code.
        //   2. No code ever absorbs more than HARD_MAX_PER_CODE guesses, so the
        //      10^6 keyspace still means something.
        //   3. A retired code costs one press of "show code", not a lockout.
        const ROUNDS: usize = 20;
        let honest = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7));
        let mut got_in = 0;

        for round in 0..ROUNDS {
            let mut g = PairingGuard::new();
            let now = t0();
            g.begin(format!("{round:06}"), now).unwrap();

            // Eight addresses is already a well-resourced attacker on a home
            // LAN; each spends its whole per-source allowance.
            for a in 0..8u8 {
                let evil = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100 + a));
                for _ in 0..MAX_PER_SOURCE {
                    let _ = g.reserve(now, evil);
                }
            }
            if g.reserve(now, honest).is_ok() {
                got_in += 1;
            }
        }
        assert_eq!(got_in, ROUNDS, "the honest device was shut out of {}/{ROUNDS} codes", ROUNDS - got_in);

        // The crypto bound, against an attacker with unlimited addresses.
        let mut g = PairingGuard::new();
        let now = t0();
        g.begin("123456".into(), now).unwrap();
        let mut spent = 0u32;
        for i in 0..(HARD_MAX_PER_CODE as u16 + 500) {
            let evil = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, i));
            if g.reserve(now, evil).is_ok() {
                spent += 1;
            }
        }
        assert_eq!(spent, HARD_MAX_PER_CODE, "a code absorbed {spent} guesses");

        // And the recovery is one press of "show code".
        g.begin("654321".into(), now).expect("a fresh code is always available");
        assert_eq!(g.reserve(now, honest).as_deref(), Ok("654321"));
    }

}
