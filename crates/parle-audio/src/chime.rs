//! The soft chime played when a take reaches the recording cap.
//!
//! Synthesised rather than shipped as a file: two warm bell tones falling a
//! fourth (D5 then A4), soft attack, long decay. Low enough not to be shrill on
//! laptop speakers, distinct enough to be heard across a room. Played on its
//! own thread through the default output device, so it never touches the
//! capture path.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Peak level of the rendered chime, well below full scale.
const PEAK: f32 = 0.22;
const LENGTH_SECS: f32 = 1.5;
/// (frequency Hz, start s): D5, then A4.
const NOTES: [(f32, f32); 2] = [(587.33, 0.0), (440.0, 0.18)];

/// Render the chime as mono samples at `sample_rate`.
pub fn render(sample_rate: u32) -> Vec<f32> {
    let sr = sample_rate as f32;
    let len = (LENGTH_SECS * sr) as usize;
    let fade = (0.03 * sr) as usize;
    let mut out = vec![0.0f32; len];
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / sr;
        let mut v = 0.0;
        for &(f, start) in &NOTES {
            let lt = t - start;
            if lt < 0.0 {
                continue;
            }
            let attack = (lt / 0.008).min(1.0);
            let tau = std::f32::consts::TAU;
            // Fundamental plus a quicker-fading octave for a bell-like edge.
            let body = (tau * f * lt).sin() * (-lt / 0.38).exp();
            let ring = 0.3 * (tau * 2.0 * f * lt).sin() * (-lt / 0.12).exp();
            v += attack * (body + ring);
        }
        // Fade the last few ms so the stream never ends on a click.
        let tail = ((len - i) as f32 / fade as f32).min(1.0);
        *s = v * tail;
    }
    let max = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if max > 0.0 {
        out.iter_mut().for_each(|s| *s *= PEAK / max);
    }
    out
}

/// Play the chime on a background thread. Best-effort: a missing or busy
/// output device is logged and otherwise ignored.
pub fn play() {
    std::thread::spawn(|| {
        if let Err(e) = play_blocking() {
            tracing::warn!("limit chime could not play: {e}");
        }
    });
}

/// Play the chime on this thread, returning once it has finished.
pub fn play_blocking() -> Result<(), String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no output device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let channels = config.channels() as usize;
    let samples = Arc::new(render(config.sample_rate().0));
    let pos = Arc::new(AtomicUsize::new(0));

    macro_rules! build {
        ($t:ty) => {{
            let (samples, pos) = (samples.clone(), pos.clone());
            device.build_output_stream(
                &config.clone().into(),
                move |data: &mut [$t], _| {
                    for frame in data.chunks_mut(channels) {
                        let i = pos.fetch_add(1, Ordering::Relaxed);
                        let v = samples.get(i).copied().unwrap_or(0.0);
                        for out in frame {
                            *out = <$t as cpal::FromSample<f32>>::from_sample_(v);
                        }
                    }
                },
                |e| tracing::warn!("limit chime stream error: {e}"),
                None,
            )
        }};
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::I32 => build!(i32),
        other => return Err(format!("unsupported output format {other:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    // Hold the stream until the chime has played out, plus a little slack for
    // the device buffer to drain.
    std::thread::sleep(Duration::from_secs_f32(LENGTH_SECS + 0.25));
    drop(stream);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chime_is_soft_audible_and_ends_silent() {
        for sr in [44_100, 48_000] {
            let s = render(sr);
            assert_eq!(s.len(), (LENGTH_SECS * sr as f32) as usize);
            let peak = s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!((peak - PEAK).abs() < 1e-4, "peak {peak}");
            // The second note really sounds: energy after it starts.
            let after = &s[(0.25 * sr as f32) as usize..(0.6 * sr as f32) as usize];
            assert!(after.iter().any(|v| v.abs() > 0.05));
            // No click at the end.
            assert!(s.last().unwrap().abs() < 1e-3);
            // Starts from silence too.
            assert!(s[0].abs() < 1e-3);
        }
    }
}
