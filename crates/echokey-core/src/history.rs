//! Unified history store: transcriptions + clipboard, plus dictionary entries.
//! SQLite (bundled), FTS5 for word search, fuzzy re-ranking on top.
//! Local-only by design. No telemetry, no cloud, ever.

use crate::dictionary::DictEntry;
use crate::search;
use crate::types::{HistoryItem, HistoryKind, TranscriptionResult};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Store {
    conn: Connection,
    /// This install's device id. Stamped onto every row written locally so a
    /// row can always be attributed to the machine that produced it — which is
    /// what makes "from MacBook / from G14" possible, and what replication
    /// keys on. Empty until set_device_id runs, in which case rows stay NULL
    /// exactly as they did before sync existed.
    device_id: Option<String>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS items (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    kind TEXT NOT NULL CHECK (kind IN ('transcription','clipboard')),
                    text TEXT NOT NULL,
                    raw_text TEXT,
                    created_at INTEGER NOT NULL,
                    pinned INTEGER NOT NULL DEFAULT 0,
                    duration_ms INTEGER,
                    model_id TEXT,
                    language TEXT,
                    app_id TEXT,
                    app_name TEXT,
                    meta TEXT,
                    source_machine TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_items_created ON items(created_at DESC);
                CREATE INDEX IF NOT EXISTS idx_items_kind ON items(kind, created_at DESC);

                CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
                    text, content='items', content_rowid='id', tokenize='unicode61'
                );
                CREATE TRIGGER IF NOT EXISTS items_ai AFTER INSERT ON items BEGIN
                    INSERT INTO items_fts(rowid, text) VALUES (new.id, new.text);
                END;
                CREATE TRIGGER IF NOT EXISTS items_ad AFTER DELETE ON items BEGIN
                    INSERT INTO items_fts(items_fts, rowid, text) VALUES ('delete', old.id, old.text);
                END;
                CREATE TRIGGER IF NOT EXISTS items_au AFTER UPDATE OF text ON items BEGIN
                    INSERT INTO items_fts(items_fts, rowid, text) VALUES ('delete', old.id, old.text);
                    INSERT INTO items_fts(rowid, text) VALUES (new.id, new.text);
                END;

                CREATE TABLE IF NOT EXISTS dictionary (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    term TEXT NOT NULL UNIQUE,
                    corrections TEXT NOT NULL DEFAULT '[]',
                    auto_learned INTEGER NOT NULL DEFAULT 0,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    created_at INTEGER NOT NULL
                );
                "#,
            )?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version < 2 {
            // v2: cross-machine sync groundwork — every row knows its origin.
            conn.execute_batch(
                "ALTER TABLE items ADD COLUMN source_machine TEXT;",
            )?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn, device_id: None })
    }

    /// Assign this install's identity. Call once, after settings load.
    pub fn set_device_id(&mut self, id: &str) {
        self.device_id = if id.is_empty() { None } else { Some(id.to_string()) };
    }

    fn source(&self) -> Option<&str> {
        self.device_id.as_deref()
    }

    /// Which machine a row came from. None for rows written before this
    /// install had an identity.
    pub fn source_machine_of(&self, id: i64) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT source_machine FROM items WHERE id=?1", params![id], |r| r.get(0))
            .optional()?
            .flatten())
    }

    // -- inserts ------------------------------------------------------------

    pub fn insert_transcription(
        &self,
        r: &TranscriptionResult,
        app_id: Option<&str>,
        app_name: Option<&str>,
    ) -> Result<i64, StoreError> {
        let meta = serde_json::json!({
            "trimmed": r.trimmed,
            "low_confidence": r.low_confidence,
            "cleanup_tier": r.cleanup_tier,
            "transcribe_ms": r.transcribe_ms,
        })
        .to_string();
        self.conn.execute(
            "INSERT INTO items (kind, text, raw_text, created_at, duration_ms, model_id, language, app_id, app_name, meta, source_machine)
             VALUES ('transcription', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                r.text,
                r.raw_text,
                now_ms(),
                r.duration_ms as i64,
                r.model_id,
                r.language,
                app_id,
                app_name,
                meta,
                self.source()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Clipboard capture. Consecutive identical texts are deduped (returns the
    /// existing row id and bumps its timestamp instead).
    pub fn insert_clipboard(
        &self,
        text: &str,
        app_id: Option<&str>,
        app_name: Option<&str>,
    ) -> Result<i64, StoreError> {
        let last: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, text FROM items WHERE kind='clipboard' ORDER BY created_at DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, prev)) = last {
            if prev == text {
                self.conn
                    .execute("UPDATE items SET created_at=?1 WHERE id=?2", params![now_ms(), id])?;
                return Ok(id);
            }
        }
        self.conn.execute(
            "INSERT INTO items (kind, text, created_at, app_id, app_name, source_machine)
             VALUES ('clipboard', ?1, ?2, ?3, ?4, ?5)",
            params![text, now_ms(), app_id, app_name, self.source()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    // -- queries ------------------------------------------------------------

    pub fn get(&self, id: i64) -> Result<Option<HistoryItem>, StoreError> {
        Ok(self
            .conn
            .query_row(&format!("{SELECT_COLS} WHERE id=?1"), params![id], row_to_item)
            .optional()?)
    }

    pub fn recent(&self, kind: Option<HistoryKind>, limit: u32) -> Result<Vec<HistoryItem>, StoreError> {
        let (sql, has_kind) = match kind {
            Some(_) => (
                format!("{SELECT_COLS} WHERE kind=?1 ORDER BY pinned DESC, created_at DESC LIMIT ?2"),
                true,
            ),
            None => (
                format!("{SELECT_COLS} ORDER BY pinned DESC, created_at DESC LIMIT ?1"),
                false,
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = if has_kind {
            stmt.query_map(params![kind_str(kind.unwrap()), limit], row_to_item)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![limit], row_to_item)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Fuzzy search: FTS5 prefix candidates + recency window, re-ranked with the
    /// nucleo fuzzy matcher (Raycast-style: pinned and recent get a boost).
    pub fn search(
        &self,
        query: &str,
        kind: Option<HistoryKind>,
        limit: u32,
    ) -> Result<Vec<HistoryItem>, StoreError> {
        let query = query.trim();
        if query.is_empty() {
            return self.recent(kind, limit);
        }

        // Candidate pool: FTS matches (words, prefix) UNION most recent items,
        // so fuzzy still finds "clcode" -> "Claude Code" even when FTS misses.
        let fts_query: String = query
            .split_whitespace()
            .map(|w| format!("\"{}\"*", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut candidates: Vec<HistoryItem> = Vec::new();
        {
            let mut stmt = self.conn.prepare(&format!(
                "{SELECT_COLS} WHERE id IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ?1) \
                 ORDER BY created_at DESC LIMIT 400"
            ))?;
            let rows = stmt.query_map(params![fts_query], row_to_item);
            // A malformed FTS query must never break search (fuzzy pool below covers it).
            if let Ok(rows) = rows {
                for r in rows {
                    candidates.push(r?);
                }
            };
        }
        {
            let mut stmt = self
                .conn
                .prepare(&format!("{SELECT_COLS} ORDER BY created_at DESC LIMIT 400"))?;
            for r in stmt.query_map([], row_to_item)? {
                let item = r?;
                if !candidates.iter().any(|c| c.id == item.id) {
                    candidates.push(item);
                }
            }
        }

        if let Some(k) = kind {
            candidates.retain(|c| c.kind == k);
        }

        let mut scored: Vec<(u32, HistoryItem)> = candidates
            .into_iter()
            .filter_map(|item| {
                search::fuzzy_score(&item.text, query).map(|mut s| {
                    if item.pinned {
                        s += 200;
                    }
                    (s, item)
                })
            })
            .collect();
        // Score desc, then recency desc.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.created_at.cmp(&a.1.created_at)));
        Ok(scored.into_iter().take(limit as usize).map(|(_, i)| i).collect())
    }

    // -- mutations ----------------------------------------------------------

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<(), StoreError> {
        self.conn
            .execute("UPDATE items SET pinned=?1 WHERE id=?2", params![pinned as i64, id])?;
        Ok(())
    }

    /// User edited an item's text. Returns (old, new) so the caller can feed
    /// the auto-learn dictionary.
    pub fn update_text(&self, id: i64, new_text: &str) -> Result<Option<(String, String)>, StoreError> {
        let old: Option<String> = self
            .conn
            .query_row("SELECT text FROM items WHERE id=?1", params![id], |r| r.get(0))
            .optional()?;
        let Some(old) = old else { return Ok(None) };
        self.conn
            .execute("UPDATE items SET text=?1 WHERE id=?2", params![new_text, id])?;
        Ok(Some((old, new_text.to_string())))
    }

    pub fn delete(&self, id: i64) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM items WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn clear(&self, kind: Option<HistoryKind>) -> Result<usize, StoreError> {
        let n = match kind {
            Some(k) => self
                .conn
                .execute("DELETE FROM items WHERE kind=?1 AND pinned=0", params![kind_str(k)])?,
            None => self.conn.execute("DELETE FROM items WHERE pinned=0", [])?,
        };
        Ok(n)
    }

    /// Retention: delete unpinned items older than `days` (0 = keep forever)
    /// and enforce `max_items` (oldest unpinned evicted first).
    pub fn prune(&self, retention_days: u32, max_items: u32) -> Result<usize, StoreError> {
        let mut removed = 0;
        if retention_days > 0 {
            let cutoff = now_ms() - (retention_days as i64) * 86_400_000;
            removed += self.conn.execute(
                "DELETE FROM items WHERE pinned=0 AND created_at < ?1",
                params![cutoff],
            )?;
        }
        if max_items > 0 {
            removed += self.conn.execute(
                "DELETE FROM items WHERE pinned=0 AND id IN (
                    SELECT id FROM items WHERE pinned=0 ORDER BY created_at DESC LIMIT -1 OFFSET ?1
                )",
                params![max_items],
            )?;
        }
        Ok(removed)
    }

    pub fn count(&self) -> Result<i64, StoreError> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?)
    }

    // -- dictionary ---------------------------------------------------------

    pub fn dict_entries(&self) -> Result<Vec<DictEntry>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, term, corrections, auto_learned, enabled FROM dictionary ORDER BY term COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            let corrections: String = row.get(2)?;
            Ok(DictEntry {
                id: row.get(0)?,
                term: row.get(1)?,
                corrections: serde_json::from_str(&corrections).unwrap_or_default(),
                auto_learned: row.get::<_, i64>(3)? != 0,
                enabled: row.get::<_, i64>(4)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Upsert MERGES corrections: an auto-learned pair must never clobber a
    /// hand-curated corrections list.
    pub fn dict_upsert(&self, term: &str, corrections: &[String], auto_learned: bool) -> Result<i64, StoreError> {
        let existing: Option<String> = self
            .conn
            .query_row("SELECT corrections FROM dictionary WHERE term=?1", params![term], |r| r.get(0))
            .optional()?;
        let mut merged: Vec<String> = existing
            .and_then(|e| serde_json::from_str(&e).ok())
            .unwrap_or_default();
        for c in corrections {
            if !merged.iter().any(|m| m.eq_ignore_ascii_case(c)) {
                merged.push(c.clone());
            }
        }
        let corr = serde_json::to_string(&merged).unwrap_or_else(|_| "[]".into());
        self.conn.execute(
            "INSERT INTO dictionary (term, corrections, auto_learned, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(term) DO UPDATE SET corrections=?2, auto_learned=min(dictionary.auto_learned, ?3)",
            params![term, corr, auto_learned as i64, now_ms()],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM dictionary WHERE term=?1", params![term], |r| r.get(0))?)
    }

    pub fn dict_set_enabled(&self, id: i64, enabled: bool) -> Result<(), StoreError> {
        self.conn
            .execute("UPDATE dictionary SET enabled=?1 WHERE id=?2", params![enabled as i64, id])?;
        Ok(())
    }

    pub fn dict_delete(&self, id: i64) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM dictionary WHERE id=?1", params![id])?;
        Ok(())
    }
}

const SELECT_COLS: &str = "SELECT id, kind, text, raw_text, created_at, pinned, duration_ms, model_id, language, app_id, app_name, meta FROM items";

fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<HistoryItem> {
    let kind: String = row.get(1)?;
    Ok(HistoryItem {
        id: row.get(0)?,
        kind: if kind == "transcription" { HistoryKind::Transcription } else { HistoryKind::Clipboard },
        text: row.get(2)?,
        raw_text: row.get(3)?,
        created_at: row.get(4)?,
        pinned: row.get::<_, i64>(5)? != 0,
        duration_ms: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
        model_id: row.get(7)?,
        language: row.get(8)?,
        app_id: row.get(9)?,
        app_name: row.get(10)?,
        meta: row.get(11)?,
    })
}

fn kind_str(k: HistoryKind) -> &'static str {
    match k {
        HistoryKind::Transcription => "transcription",
        HistoryKind::Clipboard => "clipboard",
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(text: &str) -> TranscriptionResult {
        TranscriptionResult {
            raw_text: text.to_string(),
            text: text.to_string(),
            language: Some("en".into()),
            model_id: "whisper-small-q5_1".into(),
            duration_ms: 1500,
            transcribe_ms: 300,
            segments: vec![],
            trimmed: vec![],
            low_confidence: vec![],
            cleanup_tier: 1,
        }
    }

    #[test]
    fn insert_and_search() {
        let s = Store::open_in_memory().unwrap();
        s.insert_transcription(&tr("Ship the quarterly report to Sarah"), Some("com.apple.mail"), Some("Mail")).unwrap();
        s.insert_clipboard("https://example.com/dashboard", None, None).unwrap();
        let hits = s.search("quarterly", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HistoryKind::Transcription);
        assert_eq!(hits[0].app_name.as_deref(), Some("Mail"));
    }

    #[test]
    fn fuzzy_search_finds_nonadjacent() {
        let s = Store::open_in_memory().unwrap();
        s.insert_transcription(&tr("Meeting notes about the Kubernetes migration"), None, None).unwrap();
        let hits = s.search("kubmig", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn clipboard_dedupe() {
        let s = Store::open_in_memory().unwrap();
        let a = s.insert_clipboard("same text", None, None).unwrap();
        let b = s.insert_clipboard("same text", None, None).unwrap();
        assert_eq!(a, b);
        assert_eq!(s.count().unwrap(), 1);
        let c = s.insert_clipboard("different", None, None).unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn pinned_survives_clear_and_prune() {
        let s = Store::open_in_memory().unwrap();
        let id = s.insert_clipboard("keep me", None, None).unwrap();
        s.insert_clipboard("toss me", None, None).unwrap();
        s.set_pinned(id, true).unwrap();
        s.clear(None).unwrap();
        assert_eq!(s.count().unwrap(), 1);
        assert!(s.get(id).unwrap().unwrap().pinned);
    }

    #[test]
    fn prune_max_items() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..20 {
            s.conn.execute(
                "INSERT INTO items (kind, text, created_at) VALUES ('clipboard', ?1, ?2)",
                params![format!("item {i}"), 1000 + i],
            ).unwrap();
        }
        s.prune(0, 5).unwrap();
        assert_eq!(s.count().unwrap(), 5);
        // Newest kept.
        let recent = s.recent(None, 10).unwrap();
        assert_eq!(recent[0].text, "item 19");
    }

    #[test]
    fn update_text_returns_old_and_new() {
        let s = Store::open_in_memory().unwrap();
        let id = s.insert_transcription(&tr("the farsight team"), None, None).unwrap();
        let (old, new) = s.update_text(id, "the farsiight team").unwrap().unwrap();
        assert_eq!(old, "the farsight team");
        assert_eq!(new, "the farsiight team");
        // FTS updated too.
        assert_eq!(s.search("farsiight", None, 10).unwrap().len(), 1);
    }

    #[test]
    fn dictionary_crud() {
        let s = Store::open_in_memory().unwrap();
        let id = s.dict_upsert("Claude Code", &["cloud code".into()], false).unwrap();
        let entries = s.dict_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].corrections, vec!["cloud code"]);
        s.dict_set_enabled(id, false).unwrap();
        assert!(!s.dict_entries().unwrap()[0].enabled);
        s.dict_delete(id).unwrap();
        assert!(s.dict_entries().unwrap().is_empty());
    }

    #[test]
    fn malformed_query_does_not_error() {
        let s = Store::open_in_memory().unwrap();
        s.insert_clipboard("hello world", None, None).unwrap();
        // FTS special chars must not break search.
        let hits = s.search("hel\" AND (", None, 10).unwrap();
        // Fuzzy fallback may or may not match; the call must simply not error.
        let _ = hits;
    }

    #[test]
    fn local_rows_are_stamped_with_the_device_id() {
        let mut st = Store::open_in_memory().unwrap();
        // Before an identity exists, rows stay NULL exactly as they did before
        // sync was a thing — no migration surprise for existing installs.
        let id0 = st.insert_clipboard("before", None, None).unwrap();
        assert_eq!(st.source_machine_of(id0).unwrap(), None);

        st.set_device_id("device-abc");
        let id1 = st.insert_clipboard("after", None, None).unwrap();
        assert_eq!(st.source_machine_of(id1).unwrap().as_deref(), Some("device-abc"));
    }
}
