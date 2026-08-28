// Parle's translation layer.
//
// Deliberately tiny and dependency-free: the app ships offline, and an i18n
// framework would pull in a loader, a plural engine and an async init for what
// is a few hundred fixed strings. `t()` is a synchronous lookup with English as
// the fallback, so a key that has not been translated yet renders in English
// rather than as a raw key or an empty space.

import { en } from './en';
import { fr } from './fr';
import { es } from './es';
import { de } from './de';
import { pt } from './pt';

export type Lang = 'en' | 'fr' | 'es' | 'de' | 'pt';

/// The languages offered, with their names IN their own language, because a
/// person looking for their language is looking for the word they use for it.
export const LANGUAGES: { code: Lang; label: string; english: string }[] = [
  { code: 'en', label: 'English', english: 'English' },
  { code: 'fr', label: 'Français', english: 'French' },
  { code: 'es', label: 'Español', english: 'Spanish' },
  { code: 'de', label: 'Deutsch', english: 'German' },
  { code: 'pt', label: 'Português', english: 'Portuguese' },
];

/// Which transcription language each UI language implies on first run.
///
/// Picking a UI language is the clearest signal we ever get about what someone
/// is likely to dictate in, and asking twice on first launch for the same
/// answer is a worse experience than defaulting and letting them change it.
export const TRANSCRIPTION_DEFAULT: Record<Lang, string> = {
  en: 'en',
  fr: 'fr',
  es: 'es',
  de: 'de',
  pt: 'pt',
};

type Dict = Record<string, string>;

const DICTS: Record<Lang, Dict> = { en, fr, es, de, pt };

let current: Lang = 'en';
const listeners = new Set<() => void>();

export function getLang(): Lang {
  return current;
}

export function setLang(l: Lang): void {
  if (l === current) return;
  current = l;
  document.documentElement.lang = l;
  listeners.forEach((f) => f());
}

export function onLangChange(f: () => void): () => void {
  listeners.add(f);
  return () => listeners.delete(f);
}

/// The best supported match for the OS locale, for a first run with no setting.
export function detectLang(): Lang {
  const nav = navigator.language?.toLowerCase() ?? 'en';
  const base = nav.split('-')[0];
  const hit = LANGUAGES.find((l) => l.code === base);
  return hit ? hit.code : 'en';
}

/// Translate. Unknown keys and untranslated strings fall back to English, and
/// an unknown key falls back to the key itself so a mistake is visible in
/// testing rather than rendering as blank.
export function t(key: string, vars?: Record<string, string | number>): string {
  const s = DICTS[current][key] ?? en[key] ?? key;
  if (!vars) return s;
  return s.replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? `{${k}}`));
}
