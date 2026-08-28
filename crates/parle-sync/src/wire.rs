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
//! Messages are serialised as JSON, externally tagged — `{"items": {...}}` —
//! and every struct refuses unknown fields. Both are memory-safety choices, not
//! style ones; see the note on the enum itself. An
//! unknown tag is a hard decode error, so a v1 peer never silently ignores a
//! message kind it does not understand.
//!
//! v1 is text-only. There is no binary payload, no image and no attachment;
//! anything that is not UTF-8 text cannot be expressed here at all.

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::marker::PhantomData;

use crate::identity::{DeviceId, MAX_DEVICE_NAME_BYTES};

/// Version of the message format below. Bump on any breaking change.
pub const PROTOCOL_VERSION: u16 = 4;

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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct Watermark {
    pub source_device: DeviceId,
    pub clock: u64,
    /// The origin id of the newest row received at `clock`, making the cursor a
    /// PAIR rather than a bare millisecond.
    ///
    /// Without it the sender had to guess where inside `clock` to resume, and
    /// it guessed "after the whole millisecond". A Clear stamps every tombstone
    /// for a source with ONE clock, so an interrupted clear larger than a page
    /// stranded the rest of that millisecond for ever, and a row written in the
    /// millisecond the cursor already held was never offered at all.
    ///
    /// Empty means "the whole of this millisecond may be re-offered", which is
    /// the safe direction and is what a cursor migrated from v6 carries.
    ///
    /// `default` rather than required, deliberately. A missing origin decodes
    /// to empty, which is exactly the v6 meaning, so a message that omits it is
    /// understood as conservatively as possible instead of failing the whole
    /// exchange. `deny_unknown_fields` still refuses anything it does not know.
    #[serde(default)]
    pub origin: String,
}

impl Watermark {
    pub fn validate(&self) -> Result<(), WireError> {
        // Peer-controlled and unbounded without this. Empty IS legal here,
        // unlike an item's origin id: it is what a cursor migrated from v6
        // carries and what a peer that has received nothing at this clock
        // reports, and it means "re-offer the whole millisecond".
        if self.origin.is_empty() {
            return Ok(());
        }
        validate_origin_id(&self.origin)
    }
}


/// Deserialize a batch, refusing to grow past `MAX_BATCH_LEN` as it goes.
///
/// `validate()` also checks the length, but only once the whole vector exists.
/// That is a check after the fact: one well-formed message inside the byte cap
/// could materialise tens of thousands of entries before anything looked at the
/// count. Stopping inside the visitor makes the limit a bound on allocation
/// rather than a verdict on it.
fn bounded_batch<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Bounded<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Bounded<T> {
        type Value = Vec<T>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "at most {MAX_BATCH_LEN} entries")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            // No size_hint pre-allocation: the hint is peer-controlled.
            let mut out: Vec<T> = Vec::new();
            while let Some(v) = seq.next_element::<T>()? {
                if out.len() >= MAX_BATCH_LEN {
                    return Err(de::Error::custom(format!(
                        "batch exceeds the {MAX_BATCH_LEN} entry limit"
                    )));
                }
                out.push(v);
            }
            Ok(out)
        }
    }

    d.deserialize_seq(Bounded(PhantomData))
}

/// Every message that can cross a paired session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Externally tagged, and that is a memory-safety decision rather than a style
// one. An internally tagged enum forces serde to buffer the WHOLE message into
// its own `Content` tree before it can read the tag and pick a variant, so
// `bounded_batch` never saw an oversized batch until after it had been
// materialised — a refused 60,000-entry message peaked around 17 MB, and a
// message whose payload was one ignored unknown field peaked near 16x its own
// size. Externally tagged, the variant is known before the payload is read and
// the batch limit really is a bound on allocation.
//
// `deny_unknown_fields` closes the other half: an unknown field is refused
// rather than parsed and dropped.
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
    Watermarks {
        #[serde(deserialize_with = "bounded_batch")]
        entries: Vec<Watermark>,
        more: bool,
    },
    /// A batch of rows. `more` is true when the sender intends to follow this
    /// batch with another, so the receiver knows the stream is not finished.
    Items {
        #[serde(deserialize_with = "bounded_batch")]
        items: Vec<SyncItem>,
        more: bool,
    },
    /// A batch of deletes.
    Tombstones {
        #[serde(deserialize_with = "bounded_batch")]
        entries: Vec<Tombstone>,
        more: bool,
    },
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
                // BOUNDED, not policed.
                //
                // This ran the full `validate_device_name`, so a peer whose
                // name carried a character our policy dislikes failed to decode
                // its `Hello` — the FIRST message of every exchange — and the
                // two machines could not sync at all. The user saw a network
                // failure and had no way to connect it to a name.
                //
                // A display string must never be able to deny sync. What this
                // message needs from the name is a bound on allocation, which
                // is what is checked here. The character policy belongs where
                // the name is SHOWN: discovery refuses to list such a peer, and
                // pairing sanitises before storing it. `exchange` reads the
                // Hello name and discards it, so nothing here reaches a screen.
                if device_name.is_empty() || device_name.len() > MAX_DEVICE_NAME_BYTES {
                    return Err(WireError::DeviceName(
                        crate::identity::IdentityError::InvalidDeviceName(MAX_DEVICE_NAME_BYTES),
                    ));
                }
                Ok(())
            }
            SyncMessage::Watermarks { entries, .. } => {
                check_len(entries.len())?;
                entries.iter().try_for_each(Watermark::validate)
            }
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
                    origin: String::new(),
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
            "hello": {
                "protocol_version": PROTOCOL_VERSION + 1,
                "device_id": device().as_str(),
                "device_name": "G14",
            }
        });
        let bytes = serde_json::to_vec(&wrong).unwrap();
        assert!(matches!(
            SyncMessage::decode(&bytes),
            Err(WireError::ProtocolVersionMismatch { peer, .. }) if peer == PROTOCOL_VERSION + 1
        ));
    }

    #[test]
    fn unknown_message_kind_is_a_hard_error() {
        let bytes = br#"{"attachment":{"blob":"AAAA"}}"#;
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
            br#"{"hello":{"protocol_version":1,"device_id":"nope","device_name":"G14"}}"#;
        assert!(matches!(
            SyncMessage::decode(bytes),
            Err(WireError::Malformed(_))
        ));
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 2) — demonstration of a live finding. Not a fix.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round2 {
    use super::*;

    /// FINDING: on DECODE, only `MAX_MESSAGE_BYTES` is enforced before
    /// allocation. `MAX_BATCH_LEN` is enforced by `validate()`, which runs
    /// AFTER `serde_json::from_slice` has materialised the whole `Vec`
    /// (wire.rs:214-220), so a peer picks the allocation it makes us do.
    ///
    /// The error itself proves it: `BatchTooLong { len }` can only report the
    /// length of a vector that was built. Here one 3.5 MiB message — well
    /// inside the byte cap the session layer checks — makes us allocate ~60,000
    /// heap `String`s (roughly 6 MB of live objects) before the 256-entry limit
    /// is consulted, on a connection that has proved nothing beyond the Noise
    /// handshake. With `MAX_INBOUND` sessions that is a per-round multiple of
    /// what the byte cap suggests.
    #[test]
    fn a_batch_cap_bounds_allocation_rather_than_judging_it_afterwards() {
        // Regression. `validate()` checked the entry count only once the whole
        // vector existed, so one well-formed message inside the byte cap could
        // materialise 60,000 entries — 234x the limit — before anything looked
        // at the count. The bound now applies inside the deserializer, so the
        // vector never grows past the limit in the first place.
        const ENTRIES: usize = 60_000;
        let mut json = String::from(r#"{"watermarks":{"more":false,"entries":["#);
        for i in 0..ENTRIES {
            if i > 0 {
                json.push(',');
            }
            json.push_str(
                r#"{"source_device":"3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d","clock":1}"#,
            );
        }
        json.push_str("]}}");
        assert!(
            json.len() < MAX_MESSAGE_BYTES,
            "the attack has to fit inside the byte cap to be interesting"
        );

        match SyncMessage::decode(json.as_bytes()) {
            Err(WireError::BatchTooLong { len, .. }) => panic!(
                "decode built the whole {len}-entry vector before refusing it; the cap is                  still a verdict rather than a bound"
            ),
            Err(_) => {}
            Ok(_) => panic!("an oversized batch must not decode"),
        }

        // A batch at the limit still decodes, so the bound is not off by one.
        let mut ok = String::from(r#"{"watermarks":{"more":false,"entries":["#);
        for i in 0..MAX_BATCH_LEN {
            if i > 0 {
                ok.push(',');
            }
            ok.push_str(
                r#"{"source_device":"3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d","clock":1}"#,
            );
        }
        ok.push_str("]}}");
        assert!(SyncMessage::decode(ok.as_bytes()).is_ok(), "exactly at the limit is fine");
    }
}

// ADVERSARIAL REVIEW — ROUND 4. Demonstration only; not a fix.
#[cfg(test)]
mod adversarial_round4_wire {
    use super::*;

    /// R4-W1. The module header says messages are "internally tagged"; the
    /// enum is externally tagged. And `deny_unknown_fields` is documented as
    /// closing the door on unknown fields — it does so for the variant's own
    /// fields but NOT for the structs inside a batch.
    #[test]
    fn r4_unknown_fields_are_refused_everywhere_they_are_documented_to_be() {
        // Externally tagged, as the enum attribute says. (Documents reality.)
        let m = SyncMessage::Watermarks { entries: Vec::new(), more: false };
        let json = String::from_utf8(m.encode().unwrap()).unwrap();
        assert!(json.starts_with("{\"watermarks\""), "encoding is {json}");

        // An unknown field at the variant level IS refused.
        let bad = br#"{"watermarks":{"entries":[],"more":false,"junk":1}}"#;
        assert!(SyncMessage::decode(bad).is_err(), "variant-level junk must be refused");

        // An unknown field INSIDE a batch element is not.
        let inner = br#"{"tombstones":{"entries":[{"source_device":"22222222-2222-4222-8222-222222222222","origin_id":"1","deleted_at":1,"clock":1,"junk":"padding"}],"more":false}}"#;
        assert!(
            SyncMessage::decode(inner).is_err(),
            "an unknown field inside a batch entry must be refused too"
        );
    }
}
