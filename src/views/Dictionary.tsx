// Custom dictionary: standalone terms + correction pairs, with the
// false-match warning surfaced at entry time.

import { useCallback, useEffect, useState } from 'react';
import { Plus, Sparkles, Trash2 } from 'lucide-react';
import { api } from '../api';
import type { DictEntry } from '../types';

export default function DictionaryView() {
  const [entries, setEntries] = useState<DictEntry[]>([]);
  const [term, setTerm] = useState('');
  const [corrections, setCorrections] = useState('');
  const [warning, setWarning] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api.dictList().then(setEntries);
  }, []);

  useEffect(refresh, [refresh]);

  async function add() {
    const t = term.trim();
    if (!t) return;
    const corr = corrections
      .split(',')
      .map((c) => c.trim())
      .filter(Boolean);
    const res = await api.dictAdd(t, corr);
    setWarning(res.warning);
    setTerm('');
    setCorrections('');
    refresh();
  }

  return (
    <div className="dictionary">
      <header className="view-head">
        <h1>Dictionary</h1>
        <p>
          Names, brands and jargon Parle should get right. Terms bias recognition and fix close
          misspellings — never inserting words you didn't say.
        </p>
      </header>

      <div className="dict-add">
        <input
          placeholder="Term (exact casing, e.g. “farsiight”)"
          value={term}
          onChange={(e) => setTerm(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && add()}
        />
        <input
          placeholder="Heard as… (optional, comma-separated, e.g. “far sight, foresight”)"
          value={corrections}
          onChange={(e) => setCorrections(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && add()}
        />
        <button className="btn" onClick={add}>
          <Plus size={14} /> Add
        </button>
      </div>
      {warning && <div className="callout warn">{warning}</div>}

      <div className="dict-list">
        {entries.length === 0 && (
          <div className="empty">
            <Sparkles size={26} strokeWidth={1.6} />
            <p>No terms yet. Add the names and jargon you use every day.</p>
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
                <span className="pill" title="Learned from your corrections">
                  auto
                </span>
              )}
            </div>
            <div className="dict-corrections">
              {e.corrections.length > 0 ? e.corrections.join(', ') : <span className="faint">fuzzy match</span>}
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
