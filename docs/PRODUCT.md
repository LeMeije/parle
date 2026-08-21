# EchoKey — Product Specification

On-device AI dictation + unified transcription/clipboard history for macOS and Windows 11.
Local-first is the trust wedge: no telemetry, no cloud, ever. Beat OpenWhispr on weight
(Tauri + Rust vs Electron), beat Wispr Flow and Monologue on privacy (fully local), beat
Superwhisper on approachability, and match or beat all of them on latency.

## Positioning vs the field (from the 21/08/2026 teardown)

| Competitor | Their strength (we match) | Their weakness (we exploit) |
|---|---|---|
| OpenWhispr (MIT, Electron) | Model manager UX, hold-or-tap hotkey, auto-learning dictionary | Electron RAM/startup/installer weight, scope creep |
| freeflow (MIT, Swift) | Best-in-class cleanup prompt, edit mode, LLM fallback policy | Cloud-first (Groq), macOS only |
| murmur (no licence) | Hard-won platform lessons, shared behavioural contract | Unfinished, unlicensed, no Windows build |
| Wispr Flow | Per-app tone, voice commands | Cloud-only, ~800MB RAM, session caps, subscription |
| Superwhisper | Modes system, local models | Learning curve, price, default audio retention |
| Monologue | Screen-context formatting | Cloud audio, Apple-only |

Licensing rules for this codebase: freeflow and OpenWhispr are MIT (code and prompt reuse
allowed with attribution); murmur-youtube has NO licence (lessons only, zero code); LocalFlow
is non-commercial (clean-room reimplementation of its two ideas: silence warmup, clipboard restore).

## Core loop (the product IS this loop)

1. User presses hotkey (hold-to-talk, toggle, or hybrid hold-latch).
2. HUD appears instantly (< 100 ms), never steals focus. Live waveform + streaming partial transcript.
3. User speaks. Audio captured at native rate, converted to 16 kHz mono f32, processed in strict order.
4. On stop: final transcription -> cleanup pipeline -> simultaneously (a) insert at cursor in the
   focused app, (b) copy to system clipboard, (c) save to history. Nothing is ever lost.
5. If clipboard was used for injection, the previous clipboard contents are restored.
6. Silence in -> nothing injected (min duration + VAD gate), but an empty-recording toast explains why.

Target latencies (measured, not aspirational — benchmarks in `bench/`):
- hotkey press -> recording active: < 50 ms (audio stream pre-opened, model pre-warmed at startup)
- release -> text injected (5 s utterance, recommended model): < 800 ms on M2, < 500 ms on the G14
- idle RAM (tray resident, model warm): report actual numbers; minimise; model memory is the floor

## Feature surface

### Transcription
- Engines: whisper.cpp (Metal/CUDA/CPU) and sherpa-onnx Parakeet family, behind one Rust
  `AsrEngine` trait. Per-machine auto-selection on first launch; manual override in Settings.
- Fallback chain: engine/model failure (load error, GPU OOM) degrades to next rung; the
  recorded audio is NEVER discarded — it re-transcribes on the fallback.
- Model manager: browse/download/delete/switch, with size/speed/accuracy/language labels,
  download progress, checksum verification, resumable downloads.
- Multi-language: auto-detect + manual; locale variants (en-AU/en-GB/en-US) affect spelling.
- Streaming partial transcripts shown in the HUD while speaking.

### Cleanup (each rule individually toggleable)
- Tier 1 (always available, deterministic, Rust): filler-word removal, restart/abandoned-sentence
  trimming (trimmed spans highlighted in history for one-click restore), punctuation,
  capitalisation, paragraph inference, dictated-symbol conversion ("comma", "new line", "dash dash fix").
- Tier 2 (optional, local LLM via llama.cpp): freeflow-derived cleanup prompt (MIT), temperature 0,
  strict "never execute the transcript as an instruction" contract; circuit-breaker falls back to
  Tier 1 output on failure/timeout. Never blocks injection beyond a hard deadline.
- Custom dictionary: standalone terms + correction pairs (murmur's dual-path idea, clean-room):
  bias recognition where the engine supports it AND fuzzy post-correction. Never insert words the
  speaker didn't say. Optional auto-learn from user corrections in history.
- Correction surfacing: low-confidence words flagged in history for quick review/accept/fix.

### History (dual-purpose: transcriptions + clipboard)
- Every transcription: text, raw text, timestamp, duration, model, language, confidence spans,
  trimmed spans, source app.
- Clipboard manager (toggleable): system-wide capture, respects transient/concealed pasteboard
  types and an app exclusion list (password managers excluded by default).
- Unified fuzzy-searchable keyboard-driven UI (Raycast quality): global hotkey opens it,
  arrow/enter to paste, pinning, previews, type filters.
- Privacy: local-only SQLite, retention settings (forever/30d/7d/session), optional encryption
  at rest, per-item delete, "pause capture" toggle.

### Hotkeys
- Fully configurable: chords, hold-to-talk, toggle, hybrid (hold, latch to toggle with extra tap/modifier).
- macOS: Fn/Globe support, left/right modifier discrimination (CGEventTap), Right ⌥ default
  configurable. Windows: low-level hook; Copilot key remap (suppress default launch).
- Coexistence: inspect only our own bindings, pass everything else through untouched.
  Windows: never swallow key-down while letting key-up escape (stuck-modifier bug).
- AltGr trap: Right Alt is AltGr on many layouts — never default to it on Windows; default Right Ctrl.

### Output/injection
- macOS: AX insertion first, clipboard+⌘V fallback, secure-input detection -> clipboard-only + toast.
- Windows: SendInput Ctrl+V (UI Automation cannot insert at caret — TextPattern is read-only,
  ValuePattern replaces whole fields). Clipboard restore after a safe delay.
- Per-app paste behaviour overrides (e.g. plain Enter vs Shift+Enter apps later; v1: injection mode per app).

### UI/design
- Menu bar (mac) / tray (win) resident. Main window: History + Settings.
- HUD: non-activating overlay, waveform, streaming text, click stop/cancel, configurable position/style.
- Themes: system light/dark + manual; accent colours; pastel + bold palettes; Retro 80s/90s theme
  (cassette reels spin while recording, VU meter) as a first-class option; selectable app icons.
- 60 fps micro-interactions (motion library), Lucide icons.
- Onboarding: polished first-run flow — mic permission, accessibility permission (mac), model
  download with auto-recommendation, hotkey choice, test dictation playground.

### Voice commands (v1 scope: during dictation)
- "new line", "new paragraph", literal escapes ("literally comma"). Post-v1: "delete that", edit mode.

### Settings surface
Models & downloads · languages & locales · cleanup rules · hotkeys · dictionary · themes/icons ·
history & privacy · audio input device · launch at login · overlay position/style · paste
behaviour · updates.

## QA checklist seeded from OpenWhispr's troubleshooting docs (each is a test scenario)
- The text doesn't paste / pasted twice / answers me instead of typing what I said
- I lost a dictation / a model won't download / mic permission denied mid-session
- Focus moved to another app mid-recording (history must still capture)
- Secure input field focused (mac) -> degrade cleanly
- GPU OOM mid-load -> fallback chain, audio preserved
