// Model manager: browse, download (with progress), delete, switch.

import { useCallback, useEffect, useState } from 'react';
import { Check, Download, Gauge, Globe2, Target, Trash2, X } from 'lucide-react';
import { api, onDownloadComplete, onDownloadError, onDownloadProgress } from '../api';
import type { DownloadProgress, ModelRow } from '../types';

export default function ModelsView() {
  const [models, setModels] = useState<ModelRow[]>([]);
  const [progress, setProgress] = useState<Record<string, DownloadProgress>>({});
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
    const un3 = onDownloadError((msg) => {
      setError(msg);
      setProgress({});
      refresh();
    });
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
    };
  }, [refresh]);

  return (
    <div className="models">
      <header className="view-head">
        <h1>Models</h1>
        <p>
          All transcription runs on this device.{' '}
          {engine?.warm ? (
            <span className="pill ok">Warm · {shortName(engine.loaded_model ?? '')}</span>
          ) : (
            <span className="pill">Model loads on first use</span>
          )}
        </p>
      </header>
      {error && (
        <div className="callout error">
          {error} <button onClick={() => setError(null)}>Dismiss</button>
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
                  {m.active && <span className="pill accent">Active</span>}
                </div>
                <div className="model-meta">
                  <span className="pill">{m.backend}</span>
                  <span>{formatBytes(m.size_bytes)}</span>
                  <Rating icon={<Gauge size={13} />} label="speed" value={m.speed} />
                  <Rating icon={<Target size={13} />} label="accuracy" value={m.accuracy} />
                  {m.multilingual && (
                    <span className="model-langs">
                      <Globe2 size={13} /> {m.id.startsWith('parakeet') ? '25 languages' : '99 languages'}
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
                    <button className="icon-btn" title="Cancel" onClick={() => api.cancelDownload(m.id).then(refresh)}>
                      <X size={15} />
                    </button>
                  </>
                ) : m.downloaded ? (
                  <>
                    {!m.active && (
                      <button className="btn" onClick={() => api.selectModel(m.id).then(refresh)}>
                        <Check size={14} /> Use
                      </button>
                    )}
                    {!m.active && (
                      <button
                        className="icon-btn danger"
                        title="Delete model file"
                        onClick={() => api.deleteModel(m.id).then(refresh).catch((e) => setError(String(e)))}
                      >
                        <Trash2 size={15} />
                      </button>
                    )}
                  </>
                ) : (
                  <button className="btn" onClick={() => api.downloadModel(m.id)}>
                    <Download size={14} /> Download
                  </button>
                )}
              </div>
            </div>
          );
        })}
      </div>
      <p className="hint">
        If the active model fails to load (for example under memory pressure), Parle automatically falls
        back down the ladder — your recording is never lost.
      </p>
    </div>
  );
}

function Rating({ icon, label, value }: { icon: React.ReactNode; label: string; value: number }) {
  return (
    <span className="rating" title={`${label} ${value}/5`}>
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
