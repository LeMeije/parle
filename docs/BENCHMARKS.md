# Parle Benchmarks

Reproduce with `cargo run --release --example bench -p parle-asr --features metal [runs]`
(fixtures in bench/fixtures, synthesised speech; median of 5 runs after warmup).

## macOS: Apple M2, 24 GB, Metal (measured 21/08/2026)

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

### Parakeet TDT v3 int8 (CPU, sherpa-onnx: measured 22/08/2026)

| model | fixture | audio_s | ms | xRT | notes |
|---|---|---|---|---|---|
| parakeet-tdt-v3-int8 | hello (6.1s) | 6.1 | 414 | 14.7x | perfect transcript |
| parakeet-tdt-v3-int8 | meeting (5.3s) | 5.1 | 371 | 13.9x | perfect transcript |
| parakeet-tdt-v3-int8 | email (30s) | 28.7 | 2108 | 13.6x | perfect transcript, load 876 ms |

Parakeet on CPU beats whisper-small on Metal for speed at comparable accuracy,
leaves the GPU free, and covers 25 European languages (no ja/ko/zh/hi/ar , 
whisper stays the universal default).

Live end-to-end (real microphone -> ordered pipeline -> resample -> Metal
whisper, base-q5_1, warm): **10 s of speech transcribed in 382 ms**; 6.1 s
fixture in 240 ms. Recording start latency is dominated by CoreAudio stream
open (~100-300 ms warm; first-ever open can take seconds: the recorder allows
10 s for init).

### What the numbers decided

- **Metal default = small-q5_1.** Turbo carries a ~2.6 s fixed cost per
  utterance on M2-class Metal, which breaks the "release -> text under a
  second" feel for short dictations. Small returns a 5-6 s utterance in
  ~0.7 s with full word accuracy on our fixtures. Turbo remains one click away
  for long-form quality (its cost amortises: 9.2x RT at 30 s).
- **base-q5_1 is the "fast mode"**: near-instant (0.2 s) and it scored 17/17
  expected words across fixtures. Real-world accents/noise will separate base
  and small more than synthesised fixtures do.

## Windows: Ryzen AI 9 370HX + RTX 5070 Ti, MEASURED 28/08/2026

`cargo run --release --example bench -p parle-asr --features cuda`, five runs
per fixture, warm. Only `whisper-large-v3-turbo-q8_0` is installed on this
machine, so it is the only row: the fallback ladder was never downloaded here.

| model | fixture | audio_s | median_ms | p95_ms | xRT | words_ok |
|---|---|---|---|---|---|---|
| whisper-large-v3-turbo-q8_0 | hello | 6.1 | 584 | 686 | 10.4x | 6/6 |
| whisper-large-v3-turbo-q8_0 | meeting | 5.1 | 562 | 578 | 9.2x | 4/4 |
| whisper-large-v3-turbo-q8_0 | email | 28.7 | 1147 | 1163 | 25.0x | 7/7 |

Accuracy was perfect on every fixture: 17/17 expected words.

**The prediction above this section was wrong and is corrected here.** Research
expected "well above 30x RT" for turbo on CUDA. The real number is 25x at 30
seconds and about 10x on a 5 to 6 second utterance, because the per-utterance
fixed cost dominates a short clip: roughly 500 ms of the ~580 ms is setup, which
is why xRT nearly triples when the audio gets five times longer. It amortises
exactly the way Metal's does, one third of the way up.

What matters for the product is the short-utterance latency, and 0.58 s from
release to text is comfortably inside the "under a second" feel that made
small-q5_1 the Metal default. On this machine turbo is fast enough to be the
default, which is not true on M2-class Metal.

### The CPU run is not recorded, deliberately

`cargo run --release --example bench -p parle-asr` without `--features cuda`
returned ~582 SECONDS per fixture, and, tellingly, **the same ~582 s for 6.1 s
of audio as for 28.7 s**. A figure that does not move with the length of the
input is not measuring transcription, so it is not a CPU benchmark and writing
it down as one would be worse than leaving the row empty. Most likely the
non-CUDA build falls back to a scalar reference path with no BLAS or AVX kernel
selected. Worth one session with `whisper.cpp`'s own backend logging before any
CPU number is published; until then this machine has no trustworthy CPU figure.

Parakeet int8 on Windows remains unmeasured, and the engine is still not
implemented.

## Memory: Apple M2, 24 GB, Metal (measured 13/10/2026)

Why this section exists: Parle held the speech model in memory for as long as
it ran. These numbers are the case for freeing it when idle
(`src-tauri/src/model_idle.rs`) and loading it again the moment a recording
starts (`Work::Preload`). Memory here is phys_footprint, the figure in Activity
Monitor's Memory column. The Mac was heavily loaded by unrelated builds
throughout (15 minute load average up to 350), so transcription times in this
section run two to three times those in the tables above and are only ever
compared within one run.

### The installed app before the change (Large v3 Turbo q5_0, idle)

| process | footprint |
|---|---|
| parle | 607 MB (peak 799 MB) |
| of which the model's weights, one 548 MB block shared with the GPU | 548 MB |
| main window web content | 157 MB (peak 431 MB) |
| HUD web content | 69 MB (peak 152 MB) |
| WebKit GPU and networking helpers | 17 MB and 7 MB |

`vmmap` showed 547.1 of the weights' 547.8 MB swapped or compressed out of RAM
while the app sat idle, and the same 48 s after a dictation finished, at memory
pressure "warning". ggml-metal requests residency for 3 minutes after each use,
but under pressure macOS evicts the weights anyway, so the next dictation reads
them back from swap.

### Probe

`cargo run --release --example memory -p parle-asr --features metal -- <model.bin> 0 [--cycles N] [--settle S] [--breakdown] [--cold]`

| measurement | result |
|---|---|
| nothing loaded | 5 MB |
| model loaded | 637 to 641 MB (8 runs) |
| peak during one dictation | 78 to 149 MB above loaded |
| straight after dropping the model | 108 to 231 MB |
| dropped, once macOS has reclaimed the pages (10 to 30 s) | 29 to 44 MB |
| reload, file in the OS cache, process already warm | median 0.59 s over 35 reloads, 0.21 to 1.6 s |
| reload, file evicted from the OS cache | 0.87 s, against 0.43 to 0.77 s cached in the same run |
| first pass after a reload | within the run-to-run noise of a warm pass |
| first load by a new binary, Metal shader cache cold | 19.9 s once; 0.39 to 1.35 s in later processes |

The cold-cache first load is what the startup pre-warm absorbs, and why it
stays. After that, a reload is the file read plus the GPU upload: the device,
shader library and compiled pipelines live in a process-wide singleton in
ggml-metal and survive a release.

Leads that went nowhere, recorded so nobody repeats them:

- **Flash attention.** whisper.cpp defaults it on, whisper-rs 0.16 turns it
  off. Peak 757.6 MB with it against 753.6 MB without, inside the 78 to 149 MB
  spread of the peak itself: no measurable memory benefit. It
  also changed the capitalisation and punctuation of the hello fixture's
  transcript, and its speed effect could not be judged on a loaded machine, so
  it is not part of this change.
- **The residual straight after a drop is not a leak.** Over 12
  load/dictate/drop cycles that figure rose for two cycles and then held
  between 188 and 225 MB, and a settle run fell to 38 MB with no further work.
  Draining an Objective-C autorelease pool around each cycle changed nothing,
  and `malloc_zone_pressure_relief` freed 0 bytes: macOS reclaims these pages
  lazily, on its own schedule.

### Ben's own usage (this Mac's history, Turbo q5_0, 22/08 to 13/10/2026)

1,549 dictations over 48 active days. Recording length: median 34.4 s, 5th
percentile 4.0 s, 1.3% under 2 s. Transcription time after stopping: median
12.2 s. Time between saved dictations: median 5.4 min (25th percentile 2.0,
75th 19.0).

| idle release after | dictations that start with the model released | model held per active day |
|---|---|---|
| 2 min | 75.6% | 0.94 h |
| 5 min (the macOS default) | 52.4% | 1.96 h |
| 15 min | 29.8% | 4.00 h |
| 60 min | 8.7% | 7.78 h |

At five minutes, none of the recordings that start with the model released was
shorter than one second and 95% ran past 6.4 s, so the reload finishes while
the user is still talking. "Held per active day" counts the timeout alone;
releases under memory pressure come on top.

### In the installed app after the change

Release build installed 13/10/2026 at 14:07:47; pre-warmed at 14:07:55 with a
footprint of 679 MB. Two dictations followed with the model loaded (recordings
of 300 s and 36 s). At 14:20:19 the reaper freed it, 194 s after its last use,
with memory pressure at "warning". Its own log line: "app footprint 696 MB just
before the release, 59 MB 30 s later". Checked independently: `footprint`
59 MB, `vmmap` 59.3 MB, and the weights' shared-memory block down from
547.8 MB to 1 MB.
