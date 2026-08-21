//! Audio capture pipeline. The rules here are load-bearing (see
//! docs/ARCHITECTURE.md): the device callback COPIES buffers and never blocks;
//! one consumer drains the channel IN ORDER; all ASR input is 16 kHz mono f32.

pub mod capture;
pub mod level;
pub mod recorder;
pub mod resample;
pub mod wav;

/// The sample rate every ASR engine consumes.
pub const ASR_SAMPLE_RATE: u32 = 16_000;

/// One copied chunk from the device callback, tagged for order verification.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub seq: u64,
    /// Interleaved samples as delivered by the device.
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}
