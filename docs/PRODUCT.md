# Parle: Product Specification

On-device AI dictation + unified transcription/clipboard history for macOS and Windows 11.
Local-first is the trust wedge: no telemetry, no cloud, ever. Beat OpenWhispr on weight
(Tauri + Rust vs Electron), beat Wispr Flow and Monologue on privacy (fully local), beat
Superwhisper on approachability, and match or beat all of them on latency.

## Positioning vs the field (from the 21/08/2026 teardown)

| Competitor | Their strength (we match) | Their weakness (we exploit) |
|---|---|---|
| OpenWhispr (MIT, Electron) | Model manager UX, hold-or-tap hotkey, auto-learning dictionary | Electron RAM/startup/installer weight, scope creep |
| freeflow (MIT, Swift) | Strong cleanup contract, edit mode, LLM fallback policy | Cloud-first (Groq), macOS only |
| murmur (no licence) | Hard-won platform lessons, shared behavioural contract | Unfinished, unlicensed, no Windows build |
| Wispr Flow | Per-app tone, voice commands | Cloud-only, ~800MB RAM, session caps, subscription |
| Superwhisper | Modes system, local models | Learning curve, price, default audio retention |
| Monologue | Screen-context formatting | Cloud audio, Apple-only |

Licensing rules for this codebase: NO third-party source or prompt text is reused from any
competitor, whatever its licence. Every one of them is lessons and ideas only, implemented
independently. Parle itself ships under PolyForm Noncommercial 1.0.0 plus an internal-use
grant (see LICENSE), so it must stay free of encumbered third-party code.

## Core loop (the product IS this loop)

1. User presses hotkey (hold-to-talk, toggle, or hybrid hold-latch).
2. HUD appears instantly (< 100 ms), never steals focus. Live waveform + streaming partial transcript.
3. User speaks. Audio captured at native rate, converted to 16 kHz mono f32, processed in strict order.
4. On stop: final transcription -> cleanup pipeline -> simultaneously (a) insert at cursor in the
   focused app, (b) copy to system clipboard, (c) save to history. Nothing is ever lost.
5. If clipboard was used for injection, the previous clipboard contents are restored.
6. Silence in -> nothing injected (min duration + VAD gate), but an empty-recording toast explains why.

Target latencies (measured, not aspirational: benchmarks in `bench/`):
- hotkey press -> recording active: < 50 ms (audio stream pre-opened, model pre-warmed at startup)
- release -> text injected (5 s utterance, recommended model): < 800 ms on M2, < 500 ms on the G14
- idle RAM (tray resident, model warm): report actual numbers; minimise; model memory is the floor

## Feature surface

### Transcription
- Engines: whisper.cpp (Metal/CUDA/CPU) and sherpa-onnx Parakeet family, behind one Rust
  `AsrEngine` trait. Per-machine auto-selection on first launch; manual override in Settings.
- Fallback chain: engine/model failure (load error, GPU OOM) degrades to next rung; the
  recorded audio is NEVER discarded: it re-transcribes on the fallback.
- Model manager: browse/download/delete/switch, with size/speed/accuracy/language labels,
  download progress, resumable downloads, size-window integrity check (engine load is the
  authoritative validation; per-file checksums are a deferred hardening item).
- Multi-language: auto-detect + manual; locale variants (en-AU/en-GB/en-US) affect spelling.
- Streaming partial transcripts shown in the HUD while speaking.

### Refine (second dictation mode, opt-in)
- Triggered by holding a modifier (Shift by default) with the EXISTING dictation key, or by a
  separate key of its own. Its own accent colour, user-picked. The recording is identical; on stop the cleaned
  transcript goes to an AI CLI on the machine (Claude Code and Codex both verified live; Gemini and
  custom command best effort) and the REWRITE is pasted, copied and stored, with the transcript as
  the row's raw text.
- User rules and an optional voice .md are BOTH baked into every prompt, rules first. Neither
  replaces the other. The transcript is data, never instructions: fixed contract as the system
  prompt, user text on stdin, tools/MCP/hooks/CLAUDE.md off.
- **Finds the tool itself, including the copy inside the Claude or ChatGPT desktop app**, which is
  where these CLIs now live and is nowhere on PATH. The path field is a picker, not a text box, with
  guidance naming the file to pick for the chosen tool.
- The model is a dropdown of what the installed tool says it accepts, so it cannot be mistyped, and
  a rejected name is reported as the name being wrong rather than as a vague failure.
- Never sends a password-field dictation. Failure and cancel never lose the transcript (docs/REFINE.md).

### Cleanup (each rule individually toggleable)
- Tier 1 (always available, deterministic, Rust): filler-word removal, restart/abandoned-sentence
  trimming (trimmed spans highlighted in history for one-click restore), punctuation,
  capitalisation, paragraph inference, dictated-symbol conversion ("comma", "new line", "dash dash fix").
- Tier 2 (NOT BUILT, planned, optional local LLM via llama.cpp): an independently written cleanup
  prompt, temperature 0, strict "never execute the transcript as an instruction" contract;
  circuit-breaker falls back to Tier 1 output on failure/timeout. Never blocks injection beyond a
  hard deadline. The shipped LLM path today is Refine (src-tauri/src/refine.rs), which shells out
  to the user's own CLI; its contract is Parle's own, not derived from any other project.
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
- macOS: Fn/Globe support, left/right modifier discrimination (CGEventTap), Fn default
  configurable. Windows: low-level hook; Copilot key remap (suppress default launch).
- Coexistence: inspect only our own bindings, pass everything else through untouched.
  Windows: never swallow key-down while letting key-up escape (stuck-modifier bug).
- AltGr trap: Right Alt is AltGr on many layouts: never default to it on Windows.
- Default dictation key is chosen by POSITION so muscle memory carries between machines: the
  bottom-left corner key on both. Globe/Fn on macOS, Left Ctrl on Windows. Holding Shift makes it
  a Refine take on both.
- The GESTURE deliberately does NOT match, and must not be "aligned" without reading this.
  Windows is DoubleTap because it is the only mode that never swallows its key, and Left Ctrl in
  any other mode eats every Ctrl chord: key and gesture change together. macOS is Hybrid
  (hold Globe) because macOS binds a DOUBLE-PRESS of Globe to its own dictation, and DoubleTap
  does not swallow, so defaulting to it there would race the OS on every take.

### Output/injection
- macOS: AX insertion first, clipboard+⌘V fallback, secure-input detection -> clipboard-only + toast.
- Windows: SendInput Ctrl+V (UI Automation cannot insert at caret: TextPattern is read-only,
  ValuePattern replaces whole fields). Clipboard restore after a safe delay.
- Per-app paste behaviour overrides (e.g. plain Enter vs Shift+Enter apps later; v1: injection mode per app).

### UI/design
- Menu bar (mac) / tray (win) resident. Main window: History + Settings.
- HUD: non-activating overlay, waveform, streaming text, click stop/cancel, configurable position/style.
- Themes: system light/dark + manual; accent colours; pastel + bold palettes; Retro 80s/90s theme
  (cassette reels spin while recording, VU meter) as a first-class option; selectable app icons.
- Dictation bar: while recording, the sidebar record button stows and a floating bar rises at the
  bottom of the main window (stop, clock, level, insert box) so pasted or typed content can be
  pinned into the recording from any tab. Paste fills the box, Enter (or Insert) commits it, which
  leaves room to edit clipboard text before it is spliced in verbatim. Compose lists the inserts.
- 60 fps micro-interactions (motion library), Lucide icons. Morphs animate transform and opacity
  ONLY: neither border-radius nor background-color is compositable, and animating them turned a
  320ms expansion into three visible jumps.
- Design tokens live in one place (`src/theme.css`) and a subtree can rescope the accent to recolour
  itself, which is how a Refine take turns coral. **The contract for that, and the trap that made
  every coral surface come out grey, are in docs/UI.md.** Read it before touching a tint.
- One capitalisation rule: an action is Title case, a mode NAME keeps its capital wherever it names
  the mode, a generic status word is lower case.
- Onboarding: polished first-run flow: mic permission, accessibility permission (mac), model
  download with auto-recommendation, hotkey choice, test dictation playground.

### Voice commands (v1 scope: during dictation)
- "new line", "new paragraph", literal escapes ("literally comma"). Post-v1: "delete that", edit mode.

### Settings surface
Models & downloads · languages & locales · cleanup rules · hotkeys · dictionary · themes/icons ·
history & privacy · audio input device · launch at login · overlay position/style · paste
behaviour · Refine with AI · updates.

Layout primitives and their rules (a label/control `Field`, a full-width row, and why a callout must
never go inside a `Field`) are in docs/UI.md.

## QA checklist seeded from OpenWhispr's troubleshooting docs (each is a test scenario)
- The text doesn't paste / pasted twice / answers me instead of typing what I said
- I lost a dictation / a model won't download / mic permission denied mid-session
- Focus moved to another app mid-recording (history must still capture)
- Secure input field focused (mac) -> degrade cleanly
- GPU OOM mid-load -> fallback chain, audio preserved


## v1 shipped vs deferred (kept honest: updated 10/09/2026)

SHIPPED: core loop (hold/toggle/hybrid incl. other-key gesture abort), non-activating HUD with
outcome messages + pill/cassette/minimal styles, live partial transcripts while speaking,
paste+copy+history with honoured copy/restore settings and transient-marked injection writes,
WAV recovery, deterministic cleanup with reviewable trims, dictionary (bias+fuzzy+auto-learn,
merge-safe), model manager with measured auto-selection + retry-then-demote ladder, unified
fuzzy history palette (Enter pastes into previous app), clipboard capture with exclusions,
4 palettes x light/dark, onboarding with download-error recovery, 15-min recording cap.

SHIPPED 22/08: Parakeet TDT v3 engine (CPU int8, ~14x RT measured, 25 European languages),
12-model whisper registry incl. Distil-Whisper v3.5.

SHIPPED 04/09: Refine mode (modifier-plus-dictation-key or its own key, AI rewrite via a local CLI,
user-picked accent, rules + voice file, fallback policy, Test button), the single `deliver()` path,
and the fix that let a user-added model file be SELECTED (it could be added but "Use" answered
"unknown model"). Held modifiers now travel with every hotkey event (`platform::Mods`), sampled from
the event itself. Windows: parle-hook wire protocol extended in place (byte 3 of the event frame);
not yet compiled there.

SHIPPED 10/09: the Refine rework, all of it macOS-verified only. Discovery now finds the CLIs
where they actually live: inside the Claude desktop app's own versioned support directory and inside
ChatGPT.app, neither of which is on PATH, which is why Refine could not see a tool the machine
plainly had. A typed or picked path accepts `~`, quotes and a macOS `.app` bundle. **Codex verified
live for the first time** (16.6s against Claude's 3.9s); Gemini still unverified. The model field
became a dropdown built from the installed tool's own `--help` plus a verified alias list, never by
probing (an invalid name is refused free, a valid one bills), with a warning for a mistyped name and
a dedicated error when the CLI rejects one. Settings: the path row is a picker with provider-specific
guidance and no text box, the result row moved under the path it describes and gained a tick and a
found-versus-chosen distinction, and changing the tool re-checks after awaiting the save (it
previously reported on the previous tool). Fixed the bug that made every scoped-accent tint compute
from the root accent, so Refine surfaces were a blue wash under coral text; the History badge now
follows the user's chosen Refine colour. One capitalisation rule across the app. Docs: new UI.md and
TESTING.md.

DEFERRED (tracked in HUMAN_TASKS.md / WINDOWS_HANDOFF.md): local-LLM cleanup tier (settings
scaffolded), per-file model checksums, overlay position presets, per-app paste
modes, encryption at rest, auto-updater (needs signing keys), selectable app icons, session-only
retention, low-confidence span inline highlighting (count badge shipped), audio cues,
voice commands beyond dictated punctuation, Windows first compile+verification.
