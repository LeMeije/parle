//! Wire protocol between the Parle app and the `parle-hook` helper process.
//!
//! The low-level keyboard hook lives in the helper, not in the app: a
//! WH_KEYBOARD_LL callback must return within LowLevelHooksTimeout (~300 ms) or
//! Windows silently bypasses it and delivers the key natively — which, for the
//! Copilot chord, means the shell launches Copilot. The app's own startup
//! (Tauri + WebView2 + a CUDA context + a multi-GB model) starves the hook for
//! several seconds no matter how the thread is prioritised, so the hook moved
//! into a process that does nothing else.
//!
//! Both frame sizes are fixed and every field is a byte: framing needs no
//! length prefix, decoding needs no allocation, and the hook proc can read the
//! whole binding set out of a single atomic load (see [`WireBindings::pack`]).
//!
//! This module is deliberately OS-free so it compiles everywhere.

#![forbid(unsafe_code)]

/// Pipe names are `{PIPE_PREFIX}{app pid}-{generation}` — per-process, so a
/// stale helper can never attach to a fresh app.
pub const PIPE_PREFIX: &str = r"\\.\pipe\parle-hook-";

// -- helper -> app -----------------------------------------------------------

/// Every event the helper sends is exactly this many bytes.
pub const EVENT_FRAME: usize = 4;
/// `[EV_HOTKEY, hotkey id, key phase, 0]`
pub const EV_HOTKEY: u8 = 0x01;

// -- app -> helper -----------------------------------------------------------

/// Every command the app sends is exactly this many bytes.
pub const CMD_FRAME: usize = 12;
/// `[CMD_BINDINGS, ..packed bindings (8 bytes).., 0, 0, 0]`
pub const CMD_BINDINGS: u8 = 0x10;
/// `[CMD_SUPPRESS_COPILOT, 0|1, ..padding..]`
pub const CMD_SUPPRESS_COPILOT: u8 = 0x11;
/// `[CMD_RECORDING, 0|1, ..padding..]`
pub const CMD_RECORDING: u8 = 0x12;

// -- HotkeyId on the wire (mirrors platform::HotkeyId) -----------------------

pub const HK_DICTATION: u8 = 1;
pub const HK_DICTATION_ALT: u8 = 2;
pub const HK_CANCEL: u8 = 3;
pub const HK_PALETTE: u8 = 4;

// -- KeyPhase on the wire (mirrors hotkey_logic::KeyPhase) -------------------

pub const PHASE_DOWN: u8 = 0;
pub const PHASE_UP: u8 = 1;

// -- NativeKey on the wire (mirrors platform::NativeKey) ---------------------

/// Not bound. Never a valid key code, which is what lets the packed form omit
/// per-slot "present" flags.
pub const KEY_NONE: u8 = 0;
pub const KEY_FN: u8 = 1;
pub const KEY_LEFT_SHIFT: u8 = 2;
pub const KEY_RIGHT_SHIFT: u8 = 3;
pub const KEY_LEFT_CTRL: u8 = 4;
pub const KEY_RIGHT_CTRL: u8 = 5;
pub const KEY_LEFT_ALT: u8 = 6;
pub const KEY_RIGHT_ALT: u8 = 7;
pub const KEY_LEFT_CMD: u8 = 8;
pub const KEY_RIGHT_CMD: u8 = 9;
pub const KEY_COPILOT: u8 = 10;
pub const KEY_ESCAPE: u8 = 11;

/// The full binding set, packed small enough to live in one `AtomicU64`.
///
/// The hook proc reads bindings on every keystroke. A mutex there would be a
/// blocking call inside a callback Windows is timing, so the helper keeps the
/// bindings in an atomic and unpacks them into this struct instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WireBindings {
    /// [`KEY_NONE`] when the action has no native binding.
    pub dictation_key: u8,
    pub dictation_swallow: bool,
    pub dictation_alt_key: u8,
    pub dictation_alt_swallow: bool,
    /// Consumed only while recording.
    pub cancel_key: u8,
}

impl WireBindings {
    pub fn pack(&self) -> u64 {
        u64::from_le_bytes([
            self.dictation_key,
            self.dictation_swallow as u8,
            self.dictation_alt_key,
            self.dictation_alt_swallow as u8,
            self.cancel_key,
            0,
            0,
            0,
        ])
    }

    pub fn unpack(v: u64) -> Self {
        let b = v.to_le_bytes();
        Self {
            dictation_key: b[0],
            dictation_swallow: b[1] != 0,
            dictation_alt_key: b[2],
            dictation_alt_swallow: b[3] != 0,
            cancel_key: b[4],
        }
    }

    /// Which action `key` triggers, and whether its events are swallowed.
    /// Dictation wins over the alternate binding if both name the same key.
    pub fn binding_for(&self, key: u8) -> Option<(u8, bool)> {
        if key == KEY_NONE {
            None
        } else if key == self.dictation_key {
            Some((HK_DICTATION, self.dictation_swallow))
        } else if key == self.dictation_alt_key {
            Some((HK_DICTATION_ALT, self.dictation_alt_swallow))
        } else {
            None
        }
    }

    pub fn encode(&self) -> [u8; CMD_FRAME] {
        let mut f = [0u8; CMD_FRAME];
        f[0] = CMD_BINDINGS;
        f[1..9].copy_from_slice(&self.pack().to_le_bytes());
        f
    }

    /// Decodes a [`CMD_BINDINGS`] frame. The tag byte is not re-checked.
    pub fn decode(frame: &[u8; CMD_FRAME]) -> Self {
        let mut b = [0u8; 8];
        b.copy_from_slice(&frame[1..9]);
        Self::unpack(u64::from_le_bytes(b))
    }
}

/// A one-boolean command frame ([`CMD_SUPPRESS_COPILOT`] / [`CMD_RECORDING`]).
pub fn encode_flag(tag: u8, on: bool) -> [u8; CMD_FRAME] {
    let mut f = [0u8; CMD_FRAME];
    f[0] = tag;
    f[1] = on as u8;
    f
}

pub fn encode_hotkey(id: u8, phase: u8) -> [u8; EVENT_FRAME] {
    [EV_HOTKEY, id, phase, 0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_round_trip_through_the_packed_form() {
        let b = WireBindings {
            dictation_key: KEY_COPILOT,
            dictation_swallow: true,
            dictation_alt_key: KEY_RIGHT_CTRL,
            dictation_alt_swallow: false,
            cancel_key: KEY_ESCAPE,
        };
        assert_eq!(WireBindings::unpack(b.pack()), b);
        assert_eq!(WireBindings::decode(&b.encode()), b);
    }

    #[test]
    fn unbound_slots_never_match() {
        // An unbound action leaves KEY_NONE behind; a lookup for "no key" must
        // not resolve to it, or every unrelated keystroke would fire dictation.
        let b = WireBindings::default();
        assert_eq!(b.binding_for(KEY_NONE), None);
        assert_eq!(b.binding_for(KEY_LEFT_SHIFT), None);
    }

    #[test]
    fn binding_lookup_reports_the_slot_and_its_swallow_flag() {
        let b = WireBindings {
            dictation_key: KEY_LEFT_CMD,
            dictation_swallow: false,
            dictation_alt_key: KEY_COPILOT,
            dictation_alt_swallow: true,
            cancel_key: KEY_NONE,
        };
        assert_eq!(b.binding_for(KEY_LEFT_CMD), Some((HK_DICTATION, false)));
        assert_eq!(b.binding_for(KEY_COPILOT), Some((HK_DICTATION_ALT, true)));
    }

    #[test]
    fn flag_frames_are_fixed_width() {
        assert_eq!(encode_flag(CMD_RECORDING, true)[..2], [CMD_RECORDING, 1]);
        assert_eq!(encode_flag(CMD_RECORDING, true).len(), CMD_FRAME);
        assert_eq!(encode_hotkey(HK_CANCEL, PHASE_DOWN).len(), EVENT_FRAME);
    }
}
