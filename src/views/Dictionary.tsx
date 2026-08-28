// Custom dictionary: standalone terms + correction pairs, with the
// false-match warning surfaced at entry time.

import { useCallback, useEffect, useState } from 'react';
import { Plus, Sparkles, Trash2 } from 'lucide-react';
import { api } from '../api';
import type { DictEntry } from '../types';
import { useT } from '../i18n/useT';

export default function DictionaryView() {
  const t = useT();
  const [entries, setEntries] = useState<DictEntry[]>([]);
  const [term, setTerm] = useState('');
  const [corrections, setCorrections] = useState('');
  const [warning, setWarning] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api.dictList().then(setEntries);
  }, []);

  useEffect(refresh, [refresh]);

  async function add() {
    const trimmed = term.trim();
    if (!trimmed) return;
    const corr = corrections
      .split(',')
      .map((c) => c.trim())
      .filter(Boolean);
    const res = await api.dictAdd(trimmed, corr);
    setWarning(res.warning);
    setTerm('');
    setCorrections('');
    refresh();
  }

  return (
    <div className="dictionary">
      <header className="view-head">
        <h1>{t('dictionary.title')}</h1>
        <p>{t('dictionary.subtitle')}</p>
      </header>

      <div className="dict-add">
        <input
          placeholder={t('dictionary.term.placeholder')}
          value={term}
          onChange={(e) => setTerm(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && add()}
        />
        <input
          placeholder={t('dictionary.corrections.placeholder')}
          value={corrections}
          onChange={(e) => setCorrections(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && add()}
        />
        <button className="btn" onClick={add}>
          <Plus size={14} /> {t('dictionary.add')}
        </button>
      </div>
      {warning && <div className="callout warn">{warning}</div>}

      <div className="dict-list">
        {entries.length === 0 && (
          <div className="empty">
            <Sparkles size={26} strokeWidth={1.6} />
            <p>{t('dictionary.empty')}</p>
          </div>
        )}
        {entries.map((e) => (
          <div key={e.id} className={`dict-row ${e.enabled ? '' : 'disabled'}`}>
            <label className="switch">
              <input
                type="checkbox"
                checked={e.enabled}
                onChange={(ev) => api.dictSetEnabled(e.id, ev.target.checked).then(refresh)}
              />
              <span />
            </label>
            <div className="dict-term">
              {e.term}
              {e.auto_learned && (
                <span className="pill" title={t('dictionary.autoBadgeTitle')}>
                  {t('dictionary.autoBadge')}
                </span>
              )}
            </div>
            <div className="dict-corrections">
              {e.corrections.length > 0 ? e.corrections.join(', ') : <span className="faint">{t('dictionary.fuzzyMatch')}</span>}
            </div>
            <button className="icon-btn danger" onClick={() => api.dictDelete(e.id).then(refresh)}>
              <Trash2 size={14} />
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
