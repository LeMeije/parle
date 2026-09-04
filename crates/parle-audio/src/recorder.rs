//! Utterance assembler: drains the capture channel IN ORDER on one thread,
//! resamples to 16 kHz mono, accumulates the utterance buffer, and emits
//! level updates. The audio is never discarded until the caller takes it.
//!
//! cpal's Stream is !Send, so a dedicated stream-owner thread creates and
//! holds it; the Recorder handle itself is Send.

use crate::capture::{self};
use crate::level::LevelMeter;
use crate::resample::StreamResampler;
use crate::{AudioChunk, ASR_SAMPLE_RATE};
use crossbeam_channel::Receiver;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Emitted ~30x/sec while recording, consumed by the HUD.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LevelUpdate {
    pub rms: f32,
    pub peak: f32,
    pub envelope: f32,
    pub elapsed_ms: u64,
}

pub struct Recording {
    /// 16 kHz mono f32.
    pub samples: Vec<f32>,
    pub duration_ms: u64,
    pub dropped_chunks: u64,
    pub device_name: String,
}

pub struct Recorder {
    stop_flag: Arc<AtomicBool>,
    result: Arc<Mutex<Option<RecordingState>>>,
    consumer: Option<std::thread::JoinHandle<()>>,
    stream_owner: Option<std::thread::JoinHandle<()>>,
    dropped: Arc<AtomicU64>,
    device_name: String,
}

struct RecordingState {
    samples: Vec<f32>,
    out_of_order: bool,
}

impl Recorder {
    /// Start capturing. `on_level` is called from the consumer thread.
    pub fn start(
        device_name: &str,
        mut on_level: impl FnMut(LevelUpdate) + Send + 'static,
    ) -> Result<Self, capture::CaptureError> {
        let (tx, rx) = capture::chunk_channel();
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Stream-owner thread: cpal Stream is !Send, so it lives (and dies) here.
        type InitMsg = Result<(u32, u16, String, Arc<AtomicU64>), capture::CaptureError>;
        let (init_tx, init_rx) = crossbeam_channel::bounded::<InitMsg>(1);
        let stream_owner = {
            let device_name = device_name.to_string();
            let stop_flag = stop_flag.clone();
            std::thread::Builder::new()
                .name("parle-audio-stream".into())
                .spawn(move || match capture::start(&device_name, tx) {
                    Ok(stream) => {
                        let _ = init_tx.send(Ok((
                            stream.sample_rate,
                            stream.channels,
                            stream.device_name.clone(),
                            stream.dropped_handle(),
                        )));
                        while !stop_flag.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(25));
                        }
                        drop(stream); // stops capture
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                    }
                })
                .expect("spawn stream owner")
        };

        let (input_rate, _channels, resolved_name, dropped) = match init_rx
            .recv_timeout(std::time::Duration::from_secs(10))
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(capture::CaptureError::Start("stream init timed out".into())),
        };

        let result = Arc::new(Mutex::new(Some(RecordingState {
            samples: Vec::with_capacity(ASR_SAMPLE_RATE as usize * 30),
            out_of_order: false,
        })));

        let consumer = {
            let stop_flag = stop_flag.clone();
            let result = result.clone();
            std::thread::Builder::new()
                .name("parle-audio-consumer".into())
                .spawn(move || {
                    consume(rx, input_rate, stop_flag, result, &mut on_level);
                })
                .expect("spawn audio consumer")
        };

        Ok(Self {
            stop_flag,
            result,
            consumer: Some(consumer),
            stream_owner: Some(stream_owner),
            dropped,
            device_name: resolved_name,
        })
    }

    /// Copy of the samples accumulated so far (for streaming partial passes).
    pub fn snapshot(&self) -> Vec<f32> {
        self.result
            .lock()
            .as_ref()
            .map(|s| s.samples.clone())
            .unwrap_or_default()
    }

    /// Duration captured so far, in ms.
    pub fn elapsed_ms(&self) -> u64 {
        self.result
            .lock()
            .as_ref()
            .map(|s| (s.samples.len() as u64 * 1000) / ASR_SAMPLE_RATE as u64)
            .unwrap_or(0)
    }

    /// End capture WITHOUT waiting for the threads to wind down.
    ///
    /// Split out of `stop` so the caller can cut the microphone the instant the
    /// user asks and pay the joins later on a worker thread. Until this flag is
    /// set the consumer keeps appending, so a stop that waits for a busy worker
    /// keeps recording in the meantime.
    ///
    /// Idempotent, and `stop`/`cancel` remain correct without it: calling this
    /// first only makes their joins return sooner.
    pub fn request_stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }

    /// Stop and take the recording. Blocks briefly while the consumer drains.
    pub fn stop(mut self) -> Recording {
        self.request_stop();
        if let Some(j) = self.stream_owner.take() {
            let _ = j.join();
        }
        if let Some(j) = self.consumer.take() {
            let _ = j.join();
        }
        let state = self.result.lock().take().unwrap_or(RecordingState {
            samples: Vec::new(),
            out_of_order: false,
        });
        if state.out_of_order {
            tracing::error!("audio chunks arrived out of order — transcript may be corrupted");
        }
        let duration_ms = (state.samples.len() as u64 * 1000) / ASR_SAMPLE_RATE as u64;
        Recording {
            samples: state.samples,
            duration_ms,
            dropped_chunks: self.dropped.load(Ordering::Relaxed),
            device_name: self.device_name.clone(),
        }
    }

    /// Abort without keeping audio.
    pub fn cancel(mut self) {
        self.request_stop();
        if let Some(j) = self.stream_owner.take() {
            let _ = j.join();
        }
        if let Some(j) = self.consumer.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }
}

/// Hard cap: a forgotten latched toggle must not grow without bound
/// (~230 MB/hour) or feed whisper's ~400 s per-pass limits. 15 minutes.
const MAX_SAMPLES: usize = 15 * 60 * ASR_SAMPLE_RATE as usize;

fn consume(
    rx: Receiver<AudioChunk>,
    input_rate: u32,
    stop_flag: Arc<AtomicBool>,
    result: Arc<Mutex<Option<RecordingState>>>,
    on_level: &mut (impl FnMut(LevelUpdate) + Send),
) {
    let mut resampler = StreamResampler::new(input_rate);
    let mut capped_logged = false;
    let mut meter = LevelMeter::new();
    let mut expected_seq: u64 = 0;
    let mut samples_since_level = 0usize;
    let level_interval = ASR_SAMPLE_RATE as usize / 30;

    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(chunk) => {
                let mut guard = result.lock();
                let Some(state) = guard.as_mut() else { break };
                if chunk.seq != expected_seq {
                    state.out_of_order = true;
                }
                expected_seq = chunk.seq + 1;
                let mono16k = resampler.push(&chunk.samples, chunk.channels);
                let (rms, peak, envelope) = meter.process(&mono16k);
                if state.samples.len() < MAX_SAMPLES {
                    state.samples.extend_from_slice(&mono16k);
                } else if !capped_logged {
                    capped_logged = true;
                    tracing::warn!("recording capped at 15 minutes; further audio discarded");
                }
                samples_since_level += mono16k.len();
                let elapsed_ms = (state.samples.len() as u64 * 1000) / ASR_SAMPLE_RATE as u64;
                drop(guard);
                if samples_since_level >= level_interval {
                    samples_since_level = 0;
                    on_level(LevelUpdate { rms, peak, envelope, elapsed_ms });
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if stop_flag.load(Ordering::SeqCst) {
                    // Drain whatever is still queued, in order, then flush.
                    while let Ok(chunk) = rx.try_recv() {
                        let mut guard = result.lock();
                        if let Some(state) = guard.as_mut() {
                            let mono16k = resampler.push(&chunk.samples, chunk.channels);
                            state.samples.extend_from_slice(&mono16k);
                        }
                    }
                    let tail = resampler.finish();
                    if let Some(state) = result.lock().as_mut() {
                        state.samples.extend_from_slice(&tail);
                    }
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                // Stream closed: flush and exit.
                let tail = resampler.finish();
                if let Some(state) = result.lock().as_mut() {
                    state.samples.extend_from_slice(&tail);
                }
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed synthetic chunks through the consumer logic without a real device.
    #[test]
    fn ordered_assembly_and_flush() {
        let (tx, rx) = capture::chunk_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(Some(RecordingState {
            samples: Vec::new(),
            out_of_order: false,
        })));
        let levels = Arc::new(Mutex::new(Vec::new()));

        // 1 second of 48 kHz stereo in 20 ms chunks.
        for seq in 0..50u64 {
            let samples: Vec<f32> = (0..1920)
                .map(|i| ((seq * 960 + i / 2) as f32 * 0.01).sin() * 0.4)
                .collect();
            tx.send(AudioChunk { seq, samples, channels: 2, sample_rate: 48_000 }).unwrap();
        }
        drop(tx);

        {
            let levels = levels.clone();
            let mut cb = move |u: LevelUpdate| levels.lock().push(u);
            consume(rx, 48_000, stop, result.clone(), &mut cb);
        }

        let state = result.lock().take().unwrap();
        assert!(!state.out_of_order);
        // ~1 s at 16 kHz (sinc latency tolerance).
        assert!(
            (state.samples.len() as i64 - 16_000).unsigned_abs() < 1200,
            "got {}",
            state.samples.len()
        );
        assert!(!levels.lock().is_empty());
    }

    #[test]
    fn out_of_order_detected() {
        let (tx, rx) = capture::chunk_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(Some(RecordingState {
            samples: Vec::new(),
            out_of_order: false,
        })));
        tx.send(AudioChunk { seq: 1, samples: vec![0.0; 320], channels: 1, sample_rate: 16_000 }).unwrap();
        tx.send(AudioChunk { seq: 0, samples: vec![0.0; 320], channels: 1, sample_rate: 16_000 }).unwrap();
        drop(tx);
        let mut cb = |_: LevelUpdate| {};
        consume(rx, 16_000, stop, result.clone(), &mut cb);
        assert!(result.lock().take().unwrap().out_of_order);
    }

    #[test]
    fn recorder_handle_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Recorder>();
    }
}
