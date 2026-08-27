//! Device identity: the stable per-install id and the peers we learn about.
//!
//! This module deliberately does NOT generate identity. The app owns the
//! install's UUID and the user-visible device name (settings `sync.device_name`)
//! and hands them in; the sync crate only validates and carries them.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

/// Upper bound on a friendly device name, in bytes.
///
/// Names travel in an mDNS TXT record, which is a sequence of length-prefixed
/// `key=value` strings, so both the length and the character set are limited.
pub const MAX_DEVICE_NAME_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("device id is not a canonical UUID (expected 8-4-4-4-12 hex digits)")]
    InvalidDeviceId,
    #[error("device name must be 1..={0} bytes of printable text and must not contain '='")]
    InvalidDeviceName(usize),
}

/// A stable per-install device id: the string form of a UUID.
///
/// Stored lower-cased so that two spellings of the same UUID compare equal —
/// peer identity is compared as a string in the replication tables.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DeviceId(String);

impl DeviceId {
    /// Validate and normalise a caller-supplied UUID string.
    pub fn parse(raw: &str) -> Result<Self, IdentityError> {
        if !is_canonical_uuid(raw) {
            return Err(IdentityError::InvalidDeviceId);
        }
        Ok(Self(raw.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// First 8 hex digits — enough to disambiguate an mDNS instance name.
    pub fn short(&self) -> &str {
        &self.0[..8]
    }
}

fn is_canonical_uuid(raw: &str) -> bool {
    // 8-4-4-4-12 lower/upper hex with dashes. Anything else is rejected rather
    // than "cleaned up": a device id that differs between two machines silently
    // duplicates history rows.
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let mut groups = raw.split('-');
    for len in GROUPS {
        match groups.next() {
            Some(g) if g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => return false,
        }
    }
    groups.next().is_none()
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({})", self.0)
    }
}

impl TryFrom<String> for DeviceId {
    type Error = IdentityError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        DeviceId::parse(&value)
    }
}

impl From<DeviceId> for String {
    fn from(value: DeviceId) -> Self {
        value.0
    }
}

/// Validate a user-chosen device name.
///
/// Rejects control characters and `=` because the name is carried as an mDNS
/// TXT `key=value` pair, and rejects empty/oversized names so a peer cannot
/// push an unbounded string into our UI.
pub fn validate_device_name(name: &str) -> Result<(), IdentityError> {
    // The invisible and direction-changing check lives HERE, not only in
    // `sanitise_device_name`.
    //
    // It was in the sanitiser alone, and every call site of the sanitiser
    // passes OUR OWN name: what the user typed, or the local hostname. A peer's
    // name never met it. But a peer's name is the one that is hostile: it
    // arrives in an UNSIGNED mDNS record, and it is what the user reads when
    // deciding which machine to type a 6-digit pairing code into. The filter
    // was protecting the only name that was never the threat.
    //
    // `validate_device_name` is the gate both inbound doors already use
    // (`discovery::peer_from_service` and `wire::SyncMessage::validate`), so
    // putting it here refuses such a name rather than displaying it. A device
    // announcing "Ben\u{202E}koobcaM sneB" renders as a plausible reading of
    // the user's own laptop; it does not get to appear in the list at all.
    let ok = !name.is_empty()
        && name.len() <= MAX_DEVICE_NAME_BYTES
        && !name.contains('=')
        && !name.chars().any(char::is_control)
        && !name.chars().any(is_invisible_or_bidi);
    if ok {
        Ok(())
    } else {
        Err(IdentityError::InvalidDeviceName(MAX_DEVICE_NAME_BYTES))
    }
}

/// Characters that are not control characters but do not honestly render.
///
/// A device name arrives from an UNSIGNED mDNS record and is what the user
/// reads when deciding which machine to type a 6-digit pairing code into. That
/// makes it a security-relevant label, not decoration.
///
/// `char::is_control` does not cover any of these. U+202E (right-to-left
/// override) reverses everything after it, so a hostile machine can present a
/// name that reads like the user's own laptop. U+200B, U+00AD and the other
/// format characters are invisible, so two devices can show labels that are
/// indistinguishable on screen and different on the wire.
fn is_invisible_or_bidi(c: char) -> bool {
    matches!(c,
        // Soft hyphen and the zero-width space family.
        '\u{00AD}' | '\u{200B}' | '\u{2060}' | '\u{FEFF}' | '\u{180E}'
        // Bidirectional marks, embeddings, overrides and isolates. U+061C is
        // ARABIC LETTER MARK, which IS a Bidi_Control and was missing.
        | '\u{061C}'
        | '\u{200E}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
        // Line and paragraph separators. Zl and Zp, so `char::is_control` is
        // FALSE for both, and they render as a line break in the pairing list.
        | '\u{2028}' | '\u{2029}'
        // Invisible "letters" used to make a label look blank or padded.
        | '\u{3164}' | '\u{FFA0}'
        // Variation selectors and the TAG block, the ASCII-smuggling set.
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{E0000}'..='\u{E007F}'
    )
    // U+200C ZWNJ and U+200D ZWJ are deliberately NOT here.
    //
    // They were, and stripping them is wrong in the other direction: both are
    // orthographically required. "کتاب\u{200C}های بن" became a different and
    // incorrect spelling, and "Ben 👨\u{200D}💻" became a man and a laptop.
    // Neither is an invisible-label vector in the way the set above is, so the
    // filter would have been silently corrupting correct names to close a hole
    // they do not open.
}

/// Make a user-chosen name safe to put on the wire, or say it cannot be.
///
/// `validate_device_name` is a gate on a value that is already stored, and the
/// settings layer had no matching gate: `sync_set_device_name` accepted any
/// non-empty string truncated to 64 CHARACTERS, while the validator refuses
/// `=`, refuses control characters, and counts BYTES. Two ordinary names got
/// through and then disabled sync completely: every `Hello` became unsendable
/// and discovery refused to start, which the UI reported as a network problem:
///
/// - `Ben=Work`, because the name rides in an mDNS TXT `key=value` pair;
/// - any 40-plus-character name in a non-Latin script, because 64 characters
///   of Japanese is roughly 130 bytes.
///
/// Sanitising rather than refusing is deliberate. These are mechanical
/// transport constraints, not something to make the user solve: the name is a
/// label in a pairing list, so dropping an `=` and trimming to fit costs the
/// user nothing, where an error message about byte lengths would.
///
/// Returns `None` only when nothing usable survives, which is the one case the
/// caller must report.
pub fn sanitise_device_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && *c != '=' && !is_invisible_or_bidi(*c))
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return None;
    }
    // Truncate on a CHARACTER boundary to a BYTE budget. Slicing to
    // MAX_DEVICE_NAME_BYTES directly panics mid-codepoint, which is how a
    // "safe" truncation turns a bad name into a crash.
    let mut out = String::new();
    for c in cleaned.chars() {
        if out.len() + c.len_utf8() > MAX_DEVICE_NAME_BYTES {
            break;
        }
        out.push(c);
    }
    // A leading multi-byte character can leave the trimmed result empty only if
    // the budget is smaller than one character, which it is not. Trim again
    // anyway, because dropping the tail can expose trailing whitespace.
    let out = out.trim().to_string();
    if out.is_empty() {
        None
    } else {
        debug_assert!(validate_device_name(&out).is_ok());
        Some(out)
    }
}

/// A peer we have discovered on the LAN.
///
/// Everything here is UNAUTHENTICATED until a session has been established from
/// a paired key: mDNS records are unsigned and anyone on the LAN can claim any
/// id or name. Treat `PeerInfo` as "where to dial", never as proof of identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: DeviceId,
    pub name: String,
    pub addr: IpAddr,
    pub port: u16,
}

impl PeerInfo {
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.addr, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d";

    #[test]
    fn accepts_canonical_uuid_and_lowercases() {
        let id = DeviceId::parse(&UUID.to_ascii_uppercase()).unwrap();
        assert_eq!(id.as_str(), UUID);
        assert_eq!(id.short(), "3f2b1c4d");
    }

    #[test]
    fn rejects_non_uuid_ids() {
        for bad in [
            "",
            "not-a-uuid",
            &UUID[..35],
            &format!("{UUID}-extra"),
            &UUID.replace('-', ""),
        ] {
            assert_eq!(
                DeviceId::parse(bad),
                Err(IdentityError::InvalidDeviceId),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn device_names_must_be_txt_safe() {
        assert!(validate_device_name("Ben's G14").is_ok());
        assert!(validate_device_name("").is_err());
        assert!(validate_device_name("a=b").is_err());
        assert!(validate_device_name("line\nbreak").is_err());
        assert!(validate_device_name(&"x".repeat(MAX_DEVICE_NAME_BYTES + 1)).is_err());
    }
}
