# Parle

On-device AI dictation + unified transcription/clipboard history for macOS and
Windows 11. Hold a key, speak, release: your words appear where your cursor
is. **Everything runs locally. No telemetry, no cloud, ever.**

Tauri 2 + Rust core + React UI. whisper.cpp (Metal on macOS, CUDA on Windows)
behind a fallback ladder that never loses a recording.

## Feature highlights

- **Hold / toggle / hybrid hotkey**: hold to talk, or a quick tap latches
  recording on (tap again to stop). Fn/Globe on macOS, Right Ctrl or the
  **Copilot key** on Windows (default launch suppressed), left/right modifiers
  discriminated, chords supported.
- **Non-focus-stealing HUD**: live waveform, elapsed time, streaming partial
  transcript, click to stop, Esc to cancel. Never takes keyboard focus, so
  paste-at-cursor always works. Retro cassette style included.
- **Paste + copy + history, simultaneously**: text is inserted at the cursor
  (AX insertion fast path on macOS, clipboard+paste fallback with your previous
  clipboard restored), copied, and saved to history. Nothing is ever lost:
  even a total engine failure saves the audio as a recoverable WAV.
- **Dictation bar, on every tab**: the moment a recording starts, the sidebar's
  record button becomes a floating bar at the bottom of the window: stop,
  elapsed time, level, and a box for pasting or typing content straight into
  the recording, wherever you are in the app. A paste FILLS the box and waits
  for Enter, so you can strip the quote marks or the tracking suffix before it
  is spliced into the transcript verbatim. Compose keeps the detail: every
  insert with the audio timestamp it was pinned to.
- **Refine mode (opt-in)**: hold Shift while using the dictation key you
  already have, and the overlay turns coral. Speak a brain dump; instead of
  pasting the transcript, Parle hands it to an AI command-line tool already
  installed and signed in on your machine (Claude Code and Codex both verified
  live; Gemini or any command of your own) and pastes the rewrite. Your rules
  and an optional voice file both ride along. The modifier, its colour and the
  AI are all yours to pick, or give Refine a key of its own. It finds the tool
  itself, **including the private copy inside the Claude or ChatGPT desktop
  app**, which is where these CLIs now live and is nowhere on your PATH. The
  model is a dropdown of what your tool says it accepts, so it cannot be
  mistyped. Off by default, and the only thing in Parle that sends your words
  off the device. See `docs/REFINE.md`.
- **Smart cleanup (deterministic, per-rule toggles)**: fillers, stutters,
  self-corrections ("Thursday no actually Wednesday" -> "Wednesday") with
  trimmed spans reviewable and restorable in History, dictated punctuation
  with a "literally" escape, capitalisation, paragraph-on-pause, en-AU/GB/US
  locale spelling.
- **Custom dictionary**: terms + "heard as" corrections, engine biasing plus
  fuzzy post-correction that never inserts words you didn't say, false-match
  warnings, optional auto-learning from your history edits.
- **Model manager**: download/switch/delete whisper models with speed and
  accuracy ratings; per-machine auto-recommendation on first launch;
  automatic fallback down the ladder on load failure or OOM.
- **Clipboard manager**: everything you copy, fuzzy-searchable alongside
  dictations (Raycast-style palette), pinning, editing, source-app tags.
  Password managers excluded by default; transient/concealed clipboard types
  respected. An injected transcript is NOT hidden from Win+V: it used to be,
  and marking every write as excluded also told Windows to discard the row the
  user had deliberately pressed Copy on. Only content Parle judges secret is
  excluded now.
- **Correction surfacing**: low-confidence words flagged per item.
- **Themes**: Paper / Pastel / Bold / Retro palettes, light/dark/system,
  accent colours, reduced motion; spinning cassette reels while recording if
  you want them.

## Build (macOS)

```bash
# Requires: Rust stable, Node 20+, Xcode CLT, cmake (brew install cmake)
npm install
npm run tauri dev              # development
npm run tauri build            # .app + .dmg (release)
npm run tauri build -- --bundles app   # .app only, skips the flaky DMG step
cargo test --workspace         # 718 passing, 11 ignored, incl. behavioural contract vectors
npx tsc --noEmit -p tsconfig.json      # the frontend's only gate
```

Dev note: sign dev builds with one stable certificate or macOS TCC forgets the
Accessibility grant on every rebuild: see HUMAN_TASKS.md §2. There is no CI:
**docs/TESTING.md** is the record of what is actually verified, how, and what
is only claimed.

The project shipped under a different name before it was called Parle. Nothing
internal carries the old name any more: crates, modules, the mDNS service and
the bundle identifier were all renamed on 28/08/2026. The single deliberate
exception is the `OLD_DATA_DIR` constant in `parle-core`, which still holds the
old folder name because that is the directory it migrates users FROM. See
docs/RENAME_AUDIT.md for the full account, including the leftovers that live
outside the repo.

## Build (Windows)

Status: built and in use on Windows (see `docs/WINDOWS_HANDOFF.md`). Note that
`windows.rs` cannot be compiled from the author's Mac: cross-compiling `ring`
needs a Windows C toolchain, so changes made from macOS are reviewed by reading
rather than by the compiler, and must be built on Windows before being trusted.

See **docs/WINDOWS_HANDOFF.md**: full toolchain list, verification checklist,
and a copy-paste Claude Code pickup prompt.

## Repo map

```
crates/parle-core    settings · cleanup formatter · dictionary · history (SQLite+FTS5) · fuzzy search
crates/parle-audio   cpal capture · ordered buffering · resample to 16 kHz mono · levels · WAV
crates/parle-asr     AsrEngine trait · whisper.cpp backend · model registry · downloader · fallback
src-tauri              app wiring · pipeline · gesture machine · HUD/tray · platform/{macos,windows}
src                    React UI: onboarding · history palette · models · dictionary · settings · HUD
shared                 behavioural-contract test vectors (both platforms must pass)
bench                  speech fixtures + `cargo run --release --example bench -p parle-asr`
docs                   see below
```

### Documentation map

| File | What it is for |
|---|---|
| `docs/PRODUCT.md` | What Parle is, the feature surface, shipped versus deferred |
| `docs/ARCHITECTURE.md` | Stack, threading model, data flow, failure ladder |
| `docs/UI.md` | The UI contract: design tokens, the scoped-accent trap, layout primitives, wording rules, i18n |
| `docs/REFINE.md` | Refine mode in full: what is sent, how the CLI is run and found, model selection, failure policy |
| `docs/TESTING.md` | How anything here is verified, the stub harness, and what is NOT covered |
| `docs/WINDOWS_HANDOFF.md` | Windows status, toolchain, what is verified there and what is not |
| `docs/BENCHMARKS.md` | Measured latency and accuracy (macOS only so far) |
| `docs/SYNC_DESIGN.md`, `SYNC_HANDOVER.md`, `SYNC_FIELD_TEST.md` | LAN sync design, handover and field test |
| `docs/RENAME_AUDIT.md` | The EchoKey to Parle rename, and the one constant that must survive it |
| `docs/incidents/` | Written-up diagnoses of real failures, dated |
| `docs/research/` | Pre-build research: ASR landscape, platform APIs, competitor teardown |
| `HUMAN_TASKS.md` | The runbook of everything only a human at the keyboard can do |

## Measured performance

See docs/BENCHMARKS.md. Headline (MacBook Air-class M2, Metal): 10 s of live
microphone audio transcribed in **382 ms** (base-q5_1, warm); 6.1 s fixture in
**240 ms**. Idle footprint is the model + tens of MB: no bundled Chromium.

## Licence

Source-available, not open source. See [LICENSE](LICENSE).

Parle is under **PolyForm Noncommercial 1.0.0 plus an internal-use grant**. In
plain terms:

- **Yes**: use it personally, use it at work including at a for-profit company,
  read it, modify it, fork it, and redistribute it free of charge.
- **No**: sell it, charge a fee for it or for anything based on it, or include
  it in a paid product or hosted service.

No third-party source code is reused or redistributed here. The projects that
informed Parle's design, and what each contributed, are recorded in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Model weights are downloaded
at runtime under their own licences.
