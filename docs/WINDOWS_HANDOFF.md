# EchoKey — Windows Handoff

Everything needed to finish, test, and package EchoKey on the Windows machine
(ASUS Zephyrus G14 2025: Ryzen AI 9 370HX, 64 GB RAM, RTX 5070 Ti 12 GB).
The macOS build is complete and tested; the Windows platform layer is WRITTEN
but has never been compiled or executed on Windows. This file plus the pickup
prompt at the bottom is the entire brief.

## Current state (what the Mac session delivered)

- Workspace: `echokey-core` (settings/formatter/dictionary/history — fully
  platform-independent, 44 tests + 2 contract suites), `echokey-audio` (cpal
  capture, ordered pipeline, resampler — 12 tests), `echokey-asr` (whisper.cpp
  via whisper-rs, registry, resumable downloader, fallback ladder — 11 tests),
  `src-tauri` (app: pipeline, gesture machine, HUD, tray, IPC, React UI).
- Verified on macOS: live mic -> transcript in 382 ms for 10 s audio (Metal,
  base-q5_1); onboarding UI; clipboard capture with app attribution; behavioural
  contract vectors green.
- Windows-specific code lives in
  [src-tauri/src/platform/windows.rs](../src-tauri/src/platform/windows.rs) —
  written against the researched API surface, cfg-gated, never compiled.

## Toolchain to install (in order)

1. **Rust** (stable, MSVC toolchain): https://rustup.rs — pick
   `x86_64-pc-windows-msvc` (default).
2. **Visual Studio Build Tools 2022+** with "Desktop development with C++"
   (MSVC, Windows SDK) — required by whisper-rs's cmake build.
3. **CMake** (winget install Kitware.CMake) and ensure it's on PATH.
4. **Node.js 20+** (winget install OpenJS.NodeJS.LTS).
5. **WebView2 runtime** — preinstalled on Windows 11; nothing to do.
6. **CUDA Toolkit 12.8+** (only for the `--features cuda` build; the RTX 5070 Ti
   is Blackwell/sm_120 and needs >= 12.8). CPU build works without it.
7. Optional: NSIS is bundled by tauri-cli; no separate install needed.

## Build & test sequence (run these in order, fix what breaks)

```powershell
cd echokey
npm install

# 1. Platform-independent crates must pass untouched (behavioural contract):
cargo test -p echokey-core
cargo test -p echokey-audio

# 2. ASR crate, CPU first (no CUDA needed):
cargo test -p echokey-asr

# 3. THE FIRST REAL TASK: compile the app crate. platform/windows.rs has never
#    seen a Windows compiler — expect windows-crate signature drift (HWND
#    wrappers, BOOL conversions, GlobalAlloc HGLOBAL types). Fix until clean:
cargo check -p echokey

# 4. Dev run:
npm run tauri dev

# 5. CUDA build (after CPU works):
cargo check -p echokey --features cuda
npm run tauri build -- --features cuda

# 6. Benchmarks (download models via the app's Models screen first, or place
#    GGML files in %LOCALAPPDATA%\EchoKey\models):
cargo run --release --example bench -p echokey-asr                  # CPU
cargo run --release --example bench -p echokey-asr --features cuda  # CUDA
# Append results to docs/BENCHMARKS.md.
```

## What was researched and encoded in windows.rs (verify each on hardware)

| Feature | Implementation | Verify |
|---|---|---|
| Copilot key | WH_KEYBOARD_LL; chord = LShift+LWin+VK_F23 (0x86), also VK_LAUNCH_APP1 (0xB6); swallow both down AND up (never split); dummy VK 0xFF injected while LWin held so Start menu doesn't open (PowerToys trick); LLKHF_INJECTED skipped | Press Copilot key: recording toggles, Copilot app does NOT open, Start menu does NOT open on release |
| Bare-modifier hotkeys | vkCode gives L/R directly (VK_RCONTROL etc.); binding is swallowed down+up | Default = RightCtrl. NEVER default RightAlt (AltGr on many layouts) |
| Hook discipline | Proc is allocation-light, unbounded channel send; hooks that exceed LowLevelHooksTimeout (~300 ms) are SILENTLY removed | Long dictation sessions: hotkey still works after hours. Consider a watchdog re-install |
| Paste injection | Clipboard write + SendInput Ctrl+V (UIA cannot insert at caret: TextPattern read-only, ValuePattern replaces whole fields) | Notepad, Word, Chrome, VS Code, Windows Terminal |
| Clipboard restore | GetClipboardSequenceNumber check, ~500 ms delay, OpenClipboard retry loop | Copy something, dictate, verify old clipboard returns |
| Clipboard write etiquette | Sets ExcludeClipboardContentFromMonitorProcessing + CanIncludeInClipboardHistory=0 + "Clipboard Viewer Ignore" so transcripts stay out of Win+V history | Win+V after dictation: transcript should NOT appear |
| Clipboard monitor | Polls GetClipboardSequenceNumber @400 ms (v1); upgrade path = message-only window + AddClipboardFormatListener | Copy in various apps -> rows appear in History with source exe |
| HUD no-focus | WS_EX_NOACTIVATE \| WS_EX_TOPMOST \| WS_EX_TOOLWINDOW applied to raw HWND in `harden_overlay` (tao focus:false alone is insufficient) | Focus a Notepad caret, dictate: HUD appears, caret stays, text lands in Notepad |
| Elevated windows | UIPI: hook + SendInput cannot reach elevated apps — accepted v1 gap | Document in-app if desired |

## Known gaps / decisions deferred to the Windows session

- `windows` crate version pinned at 0.58 in src-tauri/Cargo.toml — bump if the
  API has moved, but keep all listed features.
- Copilot key hold-to-talk: F23 auto-repeats — the down handler fires once
  because COPILOT_ACTIVE latches; verify repeat events don't retrigger Down.
- Windows mic permission: `permission_status()` returns "unknown" — Settings
  app handles mic consent per-app on Win 11; wire
  `Windows.Media.Capture` permission check if worth it.
- Parakeet backend is IMPLEMENTED and measured on macOS (~14x RT on M2 CPU,
  parakeet.rs; feature `parakeet`, enabled in the app). On Windows the
  sherpa-onnx crate's build script downloads prebuilt libs at build time
  (network needed during build, or set SHERPA_ONNX_LIB_DIR). Expect
  ~25-40x RT on the HX 370 CPU. Verify download+tar.bz2 extraction into
  %LOCALAPPDATA%\EchoKey\models and a full dictation with the model selected.
- Installer: `npm run tauri build` produces NSIS .exe (per-user). MSI via
  `--bundles msi`. Test the NSIS one first.
- The G14 has an OLED HDR display — check HUD transparency renders correctly.

## Behavioural contract

`shared/formatter-test-vectors.json` and `shared/dictionary-test-vectors.json`
are the spec. `cargo test -p echokey-core` runs them. If Windows behaviour must
differ (it shouldn't — the formatter/dictionary are pure Rust), change the
vectors first and flag it.

## Pickup prompt for Claude Code on the Windows machine

Copy-paste this to start the Windows session:

```
Read docs/WINDOWS_HANDOFF.md, docs/ARCHITECTURE.md, docs/PRODUCT.md and
docs/research/PLATFORM.md in the echokey repo, then finish the Windows build.

Context: EchoKey is a Tauri 2 + Rust on-device dictation + clipboard history
app. The macOS side is built and tested. src-tauri/src/platform/windows.rs was
written on the Mac against researched Windows APIs but has NEVER been compiled
here — your first job is `cargo check -p echokey` and fixing signature drift
until the workspace builds clean, without changing behaviour or weakening any
of the load-bearing rules commented in that file (never split a swallow across
key-down/key-up; dummy-key Start-menu suppression; allocation-light hook proc;
SendInput Ctrl+V as primary injection; clipboard-history exclusion formats).

Then, in order: (1) run the full test suite — the shared/*.json behavioural
contract vectors must pass untouched; (2) `npm run tauri dev`, complete
onboarding, verify the core loop end to end: hold Right Ctrl -> speak ->
release -> text pastes into Notepad, clipboard restored after ~500 ms, history
row created; (3) verify the HUD never steals focus (caret must stay in the
target app while the overlay is visible); (4) verify the Copilot key: bind it
in Settings -> Hotkeys, confirm it starts/stops dictation, the Copilot app
never launches, and the Start menu does not open on release; (5) verify Win+V
clipboard history does NOT contain injected transcripts; (6) build with
--features cuda (CUDA toolkit 12.8+ required for the RTX 5070 Ti) and run
`cargo run --release --example bench -p echokey-asr --features cuda`, then
append a Windows section to docs/BENCHMARKS.md with CPU vs CUDA numbers;
(7) `npm run tauri build -- --features cuda` and test the NSIS installer on a
clean user account; (8) update docs/WINDOWS_HANDOFF.md marking what's verified
and listing anything still open, and commit as you go with clear messages.

Work autonomously; test rigorously at each step; fix what you find. The bar is
commercial-grade: the Windows experience must be indistinguishable in polish
from the macOS one.
```
