// The recording HUD: non-focus-stealing overlay. Waveform + streaming partial
// text + click-to-stop / cancel. The cassette and metal styles render a deck:
// spinning reels either side of a segmented level meter.

import { useEffect, useRef, useState } from 'react';
import { api, onLevel, onPartial, onPipelineEvent } from './api';
import type { Settings } from './types';
import './hud.css';

const BAR_COUNT = 27;

// Deck meter: discrete segments rather than the pill's continuous bar.
const SEG_COUNT = 18;

// Reel cut-outs: three trapezes whose outer edge follows the rim as an arc.
// Fixed geometry against a 0 0 40 40 viewBox — do not redraw by hand.
const REEL_CUTS = [
  'M16.2,13.42 L8.2,8.61 A16.4,16.4 0 0 1 31.8,8.61 L23.8,13.42 A7.6,7.6 0 0 0 16.2,13.42 Z',
  'M27.6,20.0 L35.76,15.48 A16.4,16.4 0 0 1 23.97,35.91 L23.8,26.58 A7.6,7.6 0 0 0 27.6,20.0 Z',
  'M16.2,26.58 L16.03,35.91 A16.4,16.4 0 0 1 4.24,15.48 L12.4,20.0 A7.6,7.6 0 0 0 16.2,26.58 Z',
];

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
        setOutcome({ text: 'Copied. Press ⌘V to paste (secure field)', kind: 'ok' });
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
      // Perceptual mapping: speech RMS spans a narrow linear range, so a raw
      // mapping looks flat. dB scale (-44dB floor .. -12dB ceiling) with a
      // contrast power makes speech visibly dramatic against pauses.
      const db = 20 * Math.log10(Math.max(u.rms, 1e-6));
      // Floor -50 dB, ceiling -20 dB. The old window (-44..-12) put ordinary
      // speech near the bottom, and a 1.7 power then crushed what was left, so
      // the bars barely moved. A narrower window and a gentler curve put normal
      // speech across the middle of the range where it can actually be seen.
      let v = (db + 50) / 30;
      v = Math.pow(Math.max(0, Math.min(1, v)), 1.15);
      // A touch of peak keeps plosives snappy.
      const peakDb = 20 * Math.log10(Math.max(u.peak, 1e-6));
      const p = Math.max(0, Math.min(1, (peakDb + 46) / 32));
      bars.push(Math.min(1, v * 0.85 + p * 0.3));
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
  const isDeck = style === 'cassette' || style === 'metal';
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
    <div
      className={`hud hud-${style} ${state}`}
      onClick={isDeck && state === 'recording' ? () => api.stopRecording() : undefined}
    >
      {style === 'minimal' ? (
        <div
          className="hud-min-inner"
          title={state === 'recording' ? 'Recording. Click to stop' : 'Transcribing…'}
          onClick={() => (state === 'recording' ? api.stopRecording() : undefined)}
        >
          {state === 'transcribing' ? <Spinner /> : <span className="hud-dot" />}
          <span className="hud-time">{time}</span>
          <button className="hud-cancel" title="Cancel (Esc)" onClick={(e) => { e.stopPropagation(); api.cancelRecording(); }}>
            ✕
          </button>
        </div>
      ) : isDeck ? (
        <Deck
          variant={style === 'metal' ? 'metal' : 'cassette'}
          recording={state === 'recording'}
          envelope={envelopeRef.current}
          time={time}
        />
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

// Spinning reels either side of a segmented meter. Shared by the cassette
// (paper reels, racing stripe) and metal (graphite reels, orange hub) styles;
// the shell itself is drawn by the .hud-cassette / .hud-metal rules.
function Deck({
  variant,
  recording,
  envelope,
  time,
}: {
  variant: 'cassette' | 'metal';
  recording: boolean;
  envelope: number;
  time: string;
}) {
  // Same dB window as the pill waveform, quantised into discrete segments.
  const db = 20 * Math.log10(Math.max(envelope, 1e-6));
  const level = Math.min(1, Math.pow(Math.max(0, (db + 50) / 30), 1.15));
  const lit = Math.round(level * SEG_COUNT);
  return (
    <>
      <Reel variant={variant} spinning={recording} rewinding={!recording} />
      <div className="deck-mid">
        <div className="deck-seg">
          {Array.from({ length: SEG_COUNT }, (_, i) => {
            // Transcribing: there is no live level any more, so run an
            // indeterminate sweep instead of leaving the last frame frozen.
            if (!recording) {
              return (
                <i
                  key={i}
                  className={variant === 'metal' ? 'hot sweep' : 'lit sweep'}
                  style={{ animationDelay: `${i * 55}ms` }}
                />
              );
            }
            // Peak-meter colouring: on the cassette the top two lit segments
            // read hot; the metal meter is orange throughout.
            const tone =
              i >= lit ? '' : variant === 'metal' ? 'hot' : i >= lit - 2 ? 'warn' : 'lit';
            const flick = i === lit - 1 ? ' edge' : '';
            return <i key={i} className={tone + flick} />;
          })}
        </div>
        <div className={`deck-label${recording ? '' : ' working'}`}>
          {recording ? 'REC' : 'PROC'} <span className="deck-time">{time}</span>
        </div>
      </div>
      <Reel variant={variant} spinning={recording} rewinding={!recording} slow />
      {variant === 'cassette' && <span className="deck-stripe" />}
      <button
        className="deck-cancel"
        title="Cancel (Esc)"
        onClick={(e) => {
          e.stopPropagation();
          api.cancelRecording();
        }}
      >
        <svg viewBox="0 0 7 7" width="7" height="7" aria-hidden="true">
          <line x1="1.5" y1="1.5" x2="5.5" y2="5.5" />
          <line x1="5.5" y1="1.5" x2="1.5" y2="5.5" />
        </svg>
      </button>
    </>
  );
}

function Reel({
  variant,
  spinning,
  rewinding,
  slow,
}: {
  variant: 'cassette' | 'metal';
  spinning: boolean;
  rewinding?: boolean;
  slow?: boolean;
}) {
  const paper = variant === 'cassette';
  const motion = spinning ? ' spin' : rewinding ? ' rewind' : '';
  return (
    <svg
      className={`deck-reel${motion}${slow ? ' slow' : ''}`}
      viewBox="0 0 40 40"
      width="44"
      height="44"
      aria-hidden="true"
    >
      <circle cx="20" cy="20" r="19" fill={paper ? '#d9d6cf' : '#2f3237'} />
      {REEL_CUTS.map((d, i) => (
        <path key={i} d={d} fill={paper ? '#fbfbfa' : '#8f959c'} />
      ))}
      <circle cx="20" cy="20" r="6.2" fill={paper ? '#b9b5ac' : '#22252a'} />
      {!paper && <circle cx="20" cy="20" r="2.5" fill="#ff6a1f" />}
    </svg>
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
