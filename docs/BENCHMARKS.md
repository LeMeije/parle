# Parle Benchmarks

Reproduce with `cargo run --release --example bench -p parle-asr --features metal [runs]`
(fixtures in bench/fixtures, synthesised speech; median of 5 runs after warmup).

## macOS — Apple M2, 24 GB, Metal (measured 21/08/2026)

| model | fixture | audio_s | median_ms | p95_ms | xRT | words_ok |
|---|---|---|---|---|---|---|
| whisper-base-q5_1 | hello (6.1s) | 6.1 | 209 | 217 | 29.1x | 6/6 |
| whisper-base-q5_1 | meeting (5.3s) | 5.1 | 192 | 194 | 26.8x | 4/4 |
| whisper-base-q5_1 | email (30s) | 28.7 | 460 | 550 | 62.4x | 7/7 |
| whisper-small-q5_1 | hello (6.1s) | 6.1 | 709 | 716 | 8.6x | 6/6 |
| whisper-small-q5_1 | meeting (5.3s) | 5.1 | 635 | 642 | 8.1x | 4/4 |
| whisper-small-q5_1 | email (30s) | 28.7 | 1212 | 1215 | 23.7x | 7/7 |
| whisper-large-v3-turbo-q5_0 | hello (6.1s) | 6.1 | 2675 | 2680 | 2.3x | 6/6 |
| whisper-large-v3-turbo-q5_0 | meeting (5.3s) | 5.1 | 2690 | 2750 | 1.9x | 4/4 |
| whisper-large-v3-turbo-q5_0 | email (30s) | 28.7 | 3111 | 3143 | 9.2x | 7/7 |

### Parakeet TDT v3 int8 (CPU, sherpa-onnx — measured 22/08/2026)

| model | fixture | audio_s | ms | xRT | notes |
|---|---|---|---|---|---|
| parakeet-tdt-v3-int8 | hello (6.1s) | 6.1 | 414 | 14.7x | perfect transcript |
| parakeet-tdt-v3-int8 | meeting (5.3s) | 5.1 | 371 | 13.9x | perfect transcript |
| parakeet-tdt-v3-int8 | email (30s) | 28.7 | 2108 | 13.6x | perfect transcript, load 876 ms |

Parakeet on CPU beats whisper-small on Metal for speed at comparable accuracy,
leaves the GPU free, and covers 25 European languages (no ja/ko/zh/hi/ar —
whisper stays the universal default).

Live end-to-end (real microphone -> ordered pipeline -> resample -> Metal
whisper, base-q5_1, warm): **10 s of speech transcribed in 382 ms**; 6.1 s
fixture in 240 ms. Recording start latency is dominated by CoreAudio stream
open (~100-300 ms warm; first-ever open can take seconds — the recorder allows
10 s for init).

### What the numbers decided

- **Metal default = small-q5_1.** Turbo carries a ~2.6 s fixed cost per
  utterance on M2-class Metal, which breaks the "release -> text under a
  second" feel for short dictations. Small returns a 5-6 s utterance in
  ~0.7 s with full word accuracy on our fixtures. Turbo remains one click away
  for long-form quality (its cost amortises: 9.2x RT at 30 s).
- **base-q5_1 is the "fast mode"** — near-instant (0.2 s) and it scored 17/17
  expected words across fixtures. Real-world accents/noise will separate base
  and small more than synthesised fixtures do.

## Windows — Ryzen AI 9 370HX + RTX 5070 Ti (TO MEASURE)

Run on the G14 (see docs/WINDOWS_HANDOFF.md):
`cargo run --release --example bench -p parle-asr` (CPU) and
`--features cuda` (GPU), then append results here. Expected from research:
turbo-q8_0 well above 30x RT on CUDA; Parakeet int8 ~25-40x RT on CPU
(engine not yet implemented).
