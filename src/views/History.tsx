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
  // A row can vanish underneath an open list: a delete made on a paired device
  // arrives over sync and removes it from the store. Every action below then
  // rejects with "not found", and each used to be a bare promise with no
  // `.catch`, so the click did nothing at all, silently, for ever. Say so and
  // reload rather than leaving the user pressing a dead button.
  const [gone, setGone] = useState(false);
  // Names for the delete confirmation. A delete travels and is absorbing on the
  // peer, so the confirmation has to be able to say WHERE it travels to.
  const [pairedNames, setPairedNames] = useState<string[]>([]);
  useEffect(() => {
    api
      .syncStatus()
      .then((st) => setPairedNames(st.enabled ? st.paired.map((d) => d.name) : []))
      .catch(() => setPairedNames([]));
  }, []);

  function confirmDelete(item: HistoryItem): boolean {
    const where =
      pairedNames.length > 0 && !item.local_only
        ? `\n\nThis also deletes it from ${pairedNames.join(', ')}.`
        : '';
    return window.confirm(`Delete this item? It cannot be undone.${where}`);
  }
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

  // Any action on a row that is no longer there. The refresh is the important
  // half: the list is stale by definition if we got here.
  const vanished = useCallback(() => {
    setGone(true);
    refresh();
    window.setTimeout(() => setGone(false), 4000);
  }, [refresh]);

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
      // Enter pastes into the previous app (the window hides first so focus
      // returns there); Cmd/Ctrl+Enter copies only.
      if (e.metaKey || e.ctrlKey) {
        api.copyItem(items[selected].id);
      } else {
        api.pasteItem(items[selected].id);
      }
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
          placeholder="Search transcriptions and clipboard…  (↑↓ · Enter pastes · ⌘Enter copies)"
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

      {gone && (
        <div className="callout warn">
          That item is no longer here. It was deleted on another device, so the list has been
          refreshed.
        </div>
      )}

      <div className="history-list" ref={listRef}>
        {items.length === 0 && (
          <div className="empty">
            <Mic size={28} strokeWidth={1.6} />
            <p>{query ? 'No matches.' : 'Nothing here yet. Hold your hotkey and speak.'}</p>
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
              <div className="row-footer">
                <div className="row-meta">
                  <span>{timeAgo(item.created_at)}</span>
                  {/* `?? app_id`: the Windows probe returns the exe name in
                      `app_id` and hard-codes the display slot to null, so a
                      Windows-authored row showed no application at all, on both
                      machines, in the same list as Mac rows that showed one. */}
                  {(item.app_name ?? item.app_id) && <span>· {item.app_name ?? item.app_id}</span>}
                  {/* A row that will never reach the other machine says so on a
                      feature whose whole promise is that it does. */}
                  {item.local_only && (
                    <span className="badge" title="Parle could not rule out that this was a password field, so it is kept on this device and never sent to your other devices">
                      this device only
                    </span>
                  )}
                  {item.duration_ms != null && <span>· {(item.duration_ms / 1000).toFixed(1)}s</span>}
                  {item.model_id && <span>· {shortModel(item.model_id)}</span>}
                  {item.language && item.kind === 'transcription' && (
                    <span className="badge badge-lang">{item.language}</span>
                  )}
                  <TrimBadge item={item} onRestored={refresh} />
                </div>
                <div className="row-actions">
                  <button className="cta" title="Paste into the previous app (Enter)" onClick={(e) => { e.stopPropagation(); api.pasteItem(item.id).catch(vanished); }}>
                    <CornerDownLeft size={14} /> Paste
                  </button>
                  <button className="cta" title="Copy (⌘Enter)" onClick={(e) => { e.stopPropagation(); api.copyItem(item.id).catch(vanished); }}>
                    <Copy size={14} /> Copy
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
                  <button title={item.pinned ? 'Unpin' : 'Pin'} onClick={(e) => { e.stopPropagation(); api.pinItem(item.id, !item.pinned).then(refresh).catch(vanished); }}>
                    {item.pinned ? <PinOff size={14} /> : <Pin size={14} />}
                  </button>
                  {/* Confirmed, because a delete TRAVELS.
                      Clearing all history is guarded by a callout naming every
                      paired device; deleting one row, which is equally
                      irreversible and equally absorbing on the peer, was a
                      single unguarded click. The capability existed and had
                      simply not been applied to the riskier action. */}
                  <button className="danger" title="Delete" onClick={(e) => { e.stopPropagation(); if (!confirmDelete(item)) return; api.deleteItem(item.id).then(refresh).catch(vanished); }}>
                    <Trash2 size={14} />
                  </button>
                </div>
              </div>
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
        <button className="badge badge-trim" onClick={(e) => { e.stopPropagation(); setOpen(!open); }}>
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
            className="badge badge-trim"
            onClick={() => {
              api.updateItemText(item.id, item.raw_text!, false).then(() => {
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
