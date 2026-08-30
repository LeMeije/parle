use serde::{Deserialize, Serialize};

/// A span of text (byte offsets into the RAW transcript) that cleanup removed.
/// Stored with history items so the UI can highlight and restore trims.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrimmedSpan {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub reason: TrimReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrimReason {
    Filler,
    SelfCorrection,
    AbandonedFragment,
}

/// One recognised segment with engine confidence, used for correction surfacing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// 0.0..=1.0; engines that don't expose confidence report 1.0.
    pub confidence: f32,
}

/// A word flagged for user review because the engine was unsure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LowConfidenceSpan {
    pub start: usize,
    pub end: usize,
    pub word: String,
    pub confidence: f32,
}

/// The full result of one dictation, produced by the pipeline and stored in history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub raw_text: String,
    pub text: String,
    pub language: Option<String>,
    pub model_id: String,
    pub duration_ms: u64,
    pub transcribe_ms: u64,
    pub segments: Vec<Segment>,
    pub trimmed: Vec<TrimmedSpan>,
    pub low_confidence: Vec<LowConfidenceSpan>,
    /// Which cleanup tier produced `text`: 0 = raw, 1 = deterministic, 2 = local LLM.
    pub cleanup_tier: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryKind {
    Transcription,
    Clipboard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryItem {
    pub id: i64,
    pub kind: HistoryKind,
    pub text: String,
    /// Raw transcript before cleanup (transcriptions only).
    pub raw_text: Option<String>,
    /// Unix millis.
    pub created_at: i64,
    pub pinned: bool,
    pub duration_ms: Option<u64>,
    pub model_id: Option<String>,
    pub language: Option<String>,
    /// Source app (bundle id on macOS, exe name on Windows).
    pub app_id: Option<String>,
    pub app_name: Option<String>,
    /// Kept on this device and never offered to a paired device.
    ///
    /// On a feature whose whole promise is "it is on both machines", a row that
    /// will never be on the other one has to say so. The column existed from
    /// schema v8 and was honoured by `items_from` and nowhere else, so it could
    /// not reach the IPC payload and the UI could not show it.
    pub local_only: bool,
    /// The device that CREATED this row, as a sync device id.
    ///
    /// `None` means this machine: either the row predates this install having a
    /// sync identity, or it was written before sync was ever switched on. The
    /// column has been in the schema since v8 and drives the whole replication
    /// authority model, but it could not reach the UI, so a list that mixes
    /// three machines looked exactly like a list from one.
    pub source_machine: Option<String>,
    /// JSON blob: trimmed spans, low-confidence spans, cleanup tier, etc.
    pub meta: Option<String>,
}
