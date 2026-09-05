// The dictation bar.
//
// While a recording runs, the sidebar's record button stows itself and this
// rises from the bottom of the window carrying stop, the elapsed clock, the
// level, and the insert box that used to live only inside Compose.
//
// It sits at App level on purpose. The whole point of the feature is that
// copying something and pinning it into the transcript never costs a tab
// switch, so the box has to exist on History, Models, Settings, anywhere. It
// deliberately does NOT show the marks already inserted: that detail belongs
// in Compose, and the count here is a button that goes there.
//
// Pasting no longer inserts by itself. A paste fills the box and waits for
// Enter, because clipboard text usually arrives with quote marks, a tracking
// suffix or a stray line break, and a mark is spliced into the transcript
// verbatim. The old behaviour gave nowhere to fix that.
//
// "Stows itself" is literal: the bar morphs out of the record button's own
// rectangle and collapses back into it, FLIP-style (measure both boxes, run
// the delta as one transform). Which is why the bar is a child of `.app` and
// not of the content stage: half of that movement happens over the sidebar,
// and a stage that clips its own overflow would cut it in two.

import { useEffect, useRef, useState } from 'react';
import { ArrowUpRight, Link2, Square } from 'lucide-react';
import { api, onLevel } from './api';
import { fmtTime } from './format';
import { useT } from './i18n/useT';
import type { Mark } from './types';

const BAR_COUNT = 16;

export default function DictationBar({
  recording,
  marks,
  originRef,
  onOpenCompose,
  accent,
  refine,
}: {
  recording: boolean;
  /// Everything pinned into the recording so far, owned by App. The bar shows
  /// the count and the way through to Compose, never the list.
  marks: Mark[];
  /// The sidebar record button: the rectangle this grows out of.
  originRef: React.RefObject<HTMLButtonElement | null>;
  onOpenCompose: () => void;
  /// Overrides the accent for the bar's own subtree (tint, meter, buttons)
  /// while a Refine take runs. Undefined keeps the app accent.
  accent?: string;
  /// The take will go to the AI: the bar says so next to the clock.
  refine?: boolean;
}) {
  const t = useT();
  const [draft, setDraft] = useState('');
  const [elapsed, setElapsed] = useState(0);
  const [pinnedAt, setPinnedAt] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const boxRef = useRef<HTMLTextAreaElement>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const firstRender = useRef(true);
  const morphRef = useRef<Animation | null>(null);
  const tintRef = useRef<HTMLDivElement>(null);
  // True for the length of a morph. The level meter stops asking React to
  // re-render while it is set: those renders land in the middle of the
  // animation, and the meter is faded out for all of it anyway.
  const morphing = useRef(false);
  const lastElapsed = useRef(0);
  const barsRef = useRef<number[]>(new Array(BAR_COUNT).fill(0.04));
  const [, force] = useState(0);

  // Subscribed for the life of the window, not per recording: a hotkey can
  // start recording before any effect keyed on `recording` has run, and levels
  // only arrive while a recording exists anyway.
  useEffect(() => {
    const un = onLevel((u) => {
      const db = 20 * Math.log10(Math.max(u.rms, 1e-6));
      const v = Math.pow(Math.max(0, Math.min(1, (db + 44) / 32)), 1.7);
      const bars = barsRef.current;
      bars.push(v);
      if (bars.length > BAR_COUNT) bars.shift();
      lastElapsed.current = u.elapsed_ms;
      // The samples are still collected mid-morph, only the paint is deferred,
      // so the meter is correct and current the moment it becomes visible.
      if (morphing.current) return;
      setElapsed(u.elapsed_ms);
      force((n) => n + 1);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // Confirming an insert is a flash on the count, not a line of its own: the
  // bar has no room to say much and Compose says it properly.
  const lastMark = marks.length > 0 ? marks[marks.length - 1].at_ms : null;
  useEffect(() => {
    if (lastMark === null) return;
    setPinnedAt(lastMark);
    // `marks.length` is in the deps as well as the timestamp: two inserts
    // inside the same millisecond are one unchanged `lastMark`.
    const id = window.setTimeout(() => setPinnedAt(null), 2400);
    return () => window.clearTimeout(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lastMark, marks.length]);

  useEffect(() => {
    if (error === null) return;
    const id = window.setTimeout(() => setError(null), 5000);
    return () => window.clearTimeout(id);
  }, [error]);

  // The morph. Both shapes are rounded rectangles, so one transform from the
  // button's box to the bar's box (and back) is the whole trick. Measured with
  // `offsetLeft/Top/Width/Height`, not `getBoundingClientRect`: both elements
  // share `.app` as their offset parent and those numbers ignore the very
  // transforms being animated, so the reverse direction measures a real box
  // rather than the parked one.
  useEffect(() => {
    const bar = barRef.current;
    const inner = innerRef.current;
    const origin = originRef.current;
    // Nothing to morph FROM on first paint: a window opened while a hotkey
    // recording is already running just shows the bar.
    const skip = firstRender.current;
    firstRender.current = false;
    if (skip || !bar || !inner) return;

    // `data-morph` first: cancelling the previous morph below changes the
    // computed transform, and this is what stops that change from starting a
    // CSS transition of its own.
    bar.dataset.morph = 'true';
    // A half-finished morph in the other direction has to go, and a closed
    // bar's morph is still holding its last frame (`fill: 'forwards'`).
    morphRef.current?.cancel();
    morphRef.current = null;

    // Unconditionally BEFORE the bail-outs, both of them. A closed bar is held
    // shut by a filled animation, so returning here without clearing it (mid
    // session reduced-motion switch, or a missing origin) left the bar unable
    // to open at all: invisible, collapsed, and inert to boot.
    if (!origin || document.documentElement.dataset.reduceMotion === 'true') {
      delete bar.dataset.morph;
      return;
    }

    const dx = origin.offsetLeft - bar.offsetLeft;
    const dy = origin.offsetTop - bar.offsetTop;
    const sx = Math.max(origin.offsetWidth / bar.offsetWidth, 0.05);
    const sy = Math.max(origin.offsetHeight / bar.offsetHeight, 0.05);

    // TRANSFORM AND OPACITY ONLY, deliberately. The first version of this
    // animated `border-radius` and `background-color` as well, which are not
    // compositable: every frame repainted a full-width element that carries a
    // large soft shadow, the frames could not be delivered, and a 320ms
    // expansion arrived in three visible jumps. The accent fill now lives on
    // its own overlay (`.bar-tint`) that fades out instead, which is the same
    // effect on the compositor's terms.
    const shut = { transform: `translate(${dx}px, ${dy}px) scale(${sx}, ${sy})`, opacity: '1' };
    const open = { transform: 'none', opacity: '1' };

    // The contents cross-fade instead of stretching with the box: a bar scaled
    // to a fifth of its width has visibly squashed text in it otherwise.
    const shapeFrames = recording
      ? [shut, open]
      // Ending at opacity 0 matters. The CSS resting state for a closed bar is
      // parked below the fold, and letting the transition to it run after the
      // collapse gave a second, slower phase: a collapsed pill sliding down
      // and fading, which read as lag. `fill: 'forwards'` holds this last
      // frame instead, so the collapse IS the whole animation.
      : [open, { ...shut, offset: 0.82 }, { ...shut, opacity: '0' }];
    const fadeFrames = recording
      ? [{ opacity: '0', offset: 0 }, { opacity: '0', offset: 0.5 }, { opacity: '1', offset: 1 }]
      : [{ opacity: '1', offset: 0 }, { opacity: '0', offset: 0.4 }, { opacity: '0', offset: 1 }];
    // The button's blue, on top, clearing as the shape opens out.
    const tintFrames = recording
      ? [{ opacity: '1', offset: 0 }, { opacity: '0', offset: 0.65 }, { opacity: '0', offset: 1 }]
      : [{ opacity: '0', offset: 0 }, { opacity: '1', offset: 0.7 }, { opacity: '1', offset: 1 }];

    // Long enough to read as one continuous expansion. Under about half a
    // second the eye samples a fast, wide movement as a few discrete positions
    // rather than a movement, however many frames actually land.
    const duration = recording ? 520 : 360;
    // Plain ease-out both ways. The spring that was here overshot the final
    // width and then settled back, which added its own second beat to
    // something already being read as steppy.
    const easing = recording
      ? 'cubic-bezier(0.16, 0.68, 0.18, 1)'
      : 'cubic-bezier(0.45, 0.05, 0.4, 1)';

    morphing.current = true;
    const shape = bar.animate(shapeFrames, {
      duration,
      easing,
      fill: recording ? 'none' : 'forwards',
    });
    morphRef.current = shape;
    inner.animate(fadeFrames, { duration, easing: 'linear' });
    tintRef.current?.animate(tintFrames, { duration, easing: 'linear' });
    shape.finished
      .catch(() => {})
      .finally(() => {
        delete bar.dataset.morph;
        morphing.current = false;
        // Catch the meter and the clock up on whatever arrived while they were
        // holding still.
        setElapsed(lastElapsed.current);
        force((n) => n + 1);
      });
    return () => {
      delete bar.dataset.morph;
      morphing.current = false;
    };
  }, [recording, originRef]);

  // Being able to paste the instant the window comes forward is the feature,
  // so: focus the box when a recording starts, focus it again when the window
  // is activated with nothing else focused, and catch a paste that lands on
  // the document (Cmd/Ctrl+V with no field focused) instead of dropping it.
  useEffect(() => {
    if (!recording) return;
    setElapsed(0);
    setError(null);
    barsRef.current = new Array(BAR_COUNT).fill(0.04);
    // The draft is left alone on purpose: text still sitting in the box when a
    // recording ended has not been thrown away, it is waiting for the next
    // one, which is what you want after a hotkey release you did not mean.
    //
    // `preventScroll`: the bar is parked below the fold when idle, and a plain
    // focus() scrolls the stage to bring it into view, dragging the whole
    // content area up with it.
    boxRef.current?.focus({ preventScroll: true });
    const onWindowFocus = () => {
      const a = document.activeElement;
      if (!a || a === document.body) boxRef.current?.focus({ preventScroll: true });
    };
    const onPaste = (e: ClipboardEvent) => {
      // A search field, a history row being edited or the box itself owns its
      // own paste. Only an unfocused window is ours to claim.
      if (isEditable(e.target)) return;
      const text = e.clipboardData?.getData('text') ?? '';
      if (!text.trim()) return;
      e.preventDefault();
      setDraft((d) => append(d, text));
      boxRef.current?.focus({ preventScroll: true });
    };
    window.addEventListener('focus', onWindowFocus);
    document.addEventListener('paste', onPaste);
    return () => {
      window.removeEventListener('focus', onWindowFocus);
      document.removeEventListener('paste', onPaste);
    };
  }, [recording]);

  // Grow with the content. `field-sizing: content` would do this in CSS but
  // WKWebView does not have it, and this window has to render the same on both
  // platforms. The ceiling is read from the stylesheet rather than repeated
  // here: paste a whole document in and the box must stop well below the
  // sidebar's nav instead of covering it.
  useEffect(() => {
    const el = boxRef.current;
    if (!el) return;
    const cap = parseFloat(getComputedStyle(el).maxHeight);
    el.style.height = 'auto';
    el.style.height = `${Number.isFinite(cap) ? Math.min(el.scrollHeight, cap) : el.scrollHeight}px`;
  }, [draft]);

  async function insert() {
    if (!draft.trim()) return;
    try {
      // Sent unclipped: the core trims the ends and keeps the middle, so a
      // deliberate line break inside a pasted quote survives.
      await api.insertMark(draft);
      setDraft('');
      setError(null);
      boxRef.current?.focus({ preventScroll: true });
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div
      className="dictation-bar"
      ref={barRef}
      data-open={recording}
      data-refine={refine ? 'true' : undefined}
      inert={!recording}
      // Scoped accent: everything inside the bar (tint, meter, stop button)
      // reads `--accent`, so one variable on the root recolours the whole bar
      // for a Refine take and leaves the rest of the window alone.
      style={accent ? ({ '--accent': accent } as React.CSSProperties) : undefined}
    >
      <div className="bar-tint" ref={tintRef} aria-hidden="true" />
      <div className="bar-inner" ref={innerRef}>
        <div className="bar-left">
          <button className="btn danger bar-stop" onClick={() => api.stopRecording()} title={t('app.record.stop')}>
            <Square size={12} />
            <span className="bar-stop-label">{t('compose.stop')}</span>
          </button>

          <div className="bar-meter" aria-hidden="true">
            {barsRef.current.map((v, i) => (
              <div key={i} className="wave-bar" style={{ height: `${Math.max(6, v * 100)}%` }} />
            ))}
          </div>

          <span className="bar-time">{fmtTime(elapsed)}</span>
          {refine && <span className="bar-mode">{t('hud.refineTag')}</span>}

          {marks.length > 0 && (
            <button className="bar-marks" onClick={onOpenCompose} title={t('bar.openCompose')}>
              <span>
                {pinnedAt !== null
                  ? t('bar.pinnedAt', { time: fmtTime(pinnedAt) })
                  : t('bar.pinnedCount', { n: marks.length })}
              </span>
              <ArrowUpRight size={13} />
            </button>
          )}
        </div>

        <div className="bar-box">
          {error && <div className="bar-error">{error}</div>}
          <textarea
            ref={boxRef}
            rows={1}
            spellCheck={false}
            placeholder={t('compose.markPlaceholder.recording')}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              // `isComposing`: Enter is how an IME accepts a candidate, and
              // inserting a half-composed Japanese or Chinese phrase because
              // of it would be a mark you cannot take back.
              if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                insert();
              }
            }}
          />
        </div>

        <button
          className="btn primary bar-insert"
          disabled={!draft.trim()}
          onClick={insert}
          title={t('bar.insertHint')}
        >
          <Link2 size={14} />
          <span className="bar-insert-label">{t('compose.insert')}</span>
        </button>
      </div>
    </div>
  );
}

function isEditable(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.tagName !== 'string') return false;
  const tag = el.tagName.toUpperCase();
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable;
}

/// Add a document-level paste to whatever is already in the box rather than
/// replacing it: the caret is not in the box on that path, so there is no
/// insertion point to respect and clobbering a half-typed note would be rude.
function append(draft: string, text: string): string {
  if (!draft) return text;
  const joiner = draft.includes('\n') || text.includes('\n') ? '\n' : ' ';
  return draft.replace(/\s+$/, '') + joiner + text;
}
