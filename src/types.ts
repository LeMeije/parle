// Mirrors of the Rust types crossing the IPC boundary.

export interface Settings {
  /** UI language, or "" until the user has chosen one. Separate from the
   *  transcription language: French interface, English dictation is common. */
  ui_language: string;
  version: number;
  onboarding_complete: boolean;
  models: {
    active_model: string;
    fallback_chain: string[];
    backend: string;
    prewarm: boolean;
  };
  language: {
    language: string;
    locale: string;
    translate_to_english: boolean;
  };
  cleanup: {
    enabled: boolean;
    remove_fillers: boolean;
    remove_hedges: boolean;
    trim_self_corrections: boolean;
    capitalise_sentences: boolean;
    ensure_terminal_punctuation: boolean;
    dictated_punctuation: boolean;
    paragraph_on_long_pause: boolean;
    paragraph_pause_ms: number;
    locale_spelling: boolean;
    llm_enabled: boolean;
    llm_model: string;
    llm_timeout_ms: number;
  };
  hotkeys: {
    dictation: HotkeyBinding;
    dictation_alt: HotkeyBinding;
    history_palette: HotkeyBinding;
    cancel: HotkeyBinding;
    latch_ms: number;
    suppress_copilot: boolean;
  };
  dictionary: {
    enabled: boolean;
    bias_recognition: boolean;
    fuzzy_correct: boolean;
    auto_learn: boolean;
  };
  appearance: {
    theme_mode: 'system' | 'light' | 'dark';
    palette: string;
    accent: string;
    app_icon: string;
    tray_style: string;
    reduce_motion: boolean;
  };
  history: {
    clipboard_capture: boolean;
    retention_days: number;
    excluded_apps: string[];
    encrypt_at_rest: boolean;
    max_items: number;
  };
  audio: {
    input_device: string;
    min_duration_ms: number;
    sounds: boolean;
  };
  overlay: {
    position: string;
    style: string;
    show_partial_text: boolean;
    /** 0.5 to 2.0. Shifts the level window rather than scaling the bars. */
    waveform_sensitivity: number;
  };
  paste: {
    inject: boolean;
    copy_to_clipboard: boolean;
    restore_clipboard: boolean;
    restore_delay_ms: number;
    prefer_ax_insert: boolean;
    press_enter: boolean;
  };
  // Present from the build that introduced cross-machine sync. Optional so an
  // older backend (or one mid-rebuild) can't blank the Sync panel.
  sync?: SyncSettings;
  launch_at_login: boolean;
  auto_update_check: boolean;
}

export interface SyncSettings {
  enabled: boolean;
  device_id: string;
  device_name: string;
  sync_dictations: boolean;
  sync_clipboard: boolean;
}

export interface HotkeyBinding {
  key: string;
  mode: 'hold' | 'toggle' | 'hybrid' | 'double_tap';
  enabled: boolean;
}

export interface HistoryItem {
  id: number;
  kind: 'transcription' | 'clipboard';
  text: string;
  raw_text: string | null;
  created_at: number;
  pinned: boolean;
  duration_ms: number | null;
  model_id: string | null;
  language: string | null;
  app_id: string | null;
  app_name: string | null;
  // Kept on this device and never offered to a paired device. Set when the
  // secure-field probe could not rule out that this was a password field.
  local_only: boolean;
  // The device that CREATED this row, as a sync device id. Null means this
  // machine: the row predates this install having a sync identity, or was
  // written before sync was switched on.
  source_machine: string | null;
  meta: string | null;
}

export interface ModelRow {
  id: string;
  display_name: string;
  backend: string;
  size_bytes: number;
  speed: number;
  accuracy: number;
  multilingual: boolean;
  downloaded: boolean;
  active: boolean;
  /** Added by the user from their own disk: removable, never downloadable. */
  custom: boolean;
}

export interface DictEntry {
  id: number;
  term: string;
  corrections: string[];
  auto_learned: boolean;
  enabled: boolean;
}

/// Content pasted or typed into a recording, pinned to the audio timestamp it
/// was added at and spliced verbatim into the transcript at that point.
export interface Mark {
  at_ms: number;
  text: string;
}

export interface LevelUpdate {
  rms: number;
  peak: number;
  envelope: number;
  elapsed_ms: number;
}

export type PipelineEvent =
  | { kind: 'state_changed'; state: 'idle' | 'recording' | 'transcribing' }
  | { kind: 'partial'; text: string }
  | { kind: 'mark_added'; at_ms: number; text: string }
  | {
      kind: 'completed';
      item_id: number;
      // The dictation did NOT go into a syncing history. Every handler must
      // consult this before rendering `text`: on a password-field dictation
      // `text` is the password.
      withheld: boolean;
      text: string;
      duration_ms: number;
      transcribe_ms: number;
      model_id: string;
      injection: { method: string; manual_paste_required: boolean } | null;
      low_confidence_count: number;
    }
  | { kind: 'empty'; reason: string }
  | { kind: 'error'; message: string };

// ---------- Cross-machine sync ----------
// Mirrors the `sync_status` command payload and the `sync-status` event.

/** Seen on the LAN but not yet paired. */
export interface SyncPeer {
  id: string;
  name: string;
  addr: string;
  port: number;
}

export interface SyncPairedDevice {
  id: string;
  name: string;
  /** When an exchange was last ATTEMPTED, successful or not. */
  last_seen: number | null;
  /** Visible on the network right now, from mDNS alone. Presence, not health. */
  online: boolean;
  /** When an exchange with this device last actually SUCCEEDED, epoch ms. */
  last_sync_ok: number | null;
}

/** `code` is only populated when this device is the one showing it. */
export interface SyncPairingState {
  role: 'showing' | 'entering';
  code: string | null;
  peer_id: string | null;
  expires_at: number;
}

export interface SyncStatus {
  enabled: boolean;
  device_id: string;
  device_name: string;
  peers: SyncPeer[];
  paired: SyncPairedDevice[];
  pairing: SyncPairingState | null;
  /** True while discovery is running, so "none found" isn't shown instantly. */
  scanning: boolean;
  dictations: boolean;
  clipboard: boolean;
  /** Why sync isn't working, when it's enabled but dead. */
  error: string | null;
}

export interface PermissionStatus {
  accessibility: boolean;
  microphone: string;
}

export interface DownloadProgress {
  model_id: string;
  downloaded: number;
  total: number;
}

/// The paste chord, defined ONCE.
///
/// The HUD and the main window each carried their own literal and round 12
/// fixed only one of them. A shared constant is the only thing that stops the
/// two drifting apart again.
/// The copy chord, defined ONCE, for the same reason as PASTE_KEYS: History
/// hard-coded the Command glyph and told every Windows user to press a key
/// their keyboard does not have.
export const COPY_KEYS = navigator.userAgent.includes('Mac') ? '\u2318Enter' : 'Ctrl+Enter';

export const PASTE_KEYS = navigator.userAgent.includes('Mac') ? '\u2318V' : 'Ctrl+V';
