# Parle: Windows status

The Windows platform layer is **built, installed, and in daily use** on the
ASUS Zephyrus G14 2025 (Ryzen AI 9 370HX, 64 GB, RTX 5070 Ti). This file was
originally a brief for a build that had never been compiled; it is now a record
of what is verified, what changed, and what is still open.

The macOS build remains the reference for behaviour. The behavioural contract
(`shared/formatter-test-vectors.json`, `shared/dictionary-test-vectors.json`)
passes unmodified on both platforms.

## Toolchain actually used

Recorded because two of these are not obvious and both cost a build.

| Component | Version / note |
|---|---|
| Rust | stable, `x86_64-pc-windows-msvc`. `rustup-init` **prompts**: and so appears to hang: if MSVC is missing. Install Build Tools first. |
| VS Build Tools 2022 | "Desktop development with C++" (MSVC + Windows SDK). Required by whisper.cpp's cmake build. |
| CMake | 4.4.2, on PATH. |
| LLVM | Required by `bindgen`; `LIBCLANG_PATH` must point at `C:\Program Files\LLVM\bin` or the build fails with "Unable to find libclang". |
| CUDA | 13.3. MSBuild resolves the toolkit through `CUDA_PATH_V13_3`, not `CUDA_PATH`; a shell that predates the install fails with "The CUDA Toolkit directory '' does not exist". |
| Node | 20+. |
| WebView2 | preinstalled on Windows 11. |

`env.sh` at the repo root sets all of these. It is gitignored because the paths
are machine-specific, so a FRESH CLONE DOES NOT HAVE ONE: write it yourself
before anything else. It needs `LIBCLANG_PATH`, `CUDA_PATH_V13_3` and whatever
your machine needs on `PATH`. **Source it before every cargo or tauri command:**

```bash
source ./env.sh
```

## A fresh clone does not build until the sidecar is staged

`cargo test` and `cargo build` fail on a clean checkout with:

```
resource path `binaries/parle-hook-<triple>` doesn't exist
```

The keyboard hook is a separate binary that Tauri expects to find already
staged, and `cargo` does not run `beforeBuildCommand`, so nothing produces it
on a plain `cargo` invocation. Run this once after cloning, and again whenever
`crates/parle-hook` changes:

```bash
node scripts/build-hook.mjs
```

Then, in order:

```bash
npm install
node scripts/build-hook.mjs
source ./env.sh
cargo test -p parle-core && cargo test -p parle-sync && cargo test -p parle --lib
npm run tauri build
```

Do NOT run `cargo test --workspace` bare: it blows past a ten minute timeout.
Run the three packages separately, as above.

## Verified on hardware

- The workspace compiles clean. `platform/windows.rs` was written on the Mac
  against the researched API surface and compiled on Windows with **no
  signature drift**: the anticipated HWND/BOOL/HGLOBAL churn did not happen.
- Full test suite green on Windows, including the shared contract vectors.
- CUDA build, NSIS installer, install and run on this machine.
- Core loop end to end: hold hotkey, speak, release, text lands in the target
  app, clipboard restored, history row created.
- The HUD does not steal focus; the caret stays in the target app.
- **Copilot key**: starts and stops dictation, and the Copilot app never opens.
  This took a different architecture from the Mac plan: see below.
- Tray icon renders correctly (opaque squares and the white matte around the
  glyph were both real bugs, now fixed), with a distinct recording state and a
  user-selectable icon style.
- Mic permission: real consent state read from the CapabilityAccessManager
  registry, with a working "open Settings" action.
- Esc during recording no longer discards the take.

## The Copilot key needed its own process

The Mac-side plan: a `WH_KEYBOARD_LL` hook inside the app: is correct in
principle and does not survive contact with a busy Tauri process. Windows
silently removes any hook whose proc exceeds `LowLevelHooksTimeout` (~300 ms),
and the app's own startup and transcription work were enough to trip it.

The hook now lives in `crates/parle-hook`, built as a separate ~230 KB
`parle-hook.exe` helper that does nothing but pump messages. The app talks to it
over named pipes and keeps it inside a job object with
`KILL_ON_JOB_CLOSE`, so the helper cannot outlive the app.

Two things about this are load-bearing:

- **Two unidirectional pipes, not one duplex pipe.** Windows serialises I/O per
  file object, so a blocking read parked on a synchronous handle blocks
  concurrent writes on that same handle. With one duplex pipe the UI froze
  outright.
- `ERROR_PIPE_CONNECTED` from `ConnectNamedPipe` means *already connected*, not
  failure. Treating it as an error cost five reconnect attempts on every start.

The rules commented in `platform/windows.rs` are still binding: never split a
swallow across key-down and key-up; inject the dummy VK 0xFF while LWin is held
so the Start menu does not open; skip `LLKHF_INJECTED`; keep the hook proc
allocation-free.

Note on a dead end, recorded so it is not re-investigated: F23 auto-repeat was
suspected of defeating the press latch and a debounce was added for it. The
logs showed a clean 1:1 press-to-event ratio and the debounce was reverted: it
added 75 ms of latency for a bug that did not exist.

## What this round means for Windows (10/09/2026)

The Refine rework (commit `329094c`) is **macOS-verified only**. It changed no
Rust outside `src-tauri/src/refine.rs` and did not touch `crates/parle-hook`,
so **the wire protocol is unchanged by this round** and no sidecar rebuild is
needed for it. The outstanding `node scripts/build-hook.mjs` is still the one
owed from the 04/09 Refine work, which did extend the frame.

What needs checking on the G14, in order of how likely it is to be wrong:

**1. Where Claude Code actually lives.** The round's whole point was that the
tool is inside its desktop app and not on PATH. On macOS that is
`~/Library/Application Support/Claude/claude-code/<version>/claude.app/Contents/MacOS/claude`,
verified. The Windows equivalent in `desktop_app_cli_dirs()` is
`%APPDATA%\Claude\claude-code\<version>\…`, **reasoned by analogy and never
checked**. Find the truth with:

```powershell
Get-ChildItem -Path $env:APPDATA, $env:LOCALAPPDATA -Filter claude* -Recurse -Depth 3 -ErrorAction SilentlyContinue
where.exe claude
```

If the layout differs, fix `desktop_app_cli_dirs()` rather than telling the
user to set a path. The pre-existing paths are still searched and are the more
likely hit on Windows anyway: the native installer puts `claude.exe` in
`%USERPROFILE%\.local\bin`, npm puts `claude.cmd` in `%APPDATA%\npm`.

**2. Codex will not be found.** The only Codex path added is
`/Applications/ChatGPT.app/Contents/Resources`, a macOS bundle path that never
matches on Windows. If the Windows ChatGPT app ships a `codex.exe`, nobody has
looked for it. Same search as above with `codex*`.

**3. The `.app` bundle handling is a harmless no-op.** `resolve_explicit()`
only enters that branch for a path whose extension is `.app`, so on Windows it
falls through to the ordinary file check. The `~` expansion and quote stripping
DO apply and are worth a quick try in the path field.

**4. The model dropdown.** It parses the `--model` paragraph out of
`<tool> --help`. That runs through the same `probe()` helper as `--version`, so
an npm `.cmd` shim goes via `cmd.exe`; the arguments contain no newline, so the
refusal that bit the system prompt does not apply here. Expect
`fable, opus, sonnet, haiku` in the dropdown. If it comes back empty, the help
text did not parse and the field falls back to free text, which is a
degradation and not a break.

**5. The Settings layout.** The path row is now a picker with no text box, and
the result row is a full-width row with a green tick. Both are plain CSS with
no platform branching, but the guidance text is platform-specific: Windows
should read "choose the claude program file: claude.exe if it came from an
installer, claude.cmd if it came from npm" and must NOT mention `.app`
bundles.

**6. Nothing here has been compiled on Windows.** As ever, `windows.rs` cannot
be built from the Mac (cross-compiling `ring` needs a Windows C toolchain), so
this round was reviewed by reading.

A verification pass, after `source ./env.sh` and the sidecar build:

```bash
cargo test -p parle-core && cargo test -p parle-sync && cargo test -p parle --lib
cargo test -p parle --lib live_program_discovery -- --ignored --nocapture   # prints where it looked
npm run tauri build
```

`live_program_discovery` is free, runs no API call, and prints the resolved
path per provider. It is the fastest way to answer "did detection work here".

## Still open

- **Windows benchmarks have not been run.** `docs/BENCHMARKS.md` still contains
  only the M2 Metal numbers; its Windows section is a prediction, not a
  measurement. Run both and replace it:
  ```bash
  cargo run --release --example bench -p parle-asr                  # CPU
  cargo run --release --example bench -p parle-asr --features cuda  # CUDA
  ```
- **Win+V exclusion is implemented but not verified on hardware.** Dictate, then
  press Win+V: the transcript must not appear.
- **Parakeet on Windows is unverified.** The sherpa-onnx build script downloads
  prebuilt libraries at build time (needs network, or `SHERPA_ONNX_LIB_DIR`).
- **Clean-account install** of the NSIS bundle has not been tested.
- **Elevated windows**: UIPI means the hook and `SendInput` cannot reach apps
  running elevated. Accepted gap; not surfaced in the UI.
- **LAN sync** is built and covered by tests including two-peer exchanges over
  real sockets, but has never run between two physical machines: there is only
  one Windows box here. See `docs/SYNC_DESIGN.md`.
- **Linux** has not been attempted.

## A packaging trap worth remembering

`tauri-build` emits no `rerun-if-changed` for the icon files, so changing an
icon and rebuilding ships the **old** icon with no warning and no error. Force
it by touching `build.rs` or clearing `target/release/build/parle-*`. The only
reliable confirmation is a byte-search of the produced executable for the new
icon data.

## Behavioural contract

`shared/formatter-test-vectors.json` and `shared/dictionary-test-vectors.json`
are the spec; `cargo test -p parle-core` runs them. If Windows behaviour ever
has to differ: it should not, the formatter and dictionary are pure Rust , 
change the vectors first and flag it.
