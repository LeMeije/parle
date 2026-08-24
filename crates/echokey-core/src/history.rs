//! Unified history store: transcriptions + clipboard, plus dictionary entries.
//! SQLite (bundled), FTS5 for word search, fuzzy re-ranking on top.
//! Local-only by design. No telemetry, no cloud, ever.

use crate::dictionary::DictEntry;
use crate::search;
use crate::types::{HistoryItem, HistoryKind, TranscriptionResult};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

const SCHEMA_VERSION: i64 = 3;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// One replicated history row as it crosses machines.
///
/// Identity is `(source_machine, origin_id)`: the device that created the row
/// plus that device's own rowid, as text. That pair is stable forever, which is
/// what makes applying the same row twice a no-op instead of a duplicate.
///
/// Deliberately carries only the fields that are meaningful off-box. Local-only
/// detail (raw_text, model_id, duration, app attribution, meta) is not
/// replicated and stays NULL on rows that arrived from a peer.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteItem {
    pub source_machine: String,
    pub origin_id: String,
    /// "transcription" | "clipboard". Anything else is ignored on apply.
    pub kind: String,
    pub text: String,
    /// Unix milliseconds, UTC.
    pub created_at: i64,
    /// Unix milliseconds, UTC. The last-writer-wins clock.
    pub updated_at: i64,
    pub pinned: bool,
}

/// A delete, replicated. Outlives the row so a straggling copy of the row
/// cannot resurrect it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteTombstone {
    pub source_machine: String,
    pub origin_id: String,
    /// Unix milliseconds, UTC.
    pub deleted_at: i64,
}

/// What an `apply_remote_*` call actually did. `Ignored` means the local state
/// was already at least as new as what arrived — nothing changed on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Inserted,
    Updated,
    Ignored,
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

        // Migrations are sequential and each one is skipped by an already-new
        // enough db. A fresh db runs the v1 create and then every later step,
        // so a from-scratch schema is byte-identical to a migrated one.
        let mut version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
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
            // The create above already includes source_machine, so v2 is done.
            version = 2;
        }
        if version < 2 {
            // v2: cross-machine sync groundwork — every row knows its origin.
            conn.execute_batch(
                "ALTER TABLE items ADD COLUMN source_machine TEXT;",
            )?;
            version = 2;
        }
        if version < 3 {
            // v3: replication. A row is identified across machines by
            // (source_machine, origin_id); updated_at is the last-writer-wins
            // clock; tombstones make deletes propagate.
            //
            // Backfill: existing local rows are, by definition, ours — their
            // origin id is their own rowid, and they have never been edited
            // through a replicating code path, so updated_at = created_at.
            // Rows with a NULL source_machine predate this install having an
            // identity: we cannot attribute them, so they keep a NULL origin_id
            // and simply never replicate. Guessing would let two machines each
            // claim the same row.
            conn.execute_batch(
                r#"
                ALTER TABLE items ADD COLUMN origin_id TEXT;
                ALTER TABLE items ADD COLUMN updated_at INTEGER;

                UPDATE items SET updated_at = created_at WHERE updated_at IS NULL;
                UPDATE items SET origin_id = CAST(id AS TEXT)
                    WHERE origin_id IS NULL AND source_machine IS NOT NULL;

                CREATE UNIQUE INDEX IF NOT EXISTS idx_items_origin
                    ON items(source_machine, origin_id)
                    WHERE source_machine IS NOT NULL AND origin_id IS NOT NULL;
                CREATE INDEX IF NOT EXISTS idx_items_source_updated
                    ON items(source_machine, updated_at);

                CREATE TABLE IF NOT EXISTS tombstones (
                    source_machine TEXT NOT NULL,
                    origin_id TEXT NOT NULL,
                    deleted_at INTEGER NOT NULL,
                    PRIMARY KEY (source_machine, origin_id)
                );
                CREATE INDEX IF NOT EXISTS idx_tombstones_deleted
                    ON tombstones(source_machine, deleted_at);
                "#,
            )?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
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
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO items (kind, text, raw_text, created_at, updated_at, duration_ms, model_id, language, app_id, app_name, meta, source_machine)
             VALUES ('transcription', ?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                r.text,
                r.raw_text,
                now,
                r.duration_ms as i64,
                r.model_id,
                r.language,
                app_id,
                app_name,
                meta,
                self.source()
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.stamp_origin(id)?;
        Ok(id)
    }

    /// A locally written row's origin id IS its rowid — but the rowid only
    /// exists after the insert, so it is stamped in a second statement. Rows
    /// written before this install had a device identity get no origin id:
    /// an unattributable row must not be replicated.
    fn stamp_origin(&self, id: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE items SET origin_id = CAST(id AS TEXT)
             WHERE id = ?1 AND source_machine IS NOT NULL AND origin_id IS NULL",
            params![id],
        )?;
        Ok(())
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
                // Only ever dedupe against OUR OWN rows. Comparing against
                // replicated rows lets a local copy mutate a row that belongs
                // to another machine and produce no local row at all — the
                // capture would appear to vanish, and the edit would propagate
                // back as a change to the peer's item.
                "SELECT id, text FROM items                  WHERE kind='clipboard'                    AND (source_machine IS NULL OR source_machine = ?1)                  ORDER BY created_at DESC LIMIT 1",
                params![self.source()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, prev)) = last {
            if prev == text {
                // A re-copy is an edit of the existing row: bump the
                // replication clock so peers learn it moved.
                let now = now_ms();
                self.conn.execute(
                    "UPDATE items SET created_at=?1, updated_at=?1 WHERE id=?2",
                    params![now, id],
                )?;
                return Ok(id);
            }
        }
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO items (kind, text, created_at, updated_at, app_id, app_name, source_machine)
             VALUES ('clipboard', ?1, ?2, ?2, ?3, ?4, ?5)",
            params![text, now, app_id, app_name, self.source()],
        )?;
        let id = self.conn.last_insert_rowid();
        self.stamp_origin(id)?;
        Ok(id)
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
        self.conn.execute(
            "UPDATE items SET pinned=?1, updated_at=?2 WHERE id=?3",
            params![pinned as i64, now_ms(), id],
        )?;
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
        self.conn.execute(
            "UPDATE items SET text=?1, updated_at=?2 WHERE id=?3",
            params![new_text, now_ms(), id],
        )?;
        Ok(Some((old, new_text.to_string())))
    }

    /// The one local delete path. Writes a tombstone as well as removing the
    /// row, so the delete replicates instead of being undone by the next sync.
    pub fn delete(&self, id: i64) -> Result<(), StoreError> {
        self.delete_item_local(id)
    }

    /// Delete a local row AND record the tombstone that makes the delete
    /// travel. Rows that carry no (source_machine, origin_id) identity — the
    /// pre-sync legacy rows — are simply deleted: there is nothing a peer
    /// could match a tombstone against.
    ///
    /// The tombstone is stamped `max(now, row.updated_at)`. Taking the wall
    /// clock alone would lose to the row itself if that row arrived from a
    /// machine whose clock runs ahead of ours, and the delete would be undone
    /// on the next round trip.
    pub fn delete_item_local(&self, id: i64) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let row: Option<(Option<String>, Option<String>, i64)> = tx
            .query_row(
                "SELECT source_machine, origin_id, COALESCE(updated_at, created_at) FROM items WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((Some(source), Some(origin), updated_at)) = row {
            let deleted_at = now_ms().max(updated_at);
            tx.execute(
                "INSERT INTO tombstones (source_machine, origin_id, deleted_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(source_machine, origin_id)
                 DO UPDATE SET deleted_at = max(tombstones.deleted_at, excluded.deleted_at)",
                params![source, origin, deleted_at],
            )?;
        }
        tx.execute("DELETE FROM items WHERE id=?1", params![id])?;
        tx.commit()?;
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

    // -- replication --------------------------------------------------------

    /// Per source machine, the newest `updated_at` we hold locally. The caller
    /// sends these to a peer as "I have everything from you up to here".
    ///
    /// Deliberately computed over items only, NOT over tombstones. A tombstone
    /// can be newer than every surviving row from that source; folding it in
    /// would raise the watermark past items we have never seen and lose them
    /// permanently. The cost of leaving it out is that deleting the newest row
    /// from a source lowers that source's watermark, so the peer may re-send a
    /// handful of rows we already have — those are cheap and idempotent, and a
    /// row we tombstoned is refused by `apply_remote_item`.
    ///
    /// A source with no surviving rows is absent from the result, not zero.
    pub fn watermarks(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT source_machine, MAX(COALESCE(updated_at, created_at)) FROM items
             WHERE source_machine IS NOT NULL AND origin_id IS NOT NULL
             GROUP BY source_machine ORDER BY source_machine",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Rows from `source` newer than `after`, oldest first, capped at `limit`.
    ///
    /// Caveat the caller must know: the cursor is a millisecond timestamp, so
    /// if more than `limit` rows share a single `updated_at` value the page
    /// boundary can fall inside that group and the remainder is never sent.
    /// Use a limit comfortably larger than any plausible same-millisecond
    /// burst, or re-request from `last_updated_at - 1` and let idempotency
    /// absorb the overlap.
    pub fn items_since(
        &self,
        source: &str,
        after: i64,
        limit: usize,
    ) -> Result<Vec<RemoteItem>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT source_machine, origin_id, kind, text, created_at, updated_at, pinned FROM items
             WHERE source_machine = ?1 AND origin_id IS NOT NULL AND updated_at > ?2
             ORDER BY updated_at ASC, origin_id ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![source, after, limit as i64], |r| {
            Ok(RemoteItem {
                source_machine: r.get(0)?,
                origin_id: r.get(1)?,
                kind: r.get(2)?,
                text: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
                pinned: r.get::<_, i64>(6)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Tombstones from `source` newer than `after`, oldest first. Same paging
    /// caveat as [`Store::items_since`].
    pub fn tombstones_since(
        &self,
        source: &str,
        after: i64,
        limit: usize,
    ) -> Result<Vec<RemoteTombstone>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT source_machine, origin_id, deleted_at FROM tombstones
             WHERE source_machine = ?1 AND deleted_at > ?2
             ORDER BY deleted_at ASC, origin_id ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![source, after, limit as i64], |r| {
            Ok(RemoteTombstone {
                source_machine: r.get(0)?,
                origin_id: r.get(1)?,
                deleted_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Apply a row received from a peer. Insert it, update the local copy, or
    /// ignore it — never duplicate it.
    ///
    /// Resolution order:
    /// 1. A tombstone for this identity with `deleted_at >= updated_at` wins:
    ///    the row is refused. Ties go to the tombstone, so a delete is never
    ///    undone by a same-millisecond copy of the row.
    /// 2. Otherwise last-writer-wins on `updated_at`, strictly greater.
    /// 3. On an exact `updated_at` tie, the tiebreak is the payload itself:
    ///    the greater `(text, pinned)` wins, ordered bytewise with
    ///    `false < true`. This is total, so both machines pick the SAME winner
    ///    regardless of which arrived first — a tie cannot leave the two boxes
    ///    permanently disagreeing — and it is stable, because once the greater
    ///    value is stored the lesser can never win. Identical payloads change
    ///    nothing and report `Ignored`, which is what makes re-apply a no-op.
    ///
    /// A row with an unknown `kind`, or an empty identity, is ignored rather
    /// than raising: one malformed row from a peer must not abort a batch.
    pub fn apply_remote_item(&self, item: &RemoteItem) -> Result<ApplyOutcome, StoreError> {
        if item.source_machine.is_empty()
            || item.origin_id.is_empty()
            || !matches!(item.kind.as_str(), "transcription" | "clipboard")
        {
            return Ok(ApplyOutcome::Ignored);
        }

        let tx = self.conn.unchecked_transaction()?;
        let tombstone: Option<i64> = tx
            .query_row(
                "SELECT deleted_at FROM tombstones WHERE source_machine=?1 AND origin_id=?2",
                params![item.source_machine, item.origin_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(deleted_at) = tombstone {
            if deleted_at >= item.updated_at {
                return Ok(ApplyOutcome::Ignored);
            }
        }

        let existing: Option<(i64, i64, String, i64)> = tx
            .query_row(
                "SELECT id, COALESCE(updated_at, created_at), text, pinned FROM items
                 WHERE source_machine=?1 AND origin_id=?2",
                params![item.source_machine, item.origin_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;

        let outcome = match existing {
            None => {
                tx.execute(
                    "INSERT INTO items (kind, text, created_at, updated_at, pinned, source_machine, origin_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        item.kind,
                        item.text,
                        item.created_at,
                        item.updated_at,
                        item.pinned as i64,
                        item.source_machine,
                        item.origin_id
                    ],
                )?;
                ApplyOutcome::Inserted
            }
            Some((id, local_updated, local_text, local_pinned)) => {
                let local_pinned = local_pinned != 0;
                let wins = item.updated_at > local_updated
                    || (item.updated_at == local_updated
                        && (item.text.as_str(), item.pinned) > (local_text.as_str(), local_pinned));
                if wins {
                    tx.execute(
                        "UPDATE items SET text=?1, created_at=?2, updated_at=?3, pinned=?4 WHERE id=?5",
                        params![item.text, item.created_at, item.updated_at, item.pinned as i64, id],
                    )?;
                    ApplyOutcome::Updated
                } else {
                    ApplyOutcome::Ignored
                }
            }
        };
        tx.commit()?;
        Ok(outcome)
    }

    /// Apply a delete received from a peer: record it, and remove the local row
    /// unless that row is strictly newer than the delete (an edit that happened
    /// after the delete survives it).
    ///
    /// The outcome describes what changed on disk: `Inserted` for a tombstone
    /// we had never seen, `Updated` if a known tombstone moved forward or a row
    /// was actually removed, `Ignored` if we already knew and there was nothing
    /// left to delete. The row deletion is attempted every time regardless of
    /// the tombstone bookkeeping, so a tombstone that arrives before the row
    /// and again after it still kills the row.
    pub fn apply_remote_tombstone(&self, t: &RemoteTombstone) -> Result<ApplyOutcome, StoreError> {
        if t.source_machine.is_empty() || t.origin_id.is_empty() {
            return Ok(ApplyOutcome::Ignored);
        }
        let tx = self.conn.unchecked_transaction()?;
        let prior: Option<i64> = tx
            .query_row(
                "SELECT deleted_at FROM tombstones WHERE source_machine=?1 AND origin_id=?2",
                params![t.source_machine, t.origin_id],
                |r| r.get(0),
            )
            .optional()?;
        let mut outcome = match prior {
            None => {
                tx.execute(
                    "INSERT INTO tombstones (source_machine, origin_id, deleted_at) VALUES (?1, ?2, ?3)",
                    params![t.source_machine, t.origin_id, t.deleted_at],
                )?;
                ApplyOutcome::Inserted
            }
            Some(prior) if prior < t.deleted_at => {
                tx.execute(
                    "UPDATE tombstones SET deleted_at=?3 WHERE source_machine=?1 AND origin_id=?2",
                    params![t.source_machine, t.origin_id, t.deleted_at],
                )?;
                ApplyOutcome::Updated
            }
            Some(_) => ApplyOutcome::Ignored,
        };
        let effective = prior.unwrap_or(t.deleted_at).max(t.deleted_at);
        let removed = tx.execute(
            "DELETE FROM items
             WHERE source_machine=?1 AND origin_id=?2 AND COALESCE(updated_at, created_at) <= ?3",
            params![t.source_machine, t.origin_id, effective],
        )?;
        if removed > 0 && outcome == ApplyOutcome::Ignored {
            outcome = ApplyOutcome::Updated;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// Drop tombstones older than an absolute epoch-ms cutoff (NOT an age).
    /// `prune_tombstones(now_ms() - 30 * 86_400_000)` keeps thirty days.
    ///
    /// Keep the window longer than the longest realistic gap between two
    /// machines syncing: once a tombstone is gone, a peer still holding the row
    /// will happily hand it back and the delete is undone.
    pub fn prune_tombstones(&self, older_than_ms: i64) -> Result<usize, StoreError> {
        Ok(self
            .conn
            .execute("DELETE FROM tombstones WHERE deleted_at < ?1", params![older_than_ms])?)
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

    // -- replication --------------------------------------------------------

    fn remote(origin: &str, text: &str, updated_at: i64) -> RemoteItem {
        RemoteItem {
            source_machine: "mac-1".into(),
            origin_id: origin.into(),
            kind: "clipboard".into(),
            text: text.into(),
            created_at: 1_000,
            updated_at,
            pinned: false,
        }
    }

    fn store_with_device(id: &str) -> Store {
        let mut s = Store::open_in_memory().unwrap();
        s.set_device_id(id);
        s
    }

    /// The exact v1 schema as shipped, so the migration is exercised against
    /// the real historical shape rather than a convenient approximation.
    const V1_SCHEMA: &str = r#"
        CREATE TABLE items (
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
            meta TEXT
        );
        CREATE INDEX idx_items_created ON items(created_at DESC);
        CREATE INDEX idx_items_kind ON items(kind, created_at DESC);
        CREATE VIRTUAL TABLE items_fts USING fts5(
            text, content='items', content_rowid='id', tokenize='unicode61'
        );
        CREATE TRIGGER items_ai AFTER INSERT ON items BEGIN
            INSERT INTO items_fts(rowid, text) VALUES (new.id, new.text);
        END;
        CREATE TRIGGER items_ad AFTER DELETE ON items BEGIN
            INSERT INTO items_fts(items_fts, rowid, text) VALUES ('delete', old.id, old.text);
        END;
        CREATE TRIGGER items_au AFTER UPDATE OF text ON items BEGIN
            INSERT INTO items_fts(items_fts, rowid, text) VALUES ('delete', old.id, old.text);
            INSERT INTO items_fts(rowid, text) VALUES (new.id, new.text);
        END;
        CREATE TABLE dictionary (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            term TEXT NOT NULL UNIQUE,
            corrections TEXT NOT NULL DEFAULT '[]',
            auto_learned INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL
        );
        ALTER TABLE items ADD COLUMN source_machine TEXT;
    "#;

    /// A real v2 database on disk with rows already in it.
    fn seed_v2_db(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(V1_SCHEMA).unwrap();
        c.execute(
            "INSERT INTO items (kind, text, created_at, pinned, source_machine)
             VALUES ('clipboard', 'attributed', 500, 1, 'g14')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO items (kind, text, created_at) VALUES ('transcription', 'legacy orphan', 600)",
            [],
        )
        .unwrap();
        c.pragma_update(None, "user_version", 2i64).unwrap();
    }

    #[test]
    fn migrates_v2_and_backfills_without_losing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        seed_v2_db(&path);

        let s = Store::open(&path).unwrap();
        assert_eq!(s.count().unwrap(), 2, "migration must not drop rows");
        let version: i64 = s.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        // Attributed row: origin_id backfilled from its own rowid, clock seeded
        // from created_at, everything else untouched.
        let (origin, updated, pinned): (Option<String>, Option<i64>, i64) = s
            .conn
            .query_row(
                "SELECT origin_id, updated_at, pinned FROM items WHERE text='attributed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(origin.as_deref(), Some("1"));
        assert_eq!(updated, Some(500));
        assert_eq!(pinned, 1, "pin state survives the migration");

        // Legacy row with no source: unattributable, so it gets no origin id
        // and never replicates. It still gets a clock so local edits work.
        let (origin, updated): (Option<String>, Option<i64>) = s
            .conn
            .query_row(
                "SELECT origin_id, updated_at FROM items WHERE text='legacy orphan'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(origin, None, "must not guess an origin for an unattributed row");
        assert_eq!(updated, Some(600));

        // It is invisible to replication, both as data and as a watermark.
        assert_eq!(s.watermarks().unwrap(), vec![("g14".to_string(), 500)]);
        // Existing behaviour still works on a migrated db.
        assert_eq!(s.search("orphan", None, 10).unwrap().len(), 1);
    }

    #[test]
    fn migrates_a_v1_db_through_every_step() {
        // v1 predates source_machine entirely: the v2 and v3 steps must both
        // run, in order, rather than the version being stamped forward.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.db");
        {
            let c = Connection::open(&path).unwrap();
            let v1_only = V1_SCHEMA.replace("ALTER TABLE items ADD COLUMN source_machine TEXT;", "");
            c.execute_batch(&v1_only).unwrap();
            c.execute(
                "INSERT INTO items (kind, text, created_at) VALUES ('clipboard', 'ancient', 10)",
                [],
            )
            .unwrap();
            c.pragma_update(None, "user_version", 1i64).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.count().unwrap(), 1);
        let (source, origin, updated): (Option<String>, Option<String>, i64) = s
            .conn
            .query_row(
                "SELECT source_machine, origin_id, updated_at FROM items WHERE text='ancient'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(source, None);
        assert_eq!(origin, None);
        assert_eq!(updated, 10);
    }

    #[test]
    fn fresh_v3_schema_matches_a_migrated_one() {
        let dir = tempfile::tempdir().unwrap();
        let migrated_path = dir.path().join("migrated.db");
        seed_v2_db(&migrated_path);
        let migrated = Store::open(&migrated_path).unwrap();
        let fresh = Store::open(&dir.path().join("fresh.db")).unwrap();

        let columns = |s: &Store| -> Vec<(String, String)> {
            let mut stmt = s.conn.prepare("PRAGMA table_info(items)").unwrap();
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
                .unwrap();
            rows.collect::<Result<Vec<_>, _>>().unwrap()
        };
        assert_eq!(columns(&fresh), columns(&migrated));

        let objects = |s: &Store| -> Vec<(String, String)> {
            let mut stmt = s
                .conn
                .prepare("SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name")
                .unwrap();
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap();
            rows.collect::<Result<Vec<_>, _>>().unwrap()
        };
        let fresh_objects = objects(&fresh);
        assert_eq!(fresh_objects, objects(&migrated));
        assert!(fresh_objects.contains(&("table".into(), "tombstones".into())));
        assert!(fresh_objects.contains(&("index".into(), "idx_items_origin".into())));
    }

    #[test]
    fn local_rows_carry_origin_id_and_clock() {
        let s = store_with_device("g14");
        let id = s.insert_clipboard("hello", None, None).unwrap();
        let (origin, updated, created): (Option<String>, i64, i64) = s
            .conn
            .query_row(
                "SELECT origin_id, updated_at, created_at FROM items WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(origin.as_deref(), Some(id.to_string().as_str()));
        assert_eq!(updated, created);

        // Without an identity there is nothing to attribute the row to.
        let anon = Store::open_in_memory().unwrap();
        let id = anon.insert_clipboard("hello", None, None).unwrap();
        let origin: Option<String> = anon
            .conn
            .query_row("SELECT origin_id FROM items WHERE id=?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(origin, None);
        assert!(anon.watermarks().unwrap().is_empty());
    }

    #[test]
    fn local_edits_bump_the_clock() {
        let s = store_with_device("g14");
        let id = s.insert_transcription(&tr("original"), None, None).unwrap();
        let clock = |id: i64| -> i64 {
            s.conn
                .query_row("SELECT updated_at FROM items WHERE id=?1", params![id], |r| r.get(0))
                .unwrap()
        };
        // The wall clock has millisecond resolution, so force a distinct
        // starting point rather than racing it.
        s.conn
            .execute("UPDATE items SET updated_at=1 WHERE id=?1", params![id])
            .unwrap();
        s.update_text(id, "edited").unwrap();
        let after_edit = clock(id);
        assert!(after_edit > 1, "a text edit must propagate");

        s.conn
            .execute("UPDATE items SET updated_at=1 WHERE id=?1", params![id])
            .unwrap();
        s.set_pinned(id, true).unwrap();
        assert!(clock(id) > 1, "a pin change must propagate");

        // And an edited row is what items_since hands to the peer.
        let sent = s.items_since("g14", 0, 10).unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].text, "edited");
        assert!(sent[0].pinned);
    }

    #[test]
    fn apply_remote_item_is_idempotent() {
        let s = store_with_device("g14");
        let item = remote("7", "from the mac", 100);
        assert_eq!(s.apply_remote_item(&item).unwrap(), ApplyOutcome::Inserted);
        assert_eq!(s.apply_remote_item(&item).unwrap(), ApplyOutcome::Ignored);
        assert_eq!(s.apply_remote_item(&item).unwrap(), ApplyOutcome::Ignored);
        assert_eq!(s.count().unwrap(), 1, "the same row must never duplicate");
        // Replicated rows are first-class locally: searchable, listable.
        assert_eq!(s.search("mac", None, 10).unwrap().len(), 1);
        assert_eq!(s.recent(None, 10).unwrap()[0].text, "from the mac");
    }

    #[test]
    fn last_writer_wins_on_updated_at() {
        let s = store_with_device("g14");
        s.apply_remote_item(&remote("7", "v1", 100)).unwrap();

        assert_eq!(
            s.apply_remote_item(&remote("7", "v2", 200)).unwrap(),
            ApplyOutcome::Updated
        );
        assert_eq!(s.get(1).unwrap().unwrap().text, "v2");

        // A straggler from before the edit must not undo it.
        assert_eq!(
            s.apply_remote_item(&remote("7", "stale", 50)).unwrap(),
            ApplyOutcome::Ignored
        );
        assert_eq!(s.get(1).unwrap().unwrap().text, "v2");
        assert_eq!(s.count().unwrap(), 1);
    }

    #[test]
    fn equal_clocks_converge_regardless_of_arrival_order() {
        let a = remote("7", "aaa", 100);
        let b = remote("7", "bbb", 100);

        // Same two versions, opposite arrival orders, on two machines.
        let one = store_with_device("g14");
        one.apply_remote_item(&a).unwrap();
        one.apply_remote_item(&b).unwrap();

        let two = store_with_device("mac-1");
        two.apply_remote_item(&b).unwrap();
        two.apply_remote_item(&a).unwrap();

        let text_of = |s: &Store| s.get(1).unwrap().unwrap().text;
        assert_eq!(text_of(&one), text_of(&two), "a tie must not leave the boxes disagreeing");
        assert_eq!(text_of(&one), "bbb", "documented tiebreak: greater (text, pinned) wins");

        // Stable: the loser never wins later, and neither side flip-flops.
        assert_eq!(one.apply_remote_item(&a).unwrap(), ApplyOutcome::Ignored);
        assert_eq!(one.apply_remote_item(&b).unwrap(), ApplyOutcome::Ignored);
        assert_eq!(text_of(&one), "bbb");
        // Pin state breaks a tie only when the text is identical.
        let mut pinned = remote("7", "bbb", 100);
        pinned.pinned = true;
        assert_eq!(one.apply_remote_item(&pinned).unwrap(), ApplyOutcome::Updated);
        assert_eq!(one.apply_remote_item(&pinned).unwrap(), ApplyOutcome::Ignored);
    }

    #[test]
    fn tombstone_beats_item_when_the_item_arrives_first() {
        let s = store_with_device("g14");
        s.apply_remote_item(&remote("7", "doomed", 100)).unwrap();
        let t = RemoteTombstone {
            source_machine: "mac-1".into(),
            origin_id: "7".into(),
            deleted_at: 150,
        };
        assert_eq!(s.apply_remote_tombstone(&t).unwrap(), ApplyOutcome::Inserted);
        assert_eq!(s.count().unwrap(), 0);
        // Replaying the delete changes nothing.
        assert_eq!(s.apply_remote_tombstone(&t).unwrap(), ApplyOutcome::Ignored);
        // And the row cannot come back.
        assert_eq!(
            s.apply_remote_item(&remote("7", "doomed", 100)).unwrap(),
            ApplyOutcome::Ignored
        );
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn tombstone_beats_item_when_the_tombstone_arrives_first() {
        let s = store_with_device("g14");
        let t = RemoteTombstone {
            source_machine: "mac-1".into(),
            origin_id: "7".into(),
            deleted_at: 150,
        };
        assert_eq!(s.apply_remote_tombstone(&t).unwrap(), ApplyOutcome::Inserted);
        // The straggling copy of the deleted row is refused, not resurrected.
        assert_eq!(
            s.apply_remote_item(&remote("7", "doomed", 100)).unwrap(),
            ApplyOutcome::Ignored
        );
        // Ties go to the delete.
        assert_eq!(
            s.apply_remote_item(&remote("7", "doomed", 150)).unwrap(),
            ApplyOutcome::Ignored
        );
        assert_eq!(s.count().unwrap(), 0);
        // But an edit made AFTER the delete is a genuine resurrection.
        assert_eq!(
            s.apply_remote_item(&remote("7", "edited after delete", 200)).unwrap(),
            ApplyOutcome::Inserted
        );
        assert_eq!(s.count().unwrap(), 1);
    }

    #[test]
    fn local_delete_writes_a_tombstone() {
        let s = store_with_device("g14");
        let id = s.insert_clipboard("delete me", None, None).unwrap();
        s.delete(id).unwrap();
        assert_eq!(s.count().unwrap(), 0);

        let stones = s.tombstones_since("g14", 0, 10).unwrap();
        assert_eq!(stones.len(), 1);
        assert_eq!(stones[0].origin_id, id.to_string());

        // Re-inserting the same text is a NEW row with a new origin id, so the
        // old tombstone does not shoot it down.
        let again = s.insert_clipboard("delete me", None, None).unwrap();
        assert_ne!(again, id);
        assert_eq!(s.count().unwrap(), 1);
    }

    #[test]
    fn deleting_a_legacy_row_is_still_a_plain_delete() {
        let s = Store::open_in_memory().unwrap();
        let id = s.insert_clipboard("no identity", None, None).unwrap();
        s.delete(id).unwrap();
        assert_eq!(s.count().unwrap(), 0);
        assert!(s.tombstones_since("g14", 0, 10).unwrap().is_empty());
    }

    #[test]
    fn watermarks_are_per_source() {
        let s = store_with_device("g14");
        s.apply_remote_item(&remote("1", "a", 100)).unwrap();
        s.apply_remote_item(&remote("2", "b", 300)).unwrap();
        let mut other = remote("9", "c", 250);
        other.source_machine = "pixel".into();
        s.apply_remote_item(&other).unwrap();
        let local = s.insert_clipboard("mine", None, None).unwrap();
        s.conn
            .execute("UPDATE items SET updated_at=42 WHERE id=?1", params![local])
            .unwrap();

        let mut marks = s.watermarks().unwrap();
        marks.sort();
        assert_eq!(
            marks,
            vec![
                ("g14".to_string(), 42),
                ("mac-1".to_string(), 300),
                ("pixel".to_string(), 250),
            ]
        );
    }

    #[test]
    fn items_since_is_ordered_filtered_and_capped() {
        let s = store_with_device("g14");
        for (origin, updated) in [("1", 100), ("2", 300), ("3", 200), ("4", 400)] {
            s.apply_remote_item(&remote(origin, &format!("item {origin}"), updated))
                .unwrap();
        }
        let mut foreign = remote("5", "other machine", 250);
        foreign.source_machine = "pixel".into();
        s.apply_remote_item(&foreign).unwrap();

        let page = s.items_since("mac-1", 0, 10).unwrap();
        assert_eq!(
            page.iter().map(|i| i.updated_at).collect::<Vec<_>>(),
            vec![100, 200, 300, 400],
            "oldest first, so a crash mid-stream leaves a usable cursor"
        );
        assert!(page.iter().all(|i| i.source_machine == "mac-1"));

        assert_eq!(
            s.items_since("mac-1", 200, 10).unwrap().iter().map(|i| i.updated_at).collect::<Vec<_>>(),
            vec![300, 400],
            "`after` is exclusive"
        );
        assert_eq!(
            s.items_since("mac-1", 0, 2).unwrap().iter().map(|i| i.updated_at).collect::<Vec<_>>(),
            vec![100, 200],
            "limit takes the oldest, not a random slice"
        );
        assert!(s.items_since("nobody", 0, 10).unwrap().is_empty());
    }

    #[test]
    fn tombstones_since_is_ordered_and_capped() {
        let s = store_with_device("g14");
        for (origin, deleted) in [("1", 100), ("2", 300), ("3", 200)] {
            s.apply_remote_tombstone(&RemoteTombstone {
                source_machine: "mac-1".into(),
                origin_id: origin.into(),
                deleted_at: deleted,
            })
            .unwrap();
        }
        let got = s.tombstones_since("mac-1", 100, 2).unwrap();
        assert_eq!(got.iter().map(|t| t.deleted_at).collect::<Vec<_>>(), vec![200, 300]);
    }

    #[test]
    fn prune_tombstones_drops_only_the_old_ones() {
        let s = store_with_device("g14");
        for (origin, deleted) in [("1", 100), ("2", 5_000)] {
            s.apply_remote_tombstone(&RemoteTombstone {
                source_machine: "mac-1".into(),
                origin_id: origin.into(),
                deleted_at: deleted,
            })
            .unwrap();
        }
        assert_eq!(s.prune_tombstones(1_000).unwrap(), 1);
        let left = s.tombstones_since("mac-1", 0, 10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].deleted_at, 5_000);
        // The pruned delete no longer defends the row: this is the window the
        // caller is trading away by pruning.
        assert_eq!(
            s.apply_remote_item(&remote("1", "back from the dead", 50)).unwrap(),
            ApplyOutcome::Inserted
        );
    }

    #[test]
    fn malformed_remote_rows_are_ignored_not_fatal() {
        let s = store_with_device("g14");
        let mut bad = remote("7", "junk", 100);
        bad.kind = "sql injection".into();
        assert_eq!(s.apply_remote_item(&bad).unwrap(), ApplyOutcome::Ignored);
        let mut empty = remote("", "junk", 100);
        empty.origin_id = String::new();
        assert_eq!(s.apply_remote_item(&empty).unwrap(), ApplyOutcome::Ignored);
        assert_eq!(s.count().unwrap(), 0);
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
