// Which machine a history row came from, and how it is coloured.
//
// One place, because three views ask the same question and a second copy of the
// hash would eventually disagree with the first: a row would then be blue in the
// list and green in the filter, which is worse than no colour at all.

import type { HistoryItem, SyncStatus } from './types';

/// Hues for peer devices.
///
/// Chosen to stay apart from the hues already carrying meaning elsewhere in the
/// UI (amber = unsure, violet = trimmed, teal = language) so a device colour is
/// never mistaken for a status. Six is deliberate: past that, colours stop being
/// tellable apart at the 3px the edge marker gets, and the NAME is doing the
/// real work anyway.
export const DEVICE_HUES = [
  '#0091ff', // blue
  '#30a46c', // green
  '#e5484d', // red
  '#d6409f', // pink
  '#f76808', // orange
  '#ad7f58', // bronze
] as const;

/// Stable colour for a device id.
///
/// Derived from the id rather than from position in the paired list, so a device
/// keeps its colour when another one is added, removed, or comes back in a
/// different order. A colour that moves is worse than no colour: the user learns
/// "green is the Mac" and then green silently becomes the phone.
export function hueFor(deviceId: string): string {
  let h = 0;
  for (let i = 0; i < deviceId.length; i++) {
    h = (h * 31 + deviceId.charCodeAt(i)) >>> 0;
  }
  return DEVICE_HUES[h % DEVICE_HUES.length];
}

/// Did this machine write this row?
///
/// A null `source_machine` means yes: the row predates this install having a
/// sync identity, or was written before sync was ever switched on. Treating null
/// as "somewhere else" would paint every pre-sync row as foreign, which is most
/// of an existing user's history.
export function isLocal(item: HistoryItem, status: SyncStatus | null): boolean {
  if (item.source_machine == null) return true;
  if (!status) return true;
  return item.source_machine === status.device_id;
}

/// What to call a device id, for a pill or a filter row.
///
/// An id we hold no name for is shown as a short prefix rather than "Unknown":
/// it is a real device that wrote a real row, and the prefix is enough to tell
/// two of them apart. This happens after an unpair, when the rows outlive the
/// pairing that explains them.
export function deviceLabel(id: string, status: SyncStatus | null): string {
  if (!status) return id.slice(0, 8);
  if (id === status.device_id) return status.device_name;
  return status.paired.find((d) => d.id === id)?.name ?? id.slice(0, 8);
}

/// Every device that actually appears in a page of history, plus this machine.
///
/// Built from the ROWS, not from the paired roster, so a device that has been
/// unpaired still offers a filter for the rows it left behind. Sorted with this
/// machine first, then by name, so the list does not reshuffle as rows arrive.
export function devicesInItems(
  items: HistoryItem[],
  status: SyncStatus | null,
): { id: string; label: string; local: boolean }[] {
  const seen = new Map<string, boolean>();
  const localId = status?.device_id ?? '';
  for (const it of items) {
    const id = it.source_machine ?? localId;
    if (!id) continue;
    seen.set(id, id === localId);
  }
  if (localId) seen.set(localId, true);
  return [...seen.entries()]
    .map(([id, local]) => ({ id, label: deviceLabel(id, status), local }))
    .sort((a, b) => (a.local === b.local ? a.label.localeCompare(b.label) : a.local ? -1 : 1));
}
