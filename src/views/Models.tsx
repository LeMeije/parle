// Model manager: browse, download (with progress), delete, switch.

import { Fragment, useCallback, useEffect, useState } from 'react';
import { Check, Cpu, Download, FolderOpen, Gauge, Globe2, Target, Trash2, X } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { api, onDownloadCancelled, onDownloadComplete, onDownloadError, onDownloadProgress } from '../api';
import type { DownloadProgress, ModelRow } from '../types';
import { useT } from '../i18n/useT';

// Renders a translated sentence containing {tokens}, substituting React nodes
// at the tokens' positions. The whole sentence stays a single translatable
// string, so word order remains the translator's to choose rather than being
// fixed by concatenation here.
function rich(text: string, nodes: Record<string, React.ReactNode>): React.ReactNode {
  const out: React.ReactNode[] = [];
  const re = /\{(\w+)\}/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const node = nodes[m[1]];
    if (node === undefined) continue;
    if (m.index > last) out.push(text.slice(last, m.index));
    out.push(<Fragment key={out.length}>{node}</Fragment>);
    last = m.index + m[0].length;
  }
  out.push(text.slice(last));
  return <>{out}</>;
}

export default function ModelsView() {
  const t = useT();
  const [models, setModels] = useState<ModelRow[]>([]);
  const [progress, setProgress] = useState<Record<string, DownloadProgress>>({});

  async function addLocal() {
    try {
      const picked = await open({
        multiple: false,
        directory: false,
        title: t('models.picker.title'),
        filters: [{ name: t('models.picker.filter'), extensions: ['bin', 'gguf'] }],
      });
      if (typeof picked !== 'string') return;
      // The file name is the best default label, and the user can tell their
      // own files apart by it better than by anything we would invent.
      const name = picked.split(/[\\/]/).pop()?.replace(/\.(bin|gguf)$/i, '') ?? t('models.defaultLocalName');
      await api.addCustomModel(picked, name, true);
      refresh();
    } catch (e) {
      setError(String(e));
    }
  }
  const [error, setError] = useState<string | null>(null);
  const [engine, setEngine] = useState<{ loaded_model: string | null; warm: boolean } | null>(null);

  const refresh = useCallback(() => {
    api.listModels().then(setModels);
    api.engineStatus().then(setEngine);
  }, []);

  useEffect(() => {
    refresh();
    const un1 = onDownloadProgress((p) => setProgress((m) => ({ ...m, [p.model_id]: p })));
    const un2 = onDownloadComplete((id) => {
      setProgress((m) => {
        const n = { ...m };
        delete n[id];
        return n;
      });
      refresh();
    });
    const dropRow = (id: string) =>
      setProgress((m) => {
        const n = { ...m };
        delete n[id];
        return n;
      });
    const un3 = onDownloadError((msg) => {
      setError(msg);
      // Only the failed download's row. The payload is "<model_id>: <error>";
      // wiping every row punished the downloads that were still running.
      const id = msg.split(':')[0]?.trim();
      if (id) dropRow(id);
      refresh();
    });
    // A cancel is not an error: no callout, just the row going away.
    const un4 = onDownloadCancelled((id) => {
      dropRow(id);
      refresh();
    });
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
      un4.then((f) => f());
    };
  }, [refresh]);

  return (
    <div className="models">
      <header className="view-head">
        <h1>{t('models.title')}</h1>
        <p>
          {t('models.subtitle')}{' '}
          {engine?.warm ? (
            <span className="pill ok">{t('models.warm', { model: shortName(engine.loaded_model ?? '') })}</span>
          ) : (
            <span className="pill">{t('models.loadsOnFirstUse')}</span>
          )}
        </p>
      </header>
      {error && (
        <div className="callout error">
          {error} <button onClick={() => setError(null)}>{t('common.dismiss')}</button>
        </div>
      )}
      <div className="model-list">
        {models.map((m) => {
          const p = progress[m.id];
          const pct = p && p.total > 0 ? Math.round((p.downloaded / p.total) * 100) : 0;
          return (
            <div key={m.id} className={`model-card ${m.active ? 'active' : ''}`}>
              <div className="model-info">
                <div className="model-name">
                  {m.display_name}
                  {m.active && <span className="pill accent">{t('models.active')}</span>}
                </div>
                <div className="model-meta">
                  <span
                    className={`backend-pill ${m.backend.includes('CUDA') ? 'cuda' : m.backend.includes('Metal') ? 'metal' : 'cpu'}`}
                    title={t('models.backendTitle')}
                  >
                    <Cpu size={12} strokeWidth={2.2} /> {m.backend}
                  </span>
                  <span>{formatBytes(m.size_bytes)}</span>
                  {/* No invented ratings for a file we did not publish. */}
                  {m.custom ? (
                    <span className="pill">{t('models.yourFile')}</span>
                  ) : (
                    <>
                      <Rating icon={<Gauge size={13} />} title={t('models.speedRating', { value: m.speed })} value={m.speed} />
                      <Rating icon={<Target size={13} />} title={t('models.accuracyRating', { value: m.accuracy })} value={m.accuracy} />
                    </>
                  )}
                  {m.multilingual && (
                    <span className="model-langs">
                      <Globe2 size={13} /> {t('models.languageCount', { n: m.id.startsWith('parakeet') ? 25 : 99 })}
                    </span>
                  )}
                </div>
              </div>
              <div className="model-actions">
                {p ? (
                  <>
                    <div className="dl-progress">
                      <div className="dl-bar" style={{ width: `${pct}%` }} />
                      <span>{pct}%</span>
                    </div>
                    <button className="icon-btn" title={t('common.cancel')} onClick={() => api.cancelDownload(m.id).then(refresh)}>
                      <X size={15} />
                    </button>
                  </>
                ) : m.downloaded ? (
                  <>
                    {!m.active && (
                      <button className="btn" onClick={() => api.selectModel(m.id).then(refresh)}>
                        <Check size={14} /> {t('models.use')}
                      </button>
                    )}
                    {!m.active && (
                      <button
                        className="icon-btn danger"
                        // A custom model is the user's OWN file, somewhere they
                        // chose. Removing it here forgets the entry; deleting
                        // the file is not ours to do.
                        title={m.custom ? t('models.removeCustom') : t('models.deleteFile')}
                        onClick={() =>
                          (m.custom ? api.removeCustomModel(m.id) : api.deleteModel(m.id))
                            .then(refresh)
                            .catch((e) => setError(String(e)))
                        }
                      >
                        <Trash2 size={15} />
                      </button>
                    )}
                  </>
                ) : m.custom ? (
                  // Not downloadable: it is a local file that has gone missing.
                  <>
                    <span className="pill warn">{t('models.fileMissing')}</span>
                    <button
                      className="icon-btn danger"
                      title={t('models.removeFromList')}
                      onClick={() => api.removeCustomModel(m.id).then(refresh).catch((e) => setError(String(e)))}
                    >
                      <Trash2 size={15} />
                    </button>
                  </>
                ) : (
                  <button className="btn" onClick={() => api.downloadModel(m.id)}>
                    <Download size={14} /> {t('models.download')}
                  </button>
                )}
              </div>
            </div>
          );
        })}
      </div>
      <div className="model-add">
        <button className="btn" onClick={addLocal}>
          <FolderOpen size={14} /> {t('models.addLocal')}
        </button>
        <span className="hint">{rich(t('models.addLocal.hint'), { ext: <code>.bin</code> })}</span>
      </div>
      <p className="hint">{t('models.fallbackHint')}</p>
    </div>
  );
}

function Rating({ icon, title, value }: { icon: React.ReactNode; title: string; value: number }) {
  return (
    <span className="rating" title={title}>
      {icon}
      {[1, 2, 3, 4, 5].map((i) => (
        <i key={i} className={i <= value ? 'on' : ''} />
      ))}
    </span>
  );
}

function formatBytes(b: number): string {
  if (b >= 1_000_000_000) return `${(b / 1_000_000_000).toFixed(1)} GB`;
  return `${Math.round(b / 1_000_000)} MB`;
}

function shortName(id: string): string {
  return id.replace('whisper-', '');
}
