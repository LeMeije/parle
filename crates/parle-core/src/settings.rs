//! Persistent settings. One JSON file, atomically written, forward-compatible
//! (unknown fields are dropped on save, missing fields take defaults on load).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SETTINGS_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// UI language: "en" | "fr" | "es" | "de" | "pt", or "" until first run.
    ///
    /// Distinct from the TRANSCRIPTION language below: someone can run the
    /// interface in French and dictate in English, and plenty do. Picking one
    /// on first run seeds the other, and after that they move independently.
    ///
    /// Empty rather than "en" by default, so first run can tell "the user has
    /// not chosen" apart from "the user chose English" and offer the picker
    /// exactly once.
    #[serde(default)]
    pub ui_language: String,
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
    /// The second dictation mode: the transcript goes to an AI CLI on this
    /// machine and the AI's rewrite is what gets pasted. Off by default and
    /// only ever reached through its own hotkey.
    #[serde(default)]
    pub refine: RefineSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            ui_language: String::new(),
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
            refine: RefineSettings::default(),
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
    /// Models the user pointed at themselves, on disk, outside the registry.
    ///
    /// Kept in settings rather than copied into the models directory, so a
    /// 3 GB file the user already has is not duplicated and stays wherever
    /// they keep it. The consequence is that the file can move or be deleted
    /// behind our back, which is why loading one has to fail gracefully rather
    /// than assume it is there.
    #[serde(default)]
    pub custom: Vec<CustomModel>,
}

/// A whisper.cpp model file the user supplied.
///
/// Whisper GGUF/GGML only, not Parakeet: a Parakeet model is a directory of
/// several ONNX files with names we would have to guess at, and guessing wrong
/// gives the user a confusing failure instead of a clear one. A single `.bin`
/// is something they can point at unambiguously.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomModel {
    /// `custom:<something stable>`, so it can never collide with a registry id.
    pub id: String,
    pub display_name: String,
    /// Absolute path to the model file, as the user chose it.
    pub path: String,
    /// Whether to let the user pick a language for this model. Assumed true:
    /// an English-only model simply ignores the setting, whereas assuming
    /// English-only would hide the language picker for a multilingual model.
    pub multilingual: bool,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            active_model: String::new(),
            fallback_chain: Vec::new(),
            backend: "auto".into(),
            prewarm: true,
            custom: Vec::new(),
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
    /// Starts a REFINE dictation: same recording, but the transcript is sent
    /// to the configured AI and the rewrite is what gets pasted.
    ///
    /// Only used when `refine.trigger` is `OwnKey`. The default trigger holds
    /// a modifier while using the ordinary dictation key instead, so this
    /// binding is never armed unless the user asks for a separate key.
    ///
    /// The key carries a platform suggestion so choosing that option gives a
    /// working binding at once.
    #[serde(default = "default_refine_binding")]
    pub refine: HotkeyBinding,
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
                // Platform defaults applied on first launch. The KEY is chosen
                // by position so muscle memory carries between machines - the
                // bottom-left corner key on both - and holding Shift is the
                // Refine trigger on both. The GESTURE deliberately differs; see
                // `default_dictation_mode` for why it cannot be made to match.
                key: default_dictation_key().into(),
                mode: default_dictation_mode(),
                enabled: true,
            },
            dictation_alt: HotkeyBinding::default(),
            refine: default_refine_binding(),
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

/// The key people dictate with, chosen by POSITION so the muscle memory
/// transfers between a Mac and a PC: the bottom-left corner key on both.
///
/// macOS: Globe/Fn. Windows: Left Ctrl, which sits where Globe/Fn does on an
/// Apple keyboard. It is safe to bind only because the default gesture is
/// DoubleTap, which never swallows the key — see `HotkeySettings::default`.
///
/// Right Ctrl was the previous Windows default. Left is the better mirror of
/// Fn's position, and it is also the Ctrl that people actually reach for.
/// Still never RightAlt: that is AltGr on French and many other layouts.
fn default_dictation_key() -> &'static str {
    #[cfg(target_os = "macos")]
    { "Fn" }
    #[cfg(not(target_os = "macos"))]
    { "LeftCtrl" }
}

/// The default gesture, which CANNOT be the same on both platforms.
///
/// Windows: DoubleTap, and it is load-bearing rather than a preference. It is
/// the only mode that does not swallow its key (`swallow: b.mode != DoubleTap`
/// in state.rs), and Left Ctrl bound in Hold, Hybrid or Toggle would eat Ctrl+C,
/// Ctrl+V and every other Ctrl chord system-wide. The key and the gesture have
/// to change together.
///
/// macOS: Hybrid — hold Globe and talk, or tap it quickly to latch. NOT
/// DoubleTap, however tempting the symmetry is: macOS binds a DOUBLE-PRESS of
/// the Globe key to its OWN dictation by default (see `open_keyboard_settings`
/// in platform/macos.rs), and since DoubleTap never swallows the key, both
/// would fire and the user would get whichever won the race. Onboarding already
/// asks people to set "Press 🌐 key to" to Do Nothing; defaulting to the very
/// gesture the OS has taken would make that a requirement instead of a tip.
///
/// So the alignment between platforms is the key's POSITION and the Shift
/// trigger, not the gesture. Holding a key you already hold costs nothing on
/// macOS; on Windows the same hold would cost the user Ctrl.
fn default_dictation_mode() -> HotkeyMode {
    #[cfg(target_os = "macos")]
    { HotkeyMode::Hybrid }
    #[cfg(not(target_os = "macos"))]
    { HotkeyMode::DoubleTap }
}

/// The suggested Refine key.
///
/// macOS: the right Option key. It is a bare modifier the native listener
/// owns, so it works with Hold/Hybrid exactly like Fn does, and a chord such as
/// Option+E still reaches the OS: the tap swallows only the modifier's own
/// FlagsChanged event, the keydown that follows carries the hardware flag and
/// triggers the gesture abort. Right Command was the other candidate and is
/// used far more (Cmd+Space, Cmd+Tab by right-handed users).
///
/// Windows: a CHORD, deliberately, not a bare modifier. A low-level hook that
/// swallows a modifier's key-down stops the OS from registering the modifier at
/// all, so Right Shift bound bare would break every capital typed with it, and
/// Right Alt is AltGr on French and many other layouts. Ctrl+Shift+Space is
/// free of system meaning and goes through the global-shortcut plugin, which
/// delivers press and release so Hold still works.
fn default_refine_key() -> &'static str {
    #[cfg(target_os = "macos")]
    { "RightAlt" }
    #[cfg(not(target_os = "macos"))]
    { "Ctrl+Shift+Space" }
}

fn default_refine_binding() -> HotkeyBinding {
    HotkeyBinding { key: default_refine_key().into(), mode: HotkeyMode::Hybrid, enabled: false }
}

/// Which AI runs the rewrite. Every provider is a COMMAND-LINE tool already
/// installed and logged in on this machine; Parle never holds an API key and
/// never talks to a network itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RefineProvider {
    /// Claude Code (`claude -p`). The reference provider: the invocation is
    /// verified, the JSON result is parsed, and tools, hooks, MCP servers and
    /// CLAUDE.md discovery are all switched off for the call.
    #[default]
    Claude,
    /// OpenAI Codex CLI (`codex exec`). Best effort: prompt on stdin, answer on
    /// stdout.
    Codex,
    /// Google Gemini CLI (`gemini -p`). Best effort, same contract.
    Gemini,
    /// Any command the user names. Receives the whole prompt on stdin and must
    /// print the rewrite on stdout. This is how a DeepSeek, Grok or local
    /// model wrapper plugs in.
    Custom,
}

impl RefineProvider {
    /// The executable this provider is looked up as.
    pub fn default_program(self) -> &'static str {
        match self {
            RefineProvider::Claude => "claude",
            RefineProvider::Codex => "codex",
            RefineProvider::Gemini => "gemini",
            RefineProvider::Custom => "",
        }
    }
}

/// How a Refine take is STARTED.
///
/// `Modifier` is the default because it is one shortcut rather than two: the
/// user keeps the dictation key they already have (a Copilot key, a double
/// tapped Globe) and holds one extra finger down to send that take to the AI.
/// A second key has to be found, remembered, and kept clear of everything
/// else on the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RefineTrigger {
    /// Hold `RefineSettings::modifier` while using the ORDINARY dictation key,
    /// in whatever gesture that key already uses.
    #[default]
    Modifier,
    /// A separate binding of its own (`hotkeys.refine`).
    OwnKey,
}

/// The modifier held to turn a dictation into a Refine take.
///
/// SIDE-AGNOSTIC on purpose: either Shift means Shift. Discriminating left
/// from right would let someone bind a modifier whose twin does nothing, and
/// the failure is silent (you hold the wrong one and get an ordinary
/// dictation, with no way to tell why). "Hold Shift" is also how people
/// describe the gesture out loud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RefineModifier {
    #[default]
    Shift,
    Ctrl,
    /// Option on macOS, Alt on Windows.
    Alt,
    /// Command on macOS, Windows key on Windows.
    Cmd,
}

impl RefineModifier {
    /// The modifier family a NATIVE key belongs to, so a modifier bound as the
    /// dictation key can be told apart from the same modifier held as the
    /// Refine trigger. Anything that is not a modifier answers `None`.
    pub fn of_native_key(key: &str) -> Option<Self> {
        Some(match key {
            "LeftShift" | "RightShift" => RefineModifier::Shift,
            "LeftCtrl" | "LeftControl" | "RightCtrl" | "RightControl" => RefineModifier::Ctrl,
            "LeftAlt" | "LeftOption" | "RightAlt" | "RightOption" => RefineModifier::Alt,
            "LeftCmd" | "LeftCommand" | "LeftWin" | "RightCmd" | "RightCommand" | "RightWin" => {
                RefineModifier::Cmd
            }
            _ => return None,
        })
    }
}

/// What happens to the dictation when the AI cannot answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefineFallback {
    /// Copy the plain transcript to the clipboard and keep it in History, but
    /// do not insert it. The user pressed Refine BECAUSE the raw dictation was
    /// not fit to send, so landing it in their email unasked is the wrong
    /// default.
    ClipboardOnly,
    /// Behave exactly like an ordinary dictation: insert the plain transcript.
    InsertTranscript,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RefineSettings {
    /// Master switch. Off, the Refine hotkey is never registered and nothing
    /// is ever sent anywhere, which keeps the app's "nothing leaves this
    /// machine" promise intact until the user opts out of it deliberately.
    pub enabled: bool,
    /// Modifier-plus-dictation-key, or its own key.
    #[serde(default)]
    pub trigger: RefineTrigger,
    /// Which modifier, when `trigger` is `Modifier`.
    #[serde(default)]
    pub modifier: RefineModifier,
    pub provider: RefineProvider,
    /// Explicit path to the CLI. Empty means "find it": a GUI app launched
    /// from the Finder or the Start menu has a PATH with none of the places a
    /// developer tool installs to, so the app searches the usual ones and asks
    /// the login shell as a last resort.
    pub program_path: String,
    /// Custom provider only: the full command line (program plus arguments,
    /// shell-style quoting, no shell). Ignored for the built-in providers.
    pub custom_command: String,
    /// Model override handed to the CLI (`--model`). Empty means the CLI's
    /// own default.
    pub model: String,
    /// The user's standing instructions, baked into every prompt: spelling,
    /// tone, banned characters, sign-off. Plain text.
    pub rules: String,
    /// Optional Markdown file describing the user's voice, read at run time
    /// and appended to the prompt. Kept as a path rather than copied, so the
    /// user can keep editing the file they already maintain.
    pub voice_file: String,
    /// Hard deadline for the CLI, in milliseconds. On expiry the process is
    /// killed and `fallback` decides what happens to the transcript.
    pub timeout_ms: u64,
    pub fallback: RefineFallback,
    /// Accent colour for the overlay and the dictation bar while a Refine
    /// take is running, so the two modes are never mistaken for each other.
    /// Coral by default; the user picks freely, as with the main accent.
    pub accent: String,
}

pub fn default_refine_accent() -> &'static str {
    "#ff7a59"
}

impl Default for RefineSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            trigger: RefineTrigger::default(),
            modifier: RefineModifier::default(),
            provider: RefineProvider::Claude,
            program_path: String::new(),
            custom_command: String::new(),
            model: String::new(),
            rules: String::new(),
            voice_file: String::new(),
            timeout_ms: 90_000,
            fallback: RefineFallback::ClipboardOnly,
            accent: default_refine_accent().into(),
        }
    }
}

/// The history-palette chord.
///
/// NOT `Cmd+Shift+V` on macOS: that is paste-and-match-style almost everywhere,
/// so taking it globally breaks a keystroke people use constantly in every
/// other app. `Ctrl+Cmd+V` keeps the V mnemonic and is not bound by macOS to
/// anything (unlike `Cmd+Opt+V`, which is Finder's "Move Item Here").
///
/// Never `Fn+<anything>`: `Fn` is not a modifier the global-shortcut API can
/// register, so such a binding parses as nothing and silently never fires. See
/// `migrate` for the rewrite of installs that already stored one.
fn default_palette_key() -> &'static str {
    #[cfg(target_os = "macos")]
    { "Ctrl+Cmd+V" }
    #[cfg(not(target_os = "macos"))]
    { "Ctrl+Shift+V" }
}

/// Can this chord ever be registered as a global shortcut?
///
/// `Fn` is a hardware-level modifier the OS does not deliver as part of a
/// chord, so any binding using it as one is dead on arrival. `Fn` ALONE is
/// fine and is the dictation default: that goes to the native listener, not
/// to the global-shortcut API. Only the combining form is broken.
pub fn is_unregisterable_chord(key: &str) -> bool {
    let k = key.trim();
    k.contains('+') && k.split('+').any(|part| part.trim().eq_ignore_ascii_case("fn"))
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
        // Aegis is ANDROID ONLY and its application id was sitting here as
        // though it were a macOS bundle id. That is precisely what the rule
        // above forbids: a fabricated identifier protects nothing and makes a
        // test assert coverage that does not exist. Removed rather than
        // replaced, because inventing a substitute would repeat the mistake.
        (&["com.authy.authy-mac"], &["Authy Desktop.exe"]),
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
    /// How readily the waveform reacts to quiet speech. 0.5 to 2.0, 1.0 default.
    ///
    /// Applied as a dB offset on the level window rather than a multiplier on
    /// the bar height, so raising it lifts quiet speech into view instead of
    /// just stretching whatever was already visible. A quiet microphone, a
    /// distant one, or a soft speaker all need the same thing: the window
    /// moved, not the picture scaled.
    #[serde(default = "default_waveform_sensitivity")]
    pub waveform_sensitivity: f32,
}

pub fn default_waveform_sensitivity() -> f32 {
    1.0
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            position: "bottom-center".into(),
            style: "pill".into(),
            show_partial_text: true,
            waveform_sensitivity: default_waveform_sensitivity(),
        }
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
        Self::load_migrated(path).map(|(s, _)| s)
    }

    /// Load, and report whether migrating changed anything.
    ///
    /// A migration that only ever runs in memory is not a migration: it repeats
    /// its work and its log line at every launch, and the file on disk stays
    /// wrong for anything that reads it directly. Until now the only startup
    /// write was gated on `ensure_device_identity`, which fires once in an
    /// install's life, so every later migration relied on the user happening to
    /// change a setting afterwards.
    pub fn load_migrated(path: &Path) -> Result<(Self, bool), SettingsError> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let mut loaded: Self = serde_json::from_str(&s)?;
                let changed = loaded.migrate();
                Ok((loaded, changed))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((Self::default(), false)),
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
    fn migrate(&mut self) -> bool {
        let mut changed = false;
        // A chord using `Fn` can never register, so it is not a preference to
        // be respected: it is a shortcut the user believes they have and does
        // not. Settings displayed `Fn+Shift+V` while the app logged
        // "unparseable shortcut" at every single launch and the palette had no
        // binding at all.
        //
        // Rewritten rather than merely disabled, because disabling it would
        // leave the user with no palette shortcut and no indication why. This
        // is not gated on having been offered once, the way the excluded-apps
        // union is: that guards a real choice, and there is no choice to guard
        // in a value that cannot work.
        for (b, fallback) in [
            (&mut self.hotkeys.history_palette, default_palette_key()),
            (&mut self.hotkeys.dictation_alt, default_palette_key()),
            // Its OWN default, not the palette chord: resetting a dead Refine
            // key to the palette chord would make one keystroke do two things.
            (&mut self.hotkeys.refine, default_refine_key()),
        ] {
            if is_unregisterable_chord(&b.key) {
                changed = true;
                let was = std::mem::replace(&mut b.key, fallback.into());
                tracing::warn!(
                    "'{was}' cannot be registered as a global shortcut (Fn is not a chord \
                     modifier); reset to '{}'",
                    b.key
                );
            }
        }

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
            if self.excluded_defaults_seen != defaults {
                changed = true;
            }
            self.excluded_defaults_seen = defaults;
            if !added.is_empty() {
                changed = true;
                tracing::info!(
                    "settings: adding {} password managers to the exclusion list that shipped \
                     after this install was created: {:?}",
                    added.len(),
                    added
                );
                self.history.excluded_apps.extend(added);
            }
        }
        if self.version != SETTINGS_VERSION {
            changed = true;
        }
        self.version = SETTINGS_VERSION;
        changed
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

/// The pre-rename data folder name.
///
/// This is the ONLY place the old product name survives, and it must stay. It
/// is the folder `data_dir` migrates FROM: a sweep that renames it to the new
/// name turns the migration into a no-op that silently loses the user's history
/// and every downloaded model. (That is exactly what the first pass of the
/// rename did.) It is a constant rather than a literal so that a future sweep
/// has one clearly-labelled thing to skip instead of a bare string in a
/// function body.
const OLD_DATA_DIR: &str = "EchoKey";

/// Application data directory (settings, history DB, models).
///
/// `%LOCALAPPDATA%\Parle` on Windows (models must NOT live in Program Files),
/// `~/Library/Application Support/Parle` on macOS.
///
/// The folder had a different name before the rename (see `OLD_DATA_DIR`), and
/// it holds the user's entire history plus every model they have downloaded,
/// which runs to gigabytes. So an install that still has the old folder is MIGRATED by
/// renaming it, which is instant and atomic on the same volume rather than a
/// multi-gigabyte copy, and the old name is never read again afterwards.
///
/// If the rename fails for any reason the old directory keeps being used, so
/// the worst case is the previous behaviour rather than an app that has
/// silently lost its history.
pub fn data_dir() -> PathBuf {
    let (dir, outcome) = resolve_data_dir(&os_data_base());
    outcome.log();
    dir
}

/// The OS directory the data folder lives inside.
fn os_data_base() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = dirs::data_local_dir();
    #[cfg(not(target_os = "windows"))]
    let base = dirs::data_dir();
    base.unwrap_or_else(|| PathBuf::from("."))
}

/// What `resolve_data_dir` found, so the caller can report it and a retirement
/// check can ask without guessing. See `LegacyOutcome::still_needed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyOutcome {
    /// No legacy folder anywhere. On this machine the migration is finished.
    NoLegacy,
    /// The legacy folder was renamed wholesale onto the new name.
    Migrated,
    /// An empty new folder already existed, so entries were moved across.
    Merged,
    /// The move failed. The legacy folder is STILL the live data directory.
    LegacyKept,
}

impl LegacyOutcome {
    /// Does `OLD_DATA_DIR` still have work to do on this machine?
    ///
    /// `NoLegacy` is the only answer that means "nothing here needs it any
    /// more". `Migrated` and `Merged` mean it did its job THIS run, which is
    /// the opposite of safe to remove.
    pub fn still_needed(self) -> bool {
        self != LegacyOutcome::NoLegacy
    }

    fn log(self) {
        match self {
            LegacyOutcome::NoLegacy => {}
            LegacyOutcome::Migrated => {
                tracing::info!("migrated the data directory from its old location to Parle")
            }
            LegacyOutcome::Merged => {
                tracing::info!("merged the old data directory into Parle")
            }
            LegacyOutcome::LegacyKept => tracing::warn!(
                "could not rename the old data directory to Parle; \
                 continuing to use the old one so nothing is lost"
            ),
        }
    }
}

/// Resolve the data directory under `base`, migrating the legacy folder if one
/// is there.
///
/// Split out from `data_dir` so it can be tested against a temp directory. The
/// untested version of this is what silently destroyed a user's history the
/// first time the rename was attempted, and "it reads the real OS directory so
/// it cannot be tested" is exactly what let that through.
pub fn resolve_data_dir(base: &Path) -> (PathBuf, LegacyOutcome) {
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
        return (new, LegacyOutcome::NoLegacy);
    }
    let old = base.join(OLD_DATA_DIR);
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
            return (new, LegacyOutcome::Merged);
        }
        return match std::fs::rename(&old, &new) {
            Ok(()) => (new, LegacyOutcome::Migrated),
            Err(_) => (old, LegacyOutcome::LegacyKept),
        };
    }
    (new, LegacyOutcome::NoLegacy)
}

/// Is a legacy data folder still sitting beside the current one?
///
/// The retirement check for `OLD_DATA_DIR`: when this is false on every machine
/// you care about, the constant and its migration branch can be deleted. It
/// reads the disk and does not move anything.
pub fn legacy_data_dir_present() -> bool {
    let base = os_data_base();
    let old = base.join(OLD_DATA_DIR);
    old.join("history.db").exists() || old.join("settings.json").exists() || old.join("models").is_dir()
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

    // ---- the history-palette chord ----

    // ---- Refine ----

    #[test]
    fn refine_is_off_by_default_and_sends_nothing_anywhere() {
        let s = Settings::default();
        assert!(!s.refine.enabled, "the AI mode must be an opt-in");
        assert!(!s.hotkeys.refine.enabled, "no Refine key is armed until the user turns it on");
        assert_eq!(s.refine.provider, RefineProvider::Claude);
        assert_eq!(s.refine.fallback, RefineFallback::ClipboardOnly);
        assert_eq!(s.refine.accent, default_refine_accent());
    }

    #[test]
    fn the_default_trigger_is_a_modifier_on_the_dictation_key() {
        let s = Settings::default();
        assert_eq!(s.refine.trigger, RefineTrigger::Modifier);
        assert_eq!(s.refine.modifier, RefineModifier::Shift);
        // And the separate key is NOT armed, so the default costs no second
        // system-wide binding at all.
        assert!(!s.hotkeys.refine.enabled);
    }

    #[test]
    fn a_modifier_bound_as_the_dictation_key_is_recognised_as_that_family() {
        // The gesture cannot work if the dictation key IS the trigger
        // modifier, and this is how the UI and the mode decision tell.
        assert_eq!(RefineModifier::of_native_key("LeftShift"), Some(RefineModifier::Shift));
        assert_eq!(RefineModifier::of_native_key("RightShift"), Some(RefineModifier::Shift));
        assert_eq!(RefineModifier::of_native_key("RightOption"), Some(RefineModifier::Alt));
        assert_eq!(RefineModifier::of_native_key("RightWin"), Some(RefineModifier::Cmd));
        assert_eq!(RefineModifier::of_native_key("RightCtrl"), Some(RefineModifier::Ctrl));
        // Not modifiers: the two keys people actually dictate with.
        assert_eq!(RefineModifier::of_native_key("Fn"), None);
        assert_eq!(RefineModifier::of_native_key("CopilotKey"), None);
    }

    #[test]
    fn a_settings_file_from_before_the_trigger_existed_takes_the_modifier_default() {
        // The first Refine build shipped with a separate key and no trigger
        // field. An absent field must land on the new default rather than on
        // whatever `OwnKey` would imply, because the separate key it would
        // point at is disabled in those files.
        let json = r#"{"version":2,"refine":{"enabled":true,"provider":"claude","timeout_ms":90000}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.refine.enabled);
        assert_eq!(s.refine.trigger, RefineTrigger::Modifier);
        assert_eq!(s.refine.modifier, RefineModifier::Shift);
    }

    #[test]
    fn refine_key_suggestion_is_usable_and_distinct_from_the_dictation_key() {
        let s = Settings::default();
        let k = &s.hotkeys.refine.key;
        assert!(!k.is_empty(), "switching Refine on must give a working key at once");
        assert!(!is_unregisterable_chord(k));
        assert_ne!(k, &s.hotkeys.dictation.key, "the two modes must be distinguishable at the keypress");
        assert_ne!(k, &s.hotkeys.history_palette.key);
    }

    #[test]
    fn a_settings_file_from_before_refine_loads_with_refine_defaults() {
        // An older build's file has neither key. Both must fill from their own
        // defaults rather than from `Settings::default()` blanks.
        let json = r#"{"version":2,"onboarding_complete":true,"hotkeys":{"dictation":{"key":"Fn","mode":"hybrid","enabled":true}}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.refine.enabled);
        assert_eq!(s.refine.timeout_ms, 90_000);
        assert_eq!(s.hotkeys.refine.key, default_refine_key());
        assert!(!s.hotkeys.refine.enabled);
    }

    #[test]
    fn a_dead_fn_chord_on_the_refine_key_resets_to_the_refine_default_not_the_palette() {
        let mut s = Settings::default();
        s.hotkeys.refine.key = "Fn+R".into();
        assert!(s.migrate());
        assert_eq!(s.hotkeys.refine.key, default_refine_key());
        assert_ne!(s.hotkeys.refine.key, s.hotkeys.history_palette.key);
    }

    #[test]
    fn refine_settings_round_trip_every_field() {
        let mut s = Settings::default();
        s.refine.enabled = true;
        s.refine.provider = RefineProvider::Custom;
        s.refine.custom_command = "/usr/local/bin/mywrapper --fast".into();
        s.refine.rules = "No em dashes. Australian spelling.".into();
        s.refine.voice_file = "/Users/me/voice.md".into();
        s.refine.model = "sonnet".into();
        s.refine.accent = "#123456".into();
        s.refine.trigger = RefineTrigger::OwnKey;
        s.refine.modifier = RefineModifier::Ctrl;
        s.refine.fallback = RefineFallback::InsertTranscript;
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(back.refine.enabled);
        assert_eq!(back.refine.provider, RefineProvider::Custom);
        assert_eq!(back.refine.custom_command, s.refine.custom_command);
        assert_eq!(back.refine.rules, s.refine.rules);
        assert_eq!(back.refine.voice_file, s.refine.voice_file);
        assert_eq!(back.refine.model, "sonnet");
        assert_eq!(back.refine.accent, "#123456");
        assert_eq!(back.refine.trigger, RefineTrigger::OwnKey);
        assert_eq!(back.refine.modifier, RefineModifier::Ctrl);
        assert_eq!(back.refine.fallback, RefineFallback::InsertTranscript);
    }

    #[test]
    fn the_palette_default_is_registerable_and_not_paste_and_match_style() {
        let k = default_palette_key();
        assert!(!is_unregisterable_chord(k), "the default must be a chord the OS can deliver");
        // Cmd+Shift+V is paste-and-match-style nearly everywhere. Taking it
        // globally breaks a keystroke people use constantly in other apps.
        assert_ne!(k, "Cmd+Shift+V");
    }

    #[test]
    fn an_fn_chord_is_recognised_as_unregisterable_but_fn_alone_is_not() {
        assert!(is_unregisterable_chord("Fn+Shift+V"));
        assert!(is_unregisterable_chord("Shift+fn+V"), "case and position must not matter");
        // Fn alone is the dictation default and is handled by the native
        // listener, not the global-shortcut API. Flagging it would break the
        // app's primary hotkey.
        assert!(!is_unregisterable_chord("Fn"));
        assert!(!is_unregisterable_chord("Ctrl+Cmd+V"));
        assert!(!is_unregisterable_chord(""));
    }

    #[test]
    fn migrate_repairs_a_stored_fn_chord_and_leaves_a_working_one_alone() {
        let mut s = Settings::default();
        s.hotkeys.history_palette.key = "Fn+Shift+V".into();
        s.migrate();
        assert_eq!(s.hotkeys.history_palette.key, default_palette_key());
        // Still enabled: the user wanted a palette shortcut, and the point is
        // to give them a working one rather than to take it away.
        assert!(s.hotkeys.history_palette.enabled);

        let mut chosen = Settings::default();
        chosen.hotkeys.history_palette.key = "Ctrl+Alt+P".into();
        chosen.migrate();
        assert_eq!(chosen.hotkeys.history_palette.key, "Ctrl+Alt+P", "a deliberate working chord must survive");
    }

    #[test]
    fn migrate_never_touches_the_dictation_key() {
        let mut s = Settings::default();
        s.hotkeys.dictation.key = "Fn".into();
        s.migrate();
        assert_eq!(s.hotkeys.dictation.key, "Fn", "Fn alone is the whole point of the product");
    }

    #[test]
    fn a_migration_that_changes_something_reports_it_so_startup_can_persist() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");

        let mut s = Settings::default();
        s.hotkeys.history_palette.key = "Fn+Shift+V".into();
        s.save(&p).unwrap();

        let (loaded, changed) = Settings::load_migrated(&p).unwrap();
        assert!(changed, "a rewritten shortcut must be reported, or it is never written to disk");
        assert_eq!(loaded.hotkeys.history_palette.key, default_palette_key());

        // Persist it the way startup does, then confirm the second load is quiet.
        loaded.save(&p).unwrap();
        let (_, changed_again) = Settings::load_migrated(&p).unwrap();
        assert!(!changed_again, "a settled file must not re-migrate on every launch");
    }

    // ---- the legacy data-directory migration ----
    //
    // This is the code that silently destroyed a user's history the first time
    // the rename was attempted, and it had no test at all until 30/08/2026.
    // Every case below asserts on the RETURNED PATH and on the files actually
    // present afterwards, never merely that the call returned.

    /// A data directory that looks real to `occupied`.
    fn seed(dir: &Path, marker: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("history.db"), marker).unwrap();
    }

    #[test]
    fn a_fresh_install_uses_the_new_name_and_reports_no_legacy() {
        let t = tempfile::tempdir().unwrap();
        let (dir, outcome) = resolve_data_dir(t.path());
        assert_eq!(dir, t.path().join("Parle"));
        assert_eq!(outcome, LegacyOutcome::NoLegacy);
        assert!(!outcome.still_needed());
    }

    #[test]
    fn a_legacy_folder_alone_is_renamed_and_its_contents_survive() {
        let t = tempfile::tempdir().unwrap();
        seed(&t.path().join(OLD_DATA_DIR), "the user's history");

        let (dir, outcome) = resolve_data_dir(t.path());

        assert_eq!(outcome, LegacyOutcome::Migrated);
        assert_eq!(dir, t.path().join("Parle"));
        // The bytes moved, not just the directory entry.
        assert_eq!(
            std::fs::read_to_string(dir.join("history.db")).unwrap(),
            "the user's history"
        );
        assert!(!t.path().join(OLD_DATA_DIR).exists(), "the legacy folder should be gone");
    }

    #[test]
    fn an_empty_new_folder_does_not_strand_the_legacy_one() {
        // The case that makes a bare rename fail: something created `Parle`
        // first (an installer, a log file, the user), so the migration has to
        // move entries across instead.
        let t = tempfile::tempdir().unwrap();
        seed(&t.path().join(OLD_DATA_DIR), "still the user's history");
        std::fs::create_dir_all(t.path().join("Parle")).unwrap();

        let (dir, outcome) = resolve_data_dir(t.path());

        assert_eq!(outcome, LegacyOutcome::Merged);
        assert_eq!(dir, t.path().join("Parle"));
        assert_eq!(
            std::fs::read_to_string(dir.join("history.db")).unwrap(),
            "still the user's history"
        );
    }

    #[test]
    fn a_merge_never_overwrites_a_file_already_in_the_new_folder() {
        let t = tempfile::tempdir().unwrap();
        seed(&t.path().join(OLD_DATA_DIR), "old");
        std::fs::create_dir_all(t.path().join("Parle")).unwrap();
        std::fs::write(t.path().join("Parle").join("history.db"), "new").unwrap();

        let (dir, _) = resolve_data_dir(t.path());

        // The live file wins; the legacy one is left behind rather than
        // destroyed. Losing a file here would be the worst possible outcome.
        assert_eq!(std::fs::read_to_string(dir.join("history.db")).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(t.path().join(OLD_DATA_DIR).join("history.db")).unwrap(),
            "old"
        );
    }

    #[test]
    fn an_occupied_new_folder_wins_and_the_legacy_one_is_never_touched() {
        let t = tempfile::tempdir().unwrap();
        seed(&t.path().join(OLD_DATA_DIR), "old");
        seed(&t.path().join("Parle"), "current");

        let (dir, outcome) = resolve_data_dir(t.path());

        assert_eq!(dir, t.path().join("Parle"));
        assert_eq!(outcome, LegacyOutcome::NoLegacy, "an already-migrated install is finished");
        assert_eq!(std::fs::read_to_string(dir.join("history.db")).unwrap(), "current");
        // Untouched, so a user who wants the old copy back still has it.
        assert_eq!(
            std::fs::read_to_string(t.path().join(OLD_DATA_DIR).join("history.db")).unwrap(),
            "old"
        );
    }

    #[test]
    fn an_empty_legacy_folder_is_not_treated_as_data() {
        // A bare directory with nothing in it is not a data folder, and
        // migrating it would be a no-op that reports success.
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join(OLD_DATA_DIR)).unwrap();

        let (dir, outcome) = resolve_data_dir(t.path());

        assert_eq!(dir, t.path().join("Parle"));
        assert_eq!(outcome, LegacyOutcome::NoLegacy);
    }

    /// The guard on the guard.
    ///
    /// The tests above would all still pass if `OLD_DATA_DIR` were changed to
    /// the CURRENT name, because they only ever refer to it through the
    /// constant: the seed and the assertion would move together and agree with
    /// each other while the real migration did nothing. This test writes the
    /// pre-rename name as a LITERAL, so a sweep that rewrites the constant
    /// makes it fail instead of passing quietly.
    ///
    /// If you are deliberately retiring `OLD_DATA_DIR`, this is the test that
    /// is supposed to stop you, and `docs/RENAME_AUDIT.md` has the checklist.
    #[test]
    fn the_legacy_folder_name_is_still_the_pre_rename_one() {
        assert_eq!(
            OLD_DATA_DIR, "EchoKey",
            "OLD_DATA_DIR is the folder we migrate FROM. Renaming it makes the \
             migration a no-op that silently loses the user's history and every \
             downloaded model. See docs/RENAME_AUDIT.md before changing this."
        );

        let t = tempfile::tempdir().unwrap();
        seed(&t.path().join("EchoKey"), "history written before the rename");
        let (dir, outcome) = resolve_data_dir(t.path());
        assert_eq!(outcome, LegacyOutcome::Migrated);
        assert_eq!(
            std::fs::read_to_string(dir.join("history.db")).unwrap(),
            "history written before the rename"
        );
    }

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
    fn the_dictation_default_is_the_bottom_left_key_on_both_platforms() {
        // Chosen by POSITION so muscle memory carries between a Mac and a PC.
        // Never RightAlt, which is AltGr on many layouts.
        #[cfg(not(target_os = "macos"))]
        assert_eq!(HotkeySettings::default().dictation.key, "LeftCtrl");
        #[cfg(target_os = "macos")]
        assert_eq!(HotkeySettings::default().dictation.key, "Fn");
    }

    #[test]
    fn the_windows_default_gesture_never_swallows_its_key() {
        // Load-bearing, not cosmetic. DoubleTap is the only mode that leaves
        // the key's normal system behaviour intact, and the Windows default is
        // Left Ctrl — bound in any other mode it would eat Ctrl+C, Ctrl+V and
        // every other Ctrl chord. If this assertion is ever relaxed, the
        // Windows default key has to change in the same commit.
        #[cfg(not(target_os = "macos"))]
        assert_eq!(HotkeySettings::default().dictation.mode, HotkeyMode::DoubleTap);
    }

    #[test]
    fn macos_does_not_default_to_the_gesture_the_os_already_took() {
        // macOS binds a double-press of Globe to its own dictation. DoubleTap
        // does not swallow the key, so defaulting to it would put Parle in a
        // race with the OS on every take. Hold-to-talk sidesteps that entirely,
        // which is why the two platforms deliberately differ here.
        #[cfg(target_os = "macos")]
        assert_eq!(HotkeySettings::default().dictation.mode, HotkeyMode::Hybrid);
    }

    #[test]
    fn holding_shift_is_the_refine_trigger_on_both_platforms() {
        // The other half of the alignment: the same gesture on the same key,
        // and the same modifier held to send it to the AI.
        let s = Settings::default();
        assert_eq!(s.refine.trigger, RefineTrigger::Modifier);
        assert_eq!(s.refine.modifier, RefineModifier::Shift);
        // And Shift must not collide with the dictation key's own family, or
        // the clash guard in mode_for_dictation would refuse every Refine take.
        assert_ne!(
            RefineModifier::of_native_key(&s.hotkeys.dictation.key),
            Some(s.refine.modifier),
            "the default dictation key must not BE the default Refine modifier"
        );
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
    /// OFF by default, deliberately.
    ///
    /// A dictation is something the user chose to say to Parle. The clipboard
    /// is everything they copy all day, including passwords, card numbers and
    /// other people's private messages, and most of it they never think about.
    /// Sending that to a second machine has to be something they turn ON with
    /// their eyes open, not something they discover later.
    ///
    /// Widening a sync kind is already handled: the debt machinery re-offers
    /// history when the user switches this on, so defaulting to off costs them
    /// nothing but a toggle.
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
            sync_clipboard: false,
            paired: Vec::new(),
            resend_owed: Vec::new(),
        }
    }
}
