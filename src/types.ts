// Mirrors of the Rust types crossing the IPC boundary.

export interface Settings {
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
  };
  paste: {
    inject: boolean;
    copy_to_clipboard: boolean;
    restore_clipboard: boolean;
    restore_delay_ms: number;
    prefer_ax_insert: boolean;
    press_enter: boolean;
  };
  launch_at_login: boolean;
  auto_update_check: boolean;
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
}

export interface DictEntry {
  id: number;
  term: string;
  corrections: string[];
  auto_learned: boolean;
  enabled: boolean;
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
      text: string;
      duration_ms: number;
      transcribe_ms: number;
      model_id: string;
      injection: { method: string; manual_paste_required: boolean } | null;
      low_confidence_count: number;
    }
  | { kind: 'empty'; reason: string }
  | { kind: 'error'; message: string };

export interface PermissionStatus {
  accessibility: boolean;
  microphone: string;
}

export interface DownloadProgress {
  model_id: string;
  downloaded: number;
  total: number;
}
