// One place that decides which language the interface speaks.
//
// Every window loads settings independently, so without a single helper each
// would have to remember to do this and the HUD would sooner or later be left
// in English while the main window was translated.
import type { Settings } from '../types';
import { detectLang, setLang, type Lang } from './index';
import { LANGUAGES } from './index';

export function applyLang(s: Settings): void {
  // An empty setting means the user has never chosen. Falling back to the OS
  // locale means a French user sees French before they have touched anything,
  // and the first-run picker still gets to confirm it.
  const chosen = s.ui_language as Lang;
  const known = LANGUAGES.some((l) => l.code === chosen);
  setLang(known ? chosen : detectLang());
}
