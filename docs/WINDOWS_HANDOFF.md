# Parle — Windows status

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
| Rust | stable, `x86_64-pc-windows-msvc`. `rustup-init` **prompts** — and so appears to hang — if MSVC is missing. Install Build Tools first. |
| VS Build Tools 2022 | "Desktop development with C++" (MSVC + Windows SDK). Required by whisper.cpp's cmake build. |
| CMake | 4.4.2, on PATH. |
| LLVM | Required by `bindgen`; `LIBCLANG_PATH` must point at `C:\Program Files\LLVM\bin` or the build fails with "Unable to find libclang". |
| CUDA | 13.3. MSBuild resolves the toolkit through `CUDA_PATH_V13_3`, not `CUDA_PATH`; a shell that predates the install fails with "The CUDA Toolkit directory '' does not exist". |
| Node | 20+. |
| WebView2 | preinstalled on Windows 11. |

`env.sh` at the repo root sets all of these. It is gitignored because the paths
are machine-specific. **Source it before every cargo or tauri command:**

```bash
source ./env.sh
```

## Verified on hardware

- The workspace compiles clean. `platform/windows.rs` was written on the Mac
  against the researched API surface and compiled on Windows with **no
  signature drift** — the anticipated HWND/BOOL/HGLOBAL churn did not happen.
- Full test suite green on Windows, including the shared contract vectors.
- CUDA build, NSIS installer, install and run on this machine.
- Core loop end to end: hold hotkey, speak, release, text lands in the target
  app, clipboard restored, history row created.
- The HUD does not steal focus; the caret stays in the target app.
- **Copilot key**: starts and stops dictation, and the Copilot app never opens.
  This took a different architecture from the Mac plan — see below.
- Tray icon renders correctly (opaque squares and the white matte around the
  glyph were both real bugs, now fixed), with a distinct recording state and a
  user-selectable icon style.
- Mic permission: real consent state read from the CapabilityAccessManager
  registry, with a working "open Settings" action.
- Esc during recording no longer discards the take.

## The Copilot key needed its own process

The Mac-side plan — a `WH_KEYBOARD_LL` hook inside the app — is correct in
principle and does not survive contact with a busy Tauri process. Windows
silently removes any hook whose proc exceeds `LowLevelHooksTimeout` (~300 ms),
and the app's own startup and transcription work were enough to trip it.

The hook now lives in `crates/echokey-hook`, built as a separate ~230 KB
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
logs showed a clean 1:1 press-to-event ratio and the debounce was reverted — it
added 75 ms of latency for a bug that did not exist.

## Still open

- **Windows benchmarks have not been run.** `docs/BENCHMARKS.md` still contains
  only the M2 Metal numbers; its Windows section is a prediction, not a
  measurement. Run both and replace it:
  ```bash
  cargo run --release --example bench -p echokey-asr                  # CPU
  cargo run --release --example bench -p echokey-asr --features cuda  # CUDA
  ```
- **Win+V exclusion is implemented but not verified on hardware.** Dictate, then
  press Win+V: the transcript must not appear.
- **Parakeet on Windows is unverified.** The sherpa-onnx build script downloads
  prebuilt libraries at build time (needs network, or `SHERPA_ONNX_LIB_DIR`).
- **Clean-account install** of the NSIS bundle has not been tested.
- **Elevated windows**: UIPI means the hook and `SendInput` cannot reach apps
  running elevated. Accepted gap; not surfaced in the UI.
- **LAN sync** is built and covered by tests including two-peer exchanges over
  real sockets, but has never run between two physical machines — there is only
  one Windows box here. See `docs/SYNC_DESIGN.md`.
- **Linux** has not been attempted.

## A packaging trap worth remembering

`tauri-build` emits no `rerun-if-changed` for the icon files, so changing an
icon and rebuilding ships the **old** icon with no warning and no error. Force
it by touching `build.rs` or clearing `target/release/build/echokey-*`. The only
reliable confirmation is a byte-search of the produced executable for the new
icon data.

## Behavioural contract

`shared/formatter-test-vectors.json` and `shared/dictionary-test-vectors.json`
are the spec; `cargo test -p echokey-core` runs them. If Windows behaviour ever
has to differ — it should not, the formatter and dictionary are pure Rust —
change the vectors first and flag it.
