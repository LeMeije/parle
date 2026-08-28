// First-run flow: welcome -> permissions -> model download -> hotkey -> test.

import { Fragment, useEffect, useState } from 'react';
import { getLang, setLang, LANGUAGES, TRANSCRIPTION_DEFAULT, type Lang } from '../i18n';
import { Check, ChevronRight, Download, KeyRound, Mic, ShieldCheck, Sparkles } from 'lucide-react';
import { api, onDownloadComplete, onDownloadError, onDownloadProgress, onPipelineEvent } from '../api';
import type { DownloadProgress, PermissionStatus } from '../types';
import { useT } from '../i18n/useT';

// Renders a translated sentence containing {tokens}, substituting React nodes
// at the tokens' positions. The whole sentence stays a single translatable
// string, so word order remains the translator's to choose rather than being
// fixed by concatenation here.
function rich(text: string, nodes: Record<string, React.ReactNode>): React.ReactNode {
  const out: React.ReactNode[] = [];
  const re = /\{(\w+)\}/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const node = nodes[m[1]];
    if (node === undefined) continue;
    if (m.index > last) out.push(text.slice(last, m.index));
    out.push(<Fragment key={out.length}>{node}</Fragment>);
    last = m.index + m[0].length;
  }
  out.push(text.slice(last));
  return <>{out}</>;
}

const IS_MAC = navigator.userAgent.includes('Mac');

type Step = 'language' | 'welcome' | 'permissions' | 'model' | 'hotkey' | 'test';

export default function Onboarding({ onDone }: { onDone: () => void }) {
  const [step, setStep] = useState<Step>('language');
  return (
    <div className="onboarding">
      <div className="ob-card">
        {step === 'language' && <LanguageStep onNext={() => setStep('welcome')} />}
        {step === 'welcome' && <Welcome onNext={() => setStep('permissions')} />}
        {step === 'permissions' && <Permissions onNext={() => setStep('model')} />}
        {step === 'model' && <ModelStep onNext={() => setStep('hotkey')} />}
        {step === 'hotkey' && <HotkeyStep onNext={() => setStep('test')} />}
        {step === 'test' && <TestStep onDone={onDone} />}
      </div>
      <div className="ob-steps">
        {(['language', 'welcome', 'permissions', 'model', 'hotkey', 'test'] as Step[]).map((s) => (
          <span key={s} className={`ob-dot ${s === step ? 'active' : ''}`} />
        ))}
      </div>
    </div>
  );
}

function Welcome({ onNext }: { onNext: () => void }) {
  const t = useT();
  return (
    <>
      <div className="ob-icon">
        <Mic size={30} strokeWidth={2} />
      </div>
      <h1>{t('onboarding.welcome.title')}</h1>
      <p>{t('onboarding.welcome.body')}</p>
      <button className="btn primary" onClick={onNext}>
        {t('onboarding.welcome.cta')} <ChevronRight size={15} />
      </button>
    </>
  );
}

function Permissions({ onNext }: { onNext: () => void }) {
  const t = useT();
  const [perms, setPerms] = useState<PermissionStatus | null>(null);

  useEffect(() => {
    const poll = () => api.permissionStatus().then(setPerms);
    poll();
    const id = window.setInterval(poll, 1500);
    return () => window.clearInterval(id);
  }, []);

  const micOk = perms?.microphone === 'granted';
  const micUnknown = perms?.microphone === 'unknown';
  const axOk = perms?.accessibility ?? false;
  // "unknown" = the status API is unavailable on this build; don't dead-end
  // the user — recording itself will surface a real failure if any.
  // Windows genuinely reports mic consent, and it can be "denied". Waving the
  // user through meant onboarding completed and then every dictation failed
  // with "Could not start microphone". Only the PROMPT is macOS-only, and that
  // is handled separately below.
  const allOk = IS_MAC ? (micOk || micUnknown) && axOk : micOk || micUnknown;

  return (
    <>
      <div className="ob-icon">
        <ShieldCheck size={30} strokeWidth={2} />
      </div>
      <h1>{t('onboarding.permissions.title')}</h1>
      <p>{IS_MAC ? t('onboarding.permissions.introMac') : t('onboarding.permissions.introWin')}</p>
      <div className="ob-perms">
        <PermRow
          ok={micOk}
          title={t('onboarding.permissions.microphone')}
          desc={t('onboarding.permissions.microphoneDesc')}
          action={() => api.requestMicrophone()}
          settingsAction={() => api.openPermissionSettings('microphone')}
        />
        {IS_MAC && (
          <PermRow
            ok={axOk}
            title={t('onboarding.permissions.accessibility')}
            desc={t('onboarding.permissions.accessibilityDesc')}
            action={() => api.requestAccessibility()}
            settingsAction={() => api.openPermissionSettings('accessibility')}
          />
        )}
      </div>
      {IS_MAC && !axOk && (
        <p className="ob-note">
          {rich(t('onboarding.permissions.macNote'), {
            app: <strong>{t('onboarding.permissions.appName')}</strong>,
          })}
        </p>
      )}
      <button className="btn primary" disabled={!allOk} onClick={onNext}>
        {allOk ? t('onboarding.permissions.continue') : t('onboarding.permissions.waiting')}{' '}
        <ChevronRight size={15} />
      </button>
    </>
  );
}

function PermRow({
  ok,
  title,
  desc,
  action,
  settingsAction,
}: {
  ok: boolean;
  title: string;
  desc: string;
  action: () => void;
  settingsAction: () => void;
}) {
  const t = useT();
  return (
    <div className={`ob-perm ${ok ? 'ok' : ''}`}>
      <div className="ob-perm-status">{ok ? <Check size={16} /> : <span className="ob-perm-dot" />}</div>
      <div className="ob-perm-text">
        <strong>{title}</strong>
        <small>{desc}</small>
      </div>
      {!ok && (
        <div className="ob-perm-actions">
          {/* Windows has no consent prompt for unpackaged apps — Settings is
              the only place the grant can be made, so don't offer a dead button. */}
          {IS_MAC && (
            <button className="btn" onClick={action}>
              {t('common.grant')}
            </button>
          )}
          <button className="btn ghost" onClick={settingsAction}>
            {t('onboarding.permissions.openSettings')}
          </button>
        </div>
      )}
    </div>
  );
}

function ModelStep({ onNext }: { onNext: () => void }) {
  const t = useT();
  const [rec, setRec] = useState<{ model: string; profile: { total_ram_mb: number; gpu: string } } | null>(null);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [done, setDone] = useState(false);
  const [started, setStarted] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.recommendedSetup().then((r) => {
      setRec(r);
      // Already downloaded? Skip straight through.
      api.listModels().then((models) => {
        if (models.find((m) => m.id === r.model)?.downloaded) setDone(true);
      });
    });
    const un1 = onDownloadProgress(setProgress);
    const un2 = onDownloadComplete(() => setDone(true));
    const un3 = onDownloadError((msg) => {
      setError(msg);
      setStarted(false);
    });
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
    };
  }, []);

  const pct = progress && progress.total > 0 ? Math.round((progress.downloaded / progress.total) * 100) : 0;

  return (
    <>
      <div className="ob-icon">
        <Download size={30} strokeWidth={2} />
      </div>
      <h1>{t('onboarding.model.title')}</h1>
      <p>
        {rich(
          t('onboarding.model.recommendation', {
            machine: rec
              ? t('onboarding.model.machine', {
                  ram: Math.round(rec.profile.total_ram_mb / 1024),
                  gpu: rec.profile.gpu,
                })
              : '…',
          }),
          { model: <strong>{rec?.model.replace('whisper-', '') ?? '…'}</strong> },
        )}
      </p>
      {error && (
        <div className="callout error">{t('onboarding.model.downloadFailed', { error })}</div>
      )}
      {done ? (
        <button className="btn primary" onClick={onNext}>
          {t('onboarding.model.ready')} <ChevronRight size={15} />
        </button>
      ) : started ? (
        <div className="dl-progress big">
          <div className="dl-bar" style={{ width: `${pct}%` }} />
          <span>{pct}%</span>
        </div>
      ) : (
        <button
          className="btn primary"
          disabled={!rec}
          onClick={() => {
            if (rec) {
              setStarted(true);
              api.downloadModel(rec.model);
            }
          }}
        >
          {t('onboarding.model.download')} <ChevronRight size={15} />
        </button>
      )}
    </>
  );
}

function HotkeyStep({ onNext }: { onNext: () => void }) {
  const t = useT();
  return (
    <>
      <div className="ob-icon">
        <KeyRound size={30} strokeWidth={2} />
      </div>
      <h1>{t('onboarding.hotkey.title')}</h1>
      {IS_MAC ? (
        <p>
          {rich(t('onboarding.hotkey.mac'), {
            key: <strong>{t('onboarding.hotkey.macKey')}</strong>,
            doNothing: <strong>{t('onboarding.hotkey.doNothing')}</strong>,
          })}
        </p>
      ) : (
        <p>
          {rich(t('onboarding.hotkey.win'), {
            key: <strong>{t('onboarding.hotkey.winKey')}</strong>,
          })}
        </p>
      )}
      <button className="btn primary" onClick={onNext}>
        {t('onboarding.hotkey.cta')} <ChevronRight size={15} />
      </button>
    </>
  );
}

function TestStep({ onDone }: { onDone: () => void }) {
  const t = useT();
  const [result, setResult] = useState<string | null>(null);
  const [recording, setRecording] = useState(false);
  const [finishing, setFinishing] = useState(false);

  useEffect(() => {
    // Complete onboarding NOW so the pipeline (hotkeys, prewarm) is armed for the test.
    api.completeOnboarding();
    const un = onPipelineEvent((e) => {
      if (e.kind === 'state_changed') setRecording(e.state === 'recording');
      if (e.kind === 'completed') {
        // Withheld rows are never rendered. The event reaches every window.
        // `null`, not `''`: this view renders empty as "No speech detected"
        // and adds "Try again a little louder", which is an instruction to
        // re-dictate a password.
        setResult(e.withheld ? null : e.text);
        setFinishing(false);
      }
      if (e.kind === 'empty') {
        setResult('');
        setFinishing(false);
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  return (
    <>
      <div className="ob-icon">
        <Sparkles size={30} strokeWidth={2} />
      </div>
      <h1>{t('onboarding.test.title')}</h1>
      <p>{t('onboarding.test.body')}</p>
      <button
        className={`btn ${recording ? 'danger' : 'primary'}`}
        onClick={() => {
          if (recording) {
            setFinishing(true);
            api.stopRecording();
          } else {
            setResult(null);
            api.startRecording();
          }
        }}
      >
        {recording
          ? t('onboarding.test.stop')
          : finishing
            ? t('onboarding.test.transcribing')
            : t('onboarding.test.start')}
      </button>
      {result !== null && (
        <div className={`ob-result ${result ? '' : 'faint'}`}>
          {result || t('onboarding.test.noSpeech')}
        </div>
      )}
      <button className="btn ghost" onClick={onDone}>
        {t('onboarding.test.finish')}
      </button>
    </>
  );
}


/// First run, first question: which language.
///
/// Before anything else, because every screen after it is written in the
/// answer. Choosing here also seeds the TRANSCRIPTION language, since picking
/// an interface language is the clearest signal we ever get about what someone
/// is likely to dictate in, and asking the same question twice on first launch
/// is worse than defaulting and letting them change it.
function LanguageStep({ onNext }: { onNext: () => void }) {
  const t = useT();
  const [picked, setPicked] = useState<Lang>(getLang());

  // Live preview: switching the radio switches this screen, so the user can
  // see they have chosen the right one before committing.
  function choose(code: Lang) {
    setPicked(code);
    setLang(code);
  }

  async function confirm() {
    const s = await api.getSettings();
    s.ui_language = picked;
    s.language.language = TRANSCRIPTION_DEFAULT[picked] ?? 'auto';
    await api.setSettings(s);
    onNext();
  }

  return (
    <>
      <h1>{t('onboarding.language.title')}</h1>
      <p className="ob-sub">{t('onboarding.language.sub')}</p>
      <div className="ob-langs">
        {LANGUAGES.map((l) => (
          <button
            key={l.code}
            className={`ob-lang ${picked === l.code ? 'active' : ''}`}
            onClick={() => choose(l.code)}
          >
            <span className="ob-lang-name">{l.label}</span>
            <span className="ob-lang-en">{l.english}</span>
          </button>
        ))}
      </div>
      <p className="hint">{t('onboarding.language.note')}</p>
      <button className="btn primary" onClick={confirm}>
        {t('onboarding.permissions.continue')}
      </button>
    </>
  );
}
