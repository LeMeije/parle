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
    let ok = !name.is_empty()
        && name.len() <= MAX_DEVICE_NAME_BYTES
        && !name.contains('=')
        && !name.chars().any(char::is_control);
    if ok {
        Ok(())
    } else {
        Err(IdentityError::InvalidDeviceName(MAX_DEVICE_NAME_BYTES))
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
