//! Persistent settings. One JSON file, atomically written, forward-compatible
//! (unknown fields are dropped on save, missing fields take defaults on load).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub onboarding_complete: bool,
    pub models: ModelSettings,
    pub language: LanguageSettings,
    pub cleanup: CleanupSettings,
    pub hotkeys: HotkeySettings,
    pub dictionary: DictionarySettings,
    pub appearance: AppearanceSettings,
    pub history: HistorySettings,
    pub audio: AudioSettings,
    pub overlay: OverlaySettings,
    pub paste: PasteSettings,
    pub launch_at_login: bool,
    pub auto_update_check: bool,
    pub sync: SyncSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            onboarding_complete: false,
            models: ModelSettings::default(),
            language: LanguageSettings::default(),
            cleanup: CleanupSettings::default(),
            hotkeys: HotkeySettings::default(),
            dictionary: DictionarySettings::default(),
            appearance: AppearanceSettings::default(),
            history: HistorySettings::default(),
            audio: AudioSettings::default(),
            overlay: OverlaySettings::default(),
            paste: PasteSettings::default(),
            launch_at_login: false,
            auto_update_check: true,
            sync: SyncSettings::default(),
        }
    }
}

impl Settings {
    /// Assign this install's stable identity if it has none yet.
    ///
    /// Returns true when something changed, so the caller knows to save. The id
    /// must be generated exactly once and then persisted: regenerating it would
    /// orphan every history row already stamped with the old one, and would
    /// look like a brand new device to every peer we had paired with.
    pub fn ensure_device_identity(&mut self) -> bool {
        let mut changed = false;
        if self.sync.device_id.is_empty() {
            self.sync.device_id = uuid::Uuid::new_v4().to_string();
            changed = true;
        }
        if self.sync.device_name.trim().is_empty() {
            self.sync.device_name = default_device_name();
            changed = true;
        }
        changed
    }
}

/// Friendly default: the machine's hostname, falling back to the OS name so the
/// pairing list never shows a blank entry.
fn default_device_name() -> String {
    let host = gethostname::gethostname().to_string_lossy().trim().to_string();
    if !host.is_empty() {
        return host;
    }
    if cfg!(target_os = "macos") {
        "Mac".into()
    } else if cfg!(target_os = "windows") {
        "Windows PC".into()
    } else {
        "This device".into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSettings {
    /// Active model id from the registry (e.g. "whisper-small-q5_1").
    /// Empty until first-launch auto-selection runs.
    pub active_model: String,
    /// Ordered fallback chain of model ids tried when the active model fails.
    pub fallback_chain: Vec<String>,
    /// "auto" | "metal" | "cuda" | "cpu"
    pub backend: String,
    /// Pre-load and warm the model at app startup.
    pub prewarm: bool,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            active_model: String::new(),
            fallback_chain: Vec::new(),
            backend: "auto".into(),
            prewarm: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageSettings {
    /// ISO 639-1 ("en", "fr", ...) or "auto".
    pub language: String,
    /// Locale variant affecting spelling: "en-AU", "en-GB", "en-US", or "" for none.
    pub locale: String,
    /// Translate-to-English mode (whisper task=translate).
    pub translate_to_english: bool,
}

impl Default for LanguageSettings {
    fn default() -> Self {
        Self { language: "auto".into(), locale: String::new(), translate_to_english: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CleanupSettings {
    /// Master switch for the deterministic tier.
    pub enabled: bool,
    pub remove_fillers: bool,
    /// Also remove hedge phrases ("you know", "I mean", "sort of") — more aggressive.
    pub remove_hedges: bool,
    pub trim_self_corrections: bool,
    pub capitalise_sentences: bool,
    pub ensure_terminal_punctuation: bool,
    pub dictated_punctuation: bool,
    pub paragraph_on_long_pause: bool,
    /// Pause length (ms) between segments that starts a new paragraph.
    pub paragraph_pause_ms: u64,
    /// Apply locale spelling (en-AU/en-GB vs en-US) using the built-in word map.
    pub locale_spelling: bool,
    /// Tier 2: local LLM cleanup.
    pub llm_enabled: bool,
    pub llm_model: String,
    /// Hard deadline; on expiry the deterministic output is used.
    pub llm_timeout_ms: u64,
}

impl Default for CleanupSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            remove_fillers: true,
            remove_hedges: false,
            trim_self_corrections: true,
            capitalise_sentences: true,
            ensure_terminal_punctuation: true,
            dictated_punctuation: true,
            paragraph_on_long_pause: true,
            paragraph_pause_ms: 2200,
            locale_spelling: false,
            llm_enabled: false,
            llm_model: String::new(),
            llm_timeout_ms: 4000,
        }
    }
}

/// How a binding behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyMode {
    /// Hold to record, release to stop.
    Hold,
    /// Tap to start, tap to stop.
    Toggle,
    /// Hold to record; a tap shorter than `latch_ms` latches into toggle.
    Hybrid,
    /// Double-tap to start, single tap to stop. The key is NEVER swallowed in
    /// this mode, so its normal system behaviour keeps working — the
    /// no-conflict option.
    DoubleTap,
}

/// One binding. `key` uses our canonical key names; special keys the plugins
/// can't express (Fn/Globe, bare L/R modifiers, CopilotKey) are handled by the
/// native listeners.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyBinding {
    /// e.g. "Fn", "RightAlt", "RightCtrl", "CopilotKey", or a chord "Alt+Space".
    pub key: String,
    pub mode: HotkeyMode,
    pub enabled: bool,
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        Self { key: String::new(), mode: HotkeyMode::Hybrid, enabled: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeySettings {
    pub dictation: HotkeyBinding,
    /// Optional second binding (e.g. Copilot key on Windows alongside a chord).
    pub dictation_alt: HotkeyBinding,
    /// Opens the history palette.
    pub history_palette: HotkeyBinding,
    /// Cancel current recording without injecting.
    pub cancel: HotkeyBinding,
    /// Tap shorter than this latches Hybrid mode into Toggle (ms).
    pub latch_ms: u64,
    /// Windows: suppress the default Copilot launch when bound.
    pub suppress_copilot: bool,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            dictation: HotkeyBinding {
                // Platform defaults applied on first launch: Fn on macOS is set by
                // the app after checking hardware; RightCtrl on Windows (never
                // RightAlt: that's AltGr on many layouts).
                key: default_dictation_key().into(),
                mode: HotkeyMode::Hybrid,
                enabled: true,
            },
            dictation_alt: HotkeyBinding::default(),
            history_palette: HotkeyBinding {
                key: default_palette_key().into(),
                mode: HotkeyMode::Toggle,
                enabled: true,
            },
            // Off by default: Escape is pressed constantly for unrelated reasons
            // (dismissing a dialog, another window), and losing a take you have
            // already spoken is far worse than having to stop with the hotkey.
            cancel: HotkeyBinding { key: "Escape".into(), mode: HotkeyMode::Toggle, enabled: false },
            latch_ms: 450,
            suppress_copilot: true,
        }
    }
}

fn default_dictation_key() -> &'static str {
    #[cfg(target_os = "macos")]
    { "Fn" }
    #[cfg(not(target_os = "macos"))]
    { "RightCtrl" }
}

fn default_palette_key() -> &'static str {
    #[cfg(target_os = "macos")]
    { "Cmd+Shift+V" }
    #[cfg(not(target_os = "macos"))]
    { "Ctrl+Shift+V" }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DictionarySettings {
    pub enabled: bool,
    /// Feed terms to the engine as a bias prompt where supported.
    pub bias_recognition: bool,
    /// Fuzzy-correct close misspellings of terms post-transcription.
    pub fuzzy_correct: bool,
    /// Learn correction pairs from user edits in history.
    pub auto_learn: bool,
}

impl Default for DictionarySettings {
    fn default() -> Self {
        Self { enabled: true, bias_recognition: true, fuzzy_correct: true, auto_learn: false }
    }
}


/// Default tray style per platform. macOS menu bars expect a monochrome
/// template the OS tints itself; Windows tints nothing, so the filled blue
/// badge is the only style that reads on either taskbar without the user
/// having to pick. "auto" remains selectable — it follows the taskbar theme
/// with the monochrome pair.
pub fn default_tray_style() -> &'static str {
    if cfg!(target_os = "macos") {
        "template"
    } else {
        "badge"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// "system" | "light" | "dark"
    pub theme_mode: String,
    /// Palette id: "paper" | "midnight" | "pastel" | "bold" | "retro"
    pub palette: String,
    /// Accent colour hex, e.g. "#2b5cff".
    pub accent: String,
    /// App icon id.
    pub app_icon: String,
    /// Tray / menu-bar icon style:
    /// "auto" (follow the taskbar theme, Windows) | "badge" | "light" | "dark"
    /// | "color" | "template" (monochrome, macOS). On macOS "auto" is treated
    /// as "template".
    pub tray_style: String,
    pub reduce_motion: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme_mode: "system".into(),
            palette: "paper".into(),
            accent: "#2b5cff".into(),
            app_icon: "default".into(),
            tray_style: default_tray_style().into(),
            reduce_motion: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    /// Capture system clipboard into history.
    pub clipboard_capture: bool,
    /// Days to keep unpinned items; 0 = forever.
    pub retention_days: u32,
    /// App ids excluded from clipboard capture (password managers etc.).
    pub excluded_apps: Vec<String>,
    /// Reserved: encrypt the history DB at rest.
    pub encrypt_at_rest: bool,
    pub max_items: u32,
}

/// Password managers excluded from clipboard capture, BOTH identifiers for
/// every product.
///
/// Generated from one table rather than two hand-kept lists, because the two
/// lists had drifted and the drift crosses machines. LastPass carried a macOS
/// bundle id and no Windows exe name, so copying a password out of LastPass on
/// the PC was captured there and then replicated to the Mac, whose own list
/// would have refused it. The exclusion rule is applied once, at capture, on
/// the capturing machine, so an entry missing from one half is not a smaller
/// hole on that platform: it is a hole in the pair.
///
/// KeePass 2.x is a different product from KeePassXC and was in neither half.
fn default_excluded_apps() -> Vec<String> {
    // (macOS bundle ids, Windows executable names)
    const PASSWORD_MANAGERS: [(&[&str], &[&str]); 7] = [
        (&["com.1password.1password", "com.agilebits.onepassword7"], &["1Password.exe"]),
        (&["com.bitwarden.desktop"], &["Bitwarden.exe"]),
        (&["com.lastpass.LastPass"], &["LastPass.exe"]),
        (&["org.keepassxc.keepassxc"], &["KeePassXC.exe"]),
        (&["com.dashlane.Dashlane"], &["Dashlane.exe"]),
        (&["com.kee.keepass"], &["KeePass.exe"]),
        (&["in.sinew.Enpass-Desktop"], &["Enpass.exe"]),
    ];
    PASSWORD_MANAGERS
        .iter()
        .flat_map(|(mac, win)| mac.iter().chain(win.iter()))
        .map(|s| (*s).to_string())
        .collect()
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            clipboard_capture: true,
            retention_days: 0,
            excluded_apps: default_excluded_apps(),
            encrypt_at_rest: false,
            max_items: 10_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Input device name; empty = system default.
    pub input_device: String,
    /// Discard recordings shorter than this (accidental taps).
    pub min_duration_ms: u64,
    /// Play subtle start/stop sounds.
    pub sounds: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { input_device: String::new(), min_duration_ms: 300, sounds: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySettings {
    /// "bottom-center" | "bottom-right" | "top-center" | "near-cursor"
    pub position: String,
    /// "pill" | "cassette" (retro) | "metal" | "minimal"
    pub style: String,
    pub show_partial_text: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self { position: "bottom-center".into(), style: "pill".into(), show_partial_text: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PasteSettings {
    /// Insert at cursor in the focused app on stop.
    pub inject: bool,
    /// Also copy the final text to the clipboard on stop.
    pub copy_to_clipboard: bool,
    /// Restore the previous clipboard after paste-injection.
    pub restore_clipboard: bool,
    /// Delay before restoring (apps read the clipboard asynchronously).
    pub restore_delay_ms: u64,
    /// macOS: try Accessibility text insertion before clipboard+Cmd-V.
    pub prefer_ax_insert: bool,
    /// Press Enter after inserting (send-the-message mode). Off by default.
    pub press_enter: bool,
}

impl Default for PasteSettings {
    fn default() -> Self {
        Self {
            inject: true,
            copy_to_clipboard: true,
            restore_clipboard: true,
            restore_delay_ms: 700,
            prefer_ax_insert: true,
            press_enter: false,
        }
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] serde_json::Error),
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomic write: temp file + rename, so a crash never corrupts settings.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Application data directory (settings, history DB, models).
pub fn data_dir() -> PathBuf {
    // %LOCALAPPDATA%\EchoKey on Windows (models must NOT live in Program Files),
    // ~/Library/Application Support/EchoKey on macOS.
    #[cfg(target_os = "windows")]
    let base = dirs::data_local_dir();
    #[cfg(not(target_os = "windows"))]
    let base = dirs::data_dir();
    base.unwrap_or_else(|| PathBuf::from(".")).join("EchoKey")
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

pub fn history_db_path() -> PathBuf {
    data_dir().join("history.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let s = Settings::default();
        s.save(&p).unwrap();
        let loaded = Settings::load(&p).unwrap();
        assert_eq!(loaded.version, SETTINGS_VERSION);
        assert!(loaded.cleanup.enabled);
        assert_eq!(loaded.paste.restore_delay_ms, 700);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let s = Settings::load(Path::new("/definitely/not/here.json")).unwrap();
        assert!(!s.onboarding_complete);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, r#"{"version":1,"launch_at_login":true}"#).unwrap();
        let s = Settings::load(&p).unwrap();
        assert!(s.launch_at_login);
        assert!(s.cleanup.remove_fillers);
    }

    #[test]
    fn windows_default_is_not_altgr() {
        // Right Alt is AltGr on many layouts; the Windows default must be RightCtrl.
        #[cfg(not(target_os = "macos"))]
        assert_eq!(HotkeySettings::default().dictation.key, "RightCtrl");
        #[cfg(target_os = "macos")]
        assert_eq!(HotkeySettings::default().dictation.key, "Fn");
    }

    #[test]
    fn device_identity_is_assigned_once_and_kept() {
        let mut s = Settings::default();
        assert!(s.sync.device_id.is_empty(), "no identity before first run");

        assert!(s.ensure_device_identity(), "first run assigns");
        let id = s.sync.device_id.clone();
        assert!(!id.is_empty());
        assert!(!s.sync.device_name.trim().is_empty(), "name falls back to something usable");

        // Re-running must be a no-op: a changing id would orphan every history
        // row already stamped with the old one and look like a new device to
        // every paired peer.
        assert!(!s.ensure_device_identity(), "second run changes nothing");
        assert_eq!(s.sync.device_id, id);
    }

    #[test]
    fn sync_is_off_until_asked_for() {
        let s = Settings::default();
        assert!(!s.sync.enabled, "nothing leaves the machine by default");
    }
}

/// Cross-machine sync. Off until the user pairs a device.
///
/// `device_id` is this install's stable identity and is what every history row
/// is stamped with, so a row can always be attributed to the machine that
/// produced it — including before any sync is set up. It is assigned once, on
/// first run, by `ensure_device_identity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncSettings {
    pub enabled: bool,
    /// Stable per-install UUID. Empty until first run assigns one.
    pub device_id: String,
    /// Human-facing name shown when pairing and on synced rows.
    pub device_name: String,
    pub sync_dictations: bool,
    pub sync_clipboard: bool,
    /// Devices we have paired with. The KEY for each lives in the OS keychain,
    /// never here — this is only the roster needed to show a list and to know
    /// whose key to look up.
    pub paired: Vec<PairedDevice>,
    /// Devices we owe one full re-offer of our history, and where to resume it.
    ///
    /// Written when the user widens what this machine shares. The inbound half
    /// of that change lives in SQLite and is durable; this is the outbound half
    /// and has to survive a quit for the same reason.
    #[serde(default)]
    pub resend_owed: Vec<ResendDebt>,
}

/// One outstanding promise to re-offer our history to a device. Carries no
/// secret.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResendDebt {
    pub device_id: String,
    /// Clock to resume from; 0 means from the beginning.
    pub from: i64,
}

/// A device we have paired with. Carries no secret.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    /// Epoch ms, or None if never seen since pairing.
    pub last_seen: Option<i64>,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            device_id: String::new(),
            device_name: String::new(),
            sync_dictations: true,
            sync_clipboard: true,
            paired: Vec::new(),
            resend_owed: Vec::new(),
        }
    }
}
