// Full settings surface. Every control writes through to Rust immediately.

import { useEffect, useState } from 'react';
import { RefreshCw } from 'lucide-react';
import { enable as enableAutostart, disable as disableAutostart } from '@tauri-apps/plugin-autostart';
import { api, onSyncStatus } from '../api';
import type { Settings, SyncStatus } from '../types';
import { t } from '../i18n';
import { useT } from '../i18n/useT';
import { getLang, setLang, LANGUAGES as I18N_LANGUAGES, type Lang } from '../i18n';
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

// [id, image, label key]. The label is a key, resolved at render so it
// follows a language change.
const APP_ICONS: [string, string, string][] = [
  ['default', iconDefault, 'settings.appIcon.default'],
  ['keycap', iconKeycap, 'settings.appIcon.keycap'],
  ['waveform', iconWaveform, 'settings.appIcon.waveform'],
  ['echo-rings', iconEchoRings, 'settings.appIcon.echoRings'],
  ['cassette', iconCassette, 'settings.appIcon.cassette'],
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

// [value, label key, glyph].
const OVERLAY_STYLES: [string, string, React.ReactNode][] = [
  [
    'pill',
    'settings.overlayStyle.pill',
    glyph(
      <>
        <rect x="0.9" y="4.6" width="14.2" height="6.8" rx="3.4" />
        <path d="M5 7.2v1.6M7.4 6.3v3.4M9.8 6.9v2.2M12.2 7.4v1.2" strokeLinecap="round" />
      </>,
    ),
  ],
  [
    'cassette',
    'settings.overlayStyle.cassette',
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
    'settings.overlayStyle.metal',
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
    'settings.overlayStyle.minimal',
    glyph(
      <>
        <rect x="3.4" y="5.4" width="9.2" height="5.2" rx="2.6" />
        <circle cx="6.2" cy="8" r="0.9" fill="currentColor" stroke="none" />
      </>,
    ),
  ],
  [
    'hidden',
    'settings.overlayStyle.none',
    // A dashed outline with the menu-bar dot beside it: nothing is drawn on
    // screen, and the tray icon's dot is the only thing that reports.
    glyph(
      <>
        <rect x="0.9" y="5.4" width="9.6" height="5.2" rx="2.6" strokeDasharray="2 1.6" />
        <circle cx="13.6" cy="8" r="1.5" fill="currentColor" stroke="none" />
      </>,
    ),
  ],
];

// Spoken (transcription) languages, distinct from the UI languages in
// `src/i18n`. [code, label key]: the codes go to the engine, the labels are
// resolved at render.
// The INTERFACE languages, distinct from the SPOKEN ones below: someone can
// run Parle in French and dictate in Japanese, and the spoken list is much
// longer because the model supports far more languages than we have
// translated.
const UI_LANGUAGES = I18N_LANGUAGES;

const LANGUAGES: [string, string][] = [
  ['auto', 'settings.language.auto'],
  ['en', 'settings.language.en'],
  ['es', 'settings.language.es'],
  ['fr', 'settings.language.fr'],
  ['de', 'settings.language.de'],
  ['it', 'settings.language.it'],
  ['pt', 'settings.language.pt'],
  ['nl', 'settings.language.nl'],
  ['ja', 'settings.language.ja'],
  ['ko', 'settings.language.ko'],
  ['zh', 'settings.language.zh'],
  ['hi', 'settings.language.hi'],
  ['ar', 'settings.language.ar'],
  ['ru', 'settings.language.ru'],
  ['pl', 'settings.language.pl'],
  ['sv', 'settings.language.sv'],
];

const ACCENTS = ['#2b5cff', '#e0642f', '#178a50', '#8b5cf6', '#d5382f', '#0d9aa8', '#b06a00', '#d6336c'];

const IS_MAC = navigator.userAgent.includes('Mac');

// Tray/menu-bar icon styles, platform-filtered: the outline variants only make
// sense against a Windows taskbar, and macOS renders "template" as a proper
// template image. Each preview pairs the asset with the backdrop it is drawn
// for, so a white outline is never previewed on white.
const TRAY_STYLES: [string, string, [string, 'light' | 'dark'][]][] = IS_MAC
  ? [
      ['template', 'settings.tray.template', [[trayTemplate, 'light']]],
      ['badge', 'settings.tray.badge', [[trayBadge, 'light']]],
    ]
  : [
      ['badge', 'settings.tray.badge', [[trayBadge, 'light']]],
      [
        'auto',
        'settings.tray.auto',
        [
          [trayDark, 'light'],
          [trayLight, 'dark'],
        ],
      ],
      ['light', 'settings.tray.light', [[trayLight, 'dark']]],
      ['dark', 'settings.tray.dark', [[trayDark, 'light']]],
      ['color', 'settings.tray.color', [[trayColor, 'light']]],
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
  const t = useT();
  const [devices, setDevices] = useState<string[]>([]);
  const [perms, setPerms] = useState<{ accessibility: boolean; microphone: string } | null>(null);

  const [needsRestart, setNeedsRestart] = useState(false);

  const [customPicked, setCustomPicked] = useState(false);
  const [capturing, setCapturing] = useState(false);
  // Clearing history is not a local action once anything is paired: `clear()`
  // writes a tombstone for EVERY unpinned row whoever authored it, and a
  // tombstone is absorbing and travels to every paired device. That is the
  // intended design, it is what stops a cleared password coming back thirty
  // seconds later, and it is exactly why the user has to be told. The paired
  // names are fetched when the button is pressed rather than subscribed to,
  // because this section has no other use for sync state.
  const [confirmClear, setConfirmClear] = useState<string[] | null>(null);

  useEffect(() => {
    api.listAudioDevices().then(setDevices);
    // Poll: permissions can change in System Settings while this page is open.
    const poll = () => api.permissionStatus().then(setPerms);
    poll();
    const id = window.setInterval(poll, 2000);
    return () => window.clearInterval(id);
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
        <h1>{t('settings.title')}</h1>
        <p>{t('settings.subtitle')}</p>
      </header>

      <Section title={t('settings.section.hotkeys')}>
        <Field
          label={t('settings.dictationKey.label')}
          hint={IS_MAC ? t('settings.dictationKey.hintMac') : t('settings.dictationKey.hintWin')}
        >
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
            <option value={CUSTOM}>{t('settings.dictationKey.custom')}</option>
          </select>
        </Field>
        {isCustom && (
          <Field label={t('settings.customBinding.label')} hint={t('settings.customBinding.hint')}>
            <button
              className={`btn key-capture ${capturing ? 'listening' : ''}`}
              onClick={() => setCapturing(true)}
            >
              {capturing ? t('settings.customBinding.listening') : keyLabel(dictationKey)}
            </button>
          </Field>
        )}
        {isCustom && warning && <div className="callout warn">{warning}</div>}
        <Field
          label={t('settings.gesture.label')}
          hint={
            s.hotkeys.dictation.mode === 'double_tap'
              ? t('settings.gesture.hintDoubleTap')
              : t('settings.gesture.hint')
          }
        >
          <div className="seg">
            {(['hold', 'toggle', 'hybrid', 'double_tap'] as const).map((m) => (
              <button
                key={m}
                className={s.hotkeys.dictation.mode === m ? 'active' : ''}
                onClick={() => set((d) => (d.hotkeys.dictation.mode = m))}
              >
                {m === 'hold'
                  ? t('settings.gesture.hold')
                  : m === 'toggle'
                    ? t('settings.gesture.toggle')
                    : m === 'hybrid'
                      ? t('settings.gesture.hybrid')
                      : t('settings.gesture.doubleTap')}
              </button>
            ))}
          </div>
        </Field>
        <Field label={t('settings.latch.label')} hint={t('settings.latch.hint')}>
          <NumberInput value={s.hotkeys.latch_ms} min={150} max={900} step={50} suffix="ms" onChange={(v) => set((d) => (d.hotkeys.latch_ms = v))} />
        </Field>
        <Toggle
          label={t('settings.escCancel.label')}
          hint={t('settings.escCancel.hint')}
          value={s.hotkeys.cancel.enabled}
          onChange={(v) => set((d) => (d.hotkeys.cancel.enabled = v))}
        />
        <Field label={t('settings.historyPalette.label')} hint={t('settings.historyPalette.hint')}>
          <input
            className="key-input"
            value={s.hotkeys.history_palette.key}
            onChange={(e) => set((d) => (d.hotkeys.history_palette.key = e.target.value))}
          />
        </Field>
        {!IS_MAC && (
          <Toggle
            label={t('settings.suppressCopilot.label')}
            hint={t('settings.suppressCopilot.hint')}
            value={s.hotkeys.suppress_copilot}
            onChange={(v) => set((d) => (d.hotkeys.suppress_copilot = v))}
          />
        )}
        {perms && !perms.accessibility && IS_MAC && (
          <div className="callout warn">
            {t('settings.accessibilityMissing')}{' '}
            <button onClick={() => api.requestAccessibility()}>{t('common.grant')}</button>
            <button onClick={() => api.repairAccessibility()}>{t('settings.repairPermission')}</button>
            <button onClick={() => api.openPermissionSettings('accessibility')}>
              {t('common.openSystemSettings')}
            </button>
          </div>
        )}
      </Section>

      <Section title={t('settings.section.language')}>
        <Field label={t('settings.uiLanguage.label')} hint={t('settings.uiLanguage.hint')}>
          <select
            value={s.ui_language || getLang()}
            onChange={(e) => {
              const code = e.target.value as Lang;
              // Applied IMMEDIATELY as well as saved, so the panel the user is
              // looking at changes under them and they can see they picked the
              // right one. Waiting for a reload would make it feel broken.
              setLang(code);
              set((d) => (d.ui_language = code));
            }}
          >
            {UI_LANGUAGES.map((l) => (
              <option key={l.code} value={l.code}>
                {l.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t('settings.spokenLanguage.label')}>
          <select value={s.language.language} onChange={(e) => set((d) => (d.language.language = e.target.value))}>
            {LANGUAGES.map(([code, labelKey]) => (
              <option key={code} value={code}>
                {t(labelKey)}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t('settings.localeSpelling.label')} hint={t('settings.localeSpelling.hint')}>
          <select value={s.language.locale} onChange={(e) => set((d) => (d.language.locale = e.target.value))}>
            <option value="">{t('settings.locale.none')}</option>
            <option value="en-AU">{t('settings.locale.enAU')}</option>
            <option value="en-GB">{t('settings.locale.enGB')}</option>
            <option value="en-US">{t('settings.locale.enUS')}</option>
          </select>
        </Field>
        <Toggle
          label={t('settings.applyLocaleSpelling.label')}
          hint={t('settings.applyLocaleSpelling.hint')}
          value={s.cleanup.locale_spelling}
          onChange={(v) => set((d) => (d.cleanup.locale_spelling = v))}
        />
        <Toggle
          label={t('settings.translate.label')}
          hint={t('settings.translate.hint')}
          value={s.language.translate_to_english}
          onChange={(v) => set((d) => (d.language.translate_to_english = v))}
        />
      </Section>

      <Section title={t('settings.section.cleanup')}>
        <Toggle label={t('settings.smartCleanup.label')} hint={t('settings.smartCleanup.hint')} value={s.cleanup.enabled} onChange={(v) => set((d) => (d.cleanup.enabled = v))} />
        <Toggle label={t('settings.removeFillers.label')} hint={t('settings.removeFillers.hint')} value={s.cleanup.remove_fillers} onChange={(v) => set((d) => (d.cleanup.remove_fillers = v))} />
        <Toggle label={t('settings.removeHedges.label')} hint={t('settings.removeHedges.hint')} value={s.cleanup.remove_hedges} onChange={(v) => set((d) => (d.cleanup.remove_hedges = v))} />
        <Toggle
          label={t('settings.trimSelfCorrections.label')}
          hint={t('settings.trimSelfCorrections.hint')}
          value={s.cleanup.trim_self_corrections}
          onChange={(v) => set((d) => (d.cleanup.trim_self_corrections = v))}
        />
        <Toggle label={t('settings.dictatedPunctuation.label')} hint={t('settings.dictatedPunctuation.hint')} value={s.cleanup.dictated_punctuation} onChange={(v) => set((d) => (d.cleanup.dictated_punctuation = v))} />
        <Toggle label={t('settings.capitalise.label')} value={s.cleanup.capitalise_sentences} onChange={(v) => set((d) => (d.cleanup.capitalise_sentences = v))} />
        <Toggle label={t('settings.terminalPunctuation.label')} value={s.cleanup.ensure_terminal_punctuation} onChange={(v) => set((d) => (d.cleanup.ensure_terminal_punctuation = v))} />
        <Toggle label={t('settings.paragraphPause.label')} value={s.cleanup.paragraph_on_long_pause} onChange={(v) => set((d) => (d.cleanup.paragraph_on_long_pause = v))} />
      </Section>

      <Section title={t('settings.section.dictionary')}>
        <Toggle label={t('settings.dictionary.enable')} value={s.dictionary.enabled} onChange={(v) => set((d) => (d.dictionary.enabled = v))} />
        <Toggle label={t('settings.dictionary.bias.label')} hint={t('settings.dictionary.bias.hint')} value={s.dictionary.bias_recognition} onChange={(v) => set((d) => (d.dictionary.bias_recognition = v))} />
        <Toggle label={t('settings.dictionary.fuzzy.label')} value={s.dictionary.fuzzy_correct} onChange={(v) => set((d) => (d.dictionary.fuzzy_correct = v))} />
        <Toggle label={t('settings.dictionary.autoLearn.label')} hint={t('settings.dictionary.autoLearn.hint')} value={s.dictionary.auto_learn} onChange={(v) => set((d) => (d.dictionary.auto_learn = v))} />
      </Section>

      <Section title={t('settings.section.output')}>
        <Toggle label={t('settings.insertAtCursor.label')} hint={t('settings.insertAtCursor.hint')} value={s.paste.inject} onChange={(v) => set((d) => (d.paste.inject = v))} />
        <Toggle label={t('settings.copyToClipboard.label')} value={s.paste.copy_to_clipboard} onChange={(v) => set((d) => (d.paste.copy_to_clipboard = v))} />
        <Toggle label={t('settings.restoreClipboard.label')} hint={t('settings.restoreClipboard.hint')} value={s.paste.restore_clipboard} onChange={(v) => set((d) => (d.paste.restore_clipboard = v))} />
        <Field label={t('settings.restoreDelay.label')} hint={t('settings.restoreDelay.hint')}>
          <NumberInput value={s.paste.restore_delay_ms} min={200} max={2000} step={100} suffix="ms" onChange={(v) => set((d) => (d.paste.restore_delay_ms = v))} />
        </Field>
        {IS_MAC && (
          <Toggle label={t('settings.preferAxInsert.label')} hint={t('settings.preferAxInsert.hint')} value={s.paste.prefer_ax_insert} onChange={(v) => set((d) => (d.paste.prefer_ax_insert = v))} />
        )}
        <Toggle
          label={t('settings.pressEnter.label')}
          hint={t('settings.pressEnter.hint')}
          value={s.paste.press_enter}
          onChange={(v) => set((d) => (d.paste.press_enter = v))}
        />
      </Section>

      <Section title={t('settings.section.appearance')}>
        <Field label={t('settings.theme.label')}>
          <div className="seg">
            {(['system', 'light', 'dark'] as const).map((m) => (
              <button key={m} className={s.appearance.theme_mode === m ? 'active' : ''} onClick={() => set((d) => (d.appearance.theme_mode = m))}>
                {t(`settings.theme.${m}`)}
              </button>
            ))}
          </div>
        </Field>
        <Field label={t('settings.palette.label')} hint={t('settings.palette.hint')}>
          <div className="seg">
            {(['paper', 'pastel', 'bold', 'retro'] as const).map((p) => (
              <button key={p} className={s.appearance.palette === p ? 'active' : ''} onClick={() => set((d) => (d.appearance.palette = p))}>
                {t(`settings.palette.${p}`)}
              </button>
            ))}
          </div>
        </Field>
        <Field label={t('settings.accent.label')}>
          <div className="accent-row">
            {ACCENTS.map((c) => (
              <button
                key={c}
                className={`accent-dot ${s.appearance.accent === c ? 'active' : ''}`}
                style={{ background: c }}
                onClick={() => set((d) => (d.appearance.accent = c))}
              />
            ))}
            <label className="accent-custom" title={t('settings.accent.custom')}>
              <input
                type="color"
                value={s.appearance.accent}
                onChange={(e) => set((d) => (d.appearance.accent = e.target.value))}
              />
            </label>
          </div>
        </Field>
        <Field label={t('settings.appIcon.label')} hint={t('settings.appIcon.hint')}>
          <div className="icon-picker">
            {APP_ICONS.map(([id, src, labelKey]) => (
              <button
                key={id}
                className={`icon-choice ${s.appearance.app_icon === id ? 'active' : ''}`}
                title={t(labelKey)}
                onClick={() => {
                  set((d) => (d.appearance.app_icon = id));
                  api.setAppIcon(id).then(setNeedsRestart).catch(() => {});
                }}
              >
                <img src={src} alt={t(labelKey)} draggable={false} />
              </button>
            ))}
          </div>
        </Field>
        {needsRestart && (
          <div className="callout warn">
            {t('settings.iconRestart')}{' '}
            <button onClick={() => api.restartApp()}>{t('settings.restartParle')}</button>
          </div>
        )}
        <Field
          label={IS_MAC ? t('settings.trayIcon.labelMac') : t('settings.trayIcon.labelWin')}
          hint={IS_MAC ? t('settings.trayIcon.hintMac') : t('settings.trayIcon.hintWin')}
        >
          <span className="tray-preview">
            {trayPreview.map(([src, bg]) => (
              <span key={src} className="tray-chip" data-bg={bg}>
                <img src={src} alt="" draggable={false} />
              </span>
            ))}
          </span>
          <select value={trayStyle} onChange={(e) => set((d) => (d.appearance.tray_style = e.target.value))}>
            {TRAY_STYLES.map(([value, labelKey]) => (
              <option key={value} value={value}>
                {t(labelKey)}
              </option>
            ))}
          </select>
        </Field>
        <Field
          label={t('settings.overlayStyle.label')}
          hint={
            s.overlay.style === 'hidden'
              ? t('settings.overlayStyle.hintHidden')
              : t('settings.overlayStyle.hint')
          }
        >
          <div className="seg seg-icons">
            {OVERLAY_STYLES.map(([st, labelKey, icon]) => (
              <button key={st} className={s.overlay.style === st ? 'active' : ''} onClick={() => set((d) => (d.overlay.style = st))}>
                {icon}
                {t(labelKey)}
              </button>
            ))}
          </div>
        </Field>
        <Field
          label={t('settings.waveformSensitivity.label')}
          hint={t('settings.waveformSensitivity.hint')}
        >
          <div className="row-inline">
            <input
              type="range"
              min={0.5}
              max={2}
              step={0.1}
              value={s.overlay.waveform_sensitivity}
              onChange={(e) => set((d) => (d.overlay.waveform_sensitivity = Number(e.target.value)))}
            />
            <span className="mono">{s.overlay.waveform_sensitivity.toFixed(1)}x</span>
          </div>
        </Field>
        <Toggle label={t('settings.showPartial.label')} value={s.overlay.show_partial_text} onChange={(v) => set((d) => (d.overlay.show_partial_text = v))} />
        <Toggle label={t('settings.reduceMotion.label')} hint={t('settings.reduceMotion.hint')} value={s.appearance.reduce_motion} onChange={(v) => set((d) => (d.appearance.reduce_motion = v))} />
      </Section>

      <Section title={t('settings.section.historyPrivacy')}>
        <Toggle label={t('settings.clipboardCapture.label')} hint={t('settings.clipboardCapture.hint')} value={s.history.clipboard_capture} onChange={(v) => set((d) => (d.history.clipboard_capture = v))} />
        {/* Narrowing this deletes rows outright and writes NO tombstone, so the
            peer never re-offers them: they are gone from this machine for good
            even while the other one still has them. Widening is safe and is
            repaired automatically (set_retention_days clears the receipts), so
            only the narrowing direction is confirmed. */}
        <Field label={t('settings.retention.label')}>
          <select
            value={s.history.retention_days}
            onChange={(e) => {
              const next = Number(e.target.value);
              const current = s.history.retention_days;
              // 0 is "forever", so it is the WIDEST window, not the narrowest.
              const narrowing = next !== 0 && (current === 0 || next < current);
              if (narrowing && !window.confirm(t('settings.retention.confirmNarrow'))) {
                return;
              }
              set((d) => (d.history.retention_days = next));
            }}
          >
            <option value={0}>{t('settings.retention.forever')}</option>
            <option value={90}>{t('settings.retention.d90')}</option>
            <option value={30}>{t('settings.retention.d30')}</option>
            <option value={7}>{t('settings.retention.d7')}</option>
            <option value={1}>{t('settings.retention.d1')}</option>
          </select>
        </Field>
        <Field label={t('settings.excludedApps.label')} hint={t('settings.excludedApps.hint')}>
          <textarea
            className="excluded-apps"
            value={s.history.excluded_apps.join('\n')}
            onChange={(e) => set((d) => (d.history.excluded_apps = e.target.value.split('\n').map((x) => x.trim()).filter(Boolean)))}
          />
        </Field>
        <Field label={t('settings.dangerZone.label')}>
          {confirmClear === null ? (
            <button
              className="btn danger"
              onClick={() => {
                api
                  .syncStatus()
                  .then((st) => setConfirmClear(st.paired.map((d) => d.name)))
                  .catch(() => setConfirmClear([]));
              }}
            >
              {t('settings.clearHistory.button')}
            </button>
          ) : (
            <div className="callout warn">
              {confirmClear.length > 0
                ? t('settings.clearHistory.confirmWithDevices', { devices: confirmClear.join(', ') })
                : t('settings.clearHistory.confirm')}{' '}
              <button
                className="btn danger"
                onClick={() => {
                  setConfirmClear(null);
                  api.clearHistory().then(() => window.location.reload());
                }}
              >
                {t('settings.clearHistory.clearIt')}
              </button>{' '}
              <button onClick={() => setConfirmClear(null)}>{t('common.keepIt')}</button>
            </div>
          )}
        </Field>
      </Section>


      <Section title={t('settings.section.audio')}>
        <Field label={t('settings.microphone.label')}>
          <select value={s.audio.input_device} onChange={(e) => set((d) => (d.audio.input_device = e.target.value))}>
            <option value="">{t('settings.microphone.systemDefault')}</option>
            {devices.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t('settings.minDuration.label')}>
          <NumberInput value={s.audio.min_duration_ms} min={100} max={2000} step={100} suffix="ms" onChange={(v) => set((d) => (d.audio.min_duration_ms = v))} />
        </Field>
        {perms && perms.microphone === 'denied' && (
          <div className="callout warn">
            {t('settings.microphoneDenied')}{' '}
            <button onClick={() => api.openPermissionSettings('microphone')}>
              {t('common.openSystemSettings')}
            </button>
          </div>
        )}
      </Section>

      <Section title={t('settings.section.general')}>
        <Toggle
          label={t('settings.launchAtLogin.label')}
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
        <Toggle label={t('settings.prewarm.label')} hint={t('settings.prewarm.hint')} value={s.models.prewarm} onChange={(v) => set((d) => (d.models.prewarm = v))} />
      </Section>

      <footer className="settings-footer">
        Parle · {t('settings.footer.tagline')} ·{' '}
        <span className="faint">{t('settings.footer.note')}</span>
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

export function SyncSection({ sync }: { sync: Settings['sync'] }) {
  const t = useT();
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  // Seeded from settings so the toggles render correctly before the first
  // status event; the backend is the source of truth from then on.
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
  // "Sync now". Its own busy flag, not `busy`: that one disables the enable
  // toggle and the kind switches, and a manual exchange has no business
  // freezing the settings around it.
  const [nowBusy, setNowBusy] = useState(false);
  const [nowMsg, setNowMsg] = useState<string | null>(null);
  // One in-flight flag for the actions that had none. Without it the enable
  // toggle, Show a code, Cancel, Unpair and the kind switches could each be
  // fired repeatedly while their command was still running. The backend is
  // hardened against the worst of that, but flicking a kind switch queues a
  // FULL re-offer of the history to every paired device each time, which is
  // not something to let someone do by accident while wondering if it helped.
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  // When we started looking with nothing found. A denied Local Network
  // permission on macOS 14+ is INVISIBLE to us: `Discovery::start` succeeds and
  // browsing simply never resolves anyone, so `scanning` stays true, `error`
  // stays null and the peer list stays empty for ever. That is pixel-identical
  // to "the other machine is off", and the user has no way to tell the two
  // apart or to find the switch that fixes one of them. We cannot detect it, so
  // we do the next best thing: after a while of finding nothing, say what the
  // usual cause is and offer the pane.
  const [emptySince, setEmptySince] = useState<number | null>(null);

  useEffect(() => {
    api
      .syncStatus()
      .then((st) => {
        setStatus(st);
        setKinds({ dictations: st.dictations, clipboard: st.clipboard });
        setLoadError(null);
      })
      .catch((e) => setLoadError(errText(e)));
    const un = onSyncStatus((st) => {
      setStatus(st);
      // The backend owns these; adopting them here keeps the toggles honest
      // even if another window or a failed write changed them.
      setKinds({ dictations: st.dictations, clipboard: st.clipboard });
      setLoadError(null);
      setSeedCode(null);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  const nothingFound = !!status?.enabled && status.peers.length === 0;
  useEffect(() => {
    if (!nothingFound) {
      setEmptySince(null);
      return;
    }
    setEmptySince((t) => t ?? Date.now());
    // Tick so the escalation can appear without needing another status event.
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [nothingFound]);
  const searchedAWhile = emptySince !== null && now - emptySince > 25_000;

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
    const id = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(id);
  }, [expiresAt]);

  async function setEnabled(v: boolean) {
    if (busy) return;
    setBusy(true);
    setActionError(null);
    setStatus((st) => (st ? { ...st, enabled: v } : st));
    try {
      await api.syncSetEnabled(v);
    } catch (e) {
      setActionError(errText(e));
      api.syncStatus().then(setStatus).catch(() => {});
    } finally {
      setBusy(false);
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
      // The backend SANITISES rather than rejects: it strips `=` (the name
      // rides in an mDNS TXT key=value pair), strips invisible and
      // direction-changing characters, and trims to a byte budget. All of that
      // used to happen in silence, so a user typing "Ben=Work" got "BenWork"
      // back with no word about why. Compare and say so.
      const st = await api.syncStatus();
      setStatus(st);
      if (st.device_name !== next) {
        setActionError(t('sync.nameSanitised', { name: st.device_name }));
      }
    } catch (e) {
      setActionError(errText(e));
    }
  }

  async function startPairing() {
    if (busy) return;
    setBusy(true);
    setActionError(null);
    try {
      setSeedCode(await api.syncStartPairing());
    } catch (e) {
      setActionError(errText(e));
    } finally {
      setBusy(false);
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

  async function syncNow() {
    if (nowBusy) return;
    setNowBusy(true);
    setNowMsg(null);
    try {
      const started = await api.syncNow();
      // Zero is a real answer and the one worth saying out loud: it means no
      // paired device is on the network, which is a different problem from a
      // sync that ran and moved nothing.
      setNowMsg(started > 0 ? t('sync.now.ok') : t('sync.now.none'));
    } catch (e) {
      setNowMsg(errText(e));
    } finally {
      setNowBusy(false);
      window.setTimeout(() => setNowMsg(null), 6000);
    }
  }

  async function unpair(id: string) {
    if (busy) return;
    setBusy(true);
    setConfirmUnpair(null);
    setActionError(null);
    try {
      await api.syncUnpair(id);
    } catch (e) {
      setActionError(errText(e));
    } finally {
      setBusy(false);
    }
  }

  async function saveKinds(next: { dictations: boolean; clipboard: boolean }) {
    if (busy) return;
    setBusy(true);
    const prev = kinds;
    setKinds(next);
    setActionError(null);
    try {
      await api.syncSetKinds(next.dictations, next.clipboard);
    } catch (e) {
      setKinds(prev);
      setActionError(errText(e));
    } finally {
      setBusy(false);
    }
  }

  if (loadError) {
    return (
      <Section title={t('sync.section')}>
        <div className="sync-block">
          <div className="callout error">{t('sync.unavailable', { error: loadError })}</div>
        </div>
      </Section>
    );
  }

  if (!status) {
    return (
      <Section title={t('sync.section')}>
        <div className="sync-block">
          <span className="sync-empty">{t('sync.checking')}</span>
        </div>
      </Section>
    );
  }

  const unpaired = status.peers.filter((p) => !status.paired.some((d) => d.id === p.id));
  const selected = unpaired.find((p) => p.id === peerId) ?? null;

  return (
    <Section title={t('sync.section')}>
      <Toggle
        label={t('sync.enable.label')}
        hint={t('sync.enable.hint')}
        value={status.enabled}
        onChange={setEnabled}
      />

      {/* OUTSIDE the `status.enabled` guard, deliberately.
          These two callouts used to live inside it, which made the most
          important one unreachable: `SyncManager::fail` sets `error` AND clears
          `enabled` in the same breath, so a failed port bind, a missing sync
          identity or a listener that would not start wrote the explanation and
          closed the only thing that could show it. The user flipped the switch,
          watched it flip itself back, and got nothing. */}
      {actionError && (
        <div className="sync-block">
          <div className="callout error">
            {actionError} <button onClick={() => setActionError(null)}>{t('common.dismiss')}</button>
          </div>
        </div>
      )}

      {status.error && (
        <div className="sync-block">
          <div className="callout warn">
            {status.error}{' '}
            <button onClick={() => setEnabled(true)} disabled={busy}>
              {t('sync.tryAgain')}
            </button>
          </div>
        </div>
      )}

      {status.enabled && (
        <>
          <Field label={t('sync.thisDevice.label')} hint={t('sync.thisDevice.hint')}>
            <input
              className="sync-name-input"
              value={nameDraft ?? status.device_name}
              placeholder={t('sync.thisDevice.placeholder')}
              onChange={(e) => setNameDraft(e.target.value)}
              onBlur={() => commitName(status.device_name)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
                if (e.key === 'Escape') setNameDraft(null);
              }}
            />
          </Field>
          <Field label={t('sync.deviceId.label')} hint={t('sync.deviceId.hint')}>
            <code className="sync-device-id">{status.device_id}</code>
          </Field>

          <div className="sync-block">
            <div className="sync-paired-head">
              <div className="field-label">
                <span>{t('sync.paired.label')}</span>
                <small>{t('sync.paired.hint')}</small>
              </div>
              {/* Only with something to sync WITH. An exchange button on a
                  machine that has never been paired can do nothing, and a
                  control that cannot act is worse than an absent one.

                  `.btn`, not `.cta`: `.cta` is only ever styled as
                  `.row-actions button.cta`, so outside a history row it renders
                  as an unstyled browser button among properly drawn ones. Plain
                  `.btn` rather than `.btn primary`, because the primary action
                  on this screen is pairing and two competing primaries teach
                  the eye nothing. */}
              {status.paired.length > 0 && (
                <button className="btn" onClick={syncNow} disabled={nowBusy}>
                  <RefreshCw size={14} className={nowBusy ? 'spin' : undefined} />{' '}
                  {nowBusy ? t('sync.now.working') : t('sync.now.button')}
                </button>
              )}
            </div>
            {nowMsg && <div className="sync-now-msg">{nowMsg}</div>}
            {status.paired.length === 0 ? (
              <span className="sync-empty">{t('sync.paired.none')}</span>
            ) : (
              <div className="sync-list">
                {status.paired.map((d) => (
                  <div key={d.id} className="sync-device">
                    {/* The dot follows SYNCING, not mere visibility.
                        It used to follow `online`, which is mDNS presence
                        alone, so a pairing whose key the keychain refuses sat
                        here green and confident saying "Online now" while not a
                        single row moved. Presence is still shown, in words,
                        because "visible but not syncing" is exactly the state a
                        user needs to be able to see. */}
                    <span className={`sync-dot ${d.last_sync_ok ? 'online' : ''}`} />
                    <span className="sync-device-body">
                      {/* The id is shown when two devices present the SAME
                          name. Names come from an unsigned mDNS record and are
                          trimmed to a byte budget, so two distinct machines can
                          collapse to one label; picking the wrong one to trust
                          is the whole pairing decision. */}
                      <span className="sync-device-name">
                        {d.name}
                        {status.paired.filter((o) => o.name === d.name).length > 1 && (
                          <code className="sync-device-id"> {d.id.slice(0, 8)}</code>
                        )}
                      </span>
                      <span className="sync-device-meta">
                        {d.last_sync_ok
                          ? t('sync.syncedAgo', { when: agoLabel(d.last_sync_ok) })
                          : d.online
                            ? t('sync.visibleNotSynced')
                            : lastSeenLabel(d.last_seen)}
                      </span>
                    </span>
                    {confirmUnpair === d.id ? (
                      <span className="sync-confirm">
                        {/* Says what unpairing does NOT do, deliberately. It
                            destroys the key and stops future exchanges; it
                            writes no tombstones, so everything already on that
                            device stays there. Someone unpairing a laptop they
                            have just handed back needs to know that is not a
                            wipe. */}
                        <span>{t('sync.unpairConfirm', { name: d.name })}</span>
                        <button className="btn ghost" onClick={() => setConfirmUnpair(null)}>
                          {t('common.keepIt')}
                        </button>
                        <button className="btn danger" onClick={() => unpair(d.id)}>
                          {t('sync.unpair')}
                        </button>
                      </span>
                    ) : (
                      <button className="btn ghost" onClick={() => setConfirmUnpair(d.id)}>
                        {t('sync.unpair')}
                      </button>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>

          <div className="sync-block">
            <div className="field-label">
              <span>{t('sync.pairNew.label')}</span>
              <small>{t('sync.pairNew.hint')}</small>
            </div>
            <div className="seg">
              {(['show', 'enter'] as const).map((m) => (
                <button key={m} className={direction === m ? 'active' : ''} onClick={() => setDirection(m)}>
                  {m === 'show' ? t('sync.direction.show') : t('sync.direction.enter')}
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
                      ? t('sync.code.typeOnOther', { time: fmtCountdown(remaining) })
                      : t('sync.code.expired')}
                  </div>
                  {remaining > 0 ? (
                    <button className="btn" onClick={cancelPairing}>
                      {t('common.cancel')}
                    </button>
                  ) : (
                    <button className="btn primary" onClick={startPairing}>
                      {t('sync.code.showNew')}
                    </button>
                  )}
                </div>
              ) : (
                <div className="sync-showing">
                  <span className="sync-empty">{t('sync.code.explain')}</span>
                  <button className="btn primary" onClick={startPairing}>
                    {t('sync.direction.show')}
                  </button>
                </div>
              )
            ) : (
              <>
                {unpaired.length === 0 ? (
                  <span className="sync-empty">
                    {!status.scanning ? (
                      t('sync.peers.notSearching')
                    ) : searchedAWhile ? (
                      <>
                        {t('sync.peers.stillNothing')}{' '}
                        {IS_MAC ? (
                          <>
                            {t('sync.peers.macBlocked')}{' '}
                            <button onClick={() => api.openPermissionSettings('local-network')}>
                              {t('sync.peers.openLocalNetwork')}
                            </button>
                          </>
                        ) : (
                          <>
                            {t('sync.peers.winBlocked')}{' '}
                            <button onClick={() => api.openPermissionSettings('local-network')}>
                              {t('sync.peers.openFirewall')}
                            </button>
                          </>
                        )}
                        {/* The two causes that are INVISIBLE from inside the app.
                            Both were hit on the first real two-machine test, at
                            the same time, and neither shows up as an error: the
                            permission is granted, the firewall rule is there, the
                            listener is bound, and the peer list is simply empty
                            for ever. Named here rather than in a help page
                            because this empty list is where someone is actually
                            standing when it happens to them. */}
                        <span className="sync-hint-extra">
                          {t('sync.peers.vpnHint')} {t('sync.peers.isolatedHint')}
                        </span>
                      </>
                    ) : (
                      t('sync.peers.looking')
                    )}
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
                        <span className="sync-peer-name">
                          {p.name}
                          {unpaired.filter((o) => o.name === p.name).length > 1 && (
                            <code className="sync-device-id"> {p.id.slice(0, 8)}</code>
                          )}
                        </span>
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
                    placeholder="000000"
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
                    {pairBusy ? t('sync.pairing') : t('sync.pair')}
                  </button>
                </div>
                {/* Why the button is dead, said out loud.
                    Reported from the first real pairing attempt: with no device
                    discovered there is nothing to select, so Pair stays greyed
                    however correct the six digits are, and the user reads it as
                    the button being broken. A disabled control that gives no
                    reason is the failure being described. Only shown once the
                    code is complete, so it appears exactly when the user is
                    waiting for something to happen and nothing does. */}
                {!selected && code.length === 6 && (
                  <div className="sync-now-msg">{t('sync.pair.needsDevice')}</div>
                )}
                {pairError && <div className="callout error">{pairError}</div>}
              </>
            )}
          </div>

          {/* Turning either of these back ON is not a light switch: it owes
              every paired device a full re-offer of this machine's history and
              clears every receipt so the peers re-offer theirs. The hint says so
              only when there is actually a paired device to re-send to. */}
          <Toggle
            label={t('sync.dictations.label')}
            hint={
              status.paired.length > 0
                ? t('sync.dictations.hintPaired')
                : t('sync.dictations.hint')
            }
            value={kinds.dictations}
            onChange={(v) => saveKinds({ ...kinds, dictations: v })}
          />
          <Toggle
            label={t('sync.clipboard.label')}
            hint={
              status.paired.length > 0 ? t('sync.clipboard.hintPaired') : t('sync.clipboard.hint')
            }
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
  return t('sync.genericError');
}

// Timestamps may arrive as epoch seconds or milliseconds; accept both.
function toMillis(t: number): number {
  return t > 1e11 ? t : t * 1000;
}

function fmtCountdown(secs: number): string {
  const m = Math.floor(secs / 60);
  return m > 0 ? `${m}:${String(secs % 60).padStart(2, '0')}` : t('time.secondsShort', { n: secs });
}

/// "just now", "5m ago", "3h ago", "12 Mar". The bare phrase, so callers can
/// put their own verb in front of it.
function agoLabel(at: number): string {
  const ms = toMillis(at);
  const s = Math.floor((Date.now() - ms) / 1000);
  if (s < 60) return t('time.justNow');
  if (s < 3600) return t('time.minutesAgo', { n: Math.floor(s / 60) });
  if (s < 86400) return t('time.hoursAgo', { n: Math.floor(s / 3600) });
  return new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
}

function lastSeenLabel(at: number | null): string {
  if (at == null) return t('sync.neverConnected');
  return t('sync.lastSeen', { when: agoLabel(at) });
}

function keyLabel(k: string): string {
  if (k.includes('+')) return k.split('+').map(chordPartLabel).join(' + ');
  const map: Record<string, string> = {
    Fn: 'keys.fn',
    RightCommand: 'keys.rightCommand',
    LeftCommand: 'keys.leftCommand',
    RightOption: 'keys.rightOption',
    LeftOption: 'keys.leftOption',
    RightControl: 'keys.rightControl',
    LeftControl: 'keys.leftControl',
    CopilotKey: 'keys.copilot',
    RightCtrl: 'keys.rightCtrl',
    LeftCtrl: 'keys.leftCtrl',
    RightShift: 'keys.rightShift',
    LeftShift: 'keys.leftShift',
    LeftAlt: 'keys.leftAlt',
    RightAlt: 'keys.rightAlt',
    RightWin: 'keys.rightWin',
    LeftWin: 'keys.leftWin',
  };
  return map[k] ? t(map[k]) : k;
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
  if (key === 'LeftCtrl' || key === 'LeftControl') return t('settings.bindingWarning.leftCtrl');
  if (key === 'LeftShift') return t('settings.bindingWarning.leftShift');
  if (key === 'RightAlt' || key === 'RightOption') return t('settings.bindingWarning.rightAlt');
  return null;
}
