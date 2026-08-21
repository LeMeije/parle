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

/// Bindings the native listener should watch: (key, which action it maps to).
#[derive(Debug, Clone, Default)]
pub struct NativeBindings {
    pub dictation: Option<NativeKey>,
    pub dictation_alt: Option<NativeKey>,
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
