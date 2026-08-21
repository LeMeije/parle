// The recording HUD: non-focus-stealing overlay. Waveform + streaming partial
// text + click-to-stop / cancel. Retro style renders spinning cassette reels.

import { useEffect, useRef, useState } from 'react';
import { api, onLevel, onPartial, onPipelineEvent } from './api';
import type { Settings } from './types';
import './hud.css';

const BAR_COUNT = 27;

export default function Hud() {
  const [state, setState] = useState<'recording' | 'transcribing' | 'idle'>('idle');
  const [partial, setPartial] = useState('');
  const [outcome, setOutcome] = useState<{ text: string; kind: 'ok' | 'error' } | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [settings, setSettings] = useState<Settings | null>(null);
  const barsRef = useRef<number[]>(new Array(BAR_COUNT).fill(0.05));
  const [, force] = useState(0);
  const envelopeRef = useRef(0);

  useEffect(() => {
    api.getSettings().then(applyTheme).then(setSettings).catch(() => {});
    const un1 = onPipelineEvent((e) => {
      if (e.kind === 'state_changed') {
        setState(e.state === 'idle' ? 'idle' : e.state);
        if (e.state === 'recording') {
          setPartial('');
          setOutcome(null);
          setElapsed(0);
          barsRef.current = new Array(BAR_COUNT).fill(0.05);
        }
      }
      if (e.kind === 'empty') setOutcome({ text: e.reason, kind: 'ok' });
      if (e.kind === 'error') setOutcome({ text: e.message, kind: 'error' });
      if (e.kind === 'completed' && e.injection?.manual_paste_required) {
        setOutcome({ text: 'Copied — press ⌘V to paste (secure field)', kind: 'ok' });
      }
      // Theme may have changed while the HUD was hidden.
      if (e.kind === 'state_changed' && e.state === 'recording') {
        api.getSettings().then(applyTheme).then(setSettings).catch(() => {});
      }
    });
    const un2 = onLevel((u) => {
      envelopeRef.current = u.envelope;
      setElapsed(u.elapsed_ms);
      const bars = barsRef.current;
      bars.push(Math.min(1, u.envelope * 5.5 + u.peak * 0.4));
      if (bars.length > BAR_COUNT) bars.shift();
      force((n) => n + 1);
    });
    const un3 = onPartial((text) => setPartial(text));
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
    };
  }, []);

  const style = settings?.overlay.style ?? 'pill';
  const showPartial = settings?.overlay.show_partial_text ?? true;
  const mm = Math.floor(elapsed / 60000);
  const ss = Math.floor((elapsed % 60000) / 1000);
  const time = `${mm}:${ss.toString().padStart(2, '0')}`;

  if (state === 'idle' && outcome) {
    return (
      <div className={`hud hud-${style} idle`}>
        <div className={`hud-outcome ${outcome.kind}`}>{outcome.text}</div>
      </div>
    );
  }

  return (
    <div className={`hud hud-${style} ${state}`}>
      {style === 'minimal' ? (
        <div
          className="hud-min-inner"
          title={state === 'recording' ? 'Recording — click to stop' : 'Transcribing…'}
          onClick={() => (state === 'recording' ? api.stopRecording() : undefined)}
        >
          {state === 'transcribing' ? <Spinner /> : <span className="hud-dot" />}
          <span className="hud-time">{time}</span>
          <button className="hud-cancel" title="Cancel (Esc)" onClick={(e) => { e.stopPropagation(); api.cancelRecording(); }}>
            ✕
          </button>
        </div>
      ) : style === 'cassette' ? (
        <Cassette recording={state === 'recording'} envelope={envelopeRef.current} time={time} />
      ) : (
        <>
          <button
            className="hud-stop"
            title={state === 'recording' ? 'Stop and paste' : 'Working…'}
            onClick={() => (state === 'recording' ? api.stopRecording() : undefined)}
          >
            {state === 'transcribing' ? <Spinner /> : <span className="hud-dot" />}
          </button>
          <div className="hud-center">
            {state === 'transcribing' ? (
              <span className="hud-status">Transcribing…</span>
            ) : (
              <Waveform bars={barsRef.current} />
            )}
            {showPartial && partial && <div className="hud-partial">{partial}</div>}
          </div>
          <div className="hud-right">
            <span className="hud-time">{time}</span>
            <button className="hud-cancel" title="Cancel (Esc)" onClick={() => api.cancelRecording()}>
              ✕
            </button>
          </div>
        </>
      )}
    </div>
  );
}

function Waveform({ bars }: { bars: number[] }) {
  return (
    <div className="wave">
      {bars.map((v, i) => (
        <div key={i} className="wave-bar" style={{ height: `${Math.max(6, v * 100)}%` }} />
      ))}
    </div>
  );
}

function Spinner() {
  return <span className="hud-spinner" />;
}

// Spinning cassette reels for the retro theme.
function Cassette({ recording, envelope, time }: { recording: boolean; envelope: number; time: string }) {
  return (
    <div className="cassette" onClick={() => recording && api.stopRecording()}>
      <div className={`reel ${recording ? 'spin' : ''}`}>
        <div className="reel-hub" />
        {[0, 60, 120, 180, 240, 300].map((d) => (
          <div key={d} className="reel-spoke" style={{ transform: `rotate(${d}deg)` }} />
        ))}
      </div>
      <div className="cassette-mid">
        <div className="vu">
          <div className="vu-fill" style={{ width: `${Math.min(100, envelope * 450)}%` }} />
        </div>
        <div className="cassette-label">
          {recording ? 'REC' : 'PROC'} <span className="cassette-time">{time}</span>
        </div>
      </div>
      <div className={`reel ${recording ? 'spin slow' : ''}`}>
        <div className="reel-hub" />
        {[0, 60, 120, 180, 240, 300].map((d) => (
          <div key={d} className="reel-spoke" style={{ transform: `rotate(${d}deg)` }} />
        ))}
      </div>
      <button
        className="hud-cancel cassette-cancel"
        title="Cancel (Esc)"
        onClick={(e) => {
          e.stopPropagation();
          api.cancelRecording();
        }}
      >
        ✕
      </button>
    </div>
  );
}

function applyTheme(s: Settings): Settings {
  const root = document.documentElement;
  root.dataset.palette = s.appearance.palette;
  const mode =
    s.appearance.theme_mode === 'system'
      ? window.matchMedia('(prefers-color-scheme: dark)').matches
        ? 'dark'
        : 'light'
      : s.appearance.theme_mode;
  root.dataset.mode = mode;
  root.dataset.reduceMotion = String(s.appearance.reduce_motion);
  root.style.setProperty('--accent', s.appearance.accent);
  return s;
}
