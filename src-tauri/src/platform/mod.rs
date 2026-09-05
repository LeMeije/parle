//! Platform abstraction. Each OS implements: native hotkey listening (keys the
//! portable plugin can't express), text injection, clipboard write/monitor,
//! frontmost-app lookup, and permission checks.

use crate::hotkey_logic::KeyPhase;
use parle_core::settings::RefineModifier;
use serde::Serialize;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod macos_clipboard;
#[cfg(target_os = "macos")]
pub use macos as imp;

#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub use windows as imp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyId {
    Dictation,
    DictationAlt,
    /// Starts a Refine take: the transcript goes to the AI, the rewrite is
    /// what gets pasted.
    Refine,
    Cancel,
    Palette,
}

/// Which modifiers were held at the instant a hotkey event was generated.
///
/// SAMPLED FROM THE EVENT ITSELF, once, and carried with it. The alternative
/// is asking the OS "is Shift down?" later, on another thread, after the
/// dispatcher and the gesture machine have had their turn, and this repo has a
/// rule about that written in blood: a decision that reads its input twice is
/// a decision that can disagree with itself. Here the two answers would be
/// "this take goes to the AI" and "this take does not".
///
/// Side-agnostic, matching `RefineModifier`: either Shift is Shift.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub cmd: bool,
}

impl Mods {
    pub fn has(self, m: RefineModifier) -> bool {
        match m {
            RefineModifier::Shift => self.shift,
            RefineModifier::Ctrl => self.ctrl,
            RefineModifier::Alt => self.alt,
            RefineModifier::Cmd => self.cmd,
        }
    }

    /// Drop one family's bit.
    ///
    /// A modifier bound AS the dictation key contributes its own bit to the
    /// event that presses it, so "Right Option dictates" plus "hold Option to
    /// refine" would make every ordinary dictation a Refine take. The key's
    /// own contribution is removed before anything reads these.
    pub fn without(mut self, m: RefineModifier) -> Self {
        match m {
            RefineModifier::Shift => self.shift = false,
            RefineModifier::Ctrl => self.ctrl = false,
            RefineModifier::Alt => self.alt = false,
            RefineModifier::Cmd => self.cmd = false,
        }
        self
    }

    /// Pack into one byte for the Windows helper's event frame.
    pub fn to_bits(self) -> u8 {
        (self.shift as u8) | (self.ctrl as u8) << 1 | (self.alt as u8) << 2 | (self.cmd as u8) << 3
    }

    pub fn from_bits(b: u8) -> Self {
        Self {
            shift: b & 1 != 0,
            ctrl: b & 2 != 0,
            alt: b & 4 != 0,
            cmd: b & 8 != 0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PlatformEvent {
    Hotkey { id: HotkeyId, phase: KeyPhase, mods: Mods },
    /// A non-bound key went down while a bound modifier was held: the user is
    /// using the modifier normally (Fn+C, Fn+arrow). Abort any hold gesture.
    AbortGesture,
    ClipboardChanged { text: String, app_id: Option<String>, app_name: Option<String> },
}

/// Keys the NATIVE listener owns (everything else goes through
/// tauri-plugin-global-shortcut). Parsed from the settings key string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeKey {
    /// macOS Fn/Globe key.
    Fn,
    /// Bare modifiers, left/right discriminated.
    LeftShift,
    RightShift,
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    LeftCmd,
    RightCmd,
    /// Windows Copilot key (Win+Shift+F23 chord or VK_LAUNCH_APP1).
    CopilotKey,
    /// Escape — only consumed while recording.
    Escape,
}

impl NativeKey {
    /// The modifier family this key belongs to, if it is a modifier. Used to
    /// strip a bound key's own contribution from `Mods`.
    pub fn mod_family(&self) -> Option<RefineModifier> {
        Some(match self {
            NativeKey::LeftShift | NativeKey::RightShift => RefineModifier::Shift,
            NativeKey::LeftCtrl | NativeKey::RightCtrl => RefineModifier::Ctrl,
            NativeKey::LeftAlt | NativeKey::RightAlt => RefineModifier::Alt,
            NativeKey::LeftCmd | NativeKey::RightCmd => RefineModifier::Cmd,
            NativeKey::Fn | NativeKey::CopilotKey | NativeKey::Escape => return None,
        })
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Fn" | "Globe" => Some(Self::Fn),
            "LeftShift" => Some(Self::LeftShift),
            "RightShift" => Some(Self::RightShift),
            "LeftCtrl" | "LeftControl" => Some(Self::LeftCtrl),
            "RightCtrl" | "RightControl" => Some(Self::RightCtrl),
            "LeftAlt" | "LeftOption" => Some(Self::LeftAlt),
            "RightAlt" | "RightOption" => Some(Self::RightAlt),
            "LeftCmd" | "LeftCommand" | "LeftWin" => Some(Self::LeftCmd),
            "RightCmd" | "RightCommand" | "RightWin" => Some(Self::RightCmd),
            "CopilotKey" | "Copilot" => Some(Self::CopilotKey),
            "Escape" | "Esc" => Some(Self::Escape),
            _ => None,
        }
    }
}

/// One watched key + whether its events are swallowed. DoubleTap bindings
/// never swallow: single taps must keep their normal system behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedKey {
    pub key: NativeKey,
    pub swallow: bool,
}

/// Bindings the native listener should watch: (key, which action it maps to).
#[derive(Debug, Clone, Default)]
pub struct NativeBindings {
    pub dictation: Option<WatchedKey>,
    pub dictation_alt: Option<WatchedKey>,
    pub refine: Option<WatchedKey>,
    /// Cancel key (consumed only while recording).
    pub cancel: Option<NativeKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionMethod {
    /// macOS Accessibility insertion (no clipboard involved).
    AxInsert,
    /// Clipboard + synthetic paste keystroke.
    ClipboardPaste,
    /// Clipboard only (secure input active or injection disabled).
    ClipboardOnly,
}

/// What the PIPELINE already decided about the focused field.
///
/// Passed in rather than re-probed. `inject_text` used to ask
/// `focused_field_is_secure()` and `secure_input_active()` itself, so the
/// platform layer and the pipeline formed two independent opinions about one
/// dictation from two observations taken milliseconds apart. Either order of
/// disagreement is a real failure: a field the pipeline called Secure could be
/// written to the clipboard unmarked, and a field it called Ordinary could be
/// stored and replicated while the same string was concealed locally.
///
/// Round 13 made the pipeline sample once and carry the answer. This carries it
/// the rest of the way.
#[derive(Debug, Clone, Copy)]
pub struct FieldView {
    /// `Some(true)` known password field, `Some(false)` known ordinary,
    /// `None` the probe could not tell.
    pub is_secure: Option<bool>,
    /// Mark the clipboard write so other clipboard managers skip it.
    pub conceal: bool,
}

impl FieldView {
    /// For callers with no dictation context, such as pasting an existing
    /// history row the user picked themselves.
    pub fn unknown() -> Self {
        Self { is_secure: None, conceal: false }
    }
}

/// Is `app_id` US?
///
/// Cross-platform because the ANSWER is platform-specific and the QUESTION is
/// not. `frontmost_app()` returns a bundle id on macOS and an exe file name on
/// Windows, so a single hard-coded comparison against the bundle id was simply
/// always false on Windows. Two features were dead there because of it: the
/// guard that stops a dictation pasting into Parle's own search box, and the
/// "paste back into the app you came from" flow, which recorded Parle itself
/// as the target every time.
pub fn is_self(app_id: Option<&str>) -> bool {
    match app_id {
        Some(id) => {
            #[cfg(target_os = "macos")]
            {
                id == "com.novaire.parle"
            }
            #[cfg(not(target_os = "macos"))]
            {
                std::env::current_exe()
                    .ok()
                    .and_then(|e| e.file_name().map(|f| f.to_string_lossy().to_string()))
                    .map(|exe| exe.eq_ignore_ascii_case(id))
                    .unwrap_or(false)
            }
        }
        // No id at all: on macOS that happens for our own windows in some
        // states, so fall back to "are we running from the bundle".
        None => {
            #[cfg(target_os = "macos")]
            {
                std::env::current_exe()
                    .ok()
                    .and_then(|e| e.to_str().map(|p| p.contains("Parle.app")))
                    .unwrap_or(false)
            }
            #[cfg(not(target_os = "macos"))]
            {
                false
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InjectionOutcome {
    pub method: InjectionMethod,
    /// True when the user must paste manually (secure field).
    pub manual_paste_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionStatus {
    /// macOS: Accessibility trust. Windows: always true.
    pub accessibility: bool,
    /// Microphone permission: "granted" | "denied" | "undetermined" | "unknown".
    pub microphone: String,
}
