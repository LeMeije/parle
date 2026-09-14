//! Level metering for the HUD waveform and VU meter.

/// Smoothed envelope follower: fast attack, slow release — the classic VU feel.
pub struct LevelMeter {
    envelope: f32,
    attack: f32,
    release: f32,
}

impl Default for LevelMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl LevelMeter {
    pub fn new() -> Self {
        Self { envelope: 0.0, attack: 0.55, release: 0.12 }
    }

    /// Feed a block of mono samples; returns (rms, peak, envelope) for the block.
    pub fn process(&mut self, samples: &[f32]) -> (f32, f32, f32) {
        if samples.is_empty() {
            return (0.0, 0.0, self.envelope);
        }
        let mut sum_sq = 0.0f32;
        let mut peak = 0.0f32;
        for &s in samples {
            sum_sq += s * s;
            peak = peak.max(s.abs());
        }
        let rms = (sum_sq / samples.len() as f32).sqrt();
        let coeff = if rms > self.envelope { self.attack } else { self.release };
        self.envelope += coeff * (rms - self.envelope);
        (rms, peak, self.envelope)
    }

    pub fn reset(&mut self) {
        self.envelope = 0.0;
    }
}

/// Downsample a block into `n` waveform bars (max-abs per bucket) for the HUD.
pub fn waveform_bars(samples: &[f32], n: usize) -> Vec<f32> {
    if samples.is_empty() || n == 0 {
        return vec![0.0; n];
    }
    let bucket = (samples.len() / n).max(1);
    (0..n)
        .map(|i| {
            let start = i * bucket;
            let end = ((i + 1) * bucket).min(samples.len());
            if start >= samples.len() {
                0.0
            } else {
                samples[start..end].iter().fold(0.0f32, |m, s| m.max(s.abs()))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_zero() {
        let mut m = LevelMeter::new();
        let (rms, peak, env) = m.process(&vec![0.0; 1600]);
        assert_eq!(rms, 0.0);
        assert_eq!(peak, 0.0);
        assert_eq!(env, 0.0);
    }

    #[test]
    fn attack_faster_than_release() {
        let mut m = LevelMeter::new();
        let loud = vec![0.8f32; 1600];
        let (_, _, env_after_attack) = m.process(&loud);
        assert!(env_after_attack > 0.3);
        let (_, _, env_after_release) = m.process(&vec![0.0; 1600]);
        assert!(env_after_release > 0.0 && env_after_release < env_after_attack);
    }

    #[test]
    fn bars_shape() {
        let samples: Vec<f32> = (0..1600).map(|i| if i < 800 { 0.9 } else { 0.1 }).collect();
        let bars = waveform_bars(&samples, 8);
        assert_eq!(bars.len(), 8);
        assert!(bars[0] > 0.8 && bars[7] < 0.2);
    }
}
