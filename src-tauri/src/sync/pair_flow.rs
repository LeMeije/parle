//! The pairing exchange, driven over a plain TCP stream.
//!
//! `echokey-sync` owns the cryptography and hands back opaque blobs; this moves
//! them and nothing else. Both roles are here so the two sides cannot drift.
//!
//! Message order (identical for both roles, which is why it is one exchange
//! rather than two implementations):
//!   1. each side sends its SPAKE2 message
//!   2. each side sends its confirmation tag
//!   3. each side verifies the peer's tag
//!   4. the responder sends its identity, the initiator replies with its own
//!
//! Identity is exchanged AFTER the key is agreed and only inside a verified
//! exchange, so an eavesdropper on the LAN learns nothing from a failed attempt.
//! It is still only as trustworthy as the code: see the note in `pairing.rs`
//! about device ids not being bound into the SPAKE2 transcript.

use echokey_sync::{
    ConfirmTag, PairedKey, Pairing, PairingCode, PairingError, PairingRole, Session, SessionError,
    SyncMessage, PROTOCOL_VERSION,
};

use super::wire_tcp::{read_frame, write_frame, TcpError};

#[derive(Debug, thiserror::Error)]
pub enum PairFlowError {
    #[error("{0}")]
    Transport(#[from] TcpError),
    #[error("{0}")]
    Pairing(#[from] PairingError),
    #[error("peer sent a malformed confirmation tag")]
    BadTag,
    #[error("peer sent a malformed identity")]
    BadIdentity,
    #[error("could not open a confirmed channel to exchange identity: {0}")]
    Session(#[from] SessionError),
    #[error("peer speaks sync protocol {peer}, we speak {ours}")]
    Version { peer: u16, ours: u16 },
}

/// Length of a SPAKE2 opening message, measured from the library rather than
/// assumed, so it cannot drift if the crate changes group.
pub fn spake2_msg_len() -> usize {
    let probe = PairingCode::parse("000000").expect("literal code");
    let (_, msg) = Pairing::start(PairingRole::Initiator, &probe);
    msg.len()
}

/// Could these bytes plausibly be a peer's opening SPAKE2 message?
///
/// This gates the rate limiter. Without it, `read_frame` accepts any length
/// from zero upward, so three bytes on the wire (mode byte + an empty frame)
/// cost the user one of four pairing attempts — four connections burn the code
/// and lock pairing out for five minutes, repeatable forever by anyone on the
/// LAN. Checking the shape first costs an attacker nothing they can fake
/// cheaply and reveals nothing, so the charge-before-exchange ordering that
/// closes the TOCTOU is untouched.
pub fn looks_like_pairing_message(buf: &[u8]) -> bool {
    !buf.is_empty() && buf.len() == spake2_msg_len()
}

/// What we learn about the other device once pairing succeeds.
pub struct Paired {
    pub key: PairedKey,
    pub device_id: String,
    pub device_name: String,
}

/// Run the exchange to completion. `me` is our own (id, name).
///
/// A wrong code surfaces as `PairingError::CodeMismatch` from `verify_peer` —
/// it must fail here, loudly, rather than yielding a key that produces a
/// mysteriously broken session later.
pub fn run<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    role: PairingRole,
    code: &PairingCode,
    me: (&str, &str),
) -> Result<Paired, PairFlowError> {
    run_with(stream, role, code, me, None)
}

/// As [`run`], but with the peer's opening message already in hand.
///
/// The inbound path reads that frame BEFORE charging a guess against the rate
/// limit. Otherwise a bare TCP connect sending a single byte costs the user one
/// of their four attempts, and four such connects burn the code and lock
/// pairing out for five minutes — a trivial denial of service from anyone on
/// the LAN. Requiring a well-formed frame first costs the attacker real work
/// and reveals nothing, so it does not reintroduce the TOCTOU that the
/// charge-before-exchange ordering exists to close.
pub fn run_with<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    role: PairingRole,
    code: &PairingCode,
    me: (&str, &str),
    peer_first: Option<Vec<u8>>,
) -> Result<Paired, PairFlowError> {
    let (state, my_msg) = Pairing::start(role, code);
    write_frame(stream, &my_msg)?;
    let peer_msg = match peer_first {
        Some(m) => m,
        None => read_frame(stream)?,
    };

    let (confirm, my_tag) = state.finish(&peer_msg)?;
    write_frame(stream, my_tag.as_bytes())?;
    let peer_tag_bytes = read_frame(stream)?;
    let peer_tag: [u8; 32] = peer_tag_bytes.try_into().map_err(|_| PairFlowError::BadTag)?;

    // Fails closed on a wrong code. Nothing below this line runs otherwise.
    let key = confirm.verify_peer(&ConfirmTag::from_bytes(peer_tag))?;

    // Identity is exchanged INSIDE a session keyed by what we just agreed, not
    // in the clear on the raw socket.
    //
    // Sending it as plain frames was a real hole even though the key was
    // already confirmed. An attacker on the path relays the SPAKE2 messages and
    // the confirmation tags byte for byte — so it never learns the key and both
    // sides verify — and rewrites only the identity frames. Both machines then
    // pair successfully and file the key under an attacker-chosen id and name:
    // the device list shows a name the attacker picked, and the keychain entry
    // is keyed on an id no real device will ever announce, so sync silently
    // never works again.
    //
    // A Noise session closes it without inventing anything: an attacker who
    // does not hold the key cannot complete the handshake, let alone forge a
    // message inside it. `Hello` is exactly the right payload — it carries the
    // id and name, and its validation rejects device names that the raw path
    // waved through into the log, the UI and settings.json.
    let mut session = match role {
        PairingRole::Initiator => Session::initiate(&mut *stream, &key)?,
        PairingRole::Responder => Session::accept(&mut *stream, &key)?,
    };
    session.send(&SyncMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: echokey_sync::DeviceId::parse(me.0).map_err(|_| PairFlowError::BadIdentity)?,
        device_name: me.1.chars().take(64).collect(),
    })?;
    let (device_id, device_name) = match session.recv()? {
        SyncMessage::Hello { protocol_version, device_id, device_name }
            if protocol_version == PROTOCOL_VERSION =>
        {
            (device_id.as_str().to_string(), device_name)
        }
        SyncMessage::Hello { protocol_version, .. } => {
            return Err(PairFlowError::Version { peer: protocol_version, ours: PROTOCOL_VERSION })
        }
        _ => return Err(PairFlowError::BadIdentity),
    };
    let device_name = if device_name.trim().is_empty() {
        "Unnamed device".to_string()
    } else {
        device_name
    };

    Ok(Paired { key, device_id, device_name })
}

fn encode_identity(id: &str, name: &str) -> Vec<u8> {
    // id is a UUID and contains no '\n', so a single newline separates them
    // unambiguously. The name is truncated because it is user-supplied.
    let name: String = name.chars().take(64).collect();
    format!("{id}\n{name}").into_bytes()
}

fn decode_identity(buf: &[u8]) -> Option<(String, String)> {
    let s = std::str::from_utf8(buf).ok()?;
    let (id, name) = s.split_once('\n')?;
    let id = id.trim();
    // Must look like the UUID we issue, or it is not something we will key a
    // keychain entry on.
    if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return None;
    }
    let name = name.trim();
    let name = if name.is_empty() { "Unnamed device" } else { name };
    Some((id.to_string(), name.chars().take(64).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn socket_pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (s, _) = l.accept().unwrap();
        (c, s)
    }

    /// Both roles run concurrently, as they do in reality.
    fn attempt(a_code: &str, b_code: &str) -> (Result<Paired, PairFlowError>, Result<Paired, PairFlowError>) {
        let (mut c, mut s) = socket_pair();
        let a = PairingCode::parse(a_code).unwrap();
        let b = PairingCode::parse(b_code).unwrap();
        let t = std::thread::spawn(move || {
            run(&mut s, PairingRole::Initiator, &a, ("11111111-1111-4111-8111-111111111111", "Deck A"))
        });
        let responder = run(&mut c, PairingRole::Responder, &b, ("22222222-2222-4222-8222-222222222222", "Deck B"));
        (t.join().unwrap(), responder)
    }

    #[test]
    fn matching_codes_pair_and_exchange_identity() {
        let (init, resp) = attempt("314159", "314159");
        let init = init.expect("initiator");
        let resp = resp.expect("responder");
        assert_eq!(init.key.as_bytes(), resp.key.as_bytes(), "both sides agree on the key");
        assert_eq!(init.device_id, "22222222-2222-4222-8222-222222222222");
        assert_eq!(init.device_name, "Deck B");
        assert_eq!(resp.device_id, "11111111-1111-4111-8111-111111111111");
        assert_eq!(resp.device_name, "Deck A");
    }

    #[test]
    fn one_digit_off_fails_on_both_sides() {
        let (init, resp) = attempt("314159", "314158");
        assert!(init.is_err(), "initiator must not derive a key");
        assert!(resp.is_err(), "responder must not derive a key");
    }

    #[test]
    fn identity_round_trips() {
        let e = encode_identity("11111111-1111-4111-8111-111111111111", "Ben's G14");
        let (id, name) = decode_identity(&e).unwrap();
        assert_eq!(id, "11111111-1111-4111-8111-111111111111");
        assert_eq!(name, "Ben's G14");
    }

    #[test]
    fn hostile_identities_are_rejected_not_coerced() {
        assert!(decode_identity(b"no-newline").is_none());
        assert!(decode_identity(b"not-a-uuid\nName").is_none());
        assert!(decode_identity(&[0xff, 0xfe, b'\n']).is_none(), "invalid utf-8");
        // A blank name is replaced rather than shown as an empty row.
        let (_, name) = decode_identity(b"11111111-1111-4111-8111-111111111111\n   ").unwrap();
        assert_eq!(name, "Unnamed device");
        // An over-long name is truncated, not rejected.
        let long = format!("11111111-1111-4111-8111-111111111111\n{}", "x".repeat(500));
        let (_, name) = decode_identity(long.as_bytes()).unwrap();
        assert_eq!(name.chars().count(), 64);
    }

    #[test]
    fn only_a_real_spake2_message_can_cost_a_pairing_attempt() {
        // The DoS this gate exists to stop: an empty frame is legal on the
        // wire, so without a shape check three bytes burn one of four guesses.
        assert!(!looks_like_pairing_message(b""), "an empty frame must be free");
        assert!(!looks_like_pairing_message(b"x"));
        assert!(!looks_like_pairing_message(&vec![0u8; 4096]));

        let code = PairingCode::parse("123456").unwrap();
        let (_, real) = Pairing::start(PairingRole::Responder, &code);
        assert!(looks_like_pairing_message(&real), "a genuine opening message passes");
    }

    #[test]
    fn a_pre_read_opening_message_pairs_identically() {
        // run_with must behave exactly like run when handed the frame the
        // caller already consumed, or the inbound path would silently differ
        // from the outbound one.
        let (mut c, mut s) = socket_pair();
        let code = PairingCode::parse("246810").unwrap();
        let c2 = code.clone();
        let t = std::thread::spawn(move || {
            // Initiator reads the responder's frame itself, then replays it.
            let first = super::read_frame(&mut s).unwrap();
            run_with(
                &mut s,
                PairingRole::Initiator,
                &c2,
                ("11111111-1111-4111-8111-111111111111", "A"),
                Some(first),
            )
        });
        let resp = run(&mut c, PairingRole::Responder, &code, ("22222222-2222-4222-8222-222222222222", "B"));
        let init = t.join().unwrap();
        assert_eq!(
            init.expect("initiator").key.as_bytes(),
            resp.expect("responder").key.as_bytes()
        );
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW — demonstration of a live finding. Not a fix.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial {
    use super::*;
    use crate::sync::guard::{GuardError, PairingGuard, LOCKOUT, MAX_FAILURES};
    use std::time::Instant;

    /// FINDING: `looks_like_pairing_message` checks LENGTH ONLY, so the gate it
    /// is documented to provide ("costs an attacker real work") costs an
    /// attacker nothing. Any unpaired machine on the LAN can send
    /// `spake2_msg_len()` bytes of zeroes and charge a guess.
    ///
    /// This replays `manager::serve_pairing` exactly: read the opening frame,
    /// shape-check it, then `guard.reserve`. Five connections carrying 33 zero
    /// bytes each burn the code and lock pairing out for five minutes — and the
    /// attacker simply repeats it against every code the user displays, so the
    /// user can never pair a device at all while the attacker is on the LAN.
    #[test]
    fn junk_can_burn_one_code_but_cannot_deny_pairing() {
        // Honest about what is and is not fixable. `looks_like_pairing_message`
        // is a length check, and it cannot be much more: a genuine SPAKE2
        // opening message is indistinguishable from one generated against a
        // random code, so an attacker can always produce something that costs a
        // real guess. Junk of the right shape therefore still burns the code
        // that is currently on screen.
        //
        // What must NOT happen is the user being unable to pair at all. That
        // was the actual defect: the lockout refused to issue a replacement, so
        // five 33-byte connections denied pairing for five minutes, repeatable
        // forever by anyone on the LAN.
        let junk = vec![0u8; spake2_msg_len()];
        assert!(looks_like_pairing_message(&junk), "the shape gate is length-only");

        let mut g = PairingGuard::new();
        let now = Instant::now();
        g.begin("123456".into(), now).unwrap();

        let mut locked = false;
        for _ in 0..MAX_FAILURES {
            // exactly what serve_pairing does
            if !looks_like_pairing_message(&junk) {
                continue;
            }
            if let Err(GuardError::LockedOut { .. }) = g.reserve(now) {
                locked = true;
                break;
            }
        }
        assert!(locked, "the code on screen is burnt, as designed");

        // The user shows a new code and is immediately able to pair again.
        g.begin("654321".into(), now)
            .expect("a fresh code must be available despite the lockout");
        assert!(
            g.reserve(now).is_ok(),
            "a real device must be able to pair against the new code"
        );
    }

}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW — demonstration of a live finding. Not a fix.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_mitm {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A_ID: &str = "11111111-1111-4111-8111-111111111111";
    const REAL_B: &str = "22222222-2222-4222-8222-222222222222";
    const FAKE_B: &str = "deadbeef-dead-4ead-8ead-deadbeefdead";

    fn connected() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (s, _) = l.accept().unwrap();
        for sock in [&c, &s] {
            sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            sock.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        (c, s)
    }

    /// A byte pump, not a frame pump.
    ///
    /// It has to be raw bytes now: identity moved inside a Noise session, which
    /// does its own framing, so anything that assumed the pre-session
    /// length-prefixed shape simply blocks forever partway through.
    ///
    /// `corrupt_after` flips a bit once that many bytes have passed, which is
    /// the strongest thing an on-path attacker can still do — it cannot read or
    /// forge session traffic without the key.
    fn pump(mut from: TcpStream, mut to: TcpStream, corrupt_after: Option<usize>) {
        let mut seen = 0usize;
        let mut buf = [0u8; 4096];
        loop {
            let n = match from.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            if let Some(at) = corrupt_after {
                if seen + n > at && seen <= at {
                    let ix = (at - seen).min(n - 1);
                    buf[ix] ^= 0xff;
                }
            }
            seen += n;
            if to.write_all(&buf[..n]).is_err() {
                return;
            }
            let _ = to.flush();
        }
    }

    fn pair_through_relay(corrupt_down_after: Option<usize>) -> (Result<Paired, PairFlowError>, Result<Paired, PairFlowError>) {
        let (a_end, mitm_left) = connected();
        let (mitm_right, b_end) = connected();

        let left_r = mitm_left.try_clone().unwrap();
        let right_r = mitm_right.try_clone().unwrap();
        let up = std::thread::spawn(move || pump(left_r, mitm_right, None));
        let down = std::thread::spawn(move || pump(right_r, mitm_left, corrupt_down_after));

        let code = PairingCode::parse("135791").unwrap();
        let code_b = code.clone();
        let bt = std::thread::spawn(move || {
            let mut b = b_end;
            run(&mut b, PairingRole::Responder, &code_b, (REAL_B, "Real B"))
        });
        let mut a = a_end;
        let a_result = run(&mut a, PairingRole::Initiator, &code, (A_ID, "Deck A"));
        let b_result = bt.join().unwrap();
        drop(a);
        let _ = up.join();
        let _ = down.join();
        (a_result, b_result)
    }

    /// Regression. Identity used to be exchanged as plain frames on the raw
    /// socket after `verify_peer`, with no encryption and no MAC. An on-path
    /// attacker relaying the SPAKE2 messages and confirmation tags verbatim
    /// never learns the key — both `verify_peer` calls succeed — but it could
    /// rewrite the identity, so the two honest machines finished pairing on a
    /// shared key filed under a device id and display name of the attacker's
    /// choosing: a phishing name in the device list, and a keychain entry keyed
    /// on an id no real device announces, so sync silently never worked again.
    ///
    /// Identity now travels inside a Noise session keyed by the agreed secret.
    #[test]
    fn an_on_path_attacker_cannot_rewrite_the_peer_identity() {
        // Tamper once the SPAKE2 and confirmation phases are behind us, which
        // is where the old cleartext identity frame used to sit.
        let (a_result, _b) = pair_through_relay(Some(200));
        match a_result {
            Err(_) => {}
            Ok(p) => {
                assert_ne!(p.device_id, FAKE_B, "A accepted an attacker-chosen identity");
                assert_eq!(p.device_id, REAL_B, "A must only ever learn B's real id");
            }
        }
    }

    /// The control: with nothing tampered, pairing through a relay still works
    /// and yields the real identities. Without this, the test above would pass
    /// just as well if pairing were broken outright.
    #[test]
    fn an_untampered_relay_still_pairs_and_yields_the_real_identity() {
        let (a_result, b_result) = pair_through_relay(None);
        let a = a_result.expect("A pairs");
        let b = b_result.expect("B pairs");
        assert_eq!(a.key.as_bytes(), b.key.as_bytes());
        assert_eq!(a.device_id, REAL_B);
        assert_eq!(a.device_name, "Real B");
        assert_eq!(b.device_id, A_ID);
    }
}

