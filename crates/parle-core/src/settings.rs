//! Persistent settings. One JSON file, atomically written, forward-compatible
//! (unknown fields are dropped on save, missing fields take defaults on load).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SETTINGS_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// Shipped exclusions this install has ALREADY been offered.
    ///
    /// The union that adds newly shipped password managers has to fire for
    /// every future addition, on every machine, or it reproduces the defect it
    /// exists to fix. It also must not undo a deliberate removal, or the user
    /// can never take an entry off the list at all. A version gate gave the
    /// second and not the first; this gives both, because it records what was
    /// offered rather than when.
    // A FIELD-level `#[serde(default)]` fills a missing key from the FIELD's
    // Default, an empty Vec, NOT from `Settings::default()`. That is
    // deliberate, and it is the reason an absent key means "offer the shipped
    // list once".
    //
    // Round 13 proposed `#[serde(default = "default_excluded_apps")]` so that a
    // downgrade to an older build, which round-trips settings.json without the
    // key, would not re-offer an entry the user deliberately removed. DECLINED:
    // an absent key cannot distinguish that downgrade from an install that
    // simply predates this scheme, and the second is the common case. Reading
    // absent as "already offered" would leave every existing install without
    // the additions this whole mechanism exists to deliver, to protect a rare
    // downgrade from re-adding one entry once.
    //
    // So a downgrade-then-upgrade re-offers the shipped list one more time.
    // That is the same trade the union has always made, stated where the
    // decision lives.
    #[serde(default)]
    pub excluded_defaults_seen: Vec<String>,
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
            // A fresh install starts with every shipped default already ON, so
            // it has been offered all of them.
            excluded_defaults_seen: default_excluded_apps(),
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

/// Password managers and authenticators excluded from clipboard capture.
///
/// Generated from one table rather than two hand-kept lists, because the two
/// lists had drifted and the drift crosses machines: LastPass carried a macOS
/// bundle id and no Windows exe name, so copying a password out of LastPass on
/// the PC was captured there and then replicated to the Mac, whose own list
/// would have refused it. The exclusion rule is applied at capture, on the
/// capturing machine, so an entry missing from one half is not a smaller hole
/// on that platform: it is a hole in the pair.
///
/// A product may legitimately exist on only ONE platform, and the table says so
/// with an empty slice. An earlier version of this table required both halves
/// for every row and I filled a gap by inventing `com.kee.keepass` for KeePass
/// 2.x, which is a Windows .NET application with no macOS build at all. A
/// fabricated identifier is worse than an absent one: it protects nothing and
/// it reads as coverage. The macOS products in that space are separate
/// applications with their own identifiers, and they are listed as such.
fn default_excluded_apps() -> Vec<String> {
    // (macOS bundle ids, Windows executable names)
    const EXCLUDED: [(&[&str], &[&str]); 12] = [
        (&["com.1password.1password", "com.agilebits.onepassword7"], &["1Password.exe"]),
        (&["com.bitwarden.desktop"], &["Bitwarden.exe"]),
        (&["com.lastpass.LastPass"], &["LastPass.exe"]),
        (&["org.keepassxc.keepassxc"], &["KeePassXC.exe"]),
        (&["com.dashlane.Dashlane"], &["Dashlane.exe"]),
        (&["in.sinew.Enpass-Desktop"], &["Enpass.exe"]),
        // The SYSTEM password manager on macOS 15 and later, and the one most
        // likely to be in use on a Mac. It was missing entirely.
        (&["com.apple.Passwords"], &[]),
        (&["com.apple.keychainaccess"], &[]),
        // KeePass 2.x is Windows-only. Its macOS counterparts are different
        // applications, so they get their own rows rather than a made-up id.
        (&[], &["KeePass.exe"]),
        (&["com.mstc.macpass"], &[]),
        (&["com.markmcguill.strongbox.mac"], &[]),
        // Authenticators. The threat this feature names is "passwords, tokens
        // and 2FA codes", and there was not one authenticator in the list.
        (&["com.authy.authy-mac", "com.beemdevelopment.Aegis"], &["Authy Desktop.exe"]),
    ];
    EXCLUDED
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
    /// "pill" | "cassette" (retro) | "metal" | "minimal" | "hidden"
    ///
    /// "hidden" draws no overlay at all. The only indication that Parle is
    /// listening is then the menu-bar / tray icon's recording dot, which is
    /// deliberate: it is the least obtrusive way to run the app, and it is why
    /// that dot has to be legible on its own.
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
            Ok(s) => {
                let mut loaded: Self = serde_json::from_str(&s)?;
                loaded.migrate();
                Ok(loaded)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Bring a settings file written by an older build up to date.
    ///
    /// `#[serde(default)]` fills in fields that are ABSENT. It does nothing for
    /// a field that is present and stale, and `excluded_apps` is present in
    /// every settings.json this app has ever written. So additions to the
    /// shipped exclusion list reached new installs only, and every machine the
    /// app had already run on kept the list it was first given.
    ///
    /// That is not cosmetic: the round-9 additions include macOS's own
    /// Passwords app, Keychain Access and the authenticators. Without this, a
    /// password copied from the system password manager is captured and
    /// replicated to the user's other machine on every existing install, while
    /// the source reads as though it is covered.
    ///
    /// A UNION, not a replacement. A user who deliberately removed an entry
    /// gets it back once, which is the lesser wrong: the alternative is
    /// silently leaving a password manager unprotected because of a decision
    /// they may not remember making.
    fn migrate(&mut self) {
        // NO VERSION GATE. What gates the union is whether this install has been
        // OFFERED that particular entry before.
        //
        // This was `if self.version < 2`, which can only ever fire once, so the
        // next addition to `default_excluded_apps` would reach new installs and
        // nobody else, which is precisely the defect the union exists to fix.
        // Simply removing the gate is wrong in the other direction: the union
        // then re-adds an entry the user deliberately removed on every single
        // launch, so it can never be removed at all.
        //
        // Recording what was offered separates the two questions. A new default
        // is offered once, wherever the install is in its history, and a
        // removal made after that offer stands for ever.
        {
            let defaults = default_excluded_apps();
            let seen: std::collections::HashSet<String> =
                self.excluded_defaults_seen.iter().map(|a| a.to_ascii_lowercase()).collect();
            let have: std::collections::HashSet<String> =
                self.history.excluded_apps.iter().map(|a| a.to_ascii_lowercase()).collect();
            // An install with no record has never run this scheme, and the
            // version stamp cannot stand in for one: `#[serde(default)]` fills
            // a MISSING version from `Settings::default()`, which is the
            // newest, so a version-less file reads as already migrated and is
            // silently skipped. That is the exact hole a round-11 diagnostic
            // recorded and could not close.
            //
            // So the first run of this scheme offers everything, once, and
            // records it. That is round 11's own accepted trade ("a user who
            // deliberately removed an entry gets it back once, which is the
            // lesser wrong"), and from the second launch onwards a removal
            // stands for ever.
            let first_run_of_this_scheme = self.excluded_defaults_seen.is_empty();
            let added: Vec<String> = defaults
                .clone()
                .into_iter()
                .filter(|d| {
                    let k = d.to_ascii_lowercase();
                    !have.contains(&k) && (first_run_of_this_scheme || !seen.contains(&k))
                })
                .collect();
            self.excluded_defaults_seen = defaults;
            if !added.is_empty() {
                tracing::info!(
                    "settings: adding {} password managers to the exclusion list that shipped \
                     after this install was created: {:?}",
                    added.len(),
                    added
                );
                self.history.excluded_apps.extend(added);
            }
        }
        self.version = SETTINGS_VERSION;
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
///
/// `%LOCALAPPDATA%\Parle` on Windows (models must NOT live in Program Files),
/// `~/Library/Application Support/Parle` on macOS.
///
/// The folder was called `EchoKey` before the rename, and it holds the user's
/// entire history plus every model they have downloaded, which runs to
/// gigabytes. So an install that still has the old folder is MIGRATED by
/// renaming it, which is instant and atomic on the same volume rather than a
/// multi-gigabyte copy, and the old name is never read again afterwards.
///
/// If the rename fails for any reason the old directory keeps being used, so
/// the worst case is the previous behaviour rather than an app that has
/// silently lost its history.
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = dirs::data_local_dir();
    #[cfg(not(target_os = "windows"))]
    let base = dirs::data_dir();
    let base = base.unwrap_or_else(|| PathBuf::from("."));
    let new = base.join("Parle");
    // "Has real data", not "exists".
    //
    // Existence alone was the wrong test: anything that creates the directory
    // before this runs (a log file, an installer, the user) makes the migration
    // skip and the app start on an empty history with the old one still on disk.
    // What identifies a live data directory is the things only this app writes.
    let occupied = |d: &PathBuf| {
        d.join("history.db").exists() || d.join("settings.json").exists() || d.join("models").is_dir()
    };
    if occupied(&new) {
        return new;
    }
    // The literal pre-rename name. This one string must NOT be renamed with
    // the rest: it is the folder we are migrating FROM, and a sweep that
    // renames it turns this whole function into a no-op that silently loses
    // the user's history and every downloaded model. (That is exactly what the
    // first pass of the rename did.)
    let old = base.join("EchoKey");
    if occupied(&old) {
        // If an empty `new` is already there, the rename would fail on most
        // platforms, so its contents are moved across entry by entry instead.
        // Nothing is ever overwritten and nothing is ever deleted: an entry
        // that already exists in `new` is left alone, so the worst case is a
        // file staying behind rather than one being destroyed.
        if new.exists() {
            if let Ok(entries) = std::fs::read_dir(&old) {
                for e in entries.flatten() {
                    let to = new.join(e.file_name());
                    if !to.exists() {
                        let _ = std::fs::rename(e.path(), to);
                    }
                }
            }
            tracing::info!("merged the EchoKey data directory into Parle");
            return new;
        }
        match std::fs::rename(&old, &new) {
            Ok(()) => {
                tracing::info!(
                    "migrated the data directory from EchoKey to Parle ({} -> {})",
                    old.display(),
                    new.display()
                );
                return new;
            }
            Err(e) => {
                tracing::warn!(
                    "could not rename the data directory from EchoKey to Parle ({e}); \
                     continuing to use the old one so nothing is lost"
                );
                return old;
            }
        }
    }
    new
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
