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

use std::net::TcpStream;

use echokey_sync::{ConfirmTag, PairedKey, Pairing, PairingCode, PairingError, PairingRole};

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
pub fn run(
    stream: &mut TcpStream,
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
pub fn run_with(
    stream: &mut TcpStream,
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

    // Identity, now that the channel is proven to share a secret.
    let mine = encode_identity(me.0, me.1);
    let theirs = match role {
        PairingRole::Responder => {
            write_frame(stream, &mine)?;
            read_frame(stream)?
        }
        PairingRole::Initiator => {
            let t = read_frame(stream)?;
            write_frame(stream, &mine)?;
            t
        }
    };
    let (device_id, device_name) = decode_identity(&theirs).ok_or(PairFlowError::BadIdentity)?;

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
    use std::net::TcpListener;

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
