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
/// `[EV_HOTKEY, hotkey id, key phase, modifier bits]`
///
/// Byte 3 was zero padding until 04/09/2026 and now carries the modifiers held
/// when the key was pressed (bit 0 Shift, 1 Ctrl, 2 Alt, 3 Win), which is how
/// "hold Shift and use your dictation key" reaches the app. An older app reads
/// a 0 there and simply sees no modifiers, and an older helper sends one.
pub const EV_HOTKEY: u8 = 0x01;
/// Modifier bits in byte 3 of an [`EV_HOTKEY`] frame.
pub const MOD_BIT_SHIFT: u8 = 1;
pub const MOD_BIT_CTRL: u8 = 2;
pub const MOD_BIT_ALT: u8 = 4;
pub const MOD_BIT_WIN: u8 = 8;
/// `[EV_ABORT, 0, 0, 0]`: a bound modifier was held and another key went down,
/// so the gesture was a chord (Right Ctrl + C) rather than a dictation.
///
/// macOS has always sent this. Windows did not, so holding the dictation key
/// and pressing anything else started a recording the Mac would have cancelled.
/// With Right Ctrl as the shipped Windows default that fires on any accidental
/// chord.
pub const EV_ABORT: u8 = 0x02;

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
/// The Refine dictation key: same recording, the transcript goes to the AI.
pub const HK_REFINE: u8 = 5;

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
    /// The Refine binding. Packed into the two bytes that were padding, so the
    /// frame size and every existing offset are unchanged: an old helper
    /// reading a new frame simply never sees a Refine key.
    pub refine_key: u8,
    pub refine_swallow: bool,
}

impl WireBindings {
    pub fn pack(&self) -> u64 {
        u64::from_le_bytes([
            self.dictation_key,
            self.dictation_swallow as u8,
            self.dictation_alt_key,
            self.dictation_alt_swallow as u8,
            self.cancel_key,
            self.refine_key,
            self.refine_swallow as u8,
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
            refine_key: b[5],
            refine_swallow: b[6] != 0,
        }
    }

    /// Which action `key` triggers, and whether its events are swallowed.
    /// Dictation wins over the alternate binding, and both win over Refine, if
    /// two of them name the same key.
    pub fn binding_for(&self, key: u8) -> Option<(u8, bool)> {
        if key == KEY_NONE {
            None
        } else if key == self.dictation_key {
            Some((HK_DICTATION, self.dictation_swallow))
        } else if key == self.dictation_alt_key {
            Some((HK_DICTATION_ALT, self.dictation_alt_swallow))
        } else if key == self.refine_key {
            Some((HK_REFINE, self.refine_swallow))
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

pub fn encode_hotkey(id: u8, phase: u8, mods: u8) -> [u8; EVENT_FRAME] {
    [EV_HOTKEY, id, phase, mods]
}

pub fn encode_abort() -> [u8; EVENT_FRAME] {
    [EV_ABORT, 0, 0, 0]
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
            refine_key: KEY_RIGHT_SHIFT,
            refine_swallow: true,
        };
        assert_eq!(WireBindings::unpack(b.pack()), b);
        assert_eq!(WireBindings::decode(&b.encode()), b);
    }

    #[test]
    fn the_refine_slot_resolves_and_loses_ties_to_dictation() {
        let b = WireBindings {
            dictation_key: KEY_RIGHT_CTRL,
            dictation_swallow: true,
            refine_key: KEY_RIGHT_SHIFT,
            refine_swallow: false,
            ..WireBindings::default()
        };
        assert_eq!(b.binding_for(KEY_RIGHT_SHIFT), Some((HK_REFINE, false)));
        // Same key on both: the dictation binding wins, and Refine is
        // unreachable rather than firing both.
        let clash = WireBindings { refine_key: KEY_RIGHT_CTRL, ..b };
        assert_eq!(clash.binding_for(KEY_RIGHT_CTRL), Some((HK_DICTATION, true)));
    }

    #[test]
    fn a_frame_from_before_refine_decodes_with_no_refine_key() {
        // Bytes 5 and 6 were zero padding in every frame an older app sent.
        let mut f = [0u8; CMD_FRAME];
        f[0] = CMD_BINDINGS;
        f[1] = KEY_RIGHT_CTRL;
        f[2] = 1;
        let b = WireBindings::decode(&f);
        assert_eq!(b.refine_key, KEY_NONE);
        assert!(!b.refine_swallow);
        assert_eq!(b.binding_for(KEY_RIGHT_CTRL), Some((HK_DICTATION, true)));
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
            refine_key: KEY_NONE,
            refine_swallow: false,
        };
        assert_eq!(b.binding_for(KEY_LEFT_CMD), Some((HK_DICTATION, false)));
        assert_eq!(b.binding_for(KEY_COPILOT), Some((HK_DICTATION_ALT, true)));
    }

    #[test]
    fn flag_frames_are_fixed_width() {
        assert_eq!(encode_flag(CMD_RECORDING, true)[..2], [CMD_RECORDING, 1]);
        assert_eq!(encode_flag(CMD_RECORDING, true).len(), CMD_FRAME);
        assert_eq!(encode_hotkey(HK_CANCEL, PHASE_DOWN, 0).len(), EVENT_FRAME);
    }

    #[test]
    fn modifier_bits_ride_in_the_byte_that_used_to_be_padding() {
        let f = encode_hotkey(HK_DICTATION, PHASE_DOWN, MOD_BIT_SHIFT | MOD_BIT_CTRL);
        assert_eq!(f[0], EV_HOTKEY);
        assert_eq!(f[3], 0b0011);
        // The frame is the same width it always was, so an app or helper from
        // either side of this change still parses the other's frames.
        assert_eq!(f.len(), EVENT_FRAME);
        // And a frame from before this existed reads as "no modifiers".
        assert_eq!(encode_hotkey(HK_DICTATION, PHASE_DOWN, 0)[3], 0);
    }
}
