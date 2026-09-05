import { useCallback, useEffect, useRef, useState } from 'react';
import { AudioLines, BookA, Cpu, History as HistoryIcon, MonitorSmartphone, Settings as SettingsIcon } from 'lucide-react';
import appIcon from './assets/icon.png';
import { api, onPipelineEvent } from './api';
import { PASTE_KEYS } from './types';
import { applyLang } from './i18n/apply';
import DictationBar from './DictationBar';
import type { DictationMode, Mark, PipelineEvent, Settings } from './types';
import { onFocusPalette } from './api';
import { useT } from './i18n/useT';
import HistoryView from './views/History';
import ComposeView from './views/Compose';
import ModelsView from './views/Models';
import DictionaryView from './views/Dictionary';
import SettingsView from './views/SettingsView';
import SyncView from './views/Sync';
import Onboarding from './views/Onboarding';
import './app.css';

type Tab = 'history' | 'compose' | 'dictionary' | 'models' | 'sync' | 'settings';

export default function App() {
  const t = useT();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [tab, setTab] = useState<Tab>('compose');
  const [toast, setToast] = useState<{ text: string; kind: 'ok' | 'error' } | null>(null);
  const [recording, setRecording] = useState(false);
  // Which key started the live take. The dictation bar and Compose take the
  // Refine accent while a Refine take runs.
  const [mode, setMode] = useState<DictationMode>('standard');
  // Held here, not in Compose. Marks arrive whenever the dictation bar sends
  // one, and Compose is unmounted whenever you are on another tab: keeping the
  // list in the view meant switching to Compose to check what you had pinned
  // showed an empty list, which is exactly the trip the bar exists to save.
  const [marks, setMarks] = useState<Mark[]>([]);
  // The shape the dictation bar grows out of and collapses back into.
  const recordBtnRef = useRef<HTMLButtonElement>(null);

  const reload = useCallback(() => {
    api.getSettings().then((s) => {
      applyTheme(s);
      applyLang(s);
      setSettings(s);
    });
  }, []);

  useEffect(() => {
    reload();
    api.pipelineState().then((st) => {
      setRecording(st.state === 'recording');
      setMode(st.mode);
    });
    const un = onPipelineEvent((e: PipelineEvent) => {
      if (e.kind === 'mode_changed') setMode(e.mode);
      if (e.kind === 'state_changed') {
        setRecording(e.state === 'recording');
        setMode(e.mode);
        if (e.state === 'recording') setMarks([]);
      }
      if (e.kind === 'mark_added') setMarks((m) => [...m, { at_ms: e.at_ms, text: e.text }]);
      if (e.kind === 'completed') {
        // A withheld dictation has already had its own toast from the `empty`
        // event, and `e.text` is the thing we withheld. Rendering a preview of
        // it here overwrote the explanation with the first 42 characters of the
        // password.
        // Same rule as the HUD: withhold the transcript, keep the
        // instruction.
        if (e.withheld && !e.injection?.manual_paste_required) return;
        if (e.withheld) {
          showToast(t('app.toast.pasteInstruction', { keys: PASTE_KEYS }), 'ok');
          return;
        }
        const preview = e.text.length > 42 ? e.text.slice(0, 42) + '…' : e.text;
        showToast(
          e.injection?.manual_paste_required
            // Round 12 fixed both halves of this sentence in the HUD and
            // wrote down why; neither round 12 nor round 13 carried it to the
            // identical literal here. It named no key on either platform, and
            // asserted "(secure field)" on a path where the field may be known
            // ordinary and only a password manager is running.
            ? t('app.toast.pasteInstruction', { keys: PASTE_KEYS })
            : e.refined
              ? e.injection
                ? t('app.toast.refinedInserted', { text: preview })
                : t('app.toast.refinedCopied', { text: preview })
              : e.injection
                ? t('app.toast.inserted', { text: preview })
                : t('app.toast.copied', { text: preview }),
          'ok',
        );
      }
      if (e.kind === 'empty') showToast(e.reason, 'ok');
      if (e.kind === 'error') showToast(e.message, 'error');
    });
    const unPalette = onFocusPalette(() => setTab('history'));
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    const onScheme = () => api.getSettings().then(applyTheme);
    media.addEventListener('change', onScheme);
    return () => {
      un.then((f) => f());
      unPalette.then((f) => f());
      media.removeEventListener('change', onScheme);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The previous toast's timer is cleared first: left running, it dismissed
  // the NEXT toast early, which for an error following a success cut the
  // error's six seconds down to whatever the success had left.
  const toastTimer = useRef<number | null>(null);
  function showToast(text: string, kind: 'ok' | 'error') {
    if (toastTimer.current !== null) window.clearTimeout(toastTimer.current);
    setToast({ text, kind });
    toastTimer.current = window.setTimeout(() => {
      toastTimer.current = null;
      setToast(null);
    }, kind === 'error' ? 6000 : 2800);
  }

  const save = useCallback(
    async (next: Settings) => {
      applyTheme(next);
      setSettings(next);
      await api.setSettings(next);
    },
    [],
  );

  if (!settings) return null;
  if (!settings.onboarding_complete) {
    return <Onboarding onDone={reload} />;
  }

  // The Refine accent is scoped to the bar and Compose while a Refine take
  // runs; the rest of the window keeps the ordinary accent.
  const takeAccent = recording && mode === 'refine' ? settings.refine.accent : undefined;

  return (
    <div className={`app ${recording ? 'has-bar' : ''}`} data-dictation-mode={recording ? mode : ''}>
      <aside className="sidebar">
        <div className="brand">
          <img className="brand-mark-img" src={appIcon} alt="" draggable={false} />
          <span>Parle</span>
        </div>
        <nav>
          <NavItem icon={<AudioLines size={17} />} label={t('app.nav.compose')} active={tab === 'compose'} onClick={() => setTab('compose')} />
          <NavItem icon={<HistoryIcon size={17} />} label={t('app.nav.history')} active={tab === 'history'} onClick={() => setTab('history')} />
          <NavItem icon={<BookA size={17} />} label={t('app.nav.dictionary')} active={tab === 'dictionary'} onClick={() => setTab('dictionary')} />
          <NavItem icon={<Cpu size={17} />} label={t('app.nav.models')} active={tab === 'models'} onClick={() => setTab('models')} />
          {/* Before Settings, not inside it. Sync is a place with live state
              to watch, not a preference to set once. */}
          <NavItem icon={<MonitorSmartphone size={17} />} label={t('app.nav.sync')} active={tab === 'sync'} onClick={() => setTab('sync')} />
          <NavItem icon={<SettingsIcon size={17} />} label={t('app.nav.settings')} active={tab === 'settings'} onClick={() => setTab('settings')} />
        </nav>
        {/* Stowed, not hidden: while a recording runs this button has become
            the dictation bar at the bottom of the window, which carries stop.
            Leaving it in the layout keeps the sidebar from jumping. */}
        <button
          ref={recordBtnRef}
          className={`record-btn ${recording ? 'stowed' : ''}`}
          onClick={() => api.startRecording()}
          inert={recording}
        >
          <span className="record-dot" />
          {t('app.record.start')}
        </button>
      </aside>
      <div className="stage">
        <main className={`content ${recording ? 'with-bar' : ''}`}>
          {tab === 'history' && <HistoryView />}
          {tab === 'compose' && (
            <ComposeView
              marks={marks}
              refineEnabled={settings.refine.enabled}
              refineAccent={settings.refine.accent}
            />
          )}
          {tab === 'dictionary' && <DictionaryView />}
          {tab === 'models' && <ModelsView />}
          {tab === 'sync' && <SyncView settings={settings} />}
          {tab === 'settings' && <SettingsView settings={settings} onSave={save} />}
        </main>
      </div>
      <DictationBar
        recording={recording}
        marks={marks}
        originRef={recordBtnRef}
        onOpenCompose={() => setTab('compose')}
        accent={takeAccent}
        refine={recording && mode === 'refine'}
      />
      {toast && <div className={`toast toast-${toast.kind}`}>{toast.text}</div>}
    </div>
  );
}

function NavItem({
  icon,
  label,
  active,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button className={`nav-item ${active ? 'active' : ''}`} onClick={onClick}>
      {icon}
      <span>{label}</span>
    </button>
  );
}

export function applyTheme(s: Settings): Settings {
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
