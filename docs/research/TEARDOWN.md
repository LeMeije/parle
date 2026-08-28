# Competitive Teardown (verified 21/08/2026)

## Licensing rules for this codebase
| Repo | Licence | Reuse |
|---|---|---|
| OpenWhispr | MIT | code + ideas OK |
| freeflow (zachlatta) | MIT (c) 2026 Zach Latta | code + full cleanup prompt OK (attribution kept in docs/research/) |
| murmur-youtube | NO licence (all rights reserved) | lessons/ideas ONLY, zero code |
| LocalFlow | custom non-commercial | NO code; clean-room reimplement (silence warmup, clipboard restore) |

## OpenWhispr (Electron 41 + React 19 + whisper.cpp + sherpa-onnx, MIT)
Steal/improve: hold-OR-tap unified hotkey (one binding covers PTT + toggle); cleanup ON by default;
dedicated translation hotkey; dictionary auto-learns from corrections; snippets ("say a phrase, get full
text"); custom-ASR shim extension point; troubleshooting docs double as a QA checklist ("text doesn't
paste", "pasted twice", "answers me instead of typing what I said", "I lost a dictation", "model won't
download"). Exploit: Electron weight, scope creep (notes/meetings/teams/API), Intel-mac ONNX gap, cloud
account for sync.

## freeflow (native Swift, MIT, cloud-first Groq)
Steal: Fn hold-to-talk + Cmd-Fn toggle on one physical key; Edit Mode (select text, speak transformation);
AppContextService (nearby app context as spelling/tone reference ONLY); pipeline debug panel +
TestCaseExporter (real dictations -> regression cases); LLM resilience (temp 0.0, 4096 max tokens,
fallback model on 429/empty/suspected-instruction-execution, LLMCooldownManager circuit breaker returns
RAW transcript instead of failing). Full default system prompt saved at
docs/research/freeflow-postprocessing.swift (lines 40-133) — the dictation cleanup contract + command-mode
prompt. Vocabulary injected as "high-priority terms... use these spellings exactly"; context as
`CONTEXT: "..."` in the user message. Exploit: no local engine, macOS only.

## murmur-youtube (Swift 6 + planned C#/Avalonia, NO LICENCE — lessons only)
macOS lessons: HUD = .nonactivatingPanel, canBecomeKey=false ("the load-bearing detail of the whole app");
CGEventTap not NSEvent (fn + L/R modifiers don't surface via NSEvent); audio ordering explicit — one
AsyncStream drained by a single task, never task-per-buffer; AVAudioEngine recycles tap buffers the
instant the callback returns -> copy into fresh storage; TCC stores a code-signing requirement not a path
(ad-hoc sig changes per build -> grants orphaned); never run from iCloud-synced folder; injection = AX
insert with pasteboard+Cmd-V fallback; configurable Right-Opt/fn/Right-Cmd for coexistence; compare mode
injects nothing (competing transcripts fight over the field); local engine timings not comparable with
cloud timings — present separately.
Windows lessons: Parakeet TDT 0.6B int8 via sherpa-onnx ~40x realtime CPU (10s utterance in ~250ms);
v2=English (1025 tokens), v3=25 languages (8193 tokens); model ~661MB in %LOCALAPPDATA% (never Program
Files); ~2GB resident scaling to 4.3GB @300s; HARD CEILING: encoder relative-position table = 5000 frames
= 400s max utterance; CPU-only deliberate (DirectML can't do variable-length; CUDA needs toolkit); 4
threads beats 8; ARM64 must run ARM64 build; sherpa config: ModelType="nemo_transducer" mandatory,
FeatureDim=128 not 80, RuntimeIdentifier must be set or DllNotFoundException, 16kHz mono f32;
hook must never swallow key-down but let key-up escape (target believes Ctrl held forever);
RIGHT ALT IS ALTGR on German/Polish/UK/Nordic/LatAm layouts — default Right Ctrl;
UIA cannot inject (TextPattern read-only, ValuePattern replaces whole fields) — SendInput primary;
pinned versions: NAudio 2.3.0, Avalonia.Headless.XUnit 11.3.20, sherpa.onnx 1.13.5 (bundles ONNX Runtime —
never also reference Microsoft.ML.OnnxRuntime).
Cross-platform contract: shared/dictionary-test-vectors.json is the authoritative spec, both platforms run
it in CI; change vectors first, watch both fail, make both pass. Regex traps: C# needs CultureInvariant
(Turkish İ/i); NFC-normalise both sides (macOS returns decomposed); ICU folds ß->ss, .NET doesn't; stay in
a documented safe regex subset.
Dictionary design: standalone terms + correction pairs, dual-path (pre-transcription engine biasing AND
post-transcription replacement), spacing/hyphen tolerant, warn on entries likely to false-match common words.
Engines noted: Parakeet v3 via FluidAudio ~110x realtime, ~66MB resident, ANE, 25 langs (macOS standout);
Apple SpeechAnalyzer (macOS 26+) zero-dep streaming; WhisperKit large-v3 99 langs 200-500ms, ~1.5GB.

## LocalFlow (Python, non-commercial licence — clean-room only)
Ideas: model warmup = transcribe 1s of zeros at startup so first real dictation is fast; clipboard
save -> paste -> daemon-thread restore after 0.6s; in-memory 16kHz mono f32, no temp files;
MIN_DURATION_SEC=0.3 discards accidental taps; modifier canonicalisation via set-subset detection.

## Commercial UX bar
- Wispr Flow: hold-to-talk; voice commands ("delete that"); per-app tone (Gmail professional, Slack
  casual, VS Code code); auto-editing; dictionary; snippets. Cloud-only, ~800MB RAM, 6-min cap, $15/mo.
- Superwhisper: Opt+Space default; hold/release PTT; MODES system (Message/Email/Voice/Custom + per-mode
  prompts and model choice) = its signature; local-capable. Steep learning curve, $249.99 lifetime,
  default audio retention.
- Monologue: DeepContext screenshots for per-app formatting; on-device dictionary that learns proper
  nouns; cloud audio; Apple-only; $15/mo regular.
- Bar for Parle: hold-or-tap single hotkey + sub-second local latency + per-app context + modes-lite +
  auto-learning dictionary + truly local as the trust wedge.
