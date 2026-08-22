// Compose: dictate in the app with a live waveform, and paste/type content
// mid-recording. Each insert is pinned to the audio timestamp and spliced
// verbatim into the final text exactly where you were speaking.

import { useEffect, useRef, useState } from 'react';
import { Link2, Mic, Square } from 'lucide-react';
import { api, onLevel, onPartial, onPipelineEvent } from '../api';

const BAR_COUNT = 48;

interface Mark {
  at_ms: number;
  text: string;
}

export default function ComposeView() {
  const [state, setState] = useState<'idle' | 'recording' | 'transcribing'>('idle');
  const [elapsed, setElapsed] = useState(0);
  const [partial, setPartial] = useState('');
  const [marks, setMarks] = useState<Mark[]>([]);
  const [result, setResult] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [error, setError] = useState<string | null>(null);
  const barsRef = useRef<number[]>(new Array(BAR_COUNT).fill(0.04));
  const [, force] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    // A hotkey may have started recording before this view mounted.
    api.pipelineState().then((st) => {
      if (st === 'recording') setState('recording');
    });
    const un1 = onPipelineEvent((e) => {
      if (e.kind === 'state_changed') {
        setState(e.state);
        if (e.state === 'recording') {
          setMarks([]);
          setResult(null);
          setPartial('');
          setError(null);
          setElapsed(0);
          barsRef.current = new Array(BAR_COUNT).fill(0.04);
          inputRef.current?.focus();
        }
      }
      if (e.kind === 'mark_added') setMarks((m) => [...m, { at_ms: e.at_ms, text: e.text }]);
      if (e.kind === 'completed') setResult(e.text);
      if (e.kind === 'empty') setResult('');
      if (e.kind === 'error') setError(e.message);
    });
    const un2 = onLevel((u) => {
      setElapsed(u.elapsed_ms);
      const db = 20 * Math.log10(Math.max(u.rms, 1e-6));
      let v = Math.pow(Math.max(0, Math.min(1, (db + 44) / 32)), 1.7);
      const bars = barsRef.current;
      bars.push(v);
      if (bars.length > BAR_COUNT) bars.shift();
      force((n) => n + 1);
    });
    const un3 = onPartial(setPartial);
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
    };
  }, []);

  async function insert(text: string) {
    const t = text.trim();
    if (!t) return;
    try {
      await api.insertMark(t);
      setDraft('');
    } catch (e) {
      setError(String(e));
    }
  }

  const time = fmtTime(elapsed);
  const recording = state === 'recording';

  return (
    <div className="compose">
      <header className="view-head">
        <h1>Compose</h1>
        <p>
          Dictate here and paste links or text mid-sentence — each insert is pinned to the exact moment
          you added it and spliced into the final text, byte-exact.
        </p>
      </header>

      <div className="compose-stage">
        <div className="compose-wave">
          {barsRef.current.map((v, i) => (
            <div key={i} className="wave-bar" style={{ height: `${Math.max(4, v * 100)}%` }} />
          ))}
        </div>

        <div className="compose-controls">
          <button
            className={`btn ${recording ? 'danger' : 'primary'}`}
            onClick={() => (recording ? api.stopRecording() : api.startRecording())}
            disabled={state === 'transcribing'}
          >
            {recording ? <Square size={14} /> : <Mic size={14} />}
            {recording ? 'Stop' : state === 'transcribing' ? 'Transcribing…' : 'Start dictation'}
          </button>
          <span className="compose-time">{time}</span>
          {recording && <span className="pill accent">recording</span>}
          {state === 'transcribing' && <span className="pill">processing</span>}
        </div>

        {recording && partial && <div className="compose-partial">{partial}</div>}

        <div className="mark-input">
          <input
            ref={inputRef}
            placeholder={recording ? 'Paste a link or type, Enter pins it to this moment…' : 'Start dictating to insert links and text'}
            value={draft}
            disabled={!recording}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && insert(draft)}
            onPaste={(e) => {
              const text = e.clipboardData.getData('text');
              if (text.trim()) {
                e.preventDefault();
                insert(text);
              }
            }}
          />
          <button className="btn" disabled={!recording || !draft.trim()} onClick={() => insert(draft)}>
            <Link2 size={14} /> Insert
          </button>
        </div>

        {marks.length > 0 && (
          <div className="mark-chips">
            {marks.map((m, i) => (
              <div key={i} className="mark-chip">
                <span className="mark-time">{fmtTime(m.at_ms)}</span>
                <span className="mark-text">{m.text}</span>
              </div>
            ))}
          </div>
        )}

        {error && <div className="callout error">{error}</div>}
        {result !== null && (
          <div className="compose-result">
            {result || 'No speech detected.'}
          </div>
        )}
        {result && (
          <div className="compose-controls">
            <button className="btn" onClick={() => navigator.clipboard.writeText(result)}>
              Copy result
            </button>
            <span className="faint" style={{ fontSize: 12 }}>
              Also inserted at your cursor and saved to History.
            </span>
          </div>
        )}
      </div>
    </div>
  );
}

function fmtTime(ms: number): string {
  const mm = Math.floor(ms / 60000);
  const ss = Math.floor((ms % 60000) / 1000);
  return `${mm}:${ss.toString().padStart(2, '0')}`;
}
