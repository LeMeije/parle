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
npm run tauri dev          # development
npm run tauri build        # .app + .dmg (release)
cargo test --workspace     # 74 tests incl. behavioural contract vectors
```

Dev note: sign dev builds with one stable certificate or macOS TCC forgets the
Accessibility grant on every rebuild: see HUMAN_TASKS.md §2.

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
docs                   PRODUCT · ARCHITECTURE · BENCHMARKS · WINDOWS_HANDOFF · research/
```

## Measured performance

See docs/BENCHMARKS.md. Headline (MacBook Air-class M2, Metal): 10 s of live
microphone audio transcribed in **382 ms** (base-q5_1, warm); 6.1 s fixture in
**240 ms**. Idle footprint is the model + tens of MB: no bundled Chromium.

## Licence

MIT: see [LICENSE](LICENSE). Use it, modify it, ship it, commercially or not;
just keep the copyright notice.

Third-party work this builds on, and how each was used, is recorded in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). In short: Parle's cleanup
contract references freeflow (MIT, © 2026 Zach Latta); murmur-youtube informed
platform lessons only (no code reused); LocalFlow's warmup and clipboard-restore
ideas were reimplemented clean-room. Model weights are downloaded at runtime
under their own licences.
