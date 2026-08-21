# ASR Engine Research (verified 21/08/2026)

## Decision
- PRIMARY both platforms: whisper.cpp via `whisper-rs = "0.16.0"` (vendors whisper.cpp 1.8.3; cmake, no
  build-time network). Features: macOS `["metal"]` (skip coreml — encoder-only gain, mlmodelc pain);
  Windows `["cuda"]` (CUDA toolkit 12.8+ required for Blackwell/sm_120) + a CPU-only binary fallback path.
- SECONDARY (cargo feature `parakeet`): official `sherpa-onnx = "1.13.5"` crate (k2-fsa in-tree; sherpa-rs
  is ARCHIVED, do not use). Parakeet TDT 0.6B int8 on CPU (~30x RT on desktop CPUs; CoreML EP measured
  SLOWER than CPU on M-series, k2-fsa#2910 — always CPU). Build script downloads prebuilt libs unless
  SHERPA_ONNX_LIB_DIR is set — cache in CI.
- VAD: whisper.cpp built-in Silero via whisper-rs WhisperVadContext + ggml-silero-v5.1.2.bin (0.88 MB).
  `earshot` 1.2.2 (pure Rust) as the cheap always-on mic gate.
- LLM cleanup: `llama-cpp-2 = "0.1.154"` behind `cleanup` feature. Models: Qwen3-1.7B-Q8_0 (1.83 GB,
  16GB+ machines) / Qwen3-0.6B-Q8_0 (639 MB, 8 GB Mac). Opt-in download, never bundled. Use /no_think.
- WATCH: whisper.cpp v1.9.0+ has NATIVE Parakeet support; unreachable until whisper-rs bumps past 1.8.3.
  Re-check whisper-rs releases; a bump collapses the two-engine story into one.
- Excluded: faster-whisper (Python-only), candle whisper (no VAD/streaming ecosystem, slower),
  WhisperKit/FluidAudio (Swift-only; FluidAudio ANE sidecar = post-v1 macOS optimisation, 96x+ RT),
  mistral.rs (no ASR).

## Model registry (exact URLs)
whisper GGML base: https://huggingface.co/ggerganov/whisper.cpp/resolve/main/<file>
| file | size | langs |
|---|---|---|
| ggml-tiny.bin / q5_1 / q8_0 | 77.7 / 32.2 / 43.5 MB | multi (en variants exist) |
| ggml-base.bin / q5_1 / q8_0 | 148 / 59.7 / 81.8 MB | multi |
| ggml-small.bin / q5_1 / q8_0 | 488 / 190 / 264 MB | multi |
| ggml-medium.bin / q5_0 / q8_0 | 1.53 GB / 539 / 823 MB | multi |
| ggml-large-v3-turbo.bin / q5_0 / q8_0 | 1.62 GB / 574 / 874 MB | multi ONLY |
VAD: https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin (0.88 MB)
Distil (English): https://huggingface.co/distil-whisper/distil-large-v3.5-ggml

sherpa bundles: https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/<file>
| bundle | size | langs |
|---|---|---|
| sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2 | 487 MB | 25 European, auto lang ID |
| sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2 | 483 MB | English (better En WER than v3) |
| ...unified-en-0.6b-int8-streaming-{240,560,1120}ms.tar.bz2 | 501 MB | English true streaming |
| ...parakeet_tdt_ctc_110m-en-36000-int8.tar.bz2 | 104 MB | English small tier |
LLM: https://huggingface.co/Qwen/Qwen3-0.6B-GGUF (Q8_0 639 MB), Qwen/Qwen3-1.7B-GGUF (Q8_0 1.83 GB)

## Model ladder
- macOS M2-class: fast=base-q5_1, default=small-q5_1, quality=large-v3-turbo-q5_0 (16GB+; OK on this 24GB M2).
- Windows RTX: default=large-v3-turbo-q8_0, quality=large-v3-q5_0, cpu-fallback=small-q5_1.
- Speed/multilingual alt both: parakeet-tdt-0.6b-v3-int8 (CPU).

## Chunking strategy (ordering-safe)
Cheap gate (earshot) on live stream; cut utterances on 500-800ms trailing silence, forced cut at 6-15s at
nearest silence; closed chunks -> bounded queue -> ONE inference worker (order trivially preserved; whisper
contexts aren't usefully parallel on one GPU); carry last ~200ms into next chunk; optionally feed previous
text as initial_prompt. Parakeet unified streaming via OnlineRecognizer = inherent ordering + partials.

## Latency expectations (extrapolated — MUST benchmark on real hardware; bench/ exists for this)
- M2 Air Metal: turbo ~6-10x RT; small-q5 ~7x; base-q5 ~15x. (M2 Pro measured: turbo ~21x w/ flash attn.)
- Parakeet int8 CPU: ~30x RT on i7-12700KF measured; HX 370 est 25-40x; M2 CPU est 8-15x.
- RTX 5070 Ti CUDA turbo: >30x RT (estimate, no published figure).
- FluidAudio ANE reference (not our stack): TDT-CTC-110M 96.5x on M2.

## Gotchas
- sherpa config: ModelType="nemo_transducer" mandatory; FeatureDim=128; 16kHz mono f32; 400s max utterance
  (encoder position table); 4 threads > 8; ~2GB resident.
- whisper-rs: WhisperContext -> create_state per job; segment callbacks for streaming-ish partials;
  full VAD API exposed.
- Warmup: transcribe 1s of zeros at startup (clean-room LocalFlow idea).
