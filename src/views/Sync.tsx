// Sync, as a place rather than a setting.
//
// It lived at the bottom of Settings, which is where a user goes to change
// something they already know exists. Sync is the opposite: it has to be FOUND,
// and once found it is the only part of the app with live state to watch, peers
// appearing and disappearing, an exchange running, a code counting down. Those
// two things do not belong on the same screen.
//
// The panel itself still lives in SettingsView, where its helpers are. Only
// where it is REACHED FROM changed.

import type { Settings } from '../types';
import { SyncSection } from './SettingsView';

export default function SyncView({ settings }: { settings: Settings }) {
  return (
    // The settings layout, deliberately: the panel is built from the same
    // Section cards, and giving it a second container would make one screen of
    // the app sit a few pixels off from every other.
    <div className="settings">
      <SyncSection sync={settings.sync} />
    </div>
  );
}
