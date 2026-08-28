//! Resumable model downloader. HTTP Range resume into a .part file, progress
//! callbacks, size sanity check, atomic rename on completion. The real
//! validation is the engine load — a corrupt file fails there and the model
//! manager surfaces "re-download".

use crate::registry::{url_for, ModelInfo};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("network: {0}")]
    Network(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("size mismatch: expected ~{expected} bytes, got {got}")]
    SizeMismatch { expected: u64, got: u64 },
    #[error("cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadProgress {
    pub model_id: String,
    pub downloaded: u64,
    pub total: u64,
}

/// Cancellation handle shared with the UI.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub fn model_path(models_dir: &Path, model: &ModelInfo) -> PathBuf {
    models_dir.join(model.file_name)
}

pub fn is_downloaded(models_dir: &Path, model: &ModelInfo) -> bool {
    // Archive models count as downloaded once EXTRACTED (tokens.txt present).
    if let Some(dir) = crate::registry::extracted_dir(model) {
        return models_dir.join(dir).join("tokens.txt").exists();
    }
    model_path(models_dir, model)
        .metadata()
        .map(|m| plausible_size(m.len(), model.size_bytes))
        .unwrap_or(false)
}

/// Download (or resume) a model. Blocking; run on a worker thread.
pub fn download(
    models_dir: &Path,
    model: &ModelInfo,
    cancel: &CancelToken,
    mut on_progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf, DownloadError> {
    std::fs::create_dir_all(models_dir)?;
    let final_path = model_path(models_dir, model);
    if is_downloaded(models_dir, model) {
        return Ok(final_path);
    }
    let part_path = final_path.with_extension("bin.part");
    let existing = part_path.metadata().map(|m| m.len()).unwrap_or(0);

    let url = url_for(model);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(20))
        .redirects(8)
        .build();
    let mut req = agent.get(&url);
    if existing > 0 {
        req = req.set("Range", &format!("bytes={existing}-"));
    }
    let resp = req.call().map_err(|e| DownloadError::Network(e.to_string()))?;

    let (mut downloaded, append) = match resp.status() {
        206 => (existing, true),
        200 => (0u64, false),
        s => return Err(DownloadError::Network(format!("unexpected status {s}"))),
    };
    let total = if append {
        existing
            + resp
                .header("Content-Length")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(model.size_bytes)
    } else {
        resp.header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(model.size_bytes)
    };

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part_path)?;

    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut last_emit = std::time::Instant::now();
    loop {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let n = reader.read(&mut buf).map_err(|e| DownloadError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;
        if last_emit.elapsed().as_millis() >= 100 {
            last_emit = std::time::Instant::now();
            on_progress(DownloadProgress { model_id: model.id.to_string(), downloaded, total });
        }
    }
    file.flush()?;
    drop(file);

    let got = part_path.metadata()?.len();
    if !plausible_size(got, model.size_bytes) {
        // Leave the .part in place for resume unless it's overshot garbage.
        if got > model.size_bytes.saturating_mul(2) {
            let _ = std::fs::remove_file(&part_path);
        }
        return Err(DownloadError::SizeMismatch { expected: model.size_bytes, got });
    }
    std::fs::rename(&part_path, &final_path)?;

    // Archive models: extract in place, then drop the archive.
    #[cfg(feature = "parakeet")]
    if crate::registry::extracted_dir(model).is_some() {
        extract_tar_bz2(&final_path, models_dir)?;
        let _ = std::fs::remove_file(&final_path);
    }

    on_progress(DownloadProgress { model_id: model.id.to_string(), downloaded: got, total: got });
    Ok(final_path)
}

#[cfg(feature = "parakeet")]
fn extract_tar_bz2(archive: &Path, into: &Path) -> Result<(), DownloadError> {
    let file = std::fs::File::open(archive)?;
    let decompressed = bzip2::read::BzDecoder::new(std::io::BufReader::new(file));
    let mut tar = tar::Archive::new(decompressed);
    // tar's unpack sanitises paths (no absolute/.. traversal).
    tar.unpack(into)?;
    Ok(())
}

pub fn delete(models_dir: &Path, model: &ModelInfo) -> std::io::Result<()> {
    if let Some(dir) = crate::registry::extracted_dir(model) {
        let d = models_dir.join(dir);
        if d.exists() {
            std::fs::remove_dir_all(&d)?;
        }
    }
    let p = model_path(models_dir, model);
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    let part = p.with_extension("bin.part");
    if part.exists() {
        std::fs::remove_file(part)?;
    }
    Ok(())
}

/// Registry sizes are approximate (HF rounds); accept within 5%.
fn plausible_size(got: u64, expected: u64) -> bool {
    let lo = expected / 100 * 95;
    let hi = expected / 100 * 105;
    (lo..=hi).contains(&got)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::MODELS;

    #[test]
    fn plausible_size_window() {
        assert!(plausible_size(100, 100));
        assert!(plausible_size(96, 100));
        assert!(!plausible_size(80, 100));
        assert!(!plausible_size(120, 100));
    }

    #[test]
    fn not_downloaded_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_downloaded(dir.path(), &MODELS[0]));
    }

    #[test]
    fn partial_file_not_counted_as_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let p = model_path(dir.path(), &MODELS[0]);
        std::fs::write(&p, vec![0u8; 1000]).unwrap();
        assert!(!is_downloaded(dir.path(), &MODELS[0]));
    }
}
