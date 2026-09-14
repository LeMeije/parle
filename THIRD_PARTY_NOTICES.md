# Third-party notices

Parle is source-available under PolyForm Noncommercial 1.0.0 plus an
internal-use grant (see LICENSE). This file records the third-party work it
builds on, and how each was used.

No third-party source code is reused or redistributed in this repository.
Everything below was studied; every behaviour was implemented independently.

## Studied but NOT copied

**freeflow**: MIT, Copyright (c) 2026 Zach Latta
<https://github.com/zachlatta/freeflow>
Design lessons only: the idea that a dictation transcript must be treated as
data and never executed as an instruction, self-correction handling, and
vocabulary injection as a spelling reference. No code was reused and no prompt
text was reused. Parle's cleanup is a deterministic Rust implementation
(`crates/parle-core/src/formatter.rs`) and its rewriting contract
(`src-tauri/src/refine.rs`) was written independently, with different
behaviour: freeflow makes minimal literal edits, Parle reorganises.

**murmur-youtube**: no licence file, all rights reserved
<https://github.com/per-simmons/murmur-youtube>
Platform lessons only (non-activating HUD panel, CGEventTap requirement,
ordered audio streaming, TCC signing stability). No code was reused; every
behaviour was reimplemented from the documented lesson.

**LocalFlow**: custom non-commercial licence
<https://github.com/vmysla/LocalFlow>
Two ideas reimplemented clean-room in Rust: warming the model with silence at
startup, and restoring the previous clipboard after paste injection. No code
was reused.

**OpenWhispr**: MIT
<https://github.com/OpenWhispr/openwhispr>
Studied for model-manager UX and fallback design. No code reused.

## Runtime dependencies

Rust crates and npm packages carry their own licences; see `Cargo.toml`,
`Cargo.lock` and `package.json` for the full set. The notable native ones:

- **whisper.cpp** (via `whisper-rs`): MIT, Copyright (c) 2023 Georgi Gerganov
- **sherpa-onnx**: Apache-2.0, Copyright (c) k2-fsa
- **Tauri**: MIT / Apache-2.0
- **Lucide icons**: ISC

These are permissive licences. They allow Parle as a whole to be distributed
under the more restrictive terms in LICENSE; each dependency remains under its
own licence.

## Models (downloaded at runtime, not bundled)

Model weights are fetched from Hugging Face and GitHub releases on the user's
own machine and are covered by their own licences:

- **Whisper** GGML models: MIT (OpenAI weights, ggml conversions by ggerganov)
- **Distil-Whisper**: MIT
- **NVIDIA Parakeet TDT**: CC-BY-4.0 (NVIDIA), distributed via the sherpa-onnx
  model releases
