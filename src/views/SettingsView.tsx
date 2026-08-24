// Full settings surface. Every control writes through to Rust immediately.

import { useEffect, useState } from 'react';
import { enable as enableAutostart, disable as disableAutostart } from '@tauri-apps/plugin-autostart';
import { api, onSyncStatus } from '../api';
import type { Settings, SyncStatus } from '../types';
import iconDefault from '../assets/icons/default.png';
import iconKeycap from '../assets/icons/keycap.png';
import iconWaveform from '../assets/icons/waveform.png';
import iconEchoRings from '../assets/icons/echo-rings.png';
import iconCassette from '../assets/icons/cassette.png';
import trayTemplate from '../../src-tauri/icons/tray.png';
import trayBadge from '../../src-tauri/icons/tray-badge.png';
import trayLight from '../../src-tauri/icons/tray-light.png';
import trayDark from '../../src-tauri/icons/tray-dark.png';
import trayColor from '../../src-tauri/icons/tray-color.png';

const APP_ICONS: [string, string, string][] = [
  ['default', iconDefault, 'Parle'],
  ['keycap', iconKeycap, 'Keycap'],
  ['waveform', iconWaveform, 'Waveform'],
  ['echo-rings', iconEchoRings, 'Echo rings'],
  ['cassette', iconCassette, 'Cassette'],
];

// Inline glyphs for the overlay-style picker. Drawn in currentColor so they
// inherit the segmented control's active/inactive text colour.
const glyph = (children: React.ReactNode) => (
  <svg
    viewBox="0 0 16 16"
    width="14"
    height="14"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.2"
    aria-hidden="true"
  >
    {children}
  </svg>
);

const OVERLAY_STYLES: [string, string, React.ReactNode][] = [
  [
    'pill',
    'Pill',
    glyph(
      <>
        <rect x="0.9" y="4.6" width="14.2" height="6.8" rx="3.4" />
        <path d="M5 7.2v1.6M7.4 6.3v3.4M9.8 6.9v2.2M12.2 7.4v1.2" strokeLinecap="round" />
      </>,
    ),
  ],
  [
    'cassette',
    'Cassette',
    glyph(
      <>
        <rect x="0.9" y="3.4" width="14.2" height="9.2" rx="2" />
        <circle cx="5.1" cy="7.4" r="1.9" />
        <circle cx="10.9" cy="7.4" r="1.9" />
        <path d="M1.6 11.9h12.8" strokeWidth="1.5" strokeLinecap="round" />
      </>,
    ),
  ],
  [
    'metal',
    'Metal',
    glyph(
      <>
        <rect x="0.9" y="3.4" width="14.2" height="9.2" rx="2" />
        <circle cx="5.1" cy="8" r="2.1" />
        <circle cx="10.9" cy="8" r="2.1" />
        <circle cx="5.1" cy="8" r="0.7" fill="currentColor" stroke="none" />
        <circle cx="10.9" cy="8" r="0.7" fill="currentColor" stroke="none" />
      </>,
    ),
  ],
  [
    'minimal',
    'Minimal',
    glyph(
      <>
        <rect x="3.4" y="5.4" width="9.2" height="5.2" rx="2.6" />
        <circle cx="6.2" cy="8" r="0.9" fill="currentColor" stroke="none" />
      </>,
    ),
  ],
];

const LANGUAGES: [string, string][] = [
  ['auto', 'Auto-detect'],
  ['en', 'English'],
  ['es', 'Spanish'],
  ['fr', 'French'],
  ['de', 'German'],
  ['it', 'Italian'],
  ['pt', 'Portuguese'],
  ['nl', 'Dutch'],
  ['ja', 'Japanese'],
  ['ko', 'Korean'],
  ['zh', 'Chinese'],
  ['hi', 'Hindi'],
  ['ar', 'Arabic'],
  ['ru', 'Russian'],
  ['pl', 'Polish'],
  ['sv', 'Swedish'],
];

const ACCENTS = ['#2b5cff', '#e0642f', '#178a50', '#8b5cf6', '#d5382f', '#0d9aa8', '#b06a00', '#d6336c'];

const IS_MAC = navigator.userAgent.includes('Mac');

// Tray/menu-bar icon styles, platform-filtered: the outline variants only make
// sense against a Windows taskbar, and macOS renders "template" as a proper
// template image. Each preview pairs the asset with the backdrop it is drawn
// for, so a white outline is never previewed on white.
const TRAY_STYLES: [string, string, [string, 'light' | 'dark'][]][] = IS_MAC
  ? [
      ['template', 'Monochrome', [[trayTemplate, 'light']]],
      ['badge', 'Blue badge', [[trayBadge, 'light']]],
    ]
  : [
      ['badge', 'Blue badge', [[trayBadge, 'light']]],
      [
        'auto',
        'Auto — match taskbar',
        [
          [trayDark, 'light'],
          [trayLight, 'dark'],
        ],
      ],
      ['light', 'Outline light', [[trayLight, 'dark']]],
      ['dark', 'Outline dark', [[trayDark, 'light']]],
      ['color', 'Blue outline', [[trayColor, 'light']]],
    ];

const SPECIAL_KEYS = IS_MAC
  ? ['Fn', 'RightCommand', 'RightOption', 'RightControl', 'LeftControl']
  : ['CopilotKey', 'RightCtrl', 'RightShift', 'LeftAlt', 'RightWin'];

// Sentinel for the picker only — never stored in settings.
const CUSTOM = '__custom__';

// Bare modifiers that Rust's NativeKey::parse understands. Keyed by
// KeyboardEvent.code because event.key can't tell left from right.
const NATIVE_BY_CODE: Record<string, string> = IS_MAC
  ? {
      ControlLeft: 'LeftControl',
      ControlRight: 'RightControl',
      ShiftLeft: 'LeftShift',
      ShiftRight: 'RightShift',
      AltLeft: 'LeftOption',
      AltRight: 'RightOption',
      MetaLeft: 'LeftCommand',
      MetaRight: 'RightCommand',
    }
  : {
      ControlLeft: 'LeftCtrl',
      ControlRight: 'RightCtrl',
      ShiftLeft: 'LeftShift',
      ShiftRight: 'RightShift',
      AltLeft: 'LeftAlt',
      AltRight: 'RightAlt',
      MetaLeft: 'LeftWin',
      MetaRight: 'RightWin',
    };

export default function SettingsView({
  settings,
  onSave,
}: {
  settings: Settings;
  onSave: (s: Settings) => Promise<void>;
}) {
  const [devices, setDevices] = useState<string[]>([]);
  const [perms, setPerms] = useState<{ accessibility: boolean; microphone: string } | null>(null);

  const [needsRestart, setNeedsRestart] = useState(false);

  const [customPicked, setCustomPicked] = useState(false);
  const [capturing, setCapturing] = useState(false);

  useEffect(() => {
    api.listAudioDevices().then(setDevices);
    // Poll: permissions can change in System Settings while this page is open.
    const poll = () => api.permissionStatus().then(setPerms);
    poll();
    const t = window.setInterval(poll, 2000);
    return () => window.clearInterval(t);
  }, []);

  const s = settings;
  const set = (patch: (draft: Settings) => void) => {
    const next: Settings = JSON.parse(JSON.stringify(s));
    patch(next);
    onSave(next);
  };

  // While listening, swallow the keypress entirely and turn it into a binding.
  useEffect(() => {
    if (!capturing) return;
    const onKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === 'Escape') {
        setCapturing(false);
        return;
      }
      const binding = bindingFromEvent(e);
      if (!binding) return;
      setCapturing(false);
      set((d) => (d.hotkeys.dictation.key = binding));
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, [capturing, s]);

  // "auto" is a Windows concept; on macOS it simply means the monochrome
  // template icon, so show it as such rather than leaving the picker blank.
  const trayStyle = IS_MAC && s.appearance.tray_style === 'auto' ? 'template' : s.appearance.tray_style;
  const trayPreview = TRAY_STYLES.find(([v]) => v === trayStyle)?.[2] ?? [];

  const dictationKey = s.hotkeys.dictation.key;
  const isCustom = customPicked || !SPECIAL_KEYS.includes(dictationKey);
  const warning = SPECIAL_KEYS.includes(dictationKey) ? null : bindingWarning(dictationKey);

  return (
    <div className="settings">
      <header className="view-head">
        <h1>Settings</h1>
        <p>Local-only. No telemetry, no cloud, ever.</p>
      </header>

      <Section title="Hotkeys">
        <Field label="Dictation key" hint={IS_MAC ? 'Fn needs Accessibility permission' : 'Right Alt is AltGr on many layouts — Right Ctrl is safer'}>
          <select
            value={isCustom ? CUSTOM : dictationKey}
            onChange={(e) => {
              const v = e.target.value;
              if (v === CUSTOM) {
                setCustomPicked(true);
                return;
              }
              setCustomPicked(false);
              setCapturing(false);
              set((d) => (d.hotkeys.dictation.key = v));
            }}
          >
            {SPECIAL_KEYS.map((k) => (
              <option key={k} value={k}>
                {keyLabel(k)}
              </option>
            ))}
            <option value={CUSTOM}>Custom…</option>
          </select>
        </Field>
        {isCustom && (
          <Field label="Custom binding" hint="Click, then press the key or combination you want. Esc cancels.">
            <button
              className={`btn key-capture ${capturing ? 'listening' : ''}`}
              onClick={() => setCapturing(true)}
            >
              {capturing ? 'Press a key combination…' : keyLabel(dictationKey)}
            </button>
          </Field>
        )}
        {isCustom && warning && <div className="callout warn">{warning}</div>}
        <Field
          label="Gesture"
          hint={
            s.hotkeys.dictation.mode === 'double_tap'
              ? 'Double-tap starts, single tap stops. The key is never intercepted, so its normal system behaviour keeps working.'
              : 'Hybrid: hold to talk; a quick tap latches until the next tap'
          }
        >
          <div className="seg">
            {(['hold', 'toggle', 'hybrid', 'double_tap'] as const).map((m) => (
              <button
                key={m}
                className={s.hotkeys.dictation.mode === m ? 'active' : ''}
                onClick={() => set((d) => (d.hotkeys.dictation.mode = m))}
              >
                {m === 'hold' ? 'Hold' : m === 'toggle' ? 'Toggle' : m === 'hybrid' ? 'Hybrid' : 'Double tap'}
              </button>
            ))}
          </div>
        </Field>
        <Field label="Latch window" hint="Hybrid: taps shorter than this latch into toggle. Double tap: max gap between taps">
          <NumberInput value={s.hotkeys.latch_ms} min={150} max={900} step={50} suffix="ms" onChange={(v) => set((d) => (d.hotkeys.latch_ms = v))} />
        </Field>
        <Toggle
          label="Esc cancels recording"
          hint="Off by default: Esc gets pressed for all sorts of unrelated reasons, and discarding a take you already spoke is worse than stopping it with your hotkey"
          value={s.hotkeys.cancel.enabled}
          onChange={(v) => set((d) => (d.hotkeys.cancel.enabled = v))}
        />
        <Field label="History palette" hint="Chord shortcut for search">
          <input
            className="key-input"
            value={s.hotkeys.history_palette.key}
            onChange={(e) => set((d) => (d.hotkeys.history_palette.key = e.target.value))}
          />
        </Field>
        {!IS_MAC && (
          <Toggle
            label="Suppress Copilot launch"
            hint="When the Copilot key is bound (or this is on), the default Copilot app never opens"
            value={s.hotkeys.suppress_copilot}
            onChange={(v) => set((d) => (d.hotkeys.suppress_copilot = v))}
          />
        )}
        {perms && !perms.accessibility && IS_MAC && (
          <div className="callout warn">
            Accessibility permission is missing — special keys and paste-at-cursor won't work. If you
            already granted it and this warning stays, the entry went stale after a rebuild: use Repair.{' '}
            <button onClick={() => api.requestAccessibility()}>Grant</button>
            <button onClick={() => api.repairAccessibility()}>Repair permission</button>
            <button onClick={() => api.openPermissionSettings('accessibility')}>Open System Settings</button>
          </div>
        )}
      </Section>

      <Section title="Language">
        <Field label="Spoken language">
          <select value={s.language.language} onChange={(e) => set((d) => (d.language.language = e.target.value))}>
            {LANGUAGES.map(([code, name]) => (
              <option key={code} value={code}>
                {name}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Locale spelling" hint="Affects spelling of the output (colour vs color)">
          <select value={s.language.locale} onChange={(e) => set((d) => (d.language.locale = e.target.value))}>
            <option value="">No preference</option>
            <option value="en-AU">English (Australia)</option>
            <option value="en-GB">English (UK)</option>
            <option value="en-US">English (US)</option>
          </select>
        </Field>
        <Toggle
          label="Apply locale spelling"
          hint="Convert US spellings in the transcript to your locale"
          value={s.cleanup.locale_spelling}
          onChange={(v) => set((d) => (d.cleanup.locale_spelling = v))}
        />
        <Toggle
          label="Translate to English"
          hint="Speak any language, paste English"
          value={s.language.translate_to_english}
          onChange={(v) => set((d) => (d.language.translate_to_english = v))}
        />
      </Section>

      <Section title="Cleanup">
        <Toggle label="Smart cleanup" hint="Master switch for the deterministic cleanup tier" value={s.cleanup.enabled} onChange={(v) => set((d) => (d.cleanup.enabled = v))} />
        <Toggle label="Remove filler words" hint="um, uh, er…" value={s.cleanup.remove_fillers} onChange={(v) => set((d) => (d.cleanup.remove_fillers = v))} />
        <Toggle label="Remove hedges" hint="you know, sort of, I mean (more aggressive)" value={s.cleanup.remove_hedges} onChange={(v) => set((d) => (d.cleanup.remove_hedges = v))} />
        <Toggle
          label="Trim self-corrections"
          hint="“Thursday, no actually Wednesday” → “Wednesday”. Trimmed spans stay reviewable in History"
          value={s.cleanup.trim_self_corrections}
          onChange={(v) => set((d) => (d.cleanup.trim_self_corrections = v))}
        />
        <Toggle label="Dictated punctuation" hint="“comma”, “new line”, “question mark”… (“literally comma” escapes)" value={s.cleanup.dictated_punctuation} onChange={(v) => set((d) => (d.cleanup.dictated_punctuation = v))} />
        <Toggle label="Capitalise sentences" value={s.cleanup.capitalise_sentences} onChange={(v) => set((d) => (d.cleanup.capitalise_sentences = v))} />
        <Toggle label="End with punctuation" value={s.cleanup.ensure_terminal_punctuation} onChange={(v) => set((d) => (d.cleanup.ensure_terminal_punctuation = v))} />
        <Toggle label="Paragraph on long pause" value={s.cleanup.paragraph_on_long_pause} onChange={(v) => set((d) => (d.cleanup.paragraph_on_long_pause = v))} />
      </Section>

      <Section title="Dictionary">
        <Toggle label="Enable dictionary" value={s.dictionary.enabled} onChange={(v) => set((d) => (d.dictionary.enabled = v))} />
        <Toggle label="Bias recognition" hint="Feed your terms to the engine as a glossary" value={s.dictionary.bias_recognition} onChange={(v) => set((d) => (d.dictionary.bias_recognition = v))} />
        <Toggle label="Fix close misspellings" value={s.dictionary.fuzzy_correct} onChange={(v) => set((d) => (d.dictionary.fuzzy_correct = v))} />
        <Toggle label="Learn from my edits" hint="Single-word edits in History become correction pairs" value={s.dictionary.auto_learn} onChange={(v) => set((d) => (d.dictionary.auto_learn = v))} />
      </Section>

      <Section title="Output">
        <Toggle label="Insert at cursor" hint="Types the result into the focused app" value={s.paste.inject} onChange={(v) => set((d) => (d.paste.inject = v))} />
        <Toggle label="Copy to clipboard" value={s.paste.copy_to_clipboard} onChange={(v) => set((d) => (d.paste.copy_to_clipboard = v))} />
        <Toggle label="Restore previous clipboard" hint="After paste-injection, put your old clipboard back" value={s.paste.restore_clipboard} onChange={(v) => set((d) => (d.paste.restore_clipboard = v))} />
        <Field label="Restore delay" hint="Slow apps (Office, remote desktop) read the clipboard late">
          <NumberInput value={s.paste.restore_delay_ms} min={200} max={2000} step={100} suffix="ms" onChange={(v) => set((d) => (d.paste.restore_delay_ms = v))} />
        </Field>
        {IS_MAC && (
          <Toggle label="Prefer direct insertion" hint="Try Accessibility text insertion before clipboard-paste" value={s.paste.prefer_ax_insert} onChange={(v) => set((d) => (d.paste.prefer_ax_insert = v))} />
        )}
        <Toggle
          label="Press Enter after inserting"
          hint="Sends the message right after pasting — handy for chat apps. Never fires on secure fields."
          value={s.paste.press_enter}
          onChange={(v) => set((d) => (d.paste.press_enter = v))}
        />
      </Section>

      <Section title="Appearance">
        <Field label="Theme">
          <div className="seg">
            {(['system', 'light', 'dark'] as const).map((m) => (
              <button key={m} className={s.appearance.theme_mode === m ? 'active' : ''} onClick={() => set((d) => (d.appearance.theme_mode = m))}>
                {m[0].toUpperCase() + m.slice(1)}
              </button>
            ))}
          </div>
        </Field>
        <Field label="Palette" hint="Pastel tints itself from your accent colour — try it with the custom wheel">
          <div className="seg">
            {['paper', 'pastel', 'bold', 'retro'].map((p) => (
              <button key={p} className={s.appearance.palette === p ? 'active' : ''} onClick={() => set((d) => (d.appearance.palette = p))}>
                {p[0].toUpperCase() + p.slice(1)}
              </button>
            ))}
          </div>
        </Field>
        <Field label="Accent">
          <div className="accent-row">
            {ACCENTS.map((c) => (
              <button
                key={c}
                className={`accent-dot ${s.appearance.accent === c ? 'active' : ''}`}
                style={{ background: c }}
                onClick={() => set((d) => (d.appearance.accent = c))}
              />
            ))}
            <label className="accent-custom" title="Custom colour">
              <input
                type="color"
                value={s.appearance.accent}
                onChange={(e) => set((d) => (d.appearance.accent = e.target.value))}
              />
            </label>
          </div>
        </Field>
        <Field label="App icon" hint="Applies immediately in-app; the Finder icon updates after a restart">
          <div className="icon-picker">
            {APP_ICONS.map(([id, src, name]) => (
              <button
                key={id}
                className={`icon-choice ${s.appearance.app_icon === id ? 'active' : ''}`}
                title={name}
                onClick={() => {
                  set((d) => (d.appearance.app_icon = id));
                  api.setAppIcon(id).then(setNeedsRestart).catch(() => {});
                }}
              >
                <img src={src} alt={name} draggable={false} />
              </button>
            ))}
          </div>
        </Field>
        {needsRestart && (
          <div className="callout warn">
            Icon updated. Restart to refresh the Finder and Dock icon.{' '}
            <button onClick={() => api.restartApp()}>Restart Parle</button>
          </div>
        )}
        <Field
          label={IS_MAC ? 'Menu bar icon' : 'Tray icon'}
          hint={IS_MAC ? 'Monochrome follows the menu bar; the badge keeps Parle’s colour' : 'Auto picks the outline that reads against your taskbar'}
        >
          <span className="tray-preview">
            {trayPreview.map(([src, bg]) => (
              <span key={src} className="tray-chip" data-bg={bg}>
                <img src={src} alt="" draggable={false} />
              </span>
            ))}
          </span>
          <select value={trayStyle} onChange={(e) => set((d) => (d.appearance.tray_style = e.target.value))}>
            {TRAY_STYLES.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Overlay style" hint="Cassette pairs beautifully with the Retro palette">
          <div className="seg seg-icons">
            {OVERLAY_STYLES.map(([st, label, icon]) => (
              <button key={st} className={s.overlay.style === st ? 'active' : ''} onClick={() => set((d) => (d.overlay.style = st))}>
                {icon}
                {label}
              </button>
            ))}
          </div>
        </Field>
        <Toggle label="Show live transcript in overlay" value={s.overlay.show_partial_text} onChange={(v) => set((d) => (d.overlay.show_partial_text = v))} />
        <Toggle label="Reduce motion" value={s.appearance.reduce_motion} onChange={(v) => set((d) => (d.appearance.reduce_motion = v))} />
      </Section>

      <Section title="History & privacy">
        <Toggle label="Capture clipboard" hint="Everything you copy, searchable. Password managers are excluded" value={s.history.clipboard_capture} onChange={(v) => set((d) => (d.history.clipboard_capture = v))} />
        <Field label="Keep items for">
          <select value={s.history.retention_days} onChange={(e) => set((d) => (d.history.retention_days = Number(e.target.value)))}>
            <option value={0}>Forever</option>
            <option value={90}>90 days</option>
            <option value={30}>30 days</option>
            <option value={7}>7 days</option>
            <option value={1}>1 day</option>
          </select>
        </Field>
        <Field label="Excluded apps" hint="One per line — bundle id (Mac) or exe name (Windows)">
          <textarea
            className="excluded-apps"
            value={s.history.excluded_apps.join('\n')}
            onChange={(e) => set((d) => (d.history.excluded_apps = e.target.value.split('\n').map((x) => x.trim()).filter(Boolean)))}
          />
        </Field>
        <Field label="Danger zone">
          <button className="btn danger" onClick={() => api.clearHistory().then(() => window.location.reload())}>
            Clear all unpinned history
          </button>
        </Field>
      </Section>

      <SyncSection sync={s.sync} />

      <Section title="Audio">
        <Field label="Microphone">
          <select value={s.audio.input_device} onChange={(e) => set((d) => (d.audio.input_device = e.target.value))}>
            <option value="">System default</option>
            {devices.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Ignore recordings shorter than">
          <NumberInput value={s.audio.min_duration_ms} min={100} max={2000} step={100} suffix="ms" onChange={(v) => set((d) => (d.audio.min_duration_ms = v))} />
        </Field>
        {perms && perms.microphone === 'denied' && (
          <div className="callout warn">
            Microphone access is denied.{' '}
            <button onClick={() => api.openPermissionSettings('microphone')}>Open System Settings</button>
          </div>
        )}
      </Section>

      <Section title="General">
        <Toggle
          label="Launch at login"
          value={s.launch_at_login}
          onChange={async (v) => {
            try {
              if (v) await enableAutostart();
              else await disableAutostart();
            } catch {
              /* surfaced via settings state anyway */
            }
            set((d) => (d.launch_at_login = v));
          }}
        />
        <Toggle label="Pre-warm model at startup" hint="Uses memory while idle, makes the first dictation instant" value={s.models.prewarm} onChange={(v) => set((d) => (d.models.prewarm = v))} />
      </Section>

      <footer className="settings-footer">
        Parle · on-device dictation · <span className="faint">nothing ever leaves this machine</span>
      </footer>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="settings-section">
      <h2>{title}</h2>
      <div className="section-body">{children}</div>
    </section>
  );
}

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="field">
      <div className="field-label">
        <span>{label}</span>
        {hint && <small>{hint}</small>}
      </div>
      <div className="field-control">{children}</div>
    </div>
  );
}

function Toggle({
  label,
  hint,
  value,
  onChange,
}: {
  label: string;
  hint?: string;
  value: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="field">
      <div className="field-label">
        <span>{label}</span>
        {hint && <small>{hint}</small>}
      </div>
      <div className="field-control">
        <label className="switch">
          <input type="checkbox" checked={value} onChange={(e) => onChange(e.target.checked)} />
          <span />
        </label>
      </div>
    </div>
  );
}

function NumberInput({
  value,
  min,
  max,
  step,
  suffix,
  onChange,
}: {
  value: number;
  min: number;
  max: number;
  step: number;
  suffix: string;
  onChange: (v: number) => void;
}) {
  return (
    <span className="number-input">
      <input
        type="number"
        value={value}
        min={min}
        max={max}
        step={step}
        onChange={(e) => onChange(Math.min(max, Math.max(min, Number(e.target.value))))}
      />
      <span className="suffix">{suffix}</span>
    </span>
  );
}

// ---------- Sync ----------
// Cross-machine sync, device-to-device over the LAN. State is driven entirely
// by `sync_status` plus the `sync-status` event — no polling. Every command can
// reject (a wrong pairing code is the *expected* failure, and the Rust side may
// still be landing), so failures surface inline instead of blanking the panel.

function SyncSection({ sync }: { sync: Settings['sync'] }) {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  // `sync_status` doesn't report the two kind flags, so they are seeded from
  // settings on mount and held locally after that.
  const [kinds, setKinds] = useState({
    dictations: sync?.sync_dictations ?? true,
    clipboard: sync?.sync_clipboard ?? true,
  });

  const [nameDraft, setNameDraft] = useState<string | null>(null);
  const [confirmUnpair, setConfirmUnpair] = useState<string | null>(null);
  const [direction, setDirection] = useState<'show' | 'enter'>('show');
  // Optimistic code from sync_start_pairing, shown until a status event
  // supersedes it — so the digits appear instantly even if the event lags.
  const [seedCode, setSeedCode] = useState<{ code: string; expires_at: number } | null>(null);
  const [peerId, setPeerId] = useState<string | null>(null);
  const [code, setCode] = useState('');
  const [pairError, setPairError] = useState<string | null>(null);
  const [pairBusy, setPairBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    api
      .syncStatus()
      .then((st) => {
        setStatus(st);
        setLoadError(null);
      })
      .catch((e) => setLoadError(errText(e)));
    const un = onSyncStatus((st) => {
      setStatus(st);
      setLoadError(null);
      setSeedCode(null);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  const shownCode =
    status?.pairing?.role === 'showing' && status.pairing.code
      ? { code: status.pairing.code, expires_at: status.pairing.expires_at }
      : seedCode;
  const expiresAt = shownCode ? toMillis(shownCode.expires_at) : 0;
  const remaining = expiresAt ? Math.max(0, Math.ceil((expiresAt - now) / 1000)) : 0;

  // Only tick while a code is actually on screen.
  useEffect(() => {
    if (!expiresAt) return;
    setNow(Date.now());
    const t = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(t);
  }, [expiresAt]);

  async function setEnabled(v: boolean) {
    setActionError(null);
    setStatus((st) => (st ? { ...st, enabled: v } : st));
    try {
      await api.syncSetEnabled(v);
    } catch (e) {
      setActionError(errText(e));
      api.syncStatus().then(setStatus).catch(() => {});
    }
  }

  async function commitName(current: string) {
    if (nameDraft === null) return;
    const next = nameDraft.trim();
    setNameDraft(null);
    if (!next || next === current) return;
    setActionError(null);
    try {
      await api.syncSetDeviceName(next);
    } catch (e) {
      setActionError(errText(e));
    }
  }

  async function startPairing() {
    setActionError(null);
    try {
      setSeedCode(await api.syncStartPairing());
    } catch (e) {
      setActionError(errText(e));
    }
  }

  async function cancelPairing() {
    setSeedCode(null);
    setActionError(null);
    try {
      await api.syncCancelPairing();
    } catch (e) {
      setActionError(errText(e));
    }
  }

  async function pair(id: string) {
    setPairBusy(true);
    setPairError(null);
    try {
      await api.syncPairWith(id, code);
      setCode('');
      setPeerId(null);
    } catch (e) {
      // A wrong code lands here. Say so and stop — never retry on the user's behalf.
      setPairError(errText(e));
    } finally {
      setPairBusy(false);
    }
  }

  async function unpair(id: string) {
    setConfirmUnpair(null);
    setActionError(null);
    try {
      await api.syncUnpair(id);
    } catch (e) {
      setActionError(errText(e));
    }
  }

  async function saveKinds(next: { dictations: boolean; clipboard: boolean }) {
    const prev = kinds;
    setKinds(next);
    setActionError(null);
    try {
      await api.syncSetKinds(next.dictations, next.clipboard);
    } catch (e) {
      setKinds(prev);
      setActionError(errText(e));
    }
  }

  if (loadError) {
    return (
      <Section title="Sync">
        <div className="sync-block">
          <div className="callout error">Sync isn’t available right now — {loadError}</div>
        </div>
      </Section>
    );
  }

  if (!status) {
    return (
      <Section title="Sync">
        <div className="sync-block">
          <span className="sync-empty">Checking sync…</span>
        </div>
      </Section>
    );
  }

  const unpaired = status.peers.filter((p) => !status.paired.some((d) => d.id === p.id));
  const selected = unpaired.find((p) => p.id === peerId) ?? null;

  return (
    <Section title="Sync">
      <Toggle
        label="Sync with my other devices"
        hint="Off unless you turn it on. Your Mac and PC talk straight to each other over your local network, end-to-end encrypted — no account, no cloud, nothing uploaded anywhere."
        value={status.enabled}
        onChange={setEnabled}
      />

      {status.enabled && (
        <>
          {actionError && (
            <div className="sync-block">
              <div className="callout error">
                {actionError} <button onClick={() => setActionError(null)}>Dismiss</button>
              </div>
            </div>
          )}

          <Field label="This device" hint="The name the other machine sees while pairing.">
            <input
              className="sync-name-input"
              value={nameDraft ?? status.device_name}
              placeholder="Name this device"
              onChange={(e) => setNameDraft(e.target.value)}
              onBlur={() => commitName(status.device_name)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
                if (e.key === 'Escape') setNameDraft(null);
              }}
            />
          </Field>
          <Field label="Device ID" hint="This install’s identity. It never leaves your network.">
            <code className="sync-device-id">{status.device_id}</code>
          </Field>

          <div className="sync-block">
            <div className="field-label">
              <span>Paired devices</span>
              <small>Only these machines can see your history. Pairing is mutual.</small>
            </div>
            {status.paired.length === 0 ? (
              <span className="sync-empty">No devices paired yet — pair one below to start syncing.</span>
            ) : (
              <div className="sync-list">
                {status.paired.map((d) => (
                  <div key={d.id} className="sync-device">
                    <span className={`sync-dot ${d.online ? 'online' : ''}`} />
                    <span className="sync-device-body">
                      <span className="sync-device-name">{d.name}</span>
                      <span className="sync-device-meta">
                        {d.online ? 'Online now' : lastSeenLabel(d.last_seen)}
                      </span>
                    </span>
                    {confirmUnpair === d.id ? (
                      <span className="sync-confirm">
                        <span>Unpair {d.name}? It stops syncing and needs a new code to come back.</span>
                        <button className="btn ghost" onClick={() => setConfirmUnpair(null)}>
                          Keep it
                        </button>
                        <button className="btn danger" onClick={() => unpair(d.id)}>
                          Unpair
                        </button>
                      </span>
                    ) : (
                      <button className="btn ghost" onClick={() => setConfirmUnpair(d.id)}>
                        Unpair
                      </button>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>

          <div className="sync-block">
            <div className="field-label">
              <span>Pair a new device</span>
              <small>Either machine can start. Read the six digits aloud or type them across.</small>
            </div>
            <div className="seg">
              {(['show', 'enter'] as const).map((m) => (
                <button key={m} className={direction === m ? 'active' : ''} onClick={() => setDirection(m)}>
                  {m === 'show' ? 'Show a code' : 'Enter a code'}
                </button>
              ))}
            </div>

            {direction === 'show' ? (
              shownCode ? (
                <div className="sync-showing">
                  <div className="sync-code">
                    {shownCode.code.split('').map((digit, i) => (
                      <b key={i} className={i === 3 ? 'split' : undefined}>
                        {digit}
                      </b>
                    ))}
                  </div>
                  <div className="sync-countdown">
                    {remaining > 0
                      ? `Type it on the other device — expires in ${fmtCountdown(remaining)}`
                      : 'This code has expired.'}
                  </div>
                  {remaining > 0 ? (
                    <button className="btn" onClick={cancelPairing}>
                      Cancel
                    </button>
                  ) : (
                    <button className="btn primary" onClick={startPairing}>
                      Show a new code
                    </button>
                  )}
                </div>
              ) : (
                <div className="sync-showing">
                  <span className="sync-empty">
                    Parle shows six digits here; type them into the other machine to confirm it’s really yours.
                  </span>
                  <button className="btn primary" onClick={startPairing}>
                    Show a code
                  </button>
                </div>
              )
            ) : (
              <>
                {unpaired.length === 0 ? (
                  <span className="sync-empty">
                    No devices found yet. Open Parle on the other machine, turn Sync on there too, and make sure
                    both are on the same network.
                  </span>
                ) : (
                  <div className="sync-peers">
                    {unpaired.map((p) => (
                      <button
                        key={p.id}
                        className={`sync-peer ${p.id === peerId ? 'active' : ''}`}
                        onClick={() => {
                          setPeerId(p.id);
                          setPairError(null);
                        }}
                      >
                        <span className="sync-peer-name">{p.name}</span>
                        <span className="sync-peer-addr">
                          {p.addr}:{p.port}
                        </span>
                      </button>
                    ))}
                  </div>
                )}
                <div className="sync-enter">
                  <input
                    className="sync-code-input"
                    inputMode="numeric"
                    autoComplete="off"
                    spellCheck={false}
                    placeholder="——————"
                    value={code}
                    onChange={(e) => {
                      setCode(e.target.value.replace(/\D/g, '').slice(0, 6));
                      setPairError(null);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && selected && code.length === 6 && !pairBusy) pair(selected.id);
                    }}
                  />
                  <button
                    className="btn primary"
                    disabled={!selected || code.length !== 6 || pairBusy}
                    onClick={() => selected && pair(selected.id)}
                  >
                    {pairBusy ? 'Pairing…' : 'Pair'}
                  </button>
                </div>
                {pairError && <div className="callout error">{pairError}</div>}
              </>
            )}
          </div>

          <Toggle
            label="Sync dictations"
            hint="Everything you dictate shows up in History on both machines"
            value={kinds.dictations}
            onChange={(v) => saveKinds({ ...kinds, dictations: v })}
          />
          <Toggle
            label="Sync clipboard"
            hint="Copy on one machine, paste on the other"
            value={kinds.clipboard}
            onChange={(v) => saveKinds({ ...kinds, clipboard: v })}
          />
        </>
      )}
    </Section>
  );
}

// Tauri rejects with the command's error value — a plain string for
// Result<_, String>, an Error if the command doesn't exist at all.
function errText(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === 'object' && 'message' in e) return String((e as { message: unknown }).message);
  return 'Something went wrong.';
}

// Timestamps may arrive as epoch seconds or milliseconds; accept both.
function toMillis(t: number): number {
  return t > 1e11 ? t : t * 1000;
}

function fmtCountdown(secs: number): string {
  const m = Math.floor(secs / 60);
  return m > 0 ? `${m}:${String(secs % 60).padStart(2, '0')}` : `${secs}s`;
}

function lastSeenLabel(at: number | null): string {
  if (at == null) return 'Never connected';
  const ms = toMillis(at);
  const s = Math.floor((Date.now() - ms) / 1000);
  if (s < 60) return 'Last seen just now';
  if (s < 3600) return `Last seen ${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `Last seen ${Math.floor(s / 3600)}h ago`;
  return `Last seen ${new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })}`;
}

function keyLabel(k: string): string {
  if (k.includes('+')) return k.split('+').map(chordPartLabel).join(' + ');
  const map: Record<string, string> = {
    Fn: '🌐 Fn / Globe',
    RightCommand: 'Right ⌘',
    LeftCommand: 'Left ⌘',
    RightOption: 'Right ⌥',
    LeftOption: 'Left ⌥',
    RightControl: 'Right ⌃',
    LeftControl: 'Left ⌃',
    CopilotKey: 'Copilot key',
    RightCtrl: 'Right Ctrl',
    LeftCtrl: 'Left Ctrl',
    RightShift: 'Right Shift',
    LeftShift: 'Left Shift',
    LeftAlt: 'Left Alt',
    RightAlt: 'Right Alt',
    RightWin: 'Right Win',
    LeftWin: 'Left Win',
  };
  return map[k] ?? k;
}

function chordPartLabel(part: string): string {
  if (part === 'Super') return IS_MAC ? '⌘' : 'Win';
  if (part.startsWith('Arrow')) return part.slice(5);
  return part;
}

// Canonical binding string for a captured keypress, or null if unusable.
// A bare modifier becomes a NativeKey name (native listener path); anything
// else becomes a chord string for tauri-plugin-global-shortcut.
function bindingFromEvent(e: KeyboardEvent): string | null {
  const native = NATIVE_BY_CODE[e.code];
  if (native) return native;
  if (!e.code || e.code === 'Unidentified') return null;
  const main = e.code.startsWith('Key')
    ? e.code.slice(3)
    : e.code.startsWith('Digit')
      ? e.code.slice(5)
      : e.code;
  const parts: string[] = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Super');
  parts.push(main);
  return parts.join('+');
}

function bindingWarning(key: string): string | null {
  if (key === 'LeftCtrl' || key === 'LeftControl')
    return 'Left Ctrl drives most keyboard shortcuts — binding it will fire during normal use.';
  if (key === 'LeftShift')
    return 'Left Shift is pressed constantly while typing — expect false triggers.';
  if (key === 'RightAlt' || key === 'RightOption')
    return 'Right Alt is AltGr on many layouts, so it types accented characters. Right Ctrl is safer.';
  return null;
}
