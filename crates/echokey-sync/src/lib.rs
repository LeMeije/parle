//! EchoKey sync: the LAN-only, end-to-end encrypted replication protocol.
//!
//! This crate is the protocol and nothing else — no database, no settings, no
//! Tauri, no threads beyond the one mDNS translation thread. It is deliberately
//! testable on a single machine.
//!
//! # The four pieces
//!
//! | module | job |
//! |---|---|
//! | [`identity`] | [`DeviceId`] / [`PeerInfo`], the caller-supplied identity |
//! | [`discovery`] | advertise and browse `_echokey._tcp` over mDNS |
//! | [`pairing`] | 6-digit code -> SPAKE2 -> a 32-byte [`PairedKey`] |
//! | [`session`] | [`PairedKey`] -> an encrypted Noise channel over TCP |
//! | [`wire`] | the versioned replication messages that cross it |
//!
//! # Trust model
//!
//! * **LAN only.** Nothing in this crate contacts a server, a relay or an
//!   account system; there is no code path that leaves the local network.
//! * **Discovery proves nothing.** mDNS is unsigned. A [`PeerInfo`] is an
//!   address to dial and a name to show, never an identity.
//! * **Pairing is the only trust event**, and it needs a human: a 6-digit code
//!   read off one screen and typed into the other. A wrong code fails closed
//!   and says so ([`pairing::PairingError::CodeMismatch`]) — it never yields a
//!   half-working session.
//! * **Every byte after pairing is encrypted and authenticated** by a Noise
//!   session keyed on the paired key. An unpaired machine on the same LAN
//!   cannot complete a handshake.
//! * **Text only, 1 MiB per item.** Oversized rows are rejected, never
//!   truncated, and there is no way to express a binary payload on the wire.
//!
//! # What this crate does not do
//!
//! It does not decide *what* to sync. The product rule that password-manager
//! exclusions and Concealed/Transient clipboard etiquette apply *before*
//! anything leaves the machine lives in the app layer: if a row should not have
//! been stored, it must never be handed to [`wire::SyncItem`] in the first
//! place. It also does not persist the paired key (that belongs in the OS
//! keychain), does not rate-limit pairing attempts, and does not implement the
//! merge/retention policy for received rows.
//!
//! # Example: pair, then talk
//!
//! ```no_run
//! use echokey_sync::identity::DeviceId;
//! use echokey_sync::pairing::{Pairing, PairingCode, PairingRole};
//! use echokey_sync::session::Session;
//! use echokey_sync::wire::SyncMessage;
//! use std::net::TcpStream;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Device A shows this; the human types it into device B.
//! let code = PairingCode::generate()?;
//! let (state, our_msg) = Pairing::start(PairingRole::Initiator, &code);
//! # let peer_msg: Vec<u8> = Vec::new();
//! # let peer_tag = echokey_sync::pairing::ConfirmTag::from_bytes([0u8; 32]);
//! // ... exchange `our_msg` for the peer's, then the confirmation tags ...
//! let (confirm, our_tag) = state.finish(&peer_msg)?;
//! let key = confirm.verify_peer(&peer_tag)?; // wrong code => CodeMismatch
//!
//! // Later, on every reconnect:
//! let mut session = Session::initiate(TcpStream::connect("192.168.1.20:51234")?, &key)?;
//! let id = DeviceId::parse("3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d")?;
//! session.send(&SyncMessage::hello(id, "G14"))?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod discovery;
pub mod identity;
pub mod pairing;
pub mod session;
pub mod wire;

pub use discovery::{Discovery, DiscoveryConfig, DiscoveryError, DiscoveryEvent, SERVICE_TYPE};
pub use identity::{DeviceId, IdentityError, PeerInfo};
pub use pairing::{
    ConfirmTag, PairedKey, Pairing, PairingCode, PairingConfirm, PairingError, PairingRole,
};
pub use session::{Session, SessionError, NOISE_PATTERN};
pub use wire::{
    ItemKind, SyncItem, SyncMessage, Tombstone, Watermark, WireError, MAX_BATCH_LEN,
    MAX_ITEM_TEXT_BYTES, MAX_MESSAGE_BYTES, PROTOCOL_VERSION,
};
