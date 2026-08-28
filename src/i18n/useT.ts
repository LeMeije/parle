// Re-render a component when the language changes.
//
// `t()` reads a module-level current language, so nothing would repaint on a
// switch without this. Components call `useT()` and then use the returned `t`.
import { useEffect, useState } from 'react';
import { onLangChange, t } from './index';

export function useT(): typeof t {
  const [, force] = useState(0);
  useEffect(() => onLangChange(() => force((n) => n + 1)), []);
  return t;
}
