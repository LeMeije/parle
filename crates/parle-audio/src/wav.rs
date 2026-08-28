//! WAV export for recovered recordings (total-transcription-failure path) and
//! for the benchmark harness.

use crate::ASR_SAMPLE_RATE;
use std::path::Path;

pub fn write_wav_16k_mono(path: &Path, samples: &[f32]) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: ASR_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        writer.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    writer.finalize()
}

/// Read any mono/stereo 16-bit or float WAV and return 16 kHz mono f32.
/// Non-16k files are NOT resampled here (bench inputs must already be 16 kHz);
/// returns the file's rate so callers can decide.
pub fn read_wav(path: &Path) -> Result<(Vec<f32>, u32), hound::Error> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().filter_map(Result::ok).map(|s| s as f32 / max).collect()
        }
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(Result::ok).collect(),
    };
    let mono: Vec<f32> = if channels == 1 {
        samples
    } else {
        samples.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect()
    };
    Ok((mono, spec.sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("test.wav");
        let samples: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        write_wav_16k_mono(&p, &samples).unwrap();
        let (read, rate) = read_wav(&p).unwrap();
        assert_eq!(rate, ASR_SAMPLE_RATE);
        assert_eq!(read.len(), samples.len());
        // 16-bit quantisation tolerance.
        assert!((read[8000] - samples[8000]).abs() < 0.001);
    }
}
