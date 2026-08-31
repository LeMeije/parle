// Compose: dictate in the app with a live waveform, and watch what you have
// pinned into the recording build up. Each insert is tied to the audio
// timestamp and spliced verbatim into the final text exactly where you were
// speaking.
//
// The insert box itself is NOT here any more: it lives in the dictation bar
// (src/DictationBar.tsx), which floats over every tab so a paste never costs a
// trip back to this screen. This view is the detailed record of what that box
// has done, plus the transcript when it lands.

import { useEffect, useRef, useState } from 'react';
import { Mic, Square } from 'lucide-react';
import { api, onLevel, onPartial, onPipelineEvent } from '../api';
import { fmtTime } from '../format';
import { useT } from '../i18n/useT';
import type { Mark } from '../types';

const BAR_COUNT = 48;

/// `marks` is owned by App: it has to outlive this view being unmounted every
/// time you look at another tab.
export default function ComposeView({ marks }: { marks: Mark[] }) {
  const t = useT();
  const [state, setState] = useState<'idle' | 'recording' | 'transcribing'>('idle');
  const [elapsed, setElapsed] = useState(0);
  const [partial, setPartial] = useState('');
  const [result, setResult] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const barsRef = useRef<number[]>(new Array(BAR_COUNT).fill(0.04));
  const [, force] = useState(0);

  useEffect(() => {
    // A hotkey may have started recording before this view mounted.
    api.pipelineState().then((st) => {
      if (st === 'recording') setState('recording');
    });
    const un1 = onPipelineEvent((e) => {
      if (e.kind === 'state_changed') {
        setState(e.state);
        if (e.state === 'recording') {
          setResult(null);
          setPartial('');
          setError(null);
          setElapsed(0);
          barsRef.current = new Array(BAR_COUNT).fill(0.04);
        }
      }
      // `withheld` first: the event is broadcast to EVERY window, so a
      // password-field dictation aimed at a browser lands here too, and this
      // view paints the result in full and offers a Copy button for it.
      // `null`, not `''`. Empty string is already the sentinel this view
      // renders as "No speech detected", so a dictation Parle heard perfectly
      // and withheld on purpose was reported as a hardware failure.
      if (e.kind === 'completed' && e.withheld) setResult(null);
      else if (e.kind === 'completed') setResult(e.text);
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

  const time = fmtTime(elapsed);
  const recording = state === 'recording';

  return (
    <div className="compose">
      <header className="view-head">
        <h1>{t('compose.title')}</h1>
        <p>{t('compose.intro')}</p>
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
            {recording
              ? t('compose.stop')
              : state === 'transcribing'
                ? t('compose.transcribing')
                : t('compose.start')}
          </button>
          <span className="compose-time">{time}</span>
          {recording && <span className="pill accent">{t('compose.recording')}</span>}
          {state === 'transcribing' && <span className="pill">{t('compose.processing')}</span>}
        </div>

        {recording && partial && <div className="compose-partial">{partial}</div>}

        <p className="mark-note faint">
          {recording ? t('compose.barActive') : t('compose.markPlaceholder.idle')}
        </p>

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
            {result || t('compose.noSpeech')}
          </div>
        )}
        {result && (
          <div className="compose-controls">
            <button className="btn" onClick={() => navigator.clipboard.writeText(result)}>
              {t('compose.copyResult')}
            </button>
            <span className="faint" style={{ fontSize: 12 }}>
              {t('compose.alsoInserted')}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
