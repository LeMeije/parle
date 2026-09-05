# Parle Architecture

## Stack decision (performance-justified)
Tauri 2 + Rust core + React/Vite UI. Rejected Electron (OpenWhispr's exploitable weakness: bundled
Chromium + Node = 80-120 MB installers, 200-400 MB idle RAM). Rejected fully-native-per-platform (two
UIs, two codebases; murmur proves the cost: its Windows side never shipped). Tauri gives one Rust core
where ALL hot paths live (audio, ASR, hotkeys, injection are pure native), with the webview only
rendering UI. Idle footprint target: tens of MB + model memory; installers under 15 MB before models.

## Workspace layout
```
parle/
  Cargo.toml               # workspace root
  src-tauri/               # app crate: wiring, tray, windows, IPC commands, platform modules
    src/platform/macos/    #   CGEventTap hotkeys, AX+Cmd-V injection, pasteboard monitor, TCC
    src/platform/windows/  #   WH_KEYBOARD_LL (incl. Copilot key), SendInput, clipboard listener
  crates/
    parle-core/          # settings, history store (SQLite+FTS5), text pipeline, dictionary, types
    parle-audio/         # cpal capture, buffer copying, resample->16kHz mono f32, levels, WAV debug
    parle-asr/           # AsrEngine trait, whisper backend, model registry+downloader, fallback chain
  src/                     # React UI: HUD window + main window (history, settings, onboarding)
  shared/                  # behavioural contract JSON test vectors (formatter, dictionary)
  docs/                    # PRODUCT, ARCHITECTURE, research/, WINDOWS_HANDOFF, HUMAN_TASKS
  bench/                   # latency + accuracy benchmark harness
```

## Threading model (the ordering rules are load-bearing)
- cpal audio callback: COPIES each buffer into fresh storage (device buffers are recycled the instant
  the callback returns) and pushes to a bounded crossbeam channel. Never blocks, never allocates beyond
  the copy, never does IO.
- Audio thread: drains the channel IN ORDER, resamples to 16 kHz mono f32, appends to the utterance
  buffer, computes RMS/peak levels (sent to HUD via tauri ipc::Channel at ~30 Hz).
- ASR worker: exactly ONE inference worker drains a bounded chunk queue: ordering is structural, not
  incidental. Partial-pass transcriptions stream to the HUD; final pass on stop.
- Pipeline state machine (owned by `PipelineController`): Idle -> Starting -> Listening -> Finishing -> Idle,
  with Cancelled as an exit from any active state. Hotkey events are the only inputs that start/stop it.
- UI: app.emit broadcasts for all pipeline events (levels are throttled to ~30 Hz with tiny
  payloads, measured fine; migrate to ipc::Channel only if profiling ever shows IPC cost).

## Data flow on stop
audio buffer -> final ASR pass -> Tier-1 deterministic cleanup (Rust) -> dictionary post-correction ->
[optional Tier-2 local LLM with hard deadline + circuit breaker, falls back to Tier-1 output] ->
[Refine mode only: the cleaned transcript goes to an AI CLI child process with a hard deadline and a
cancel token; its rewrite replaces the text, the transcript becomes raw_text; see docs/REFINE.md] ->
`Pipeline::deliver` (ONE path for both the plain and the mark-splice take): inject at cursor + set
clipboard + insert history row. The RAW transcript and the audio
duration/model/language/confidence metadata are stored alongside the cleaned text; trimmed spans stored
as offsets so the UI can highlight/restore.

## Failure ladder (audio is never lost)
model load error / GPU OOM -> next rung: quality model -> default model -> fast model -> CPU build of same.
The captured audio stays in memory until a transcription succeeds or the user cancels; on total failure
it is saved as WAV into the history dir and surfaced as a "recovered recording" row.

## Engines
trait AsrEngine { load, warmup (1s zeros), transcribe(chunk|full, lang, bias-prompt) -> segments with
confidence + timestamps, streaming partial callback }. Backends: whisper-rs (v1), sherpa-onnx Parakeet
(feature `parakeet`), llama-cpp-2 cleanup (feature `cleanup`). Model registry is static data (URLs,
sizes, sha256 where published, speed/accuracy labels per machine class); downloader is resumable with
progress events.

## Behavioural contract (Windows parity)
shared/*.json test vectors are the spec for everything platform-independent (formatter, dictionary,
fuzzy search ranking). The same vectors are meant to run on both platforms;
there is no CI in this repo yet, and the Windows build cannot be compiled on
the author's Mac, so "Windows passes" is currently a claim rather than a
measurement. Platform-specific behaviour is
documented per-feature in docs/WINDOWS_HANDOFF.md with the murmur-derived lessons inlined.

## Key platform decisions (from docs/research/PLATFORM.md)
- Hotkeys: tauri-plugin-global-shortcut for ordinary user chords; hand-rolled CGEventTap (macOS,
  active tap, Accessibility) and WH_KEYBOARD_LL (Windows) for Fn/Globe, L/R modifiers, Copilot key.
- HUD: tauri-nspanel (git v2.1) NonactivatingPanel on macOS; WS_EX_NOACTIVATE|TOPMOST|TOOLWINDOW on Windows.
- Injection: macOS AX-selected-text fast path -> clipboard+CGEventPost Cmd-V fallback (restore ~700ms,
  changeCount-checked, marked Concealed only when the content is judged secret;
  the earlier TransientType marking was reversed because it told every other
  clipboard manager to bin the user's own copy); Windows clipboard+SendInput
  Ctrl-V (restore 700ms,
  sequence-number-checked, monitor-excluded formats). Secure input detected -> clipboard-only + HUD hint.
- History: rusqlite bundled SQLite, FTS5 external-content table, optional SQLCipher later (v1: OS-level
  file protection + retention controls; encryption-at-rest flag reserved in settings schema).
