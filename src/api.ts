import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import type {
  DictEntry,
  DownloadProgress,
  HistoryItem,
  LevelUpdate,
  ModelRow,
  PermissionStatus,
  PipelineEvent,
  Settings,
  SyncStatus,
} from './types';

export const api = {
  getSettings: () => invoke<Settings>('get_settings'),
  setSettings: (settings: Settings) => invoke<void>('set_settings', { settings }),

  searchHistory: (query: string, kind?: string, limit?: number) =>
    invoke<HistoryItem[]>('search_history', { query, kind: kind ?? null, limit: limit ?? 60 }),
  pinItem: (id: number, pinned: boolean) => invoke<void>('pin_item', { id, pinned }),
  deleteItem: (id: number) => invoke<void>('delete_item', { id }),
  clearHistory: (kind?: string) => invoke<number>('clear_history', { kind: kind ?? null }),
  updateItemText: (id: number, text: string, learn = true) => invoke<void>('update_item_text', { id, text, learn }),
  copyItem: (id: number) => invoke<void>('copy_item', { id }),
  pasteItem: (id: number) => invoke<void>('paste_item', { id }),

  listModels: () => invoke<ModelRow[]>('list_models'),
  downloadModel: (modelId: string) => invoke<void>('download_model', { modelId }),
  addCustomModel: (path: string, displayName: string, multilingual: boolean) =>
    invoke<string>('add_custom_model', { path, displayName, multilingual }),
  removeCustomModel: (modelId: string) => invoke<void>('remove_custom_model', { modelId }),
  cancelDownload: (modelId: string) => invoke<void>('cancel_download', { modelId }),
  deleteModel: (modelId: string) => invoke<void>('delete_model', { modelId }),
  selectModel: (modelId: string) => invoke<void>('select_model', { modelId }),
  engineStatus: () => invoke<{ loaded_model: string | null; warm: boolean }>('engine_status'),

  dictList: () => invoke<DictEntry[]>('dict_list'),
  dictAdd: (term: string, corrections: string[]) =>
    invoke<{ id: number; warning: string | null }>('dict_add', { term, corrections }),
  dictSetEnabled: (id: number, enabled: boolean) => invoke<void>('dict_set_enabled', { id, enabled }),
  dictDelete: (id: number) => invoke<void>('dict_delete', { id }),

  syncStatus: () => invoke<SyncStatus>('sync_status'),
  syncSetEnabled: (enabled: boolean) => invoke<void>('sync_set_enabled', { enabled }),
  syncSetDeviceName: (name: string) => invoke<void>('sync_set_device_name', { name }),
  syncStartPairing: () => invoke<{ code: string; expires_at: number }>('sync_start_pairing'),
  syncCancelPairing: () => invoke<void>('sync_cancel_pairing'),
  syncPairWith: (peerId: string, code: string) => invoke<void>('sync_pair_with', { peerId, code }),
  /** Exchange with every visible paired device now. Resolves to how many
   *  exchanges actually started; 0 means nobody was reachable. */
  syncNow: () => invoke<number>('sync_now'),
  syncUnpair: (deviceId: string) => invoke<void>('sync_unpair', { deviceId }),
  syncSetKinds: (dictations: boolean, clipboard: boolean) =>
    invoke<void>('sync_set_kinds', { dictations, clipboard }),

  startRecording: () => invoke<void>('start_recording'),
  stopRecording: () => invoke<void>('stop_recording'),
  cancelRecording: () => invoke<void>('cancel_recording'),

  permissionStatus: () => invoke<PermissionStatus>('permission_status'),
  requestMicrophone: () => invoke<void>('request_microphone'),
  requestAccessibility: () => invoke<void>('request_accessibility'),
  repairAccessibility: () => invoke<void>('repair_accessibility'),
  setAppIcon: (iconId: string) => invoke<boolean>('set_app_icon', { iconId }),
  restartApp: () => invoke<void>('restart_app'),
  insertMark: (text: string) => invoke<number>('insert_mark', { text }),
  pipelineState: () => invoke<'recording' | 'idle'>('pipeline_state'),
  openPermissionSettings: (which: string) => invoke<void>('open_permission_settings', { which }),
  listAudioDevices: () => invoke<string[]>('list_audio_devices'),
  recommendedSetup: () =>
    invoke<{ profile: { os: string; total_ram_mb: number; gpu: string }; model: string; fallback_chain: string[] }>(
      'recommended_setup',
    ),
  completeOnboarding: () => invoke<void>('complete_onboarding'),
};

export function onPipelineEvent(cb: (e: PipelineEvent) => void): Promise<UnlistenFn> {
  return listen<PipelineEvent>('pipeline-event', (e) => cb(e.payload));
}

export function onLevel(cb: (u: LevelUpdate) => void): Promise<UnlistenFn> {
  return listen<{ kind: string } & LevelUpdate>('pipeline-level', (e) => cb(e.payload as unknown as LevelUpdate));
}

export function onPartial(cb: (text: string) => void): Promise<UnlistenFn> {
  return listen<{ kind: string; text: string }>('pipeline-partial', (e) => cb(e.payload.text));
}

export function onHistoryChanged(cb: () => void): Promise<UnlistenFn> {
  return listen('history-changed', () => cb());
}

export function onDownloadProgress(cb: (p: DownloadProgress) => void): Promise<UnlistenFn> {
  return listen<DownloadProgress>('model-download-progress', (e) => cb(e.payload));
}

export function onDownloadComplete(cb: (modelId: string) => void): Promise<UnlistenFn> {
  return listen<string>('model-download-complete', (e) => cb(e.payload));
}

export function onDownloadError(cb: (message: string) => void): Promise<UnlistenFn> {
  return listen<string>('model-download-error', (e) => cb(e.payload));
}

export function onSyncStatus(cb: (s: SyncStatus) => void): Promise<UnlistenFn> {
  return listen<SyncStatus>('sync-status', (e) => cb(e.payload));
}

export function onFocusPalette(cb: () => void): Promise<UnlistenFn> {
  return listen('focus-palette', () => cb());
}
