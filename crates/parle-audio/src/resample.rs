//! Downmix + resample to 16 kHz mono f32 (the only format ASR engines see).
//! rubato's sinc resampler for quality; a passthrough fast path when the
//! device already runs at 16 kHz.

use crate::ASR_SAMPLE_RATE;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

pub struct StreamResampler {
    inner: Option<SincFixedIn<f32>>,
    input_rate: u32,
    /// Mono samples awaiting a full resampler frame.
    pending: Vec<f32>,
    chunk_in: usize,
}

impl StreamResampler {
    pub fn new(input_rate: u32) -> Self {
        let chunk_in = (input_rate as usize / 50).max(64); // ~20 ms frames
        let inner = if input_rate == ASR_SAMPLE_RATE {
            None
        } else {
            let params = SincInterpolationParameters {
                sinc_len: 128,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };
            Some(
                SincFixedIn::<f32>::new(
                    ASR_SAMPLE_RATE as f64 / input_rate as f64,
                    1.0,
                    params,
                    chunk_in,
                    1,
                )
                .expect("resampler construction"),
            )
        };
        Self { inner, input_rate, pending: Vec::new(), chunk_in }
    }

    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    /// Push interleaved device samples; returns any newly available 16 kHz mono
    /// samples. Ordering is the caller's responsibility (single consumer).
    pub fn push(&mut self, interleaved: &[f32], channels: u16) -> Vec<f32> {
        let mono = downmix(interleaved, channels);
        match &mut self.inner {
            None => mono,
            Some(rs) => {
                self.pending.extend_from_slice(&mono);
                let mut out = Vec::new();
                while self.pending.len() >= self.chunk_in {
                    let frame: Vec<f32> = self.pending.drain(..self.chunk_in).collect();
                    if let Ok(mut res) = rs.process(&[frame], None) {
                        out.append(&mut res.remove(0));
                    }
                }
                out
            }
        }
    }

    /// Flush remaining buffered samples at end of recording (pads with silence).
    pub fn finish(&mut self) -> Vec<f32> {
        match &mut self.inner {
            None => Vec::new(),
            Some(rs) => {
                if self.pending.is_empty() {
                    return Vec::new();
                }
                let mut frame: Vec<f32> = self.pending.drain(..).collect();
                frame.resize(self.chunk_in, 0.0);
                rs.process(&[frame], None).map(|mut r| r.remove(0)).unwrap_or_default()
            }
        }
    }
}

/// Average all channels into mono.
pub fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    if ch == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn passthrough_at_16k() {
        let mut r = StreamResampler::new(16_000);
        let input = sine(16_000, 440.0, 0.1);
        let out = r.push(&input, 1);
        assert_eq!(out.len(), input.len());
    }

    #[test]
    fn ratio_48k_to_16k() {
        let mut r = StreamResampler::new(48_000);
        let input = sine(48_000, 440.0, 1.0);
        let mut out = r.push(&input, 1);
        out.extend(r.finish());
        let expected = 16_000;
        let tolerance = expected / 20; // sinc latency + rounding
        assert!(
            (out.len() as i64 - expected as i64).unsigned_abs() < tolerance as u64,
            "got {} samples, expected ~{expected}",
            out.len()
        );
    }

    #[test]
    fn ratio_44k1_to_16k() {
        let mut r = StreamResampler::new(44_100);
        let input = sine(44_100, 440.0, 1.0);
        let mut out = r.push(&input, 1);
        out.extend(r.finish());
        let expected = 16_000;
        let tolerance = expected / 20;
        assert!(
            (out.len() as i64 - expected as i64).unsigned_abs() < tolerance as u64,
            "got {} samples, expected ~{expected}",
            out.len()
        );
    }

    #[test]
    fn stereo_downmix() {
        let interleaved = vec![0.5, -0.5, 1.0, 0.0, -1.0, 1.0];
        let mono = downmix(&interleaved, 2);
        assert_eq!(mono, vec![0.0, 0.5, 0.0]);
    }

    #[test]
    fn signal_survives_resampling() {
        // A 440 Hz tone must still dominate the spectrum after 48k -> 16k.
        let mut r = StreamResampler::new(48_000);
        let input = sine(48_000, 440.0, 0.5);
        let out = r.push(&input, 1);
        assert!(out.len() > 1000);
        let energy: f32 = out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32;
        assert!(energy > 0.05, "energy after resample too low: {energy}");
    }
}
