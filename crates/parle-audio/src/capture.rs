//! cpal input stream management.
//!
//! Callback discipline: copy the samples, stamp a sequence number, try_send to
//! a bounded channel, return. Never block, never allocate more than the copy,
//! never touch a lock the UI holds. If the channel is full we drop the newest
//! chunk and count it (the consumer sees the gap via `seq`).

use crate::AudioChunk;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no input device available")]
    NoDevice,
    #[error("input device '{0}' not found")]
    DeviceNotFound(String),
    #[error("device has no supported input config: {0}")]
    NoConfig(String),
    #[error("failed to build input stream: {0}")]
    Build(String),
    #[error("failed to start input stream: {0}")]
    Start(String),
}

pub struct CaptureStream {
    // Kept alive; dropping stops capture.
    _stream: cpal::Stream,
    pub sample_rate: u32,
    pub channels: u16,
    pub device_name: String,
    dropped: Arc<AtomicU64>,
}

impl CaptureStream {
    /// Chunks dropped because the consumer fell behind (should stay 0).
    pub fn dropped_chunks(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Shareable handle to the drop counter (the stream itself is !Send).
    pub fn dropped_handle(&self) -> Arc<AtomicU64> {
        self.dropped.clone()
    }
}

/// List input device names for the settings UI.
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Open the named device ("" = system default) and start capturing into `tx`.
pub fn start(device_name: &str, tx: Sender<AudioChunk>) -> Result<CaptureStream, CaptureError> {
    let host = cpal::default_host();
    let device = if device_name.is_empty() {
        host.default_input_device().ok_or(CaptureError::NoDevice)?
    } else {
        host.input_devices()
            .map_err(|e| CaptureError::NoConfig(e.to_string()))?
            .find(|d| d.name().map(|n| n == device_name).unwrap_or(false))
            .ok_or_else(|| CaptureError::DeviceNotFound(device_name.to_string()))?
    };
    let resolved_name = device.name().unwrap_or_else(|_| "unknown".into());

    let config = device
        .default_input_config()
        .map_err(|e| CaptureError::NoConfig(e.to_string()))?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

    let seq = Arc::new(AtomicU64::new(0));
    let dropped = Arc::new(AtomicU64::new(0));
    let err_fn = |e| tracing::error!("audio stream error: {e}");

    macro_rules! build {
        ($ty:ty, $convert:expr) => {{
            let tx = tx.clone();
            let seq = seq.clone();
            let dropped = dropped.clone();
            device
                .build_input_stream(
                    &stream_config,
                    move |data: &[$ty], _| {
                        // COPY out of the device buffer — it is recycled the
                        // instant this callback returns.
                        let samples: Vec<f32> = data.iter().map($convert).collect();
                        let chunk = AudioChunk {
                            seq: seq.fetch_add(1, Ordering::Relaxed),
                            samples,
                            channels,
                            sample_rate,
                        };
                        match tx.try_send(chunk) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => {
                                dropped.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(TrySendError::Disconnected(_)) => {}
                        }
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| CaptureError::Build(e.to_string()))?
        }};
    }

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build!(f32, |s: &f32| *s),
        cpal::SampleFormat::I16 => build!(i16, |s: &i16| *s as f32 / 32768.0),
        cpal::SampleFormat::U16 => build!(u16, |s: &u16| (*s as f32 - 32768.0) / 32768.0),
        cpal::SampleFormat::I32 => build!(i32, |s: &i32| *s as f32 / 2_147_483_648.0),
        other => {
            return Err(CaptureError::NoConfig(format!("unsupported sample format {other:?}")))
        }
    };

    stream.play().map_err(|e| CaptureError::Start(e.to_string()))?;
    Ok(CaptureStream {
        _stream: stream,
        sample_rate,
        channels,
        device_name: resolved_name,
        dropped,
    })
}

/// Bounded channel sized for ~4 s of audio at typical callback sizes, so a
/// briefly stalled consumer never loses a recording.
pub fn chunk_channel() -> (Sender<AudioChunk>, Receiver<AudioChunk>) {
    crossbeam_channel::bounded(512)
}
