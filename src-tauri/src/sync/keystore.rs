//! Where paired keys live.
//!
//! A paired key IS the identity of the link: anyone holding it can impersonate
//! either side of that pair. It must not sit in settings.json next to the
//! theme, which is world-readable to anything running as the user and gets
//! copied around, backed up and pasted into bug reports.
//!
//! So it goes in the OS credential store — Credential Manager on Windows,
//! Keychain Services on macOS — which is what those exist for.
//!
//! Failure to reach the keychain is reported, never silently downgraded to
//! writing the key somewhere else.

use echokey_sync::PairedKey;

const SERVICE: &str = "Parle sync";

#[derive(Debug, thiserror::Error)]
pub enum KeystoreError {
    #[error("credential store unavailable: {0}")]
    Backend(String),
    #[error("stored key for {0} is malformed")]
    Malformed(String),
}

fn entry(device_id: &str) -> Result<keyring::Entry, KeystoreError> {
    keyring::Entry::new(SERVICE, device_id).map_err(|e| KeystoreError::Backend(e.to_string()))
}

/// Persist the key agreed with `device_id`.
pub fn store(device_id: &str, key: &PairedKey) -> Result<(), KeystoreError> {
    entry(device_id)?
        .set_password(&to_hex(key.as_bytes()))
        .map_err(|e| KeystoreError::Backend(e.to_string()))
}

/// Load the key for `device_id`, or None if this pair is unknown.
pub fn load(device_id: &str) -> Result<Option<PairedKey>, KeystoreError> {
    let e = entry(device_id)?;
    match e.get_password() {
        Ok(hex) => {
            let bytes = from_hex(&hex)
                .ok_or_else(|| KeystoreError::Malformed(device_id.to_string()))?;
            Ok(Some(PairedKey::from_bytes(bytes)))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(KeystoreError::Backend(e.to_string())),
    }
}

/// Forget the key for `device_id`. Unpairing must actually destroy it, not
/// merely hide the device from a list.
pub fn delete(device_id: &str) -> Result<(), KeystoreError> {
    match entry(device_id)?.delete_credential() {
        Ok(()) => Ok(()),
        // Already gone is the desired state.
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(KeystoreError::Backend(e.to_string())),
    }
}

fn to_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    s
}

fn from_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let b = s.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (b[i * 2] as char).to_digit(16)?;
        let lo = (b[i * 2 + 1] as char).to_digit(16)?;
        *slot = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        let hex = to_hex(&bytes);
        assert_eq!(hex.len(), 64);
        assert_eq!(from_hex(&hex), Some(bytes));
    }

    #[test]
    fn malformed_hex_is_rejected_rather_than_guessed_at() {
        assert_eq!(from_hex(""), None);
        assert_eq!(from_hex("abc"), None);              // wrong length
        assert_eq!(from_hex(&"z".repeat(64)), None);    // not hex
        assert_eq!(from_hex(&"a".repeat(63)), None);    // odd length
    }

    #[test]
    fn edge_byte_values_survive() {
        for probe in [[0u8; 32], [0xffu8; 32]] {
            assert_eq!(from_hex(&to_hex(&probe)), Some(probe));
        }
    }
}
