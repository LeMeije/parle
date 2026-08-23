//! The encrypted, authenticated session two paired devices talk over.
//!
//! # Noise pattern: `Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s`
//!
//! Chosen because of what pairing actually leaves us with: a 32-byte symmetric
//! secret, and nothing else. Device identity in EchoKey is a UUID string, not a
//! key pair, so there are no long-term static Diffie-Hellman keys for a `KK`
//! pattern to authenticate against. `KKpsk0` would require both sides to have
//! exchanged static public keys at pairing time and to have stored them; that is
//! a strictly larger design (key storage, rotation, unpair semantics) for a
//! stronger property than v1 needs, and it is the natural v2 upgrade.
//!
//! What `NNpsk0` gives us here:
//!
//! * **Mutual authentication from the PSK.** With `psk0` the pre-shared key is
//!   mixed in before the first ephemeral, so neither side can complete the
//!   handshake without the paired key. An unpaired machine on the same LAN
//!   cannot get past the handshake, and there is no "unauthenticated but
//!   encrypted" state to accidentally use.
//! * **Forward secrecy against later key theft.** Session keys come from the
//!   ephemeral-ephemeral DH as well as the PSK, so recorded traffic is not
//!   readable by someone who steals the paired key afterwards.
//! * **Key confirmation.** A wrong key fails as a decrypt error during the
//!   handshake, not as garbage later.
//!
//! What it does NOT give us, stated plainly:
//!
//! * The PSK *is* the identity. Anyone holding the paired key can impersonate
//!   either device to the other, and if the key leaks, an active attacker can
//!   MITM future sessions. Both devices legitimately hold it, so this is a
//!   statement about key storage: the paired key belongs in the OS keychain.
//! * There is no per-device identity inside the channel. The device id in
//!   [`SyncMessage::Hello`] is a label, not proof (see [`crate::pairing`]).
//! * No replay protection *across* sessions. Noise's counter stops replay
//!   within a session; a full recorded session cannot be replayed to a live
//!   peer because both ephemerals are fresh, but the replication layer should
//!   still treat items idempotently by `(source_device, origin_id)`.
//!
//! The prologue [`NOISE_PROLOGUE`] carries the transport epoch, so it is mixed
//! into the handshake hash: peers running incompatible framing cannot complete
//! a handshake at all.
//!
//! # Framing
//!
//! Two layers, both length-prefixed:
//!
//! ```text
//! TCP:   [u16 BE ciphertext len][Noise message]     (len <= 65535, structural)
//! Noise: [u32 BE message len][JSON message]         (len <= MAX_MESSAGE_BYTES)
//! ```
//!
//! A Noise message cannot exceed 65535 bytes, so the outer frame is capped by
//! its own width — a peer cannot make us allocate more than 64 KiB per read. An
//! application message may be larger than one Noise message (a 1 MiB item is
//! legal), so it is split across as many Noise messages as needed. The inner
//! length is checked against [`MAX_MESSAGE_BYTES`] *before* anything is
//! reserved or read for it: an oversized declaration is refused, not allocated.

use std::io::{Read, Write};

use snow::{Builder, TransportState};

use crate::pairing::PairedKey;
use crate::wire::{SyncMessage, WireError, MAX_MESSAGE_BYTES};

/// The Noise pattern, spelled out for auditability.
pub const NOISE_PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";

/// Transport epoch. Mixed into the handshake hash; bump it when the framing or
/// the Noise pattern changes.
pub const NOISE_PROLOGUE: &[u8] = b"echokey-sync/noise/1";

/// A Noise message can never exceed this (Noise spec).
const MAX_NOISE_FRAME: usize = 65535;
/// ChaChaPoly tag length.
const TAG_LEN: usize = 16;
/// Largest plaintext we put into one Noise message.
const MAX_PLAINTEXT_CHUNK: usize = MAX_NOISE_FRAME - TAG_LEN;
/// Length of the inner message header.
const HEADER_LEN: usize = 4;

// The inner header is a u32, so the cap must be expressible in one, and a
// chunk plus its tag must fit a Noise message. Fail the build, not the sync.
const _: () = assert!(MAX_MESSAGE_BYTES <= u32::MAX as usize);
const _: () = assert!(MAX_PLAINTEXT_CHUNK + TAG_LEN <= MAX_NOISE_FRAME);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("noise error: {0}")]
    Noise(#[from] snow::Error),
    #[error(transparent)]
    Wire(#[from] WireError),
    /// The peer declared a message larger than we will ever accept. Nothing of
    /// that size was allocated or read.
    #[error("peer declared a {declared}-byte message; limit is {max}")]
    MessageTooLarge { declared: usize, max: usize },
    /// The session hit an error that desynchronised the Noise counters, so it
    /// can no longer be used. Reconnect.
    #[error("session is unusable after an earlier failure")]
    Poisoned,
}

/// An established, encrypted, mutually authenticated channel.
///
/// Generic over the transport so it can be tested over anything; in the app
/// this is a `TcpStream`. Read/write timeouts are the caller's business — set
/// them on the stream before handing it over, or a hung peer hangs the thread.
pub struct Session<S: Read + Write> {
    stream: S,
    noise: TransportState,
    /// Decrypted bytes not yet handed to a caller.
    rx: Vec<u8>,
    /// How much of `rx` the current message has consumed.
    consumed: usize,
    /// Scratch for one Noise frame (ciphertext).
    frame: Vec<u8>,
    /// Scratch for one decrypted chunk.
    plain: Vec<u8>,
    poisoned: bool,
}

impl<S: Read + Write> Session<S> {
    /// Dial side of the handshake (`-> psk, e` then `<- e, ee`).
    pub fn initiate(stream: S, key: &PairedKey) -> Result<Self, SessionError> {
        Self::handshake(stream, key, true)
    }

    /// Listen side of the handshake.
    pub fn accept(stream: S, key: &PairedKey) -> Result<Self, SessionError> {
        Self::handshake(stream, key, false)
    }

    fn handshake(mut stream: S, key: &PairedKey, initiator: bool) -> Result<Self, SessionError> {
        let builder = Builder::new(NOISE_PATTERN.parse()?)
            .prologue(NOISE_PROLOGUE)?
            .psk(0, key.as_bytes())?;
        let mut hs = if initiator {
            builder.build_initiator()?
        } else {
            builder.build_responder()?
        };

        let mut frame = vec![0u8; MAX_NOISE_FRAME];
        let mut plain = vec![0u8; MAX_NOISE_FRAME];

        if initiator {
            let n = hs.write_message(&[], &mut frame)?;
            write_frame(&mut stream, &frame[..n])?;
            stream.flush()?;
            let n = read_frame(&mut stream, &mut frame)?;
            // A wrong paired key fails right here, as a decrypt error.
            hs.read_message(&frame[..n], &mut plain)?;
        } else {
            let n = read_frame(&mut stream, &mut frame)?;
            hs.read_message(&frame[..n], &mut plain)?;
            let n = hs.write_message(&[], &mut frame)?;
            write_frame(&mut stream, &frame[..n])?;
            stream.flush()?;
        }

        Ok(Self {
            stream,
            noise: hs.into_transport_mode()?,
            rx: Vec::new(),
            consumed: 0,
            frame,
            plain,
            poisoned: false,
        })
    }

    /// Encrypt and send one message.
    pub fn send(&mut self, msg: &SyncMessage) -> Result<(), SessionError> {
        self.guard()?;
        let body = msg.encode()?;
        let mut payload = Vec::with_capacity(HEADER_LEN + body.len());
        payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        payload.extend_from_slice(&body);
        self.poison_on_err(|me| me.send_payload(&payload))
    }

    /// Receive one message, blocking until it is complete.
    pub fn recv(&mut self) -> Result<SyncMessage, SessionError> {
        self.guard()?;
        self.poison_on_err(Self::recv_message)
    }

    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    pub fn into_inner(self) -> S {
        self.stream
    }

    fn send_payload(&mut self, payload: &[u8]) -> Result<(), SessionError> {
        for chunk in payload.chunks(MAX_PLAINTEXT_CHUNK) {
            let n = self.noise.write_message(chunk, &mut self.frame)?;
            write_frame(&mut self.stream, &self.frame[..n])?;
        }
        self.stream.flush()?;
        Ok(())
    }

    fn recv_message(&mut self) -> Result<SyncMessage, SessionError> {
        self.fill(HEADER_LEN)?;
        let mut header = [0u8; HEADER_LEN];
        header.copy_from_slice(&self.rx[self.consumed..self.consumed + HEADER_LEN]);
        let declared = u32::from_be_bytes(header) as usize;

        // The cap is checked here, before `fill` is asked for `declared` bytes.
        // Nothing is reserved for the declared size: `rx` only ever grows by
        // what has actually arrived (at most one 64 KiB frame at a time), so a
        // peer claiming a 4 GiB message costs us this comparison and nothing
        // else. The session is poisoned by the caller, which is correct: we
        // have no idea where the next message boundary is.
        if declared > MAX_MESSAGE_BYTES {
            return Err(SessionError::MessageTooLarge {
                declared,
                max: MAX_MESSAGE_BYTES,
            });
        }
        self.consumed += HEADER_LEN;

        self.fill(declared)?;
        let msg = SyncMessage::decode(&self.rx[self.consumed..self.consumed + declared])?;
        self.consumed += declared;

        self.rx.drain(..self.consumed);
        self.consumed = 0;
        Ok(msg)
    }

    /// Read and decrypt Noise frames until `n` unconsumed plaintext bytes are
    /// buffered. Blocks; never spins.
    fn fill(&mut self, n: usize) -> Result<(), SessionError> {
        while self.rx.len() - self.consumed < n {
            let len = read_frame(&mut self.stream, &mut self.frame)?;
            let plain_len = self
                .noise
                .read_message(&self.frame[..len], &mut self.plain)?;
            self.rx.extend_from_slice(&self.plain[..plain_len]);
        }
        Ok(())
    }

    fn guard(&self) -> Result<(), SessionError> {
        if self.poisoned {
            Err(SessionError::Poisoned)
        } else {
            Ok(())
        }
    }

    /// Any failure mid-frame leaves the Noise nonces out of step with the peer,
    /// so the session is dead. Fail loudly on every later use rather than
    /// producing plausible-looking garbage.
    fn poison_on_err<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, SessionError>,
    ) -> Result<T, SessionError> {
        let out = f(self);
        if out.is_err() {
            self.poisoned = true;
        }
        out
    }

    #[cfg(test)]
    fn rx_capacity(&self) -> usize {
        self.rx.capacity()
    }
}

/// Write one length-prefixed Noise frame.
fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> Result<(), SessionError> {
    debug_assert!(payload.len() <= MAX_NOISE_FRAME);
    w.write_all(&(payload.len() as u16).to_be_bytes())?;
    w.write_all(payload)?;
    Ok(())
}

/// Read one length-prefixed Noise frame into `buf`, returning its length.
///
/// The length is a `u16`, so the allocation here is capped at 64 KiB by
/// construction — there is no attacker-controlled size to bound.
fn read_frame<R: Read>(r: &mut R, buf: &mut Vec<u8>) -> Result<usize, SessionError> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len)?;
    let len = u16::from_be_bytes(len) as usize;
    if buf.len() < len {
        buf.resize(len, 0);
    }
    r.read_exact(&mut buf[..len])?;
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceId;
    use crate::wire::{ItemKind, SyncItem};
    use std::net::{TcpListener, TcpStream};

    fn device() -> DeviceId {
        DeviceId::parse("3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d").unwrap()
    }

    /// A connected pair of loopback sockets.
    fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || TcpStream::connect(addr).unwrap());
        let (server, _) = listener.accept().unwrap();
        (client.join().unwrap(), server)
    }

    /// Handshake both ends concurrently (each side blocks on the other).
    fn session_pair(
        key_a: PairedKey,
        key_b: PairedKey,
    ) -> (
        Result<Session<TcpStream>, SessionError>,
        Result<Session<TcpStream>, SessionError>,
    ) {
        let (a, b) = socket_pair();
        let responder = std::thread::spawn(move || Session::accept(b, &key_b));
        let initiator = Session::initiate(a, &key_a);
        (initiator, responder.join().unwrap())
    }

    fn item(text: &str) -> SyncItem {
        SyncItem {
            source_device: device(),
            origin_id: "row-1".into(),
            kind: ItemKind::Clipboard,
            text: text.into(),
            created_at: 1,
            updated_at: 1,
            pinned: false,
            clock: 1,
        }
    }

    #[test]
    fn matching_keys_round_trip_messages_both_ways() {
        let key = PairedKey::from_bytes([9u8; 32]);
        let (a, b) = session_pair(key.clone(), key);
        let (mut a, mut b) = (a.unwrap(), b.unwrap());

        let hello = SyncMessage::hello(device(), "G14");
        a.send(&hello).unwrap();
        assert_eq!(b.recv().unwrap(), hello);

        let items = SyncMessage::Items {
            items: vec![item("back the other way")],
            more: false,
        };
        b.send(&items).unwrap();
        assert_eq!(a.recv().unwrap(), items);
    }

    #[test]
    fn a_message_larger_than_one_noise_frame_is_chunked_and_reassembled() {
        let key = PairedKey::from_bytes([3u8; 32]);
        let (a, b) = session_pair(key.clone(), key);
        let (mut a, mut b) = (a.unwrap(), b.unwrap());

        // 1 MiB of text: ~17 Noise frames.
        let big = SyncMessage::Items {
            items: vec![item(&"x".repeat(crate::wire::MAX_ITEM_TEXT_BYTES))],
            more: false,
        };
        let sender = std::thread::spawn(move || {
            a.send(&big).unwrap();
            big
        });
        let received = b.recv().unwrap();
        assert_eq!(received, sender.join().unwrap());
    }

    #[test]
    fn a_wrong_paired_key_fails_the_handshake() {
        let (a, b) = session_pair(
            PairedKey::from_bytes([1u8; 32]),
            PairedKey::from_bytes([2u8; 32]),
        );
        // Whichever side notices first, neither ends up with a session.
        assert!(a.is_err() || b.is_err());
        if let Err(e) = a {
            assert!(matches!(e, SessionError::Noise(_) | SessionError::Io(_)));
        }
    }

    #[test]
    fn an_oversized_declared_length_is_rejected_without_allocating() {
        let key = PairedKey::from_bytes([5u8; 32]);
        let (a, b) = session_pair(key.clone(), key);
        let (mut a, mut b) = (a.unwrap(), b.unwrap());

        // A paired but hostile peer: a header claiming 4 GiB, and no body.
        let hostile = u32::MAX.to_be_bytes();
        a.send_payload(&hostile).unwrap();

        let err = b.recv().unwrap_err();
        assert!(
            matches!(
                err,
                SessionError::MessageTooLarge { declared, max }
                    if declared == u32::MAX as usize && max == MAX_MESSAGE_BYTES
            ),
            "unexpected error: {err:?}"
        );
        // Proof that we refused rather than reserved: the receive buffer never
        // grew past the four bytes that actually arrived.
        assert!(
            b.rx_capacity() < MAX_MESSAGE_BYTES,
            "receive buffer grew to {} bytes on a bogus declaration",
            b.rx_capacity()
        );
        // And the session is dead, not silently resynchronised.
        assert!(matches!(b.recv(), Err(SessionError::Poisoned)));
    }
}
