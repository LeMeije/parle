//! Pairing: turn a 6-digit code the human carries between two machines into a
//! 32-byte long-term shared key.
//!
//! # Shape of the exchange
//!
//! ```text
//! initiator (shows the code)                responder (types the code)
//!   Pairing::start(Initiator, code) -> msg_i ------->
//!                                    <------- msg_r <- Pairing::start(Responder, code)
//!   finish(msg_r) -> (confirm, tag_i) ------->
//!                                    <------- tag_r <- finish(msg_i)
//!   confirm.verify_peer(tag_r)                confirm.verify_peer(tag_i)
//!        -> PairedKey                              -> PairedKey
//! ```
//!
//! Four messages total; both sides end with the same [`PairedKey`] or with an
//! error. The transport is the caller's problem — this is a pure state machine.
//!
//! # Why there is a confirmation round at all (the important bit)
//!
//! SPAKE2 on its own does NOT tell you the password was right. With mismatched
//! codes both sides complete the exchange happily and simply derive *different*
//! keys. Left there, a mistyped digit would produce a "successful" pairing that
//! stores a garbage key, and the user would only find out later as an
//! inscrutable session failure — exactly the silently-broken session we must
//! not ship. So each side sends a MAC over the SPAKE2 transcript, keyed by the
//! SPAKE2 output, and verifies the peer's before any key is released. A wrong
//! code therefore fails closed and unambiguously as
//! [`PairingError::CodeMismatch`], and no [`PairedKey`] value can exist unless
//! confirmation passed — that is enforced by the type, not by discipline: the
//! only constructor reachable from a pairing run is
//! [`PairingConfirm::verify_peer`].
//!
//! # What the SPAKE2 identities are bound to
//!
//! `idA` / `idB` are the fixed role labels [`IDENTITY_INITIATOR`] /
//! [`IDENTITY_RESPONDER`], which include the protocol name and version. They
//! are role labels rather than device ids on purpose: at pairing time neither
//! side has an authenticated view of the other's device id (mDNS is unsigned),
//! so binding an attacker-chosen value would only produce spurious failures.
//! Asymmetric labels prevent a reflection attack, where an attacker bounces a
//! device's own message back at it to make it pair with itself.
//!
//! Consequence to be aware of: the paired key authenticates the *channel*, not
//! the device id. A device that legitimately paired can later announce any
//! device id it likes in [`crate::wire::SyncMessage::Hello`]. Binding the ids
//! into the transcript is deferred to a future version.
//!
//! # Brute force
//!
//! A 6-digit code is 10^6 possibilities. SPAKE2 limits an attacker to ONE guess
//! per protocol run (that is the point of a PAKE), so the defence is that the
//! caller must not allow unlimited runs: rate-limit pairing attempts, use each
//! code once, and expire it. This crate cannot enforce that — it has no timer
//! and no memory across runs — so the app layer owns it.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use subtle::ConstantTimeEq;

/// Number of digits in a pairing code.
pub const PAIRING_CODE_DIGITS: usize = 6;

/// SPAKE2 identity for the side that displays the code.
pub const IDENTITY_INITIATOR: &[u8] = b"echokey-sync/1/pairing/initiator";
/// SPAKE2 identity for the side that types the code in.
pub const IDENTITY_RESPONDER: &[u8] = b"echokey-sync/1/pairing/responder";

const TRANSCRIPT_LABEL: &[u8] = b"echokey-sync/1/pairing/transcript";
const LABEL_LONG_TERM_KEY: &[u8] = b"echokey-sync/1/pairing/long-term-key";
const LABEL_CONFIRM_INITIATOR: &[u8] = b"echokey-sync/1/pairing/confirm/initiator";
const LABEL_CONFIRM_RESPONDER: &[u8] = b"echokey-sync/1/pairing/confirm/responder";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    #[error("pairing code must be exactly {0} digits")]
    MalformedCode(usize),
    #[error("could not read the system RNG: {0}")]
    Rng(String),
    #[error("peer's pairing message was malformed: {0}")]
    MalformedPeerMessage(spake2::Error),
    /// The codes did not match (or someone tampered with the exchange). No key
    /// is produced; the caller must start over with a fresh code.
    #[error("pairing failed: the codes do not match")]
    CodeMismatch,
}

/// Which side of the pairing exchange we are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingRole {
    /// Displays the code (device A).
    Initiator,
    /// Types the code in (device B).
    Responder,
}

/// A 6-digit numeric pairing code.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode(String);

impl PairingCode {
    /// Draw a fresh code from the OS CSPRNG, uniformly over 000000..=999999.
    pub fn generate() -> Result<Self, PairingError> {
        // Rejection sampling: plain `% 1_000_000` would bias the low codes.
        // 4293 * 1_000_000 is the largest multiple of 10^6 inside u32::MAX, so
        // the expected number of retries is under 1.002.
        const LIMIT: u32 = 4293 * 1_000_000;
        loop {
            let raw = getrandom::u32().map_err(|e| PairingError::Rng(e.to_string()))?;
            if raw < LIMIT {
                return Ok(Self(format!("{:06}", raw % 1_000_000)));
            }
        }
    }

    /// Parse user input. Exactly six ASCII digits; no spaces, no dashes.
    pub fn parse(raw: &str) -> Result<Self, PairingError> {
        if raw.len() != PAIRING_CODE_DIGITS || !raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err(PairingError::MalformedCode(PAIRING_CODE_DIGITS));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log a live pairing code.
        f.write_str("PairingCode(******)")
    }
}

/// The long-term shared key produced by a successful pairing.
///
/// The app persists this (in the OS keychain/credential store, not in the
/// history database) and feeds it to [`crate::session`] on every reconnect.
#[derive(Clone, PartialEq, Eq)]
pub struct PairedKey([u8; 32]);

impl PairedKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Rehydrate a key that was persisted after a successful pairing.
    ///
    /// Only for reloading a key this crate previously produced. Feeding in
    /// arbitrary bytes gives you a session no peer can complete.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for PairedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairedKey(<redacted>)")
    }
}

/// A confirmation MAC. Send ours to the peer, feed theirs to
/// [`PairingConfirm::verify_peer`].
#[derive(Clone, PartialEq, Eq)]
pub struct ConfirmTag([u8; 32]);

impl ConfirmTag {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for ConfirmTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConfirmTag(<redacted>)")
    }
}

/// Step 1: the SPAKE2 exchange.
pub struct Pairing {
    role: PairingRole,
    spake: Spake2<Ed25519Group>,
    our_msg: Vec<u8>,
}

impl Pairing {
    /// Begin pairing. Returns the state plus the message to hand the peer.
    pub fn start(role: PairingRole, code: &PairingCode) -> (Self, Vec<u8>) {
        let password = Password::new(code.as_str().as_bytes());
        let id_initiator = Identity::new(IDENTITY_INITIATOR);
        let id_responder = Identity::new(IDENTITY_RESPONDER);
        let (spake, our_msg) = match role {
            PairingRole::Initiator => {
                Spake2::<Ed25519Group>::start_a(&password, &id_initiator, &id_responder)
            }
            PairingRole::Responder => {
                Spake2::<Ed25519Group>::start_b(&password, &id_initiator, &id_responder)
            }
        };
        let state = Self {
            role,
            spake,
            our_msg: our_msg.clone(),
        };
        (state, our_msg)
    }

    /// Step 2: absorb the peer's SPAKE2 message.
    ///
    /// Returns the confirmation tag to send, and the state that will verify the
    /// peer's tag. Note that this succeeding says NOTHING about the code being
    /// right — only that the peer's message was well formed.
    pub fn finish(self, peer_msg: &[u8]) -> Result<(PairingConfirm, ConfirmTag), PairingError> {
        let shared = self
            .spake
            .finish(peer_msg)
            .map_err(PairingError::MalformedPeerMessage)?;

        // Fixed order (initiator's message first) so both sides hash the same
        // transcript regardless of who is running the code.
        let (msg_i, msg_r) = match self.role {
            PairingRole::Initiator => (self.our_msg.as_slice(), peer_msg),
            PairingRole::Responder => (peer_msg, self.our_msg.as_slice()),
        };
        let transcript = transcript(msg_i, msg_r);

        let key = derive(&shared, LABEL_LONG_TERM_KEY, &transcript);
        let tag_initiator = derive(&shared, LABEL_CONFIRM_INITIATOR, &transcript);
        let tag_responder = derive(&shared, LABEL_CONFIRM_RESPONDER, &transcript);

        let (ours, theirs) = match self.role {
            PairingRole::Initiator => (tag_initiator, tag_responder),
            PairingRole::Responder => (tag_responder, tag_initiator),
        };

        Ok((
            PairingConfirm {
                key,
                expected_peer_tag: theirs,
            },
            ConfirmTag(ours),
        ))
    }
}

/// Step 3: the gate. Holds the derived key hostage until the peer proves it
/// derived the same one.
pub struct PairingConfirm {
    key: [u8; 32],
    expected_peer_tag: [u8; 32],
}

impl std::fmt::Debug for PairingConfirm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingConfirm(<redacted>)")
    }
}

impl PairingConfirm {
    /// Verify the peer's confirmation tag and release the long-term key.
    ///
    /// Consumes `self`: a failed verification leaves no state from which the
    /// key could be recovered, and there is no "retry with the same code" path.
    pub fn verify_peer(self, peer_tag: &ConfirmTag) -> Result<PairedKey, PairingError> {
        // Constant-time: a tag comparison that leaks timing leaks the tag.
        if bool::from(self.expected_peer_tag.ct_eq(&peer_tag.0)) {
            Ok(PairedKey(self.key))
        } else {
            Err(PairingError::CodeMismatch)
        }
    }
}

/// Length-prefixed concatenation, so no pair of distinct messages can produce
/// the same transcript.
fn transcript(msg_initiator: &[u8], msg_responder: &[u8]) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(TRANSCRIPT_LABEL.len() + msg_initiator.len() + msg_responder.len() + 8);
    out.extend_from_slice(TRANSCRIPT_LABEL);
    for msg in [msg_initiator, msg_responder] {
        out.extend_from_slice(&(msg.len() as u32).to_be_bytes());
        out.extend_from_slice(msg);
    }
    out
}

/// HMAC-SHA256 keyed by the SPAKE2 output, domain-separated by `label`.
fn derive(shared: &[u8], label: &[u8], transcript: &[u8]) -> [u8; 32] {
    // `new_from_slice` only fails for key sizes HMAC cannot take; HMAC accepts
    // any length, so this cannot fail.
    let mut mac = HmacSha256::new_from_slice(shared).expect("HMAC accepts keys of any length");
    mac.update(label);
    mac.update(transcript);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a full four-message exchange between two codes.
    fn run(
        code_a: &PairingCode,
        code_b: &PairingCode,
    ) -> (
        Result<PairedKey, PairingError>,
        Result<PairedKey, PairingError>,
    ) {
        let (init, msg_i) = Pairing::start(PairingRole::Initiator, code_a);
        let (resp, msg_r) = Pairing::start(PairingRole::Responder, code_b);

        let (confirm_i, tag_i) = init.finish(&msg_r).expect("well-formed peer message");
        let (confirm_r, tag_r) = resp.finish(&msg_i).expect("well-formed peer message");

        (confirm_i.verify_peer(&tag_r), confirm_r.verify_peer(&tag_i))
    }

    #[test]
    fn matching_codes_agree_on_a_key() {
        let code = PairingCode::parse("428193").unwrap();
        let (a, b) = run(&code, &code);
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(a.as_bytes(), b.as_bytes());
        assert_ne!(a.as_bytes(), &[0u8; 32], "key must not be all zeroes");
    }

    #[test]
    fn mismatched_codes_fail_closed_on_both_sides() {
        let (a, b) = run(
            &PairingCode::parse("428193").unwrap(),
            &PairingCode::parse("428194").unwrap(),
        );
        assert_eq!(a.unwrap_err(), PairingError::CodeMismatch);
        assert_eq!(b.unwrap_err(), PairingError::CodeMismatch);
    }

    #[test]
    fn every_run_produces_a_different_key() {
        let code = PairingCode::parse("000000").unwrap();
        let (first, _) = run(&code, &code);
        let (second, _) = run(&code, &code);
        assert_ne!(
            first.unwrap().as_bytes(),
            second.unwrap().as_bytes(),
            "SPAKE2 must be randomised per run, not a function of the code"
        );
    }

    #[test]
    fn a_tampered_confirmation_tag_is_rejected() {
        let code = PairingCode::parse("135790").unwrap();
        let (init, msg_i) = Pairing::start(PairingRole::Initiator, &code);
        let (resp, msg_r) = Pairing::start(PairingRole::Responder, &code);
        let (confirm_i, _) = init.finish(&msg_r).unwrap();
        let (_, tag_r) = resp.finish(&msg_i).unwrap();

        let mut flipped = *tag_r.as_bytes();
        flipped[0] ^= 0x01;
        assert_eq!(
            confirm_i
                .verify_peer(&ConfirmTag::from_bytes(flipped))
                .unwrap_err(),
            PairingError::CodeMismatch
        );
    }

    #[test]
    fn a_garbage_peer_message_is_a_distinct_error() {
        let code = PairingCode::parse("246810").unwrap();
        let (init, _) = Pairing::start(PairingRole::Initiator, &code);
        assert!(matches!(
            init.finish(b"not a spake2 message").unwrap_err(),
            PairingError::MalformedPeerMessage(_)
        ));
    }

    #[test]
    fn a_reflected_message_does_not_pair_a_device_with_itself() {
        // The asymmetric identities mean an initiator's own message is not a
        // valid responder message.
        let code = PairingCode::parse("112233").unwrap();
        let (init, msg_i) = Pairing::start(PairingRole::Initiator, &code);
        assert!(matches!(
            init.finish(&msg_i).unwrap_err(),
            PairingError::MalformedPeerMessage(_)
        ));
    }

    #[test]
    fn codes_are_six_digits_and_parse_strictly() {
        for bad in ["", "12345", "1234567", "12 345", "abcdef", "12-345"] {
            assert_eq!(
                PairingCode::parse(bad).unwrap_err(),
                PairingError::MalformedCode(PAIRING_CODE_DIGITS),
                "{bad:?}"
            );
        }
        assert_eq!(PairingCode::parse("000000").unwrap().as_str(), "000000");
    }

    #[test]
    fn generated_codes_are_six_digits() {
        for _ in 0..64 {
            let code = PairingCode::generate().unwrap();
            assert_eq!(code.as_str().len(), PAIRING_CODE_DIGITS);
            assert!(code.as_str().bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn debug_never_leaks_secrets() {
        let code = PairingCode::parse("424242").unwrap();
        assert!(!format!("{code:?}").contains("424242"));
        let key = PairedKey::from_bytes([7u8; 32]);
        assert_eq!(format!("{key:?}"), "PairedKey(<redacted>)");
    }
}
