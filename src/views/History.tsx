// Unified history: transcriptions + clipboard. Fuzzy search, keyboard-driven,
// pin/edit/copy/paste, trimmed-span review with one-click restore.

import { useCallback, useEffect, useRef, useState } from 'react';
import {
  ClipboardList,
  Copy,
  CornerDownLeft,
  Mic,
  Pencil,
  Pin,
  PinOff,
  Trash2,
} from 'lucide-react';
import { api, onFocusPalette, onHistoryChanged, onPipelineEvent } from '../api';
import type { HistoryItem } from '../types';

type Filter = 'all' | 'transcription' | 'clipboard';

export default function HistoryView() {
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [items, setItems] = useState<HistoryItem[]>([]);
  const [selected, setSelected] = useState(0);
  const [editing, setEditing] = useState<number | null>(null);
  const [editText, setEditText] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const refresh = useCallback(() => {
    const kind = filter === 'all' ? undefined : filter;
    api.searchHistory(query, kind).then((r) => {
      setItems(r);
      setSelected((s) => Math.min(s, Math.max(0, r.length - 1)));
    });
  }, [query, filter]);

  useEffect(refresh, [refresh]);

  useEffect(() => {
    const un1 = onHistoryChanged(refresh);
    const un2 = onPipelineEvent((e) => {
      if (e.kind === 'completed') refresh();
    });
    const un3 = onFocusPalette(() => inputRef.current?.focus());
    inputRef.current?.focus();
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
    };
  }, [refresh]);

  function onKeyDown(e: React.KeyboardEvent) {
    if (editing !== null) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setSelected((s) => Math.min(s + 1, items.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setSelected((s) => Math.max(s - 1, 0));
    } else if (e.key === 'Enter' && items[selected]) {
      e.preventDefault();
      api.copyItem(items[selected].id);
    }
  }

  useEffect(() => {
    listRef.current
      ?.querySelector(`[data-index="${selected}"]`)
      ?.scrollIntoView({ block: 'nearest' });
  }, [selected]);

  return (
    <div className="history" onKeyDown={onKeyDown}>
      <div className="history-top">
        <input
          ref={inputRef}
          className="search"
          placeholder="Search transcriptions and clipboard…  (↑↓ then Enter copies)"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoFocus
        />
        <div className="seg">
          {(['all', 'transcription', 'clipboard'] as Filter[]).map((f) => (
            <button key={f} className={filter === f ? 'active' : ''} onClick={() => setFilter(f)}>
              {f === 'all' ? 'All' : f === 'transcription' ? 'Dictations' : 'Clipboard'}
            </button>
          ))}
        </div>
      </div>

      <div className="history-list" ref={listRef}>
        {items.length === 0 && (
          <div className="empty">
            <Mic size={28} strokeWidth={1.6} />
            <p>{query ? 'No matches.' : 'Nothing here yet — hold your hotkey and speak.'}</p>
          </div>
        )}
        {items.map((item, i) => (
          <div
            key={item.id}
            data-index={i}
            className={`row ${i === selected ? 'selected' : ''} ${item.pinned ? 'pinned' : ''}`}
            onClick={() => setSelected(i)}
            onDoubleClick={() => api.copyItem(item.id)}
          >
            <div className="row-icon">
              {item.kind === 'transcription' ? <Mic size={15} /> : <ClipboardList size={15} />}
            </div>
            <div className="row-body">
              {editing === item.id ? (
                <textarea
                  className="row-edit"
                  value={editText}
                  autoFocus
                  onChange={(e) => setEditText(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                      api.updateItemText(item.id, editText).then(() => {
                        setEditing(null);
                        refresh();
                      });
                    }
                    if (e.key === 'Escape') setEditing(null);
                  }}
                  onBlur={() => setEditing(null)}
                />
              ) : (
                <div className="row-text">{item.text}</div>
              )}
              <div className="row-meta">
                <span>{timeAgo(item.created_at)}</span>
                {item.app_name && <span>· {item.app_name}</span>}
                {item.duration_ms != null && <span>· {(item.duration_ms / 1000).toFixed(1)}s</span>}
                {item.model_id && <span>· {shortModel(item.model_id)}</span>}
                <TrimBadge item={item} onRestored={refresh} />
              </div>
            </div>
            <div className="row-actions">
              <button title="Paste into the previous app" onClick={(e) => { e.stopPropagation(); api.pasteItem(item.id); }}>
                <CornerDownLeft size={14} />
              </button>
              <button title="Copy" onClick={(e) => { e.stopPropagation(); api.copyItem(item.id); }}>
                <Copy size={14} />
              </button>
              <button
                title="Edit (feeds auto-learn)"
                onClick={(e) => {
                  e.stopPropagation();
                  setEditing(item.id);
                  setEditText(item.text);
                }}
              >
                <Pencil size={14} />
              </button>
              <button title={item.pinned ? 'Unpin' : 'Pin'} onClick={(e) => { e.stopPropagation(); api.pinItem(item.id, !item.pinned).then(refresh); }}>
                {item.pinned ? <PinOff size={14} /> : <Pin size={14} />}
              </button>
              <button className="danger" title="Delete" onClick={(e) => { e.stopPropagation(); api.deleteItem(item.id).then(refresh); }}>
                <Trash2 size={14} />
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/// Shows "n trimmed" for transcriptions whose cleanup removed spans; click to
/// review and restore the raw transcript.
function TrimBadge({ item, onRestored }: { item: HistoryItem; onRestored: () => void }) {
  const [open, setOpen] = useState(false);
  if (item.kind !== 'transcription' || !item.meta || !item.raw_text) return null;
  let trimmed: { text: string; reason: string }[] = [];
  let lowConf = 0;
  try {
    const meta = JSON.parse(item.meta);
    trimmed = meta.trimmed ?? [];
    lowConf = (meta.low_confidence ?? []).length;
  } catch {
    return null;
  }
  if (trimmed.length === 0 && lowConf === 0) return null;
  return (
    <>
      {trimmed.length > 0 && (
        <button className="badge" onClick={(e) => { e.stopPropagation(); setOpen(!open); }}>
          {trimmed.length} trimmed
        </button>
      )}
      {lowConf > 0 && <span className="badge badge-warn">{lowConf} unsure</span>}
      {open && (
        <span className="trim-review" onClick={(e) => e.stopPropagation()}>
          {trimmed.map((t, i) => (
            <s key={i}>{t.text}</s>
          ))}
          <button
            className="badge"
            onClick={() => {
              api.updateItemText(item.id, item.raw_text!).then(() => {
                setOpen(false);
                onRestored();
              });
            }}
          >
            Restore raw
          </button>
        </span>
      )}
    </>
  );
}

function timeAgo(ms: number): string {
  const s = Math.floor((Date.now() - ms) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  const d = new Date(ms);
  return d.toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
}

function shortModel(id: string): string {
  return id.replace('whisper-', '').replace(/-q\d.*/, '');
}
