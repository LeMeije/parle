//! The replication wire format.
//!
//! # Versioning
//!
//! Two independent version numbers exist, and both fail closed:
//!
//! * The *transport epoch* — [`crate::session::NOISE_PROLOGUE`] — is mixed into
//!   the Noise handshake hash. Two peers that disagree about the framing/Noise
//!   layer cannot complete a handshake at all; there is no negotiation, so there
//!   is nothing for an attacker to downgrade.
//! * The *message version* — [`PROTOCOL_VERSION`] — is announced in
//!   [`SyncMessage::Hello`] and checked by the receiver, which yields a legible
//!   [`WireError::ProtocolVersionMismatch`] ("update the other device") instead
//!   of an opaque crypto failure.
//!
//! Messages are serialised as JSON with an internally tagged `"type"` field. An
//! unknown tag is a hard decode error, so a v1 peer never silently ignores a
//! message kind it does not understand.
//!
//! v1 is text-only. There is no binary payload, no image and no attachment;
//! anything that is not UTF-8 text cannot be expressed here at all.

use serde::{Deserialize, Serialize};

use crate::identity::{validate_device_name, DeviceId};

/// Version of the message format below. Bump on any breaking change.
pub const PROTOCOL_VERSION: u16 = 2;

/// Hard cap on the text of a single item: 1 MiB.
///
/// Oversized items are REJECTED, never truncated — a truncated dictation that
/// silently loses its tail is worse than a refused sync the user can see.
pub const MAX_ITEM_TEXT_BYTES: usize = 1024 * 1024;

/// Hard cap on one encoded message. Bounds the receive buffer.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// Hard cap on how many items/tombstones/watermarks one message may carry.
pub const MAX_BATCH_LEN: usize = 256;

/// Hard cap on an origin id (the primary key on the originating device).
pub const MAX_ORIGIN_ID_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("peer speaks protocol version {peer}, we speak {ours}")]
    ProtocolVersionMismatch { ours: u16, peer: u16 },
    #[error("item text is {bytes} bytes, limit is {max}")]
    ItemTextTooLarge { bytes: usize, max: usize },
    #[error("origin id is {bytes} bytes, limit is 1..={max}")]
    InvalidOriginId { bytes: usize, max: usize },
    #[error("batch holds {len} entries, limit is {max}")]
    BatchTooLong { len: usize, max: usize },
    #[error("encoded message is {bytes} bytes, limit is {max}")]
    MessageTooLarge { bytes: usize, max: usize },
    #[error("invalid device name in message: {0}")]
    DeviceName(#[from] crate::identity::IdentityError),
    #[error("malformed message: {0}")]
    Malformed(String),
}

/// What kind of history row this is. Mirrors the app's own item kinds, but is
/// declared here so the wire format is not hostage to a refactor elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Transcription,
    Clipboard,
}

/// One replicated history row.
///
/// Identity is `(source_device, origin_id)`: the device the row was created on
/// plus that device's own primary key. `clock` is a per-source monotonically
/// increasing counter used for watermarks; `updated_at` is the last-writer-wins
/// tiebreak for edits (pin toggles, text edits).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncItem {
    pub source_device: DeviceId,
    pub origin_id: String,
    pub kind: ItemKind,
    pub text: String,
    /// Unix milliseconds, UTC.
    pub created_at: i64,
    /// Unix milliseconds, UTC. Last-writer-wins tiebreak.
    pub updated_at: i64,
    pub pinned: bool,
    /// Per-source replication counter; see [`Watermark`].
    pub clock: u64,
}

impl SyncItem {
    pub fn validate(&self) -> Result<(), WireError> {
        if self.text.len() > MAX_ITEM_TEXT_BYTES {
            return Err(WireError::ItemTextTooLarge {
                bytes: self.text.len(),
                max: MAX_ITEM_TEXT_BYTES,
            });
        }
        validate_origin_id(&self.origin_id)
    }
}

/// A delete. Tombstones carry a clock so they replicate through the same
/// watermark machinery as items, and so a delete is not resurrected by a stale
/// copy of the row arriving later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub source_device: DeviceId,
    pub origin_id: String,
    /// Unix milliseconds, UTC.
    pub deleted_at: i64,
    pub clock: u64,
}

impl Tombstone {
    pub fn validate(&self) -> Result<(), WireError> {
        validate_origin_id(&self.origin_id)
    }
}

/// "I already hold everything from `source_device` up to and including `clock`."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watermark {
    pub source_device: DeviceId,
    pub clock: u64,
}

/// Every message that can cross a paired session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncMessage {
    /// First message from each side once the Noise handshake completes.
    ///
    /// The device id here is trustworthy only insofar as the peer proved
    /// knowledge of the paired key during the handshake — see the crate docs.
    Hello {
        protocol_version: u16,
        device_id: DeviceId,
        device_name: String,
    },
    /// What the sender already holds, per source device. The receiver replies
    /// with everything it has above those clocks.
    ///
    /// `more` works exactly as it does for `Items`: the list is capped at
    /// `MAX_BATCH_LEN` per message, and a sender with more sources than that
    /// splits across several. Without the flag the receiver read exactly one
    /// message and the remaining chunks arrived where rows were expected,
    /// desynchronising the stream and ending the exchange early — a store that
    /// had ever seen more than `MAX_BATCH_LEN` sources could never sync again.
    Watermarks { entries: Vec<Watermark>, more: bool },
    /// A batch of rows. `more` is true when the sender intends to follow this
    /// batch with another, so the receiver knows the stream is not finished.
    Items { items: Vec<SyncItem>, more: bool },
    /// A batch of deletes.
    Tombstones { entries: Vec<Tombstone>, more: bool },
}

impl SyncMessage {
    /// Enforce every invariant this crate promises. Called on both encode and
    /// decode, so a malicious or buggy peer cannot push oversized rows into us.
    pub fn validate(&self) -> Result<(), WireError> {
        match self {
            SyncMessage::Hello {
                protocol_version,
                device_name,
                ..
            } => {
                if *protocol_version != PROTOCOL_VERSION {
                    return Err(WireError::ProtocolVersionMismatch {
                        ours: PROTOCOL_VERSION,
                        peer: *protocol_version,
                    });
                }
                validate_device_name(device_name)?;
                Ok(())
            }
            SyncMessage::Watermarks { entries, .. } => check_len(entries.len()),
            SyncMessage::Items { items, .. } => {
                check_len(items.len())?;
                items.iter().try_for_each(SyncItem::validate)
            }
            SyncMessage::Tombstones { entries, .. } => {
                check_len(entries.len())?;
                entries.iter().try_for_each(Tombstone::validate)
            }
        }
    }

    /// Validate, then serialise. Refuses to produce a message we would not
    /// accept ourselves, so the two directions cannot drift apart.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|e| WireError::Malformed(e.to_string()))?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(WireError::MessageTooLarge {
                bytes: bytes.len(),
                max: MAX_MESSAGE_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Deserialise, then validate.
    ///
    /// The session layer has already refused to buffer more than
    /// [`MAX_MESSAGE_BYTES`], so the allocation performed here is bounded before
    /// we ever look at the contents.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(WireError::MessageTooLarge {
                bytes: bytes.len(),
                max: MAX_MESSAGE_BYTES,
            });
        }
        let msg: SyncMessage =
            serde_json::from_slice(bytes).map_err(|e| WireError::Malformed(e.to_string()))?;
        msg.validate()?;
        Ok(msg)
    }

    /// Convenience constructor for the handshake message.
    pub fn hello(device_id: DeviceId, device_name: impl Into<String>) -> Self {
        SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id,
            device_name: device_name.into(),
        }
    }
}

fn check_len(len: usize) -> Result<(), WireError> {
    if len > MAX_BATCH_LEN {
        Err(WireError::BatchTooLong {
            len,
            max: MAX_BATCH_LEN,
        })
    } else {
        Ok(())
    }
}

fn validate_origin_id(origin_id: &str) -> Result<(), WireError> {
    if origin_id.is_empty() || origin_id.len() > MAX_ORIGIN_ID_BYTES {
        Err(WireError::InvalidOriginId {
            bytes: origin_id.len(),
            max: MAX_ORIGIN_ID_BYTES,
        })
    } else {
        Ok(())
    }
}

// A single maximum-size item must still fit inside a message, JSON escaping and
// all; if these constants are ever edited apart, fail the build, not the sync.
const _: () = assert!(MAX_ITEM_TEXT_BYTES * 2 < MAX_MESSAGE_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> DeviceId {
        DeviceId::parse("3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d").unwrap()
    }

    fn item(text: String) -> SyncItem {
        SyncItem {
            source_device: device(),
            origin_id: "row-1".into(),
            kind: ItemKind::Transcription,
            text,
            created_at: 1_700_000_000_000,
            updated_at: 1_700_000_000_000,
            pinned: false,
            clock: 7,
        }
    }

    #[test]
    fn round_trips_every_message_kind() {
        let msgs = vec![
            SyncMessage::hello(device(), "G14"),
            SyncMessage::Watermarks {
                entries: vec![Watermark {
                    source_device: device(),
                    clock: 42,
                }],
                more: false,
            },
            SyncMessage::Items {
                items: vec![item("hello".into())],
                more: false,
            },
            SyncMessage::Tombstones {
                entries: vec![Tombstone {
                    source_device: device(),
                    origin_id: "row-1".into(),
                    deleted_at: 1,
                    clock: 8,
                }],
                more: true,
            },
        ];
        for msg in msgs {
            let bytes = msg.encode().unwrap();
            assert_eq!(SyncMessage::decode(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn item_text_at_the_cap_is_accepted() {
        let msg = SyncMessage::Items {
            items: vec![item("a".repeat(MAX_ITEM_TEXT_BYTES))],
            more: false,
        };
        assert!(msg.encode().is_ok());
    }

    #[test]
    fn item_text_over_one_mib_is_rejected_on_encode_and_decode() {
        let msg = SyncMessage::Items {
            items: vec![item("a".repeat(MAX_ITEM_TEXT_BYTES + 1))],
            more: false,
        };
        assert!(matches!(
            msg.encode(),
            Err(WireError::ItemTextTooLarge { bytes, max })
                if bytes == MAX_ITEM_TEXT_BYTES + 1 && max == MAX_ITEM_TEXT_BYTES
        ));

        // A peer that skips our encoder must still be rejected on the way in.
        let smuggled = serde_json::to_vec(&msg).unwrap();
        assert!(matches!(
            SyncMessage::decode(&smuggled),
            Err(WireError::ItemTextTooLarge { .. })
        ));
    }

    #[test]
    fn protocol_version_mismatch_is_explicit() {
        let wrong = serde_json::json!({
            "type": "hello",
            "protocol_version": PROTOCOL_VERSION + 1,
            "device_id": device().as_str(),
            "device_name": "G14",
        });
        let bytes = serde_json::to_vec(&wrong).unwrap();
        assert!(matches!(
            SyncMessage::decode(&bytes),
            Err(WireError::ProtocolVersionMismatch { peer, .. }) if peer == PROTOCOL_VERSION + 1
        ));
    }

    #[test]
    fn unknown_message_kind_is_a_hard_error() {
        let bytes = br#"{"type":"attachment","blob":"AAAA"}"#;
        assert!(matches!(
            SyncMessage::decode(bytes),
            Err(WireError::Malformed(_))
        ));
    }

    #[test]
    fn oversized_batches_and_origin_ids_are_rejected() {
        let msg = SyncMessage::Items {
            items: (0..MAX_BATCH_LEN + 1).map(|_| item("x".into())).collect(),
            more: false,
        };
        assert!(matches!(msg.encode(), Err(WireError::BatchTooLong { .. })));

        let mut bad = item("x".into());
        bad.origin_id = "z".repeat(MAX_ORIGIN_ID_BYTES + 1);
        let msg = SyncMessage::Items {
            items: vec![bad],
            more: false,
        };
        assert!(matches!(
            msg.encode(),
            Err(WireError::InvalidOriginId { .. })
        ));
    }

    #[test]
    fn malformed_device_id_fails_to_decode() {
        let bytes =
            br#"{"type":"hello","protocol_version":1,"device_id":"nope","device_name":"G14"}"#;
        assert!(matches!(
            SyncMessage::decode(bytes),
            Err(WireError::Malformed(_))
        ));
    }
}
