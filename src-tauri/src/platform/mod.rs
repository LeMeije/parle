//! Platform abstraction. Each OS implements: native hotkey listening (keys the
//! portable plugin can't express), text injection, clipboard write/monitor,
//! frontmost-app lookup, and permission checks.

use crate::hotkey_logic::KeyPhase;
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
    Cancel,
    Palette,
}

#[derive(Debug, Clone)]
pub enum PlatformEvent {
    Hotkey { id: HotkeyId, phase: KeyPhase },
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
