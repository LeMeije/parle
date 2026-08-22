// Full settings surface. Every control writes through to Rust immediately.

import { useEffect, useState } from 'react';
import { enable as enableAutostart, disable as disableAutostart } from '@tauri-apps/plugin-autostart';
import { api } from '../api';
import type { Settings } from '../types';
import iconDefault from '../assets/icons/default.png';
import iconKeycap from '../assets/icons/keycap.png';
import iconWaveform from '../assets/icons/waveform.png';
import iconEchoRings from '../assets/icons/echo-rings.png';
import iconCassette from '../assets/icons/cassette.png';

const APP_ICONS: [string, string, string][] = [
  ['default', iconDefault, 'EchoKey'],
  ['keycap', iconKeycap, 'Keycap'],
  ['waveform', iconWaveform, 'Waveform'],
  ['echo-rings', iconEchoRings, 'Echo rings'],
  ['cassette', iconCassette, 'Cassette'],
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

const SPECIAL_KEYS = IS_MAC
  ? ['Fn', 'RightCommand', 'RightOption', 'RightControl', 'LeftControl']
  : ['CopilotKey', 'RightCtrl', 'RightShift', 'LeftAlt', 'RightWin'];

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

  return (
    <div className="settings">
      <header className="view-head">
        <h1>Settings</h1>
        <p>Local-only. No telemetry, no cloud, ever.</p>
      </header>

      <Section title="Hotkeys">
        <Field label="Dictation key" hint={IS_MAC ? 'Fn needs Accessibility permission' : 'Right Alt is AltGr on many layouts — Right Ctrl is safer'}>
          <select value={s.hotkeys.dictation.key} onChange={(e) => set((d) => (d.hotkeys.dictation.key = e.target.value))}>
            {SPECIAL_KEYS.map((k) => (
              <option key={k} value={k}>
                {keyLabel(k)}
              </option>
            ))}
            {!SPECIAL_KEYS.includes(s.hotkeys.dictation.key) && (
              <option value={s.hotkeys.dictation.key}>{s.hotkeys.dictation.key}</option>
            )}
          </select>
        </Field>
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
            Accessibility permission is missing — special keys and paste-at-cursor won't work.{' '}
            <button onClick={() => api.requestAccessibility()}>Grant</button>
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
        <Field label="Palette">
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
            <button onClick={() => api.restartApp()}>Restart EchoKey</button>
          </div>
        )}
        <Field label="Overlay style" hint="Cassette pairs beautifully with the Retro palette">
          <div className="seg">
            {['pill', 'cassette', 'minimal'].map((st) => (
              <button key={st} className={s.overlay.style === st ? 'active' : ''} onClick={() => set((d) => (d.overlay.style = st))}>
                {st[0].toUpperCase() + st.slice(1)}
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
        EchoKey · on-device dictation · <span className="faint">nothing ever leaves this machine</span>
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

function keyLabel(k: string): string {
  const map: Record<string, string> = {
    Fn: '🌐 Fn / Globe',
    RightCommand: 'Right ⌘',
    RightOption: 'Right ⌥',
    RightControl: 'Right ⌃',
    LeftControl: 'Left ⌃',
    CopilotKey: 'Copilot key',
    RightCtrl: 'Right Ctrl',
    RightShift: 'Right Shift',
    LeftAlt: 'Left Alt',
    RightWin: 'Right Win',
  };
  return map[k] ?? k;
}
