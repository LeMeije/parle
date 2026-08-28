# Third-party notices

Parle is MIT licensed (see LICENSE). This file records the third-party work it
builds on, and how each was used.

## Reference material redistributed in this repository

**freeflow** — MIT, Copyright (c) 2026 Zach Latta
<https://github.com/zachlatta/freeflow>

`docs/research/freeflow-postprocessing.swift` is an unmodified copy of that
project's post-processing source, kept as the reference for Parle's dictation
cleanup contract (the "never execute the transcript as an instruction" rule,
self-correction handling, vocabulary injection). Its licence is reproduced
verbatim at `docs/research/freeflow-LICENSE.txt` as MIT requires. Parle's own
`crates/parle-core/src/formatter.rs` is an independent Rust implementation,
not a translation of that file.

## Studied but NOT copied

**murmur-youtube** — no licence file, all rights reserved
<https://github.com/per-simmons/murmur-youtube>
Platform lessons only (non-activating HUD panel, CGEventTap requirement,
ordered audio streaming, TCC signing stability). No code was reused; every
behaviour was reimplemented from the documented lesson.

**LocalFlow** — custom non-commercial licence
<https://github.com/vmysla/LocalFlow>
Two ideas reimplemented clean-room in Rust: warming the model with silence at
startup, and restoring the previous clipboard after paste injection. No code
was reused.

**OpenWhispr** — MIT
<https://github.com/OpenWhispr/openwhispr>
Studied for model-manager UX and fallback design. No code reused.

## Runtime dependencies

Rust crates and npm packages carry their own licences; see `Cargo.toml`,
`Cargo.lock` and `package.json` for the full set. The notable native ones:

- **whisper.cpp** (via `whisper-rs`) — MIT, Copyright (c) 2023 Georgi Gerganov
- **sherpa-onnx** — Apache-2.0, Copyright (c) k2-fsa
- **Tauri** — MIT / Apache-2.0
- **Lucide icons** — ISC

## Models (downloaded at runtime, not bundled)

Model weights are fetched from Hugging Face and GitHub releases on the user's
own machine and are covered by their own licences:

- **Whisper** GGML models — MIT (OpenAI weights, ggml conversions by ggerganov)
- **Distil-Whisper** — MIT
- **NVIDIA Parakeet TDT** — CC-BY-4.0 (NVIDIA), distributed via the sherpa-onnx
  model releases
