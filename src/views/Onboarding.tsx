// First-run flow: welcome -> permissions -> model download -> hotkey -> test.

import { useEffect, useState } from 'react';
import { Check, ChevronRight, Download, KeyRound, Mic, ShieldCheck, Sparkles } from 'lucide-react';
import { api, onDownloadComplete, onDownloadProgress, onPipelineEvent } from '../api';
import type { DownloadProgress, PermissionStatus } from '../types';

const IS_MAC = navigator.userAgent.includes('Mac');

type Step = 'welcome' | 'permissions' | 'model' | 'hotkey' | 'test';

export default function Onboarding({ onDone }: { onDone: () => void }) {
  const [step, setStep] = useState<Step>('welcome');
  return (
    <div className="onboarding">
      <div className="ob-card">
        {step === 'welcome' && <Welcome onNext={() => setStep('permissions')} />}
        {step === 'permissions' && <Permissions onNext={() => setStep('model')} />}
        {step === 'model' && <ModelStep onNext={() => setStep('hotkey')} />}
        {step === 'hotkey' && <HotkeyStep onNext={() => setStep('test')} />}
        {step === 'test' && <TestStep onDone={onDone} />}
      </div>
      <div className="ob-steps">
        {(['welcome', 'permissions', 'model', 'hotkey', 'test'] as Step[]).map((s) => (
          <span key={s} className={`ob-dot ${s === step ? 'active' : ''}`} />
        ))}
      </div>
    </div>
  );
}

function Welcome({ onNext }: { onNext: () => void }) {
  return (
    <>
      <div className="ob-icon">
        <Mic size={30} strokeWidth={2} />
      </div>
      <h1>Welcome to EchoKey</h1>
      <p>
        Hold a key, speak, release — your words appear where your cursor is. Transcription runs entirely
        on this device. Nothing you say ever leaves it.
      </p>
      <button className="btn primary" onClick={onNext}>
        Set up <ChevronRight size={15} />
      </button>
    </>
  );
}

function Permissions({ onNext }: { onNext: () => void }) {
  const [perms, setPerms] = useState<PermissionStatus | null>(null);

  useEffect(() => {
    const poll = () => api.permissionStatus().then(setPerms);
    poll();
    const t = window.setInterval(poll, 1500);
    return () => window.clearInterval(t);
  }, []);

  const micOk = perms?.microphone === 'granted';
  const axOk = perms?.accessibility ?? false;
  const allOk = IS_MAC ? micOk && axOk : true;

  return (
    <>
      <div className="ob-icon">
        <ShieldCheck size={30} strokeWidth={2} />
      </div>
      <h1>Permissions</h1>
      <p>EchoKey needs two grants to hear you and type for you. Both stay on this machine.</p>
      <div className="ob-perms">
        <PermRow
          ok={micOk}
          title="Microphone"
          desc="To hear your dictation"
          action={() => {
            // Trigger the system prompt by opening a capture stream briefly.
            api.startRecording().then(() => setTimeout(() => api.cancelRecording(), 400));
          }}
          settingsAction={() => api.openPermissionSettings('microphone')}
        />
        {IS_MAC && (
          <PermRow
            ok={axOk}
            title="Accessibility"
            desc="To watch your hotkey and paste at the cursor"
            action={() => api.openPermissionSettings('accessibility')}
            settingsAction={() => api.openPermissionSettings('accessibility')}
          />
        )}
      </div>
      {IS_MAC && !axOk && (
        <p className="ob-note">
          In System Settings, add <strong>EchoKey</strong> under Privacy &amp; Security → Accessibility, then
          come back — this page updates by itself. A restart of EchoKey may be needed after granting.
        </p>
      )}
      <button className="btn primary" disabled={!allOk} onClick={onNext}>
        {allOk ? 'Continue' : 'Waiting for permissions…'} <ChevronRight size={15} />
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
  return (
    <div className={`ob-perm ${ok ? 'ok' : ''}`}>
      <div className="ob-perm-status">{ok ? <Check size={16} /> : <span className="ob-perm-dot" />}</div>
      <div className="ob-perm-text">
        <strong>{title}</strong>
        <small>{desc}</small>
      </div>
      {!ok && (
        <div className="ob-perm-actions">
          <button className="btn" onClick={action}>
            Grant
          </button>
          <button className="btn ghost" onClick={settingsAction}>
            Open Settings
          </button>
        </div>
      )}
    </div>
  );
}

function ModelStep({ onNext }: { onNext: () => void }) {
  const [rec, setRec] = useState<{ model: string; profile: { total_ram_mb: number; gpu: string } } | null>(null);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [done, setDone] = useState(false);
  const [started, setStarted] = useState(false);

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
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
    };
  }, []);

  const pct = progress && progress.total > 0 ? Math.round((progress.downloaded / progress.total) * 100) : 0;

  return (
    <>
      <div className="ob-icon">
        <Download size={30} strokeWidth={2} />
      </div>
      <h1>Your model</h1>
      <p>
        Based on this machine ({rec ? `${Math.round(rec.profile.total_ram_mb / 1024)} GB RAM, ${rec.profile.gpu}` : '…'}),
        we recommend <strong>{rec?.model.replace('whisper-', '') ?? '…'}</strong>. You can add or switch models
        any time in Settings → Models.
      </p>
      {done ? (
        <button className="btn primary" onClick={onNext}>
          Model ready <ChevronRight size={15} />
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
          Download <ChevronRight size={15} />
        </button>
      )}
    </>
  );
}

function HotkeyStep({ onNext }: { onNext: () => void }) {
  return (
    <>
      <div className="ob-icon">
        <KeyRound size={30} strokeWidth={2} />
      </div>
      <h1>Your key</h1>
      {IS_MAC ? (
        <p>
          Default: the <strong>🌐 Fn key</strong>. Hold it and talk, release to paste — or tap it quickly to
          latch recording on. Tip: set System Settings → Keyboard → “Press 🌐 key to” to{' '}
          <strong>Do Nothing</strong> so macOS dictation doesn't fight for it.
        </p>
      ) : (
        <p>
          Default: <strong>Right Ctrl</strong>. Hold it and talk, release to paste — or tap it quickly to
          latch recording on. Have a Copilot key? Bind it in Settings → Hotkeys and EchoKey will take it
          over completely.
        </p>
      )}
      <button className="btn primary" onClick={onNext}>
        Got it <ChevronRight size={15} />
      </button>
    </>
  );
}

function TestStep({ onDone }: { onDone: () => void }) {
  const [result, setResult] = useState<string | null>(null);
  const [recording, setRecording] = useState(false);
  const [finishing, setFinishing] = useState(false);

  useEffect(() => {
    // Complete onboarding NOW so the pipeline (hotkeys, prewarm) is armed for the test.
    api.completeOnboarding();
    const un = onPipelineEvent((e) => {
      if (e.kind === 'state_changed') setRecording(e.state === 'recording');
      if (e.kind === 'completed') {
        setResult(e.text);
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
      <h1>Try it</h1>
      <p>Click the button (or use your hotkey), say something, then stop.</p>
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
        {recording ? 'Stop' : finishing ? 'Transcribing…' : 'Start test dictation'}
      </button>
      {result !== null && (
        <div className={`ob-result ${result ? '' : 'faint'}`}>
          {result || 'No speech detected — try again a little louder.'}
        </div>
      )}
      <button className="btn ghost" onClick={onDone}>
        Finish setup
      </button>
    </>
  );
}
