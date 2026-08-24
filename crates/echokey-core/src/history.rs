//! Unified history store: transcriptions + clipboard, plus dictionary entries.
//! SQLite (bundled), FTS5 for word search, fuzzy re-ranking on top.
//! Local-only by design. No telemetry, no cloud, ever.

use crate::dictionary::DictEntry;
use crate::search;
use crate::types::{HistoryItem, HistoryKind, TranscriptionResult};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

const SCHEMA_VERSION: i64 = 4;
/// How far ahead of us a peer's clock may be before we stop believing it.
/// Generous enough for a real timezone or NTP wobble, small enough that a
/// nonsense timestamp cannot win every conflict forever.
const MAX_CLOCK_SKEW_MS: i64 = 24 * 60 * 60 * 1000;
/// Tombstones are never dropped sooner than this, whatever retention says.
const TOMBSTONE_MIN_DAYS: u32 = 180;

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

    /// Open on a caller-supplied connection. Tests use it to build a database
    /// in a specific historical state and prove the migration copes.
    #[cfg(test)]
    pub fn from_connection_for_test(conn: Connection) -> Result<Self, StoreError> {
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn conn_for_test(&self) -> &Connection {
        &self.conn
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
            // Each ALTER is guarded, because SQLite has no ADD COLUMN IF NOT
            // EXISTS and this step is not atomic on its own: a crash or a force
            // quit between the two ALTERs previously left user_version at 2 with
            // origin_id already present, and every subsequent launch died on
            // "duplicate column name" with no repair path — the user's entire
            // history unreachable. The whole step also runs in one transaction
            // so it either lands or does not.
            let has_col = |name: &str| -> Result<bool, StoreError> {
                let mut st = conn.prepare("PRAGMA table_info(items)")?;
                let mut rows = st.query([])?;
                while let Some(r) = rows.next()? {
                    let c: String = r.get(1)?;
                    if c == name {
                        return Ok(true);
                    }
                }
                Ok(false)
            };
            if !has_col("origin_id")? {
                conn.execute("ALTER TABLE items ADD COLUMN origin_id TEXT", [])?;
            }
            if !has_col("updated_at")? {
                conn.execute("ALTER TABLE items ADD COLUMN updated_at INTEGER", [])?;
            }
            conn.execute_batch(
                r#"

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

        if version < 4 {
            // v4: stop deriving "what we have received from a peer" from the
            // rows we are currently holding for it.
            //
            // The advertised watermark used to be MAX(updated_at) over every
            // row of a source. Two things poisoned it. A local edit — pinning a
            // peer's row on a machine whose clock ran fast — wrote that peer's
            // watermark into the future, so every genuine row it produced
            // afterwards fell below the mark and was never offered again:
            // silent, permanent, and not repaired by fixing the clock. And a
            // single row stamped i64::MAX, by a dead RTC or by a peer that
            // simply says so, muted that source forever.
            //
            // A receipt is not a property of a row, so it does not belong on
            // one: clear() deletes the rows and retention evicts them, and the
            // receipt has to outlive both or every exchange re-offers the whole
            // history. It lives in its own table, is written only when
            // something actually arrives from that source, and only ever moves
            // forward.
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS source_marks (
                     source_machine TEXT PRIMARY KEY,
                     received_clock INTEGER NOT NULL
                 );",
            )?;
            // Seed from what we hold, which is the best estimate available for
            // a store that predates the table. Slightly conservative is fine —
            // re-offered rows are idempotent — but a hole would not be.
            conn.execute(
                "INSERT INTO source_marks (source_machine, received_clock)
                 SELECT source_machine, MAX(clock) FROM (
                     SELECT source_machine, COALESCE(updated_at, created_at) AS clock
                       FROM items WHERE source_machine IS NOT NULL AND origin_id IS NOT NULL
                     UNION ALL
                     SELECT source_machine, deleted_at AS clock FROM tombstones
                 )
                 GROUP BY source_machine
                 ON CONFLICT(source_machine) DO NOTHING",
                [],
            )?;
            version = 4;
        }
        // Every step leaves `version` at its own level so the next one can read
        // it. The last assignment has no reader yet, and will the moment a v5
        // step is added; dropping it would make that addition a silent bug.
        let _ = version;
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

    /// Delete unpinned history, writing tombstones so the deletes propagate.
    ///
    /// Without the tombstones this was worse than useless on a synced pair:
    /// clearing removed the rows locally, the source then vanished from
    /// `watermarks()` (which only sees surviving rows), the peer therefore
    /// served from zero, and the "cleared" history came straight back on the
    /// next exchange. Someone who pasted a password, panicked and hit Clear
    /// History got it returned to them thirty seconds later.
    pub fn clear(&self, kind: Option<HistoryKind>) -> Result<usize, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let now = now_ms();
        let n = match kind {
            Some(k) => {
                tx.execute(
                    "INSERT INTO tombstones (source_machine, origin_id, deleted_at)
                     SELECT source_machine, origin_id, max(?2, COALESCE(updated_at, created_at))
                       FROM items
                      WHERE kind=?1 AND pinned=0
                        AND source_machine IS NOT NULL AND origin_id IS NOT NULL
                     ON CONFLICT(source_machine, origin_id)
                     DO UPDATE SET deleted_at = max(tombstones.deleted_at, excluded.deleted_at)",
                    params![kind_str(k), now],
                )?;
                tx.execute("DELETE FROM items WHERE kind=?1 AND pinned=0", params![kind_str(k)])?
            }
            None => {
                tx.execute(
                    "INSERT INTO tombstones (source_machine, origin_id, deleted_at)
                     SELECT source_machine, origin_id, max(?1, COALESCE(updated_at, created_at))
                       FROM items
                      WHERE pinned=0
                        AND source_machine IS NOT NULL AND origin_id IS NOT NULL
                     ON CONFLICT(source_machine, origin_id)
                     DO UPDATE SET deleted_at = max(tombstones.deleted_at, excluded.deleted_at)",
                    params![now],
                )?;
                tx.execute("DELETE FROM items WHERE pinned=0", [])?
            }
        };
        tx.commit()?;
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
        // Tombstones are pruned here rather than by a caller, because a caller
        // that forgets means a table that only ever grows — one row per delete,
        // for the life of the install.
        //
        // Dropping a tombstone is only safe because the receipt for its source
        // outlives it: a peer is offered nothing at or below the mark we have
        // already recorded, so a row we deleted long ago is not re-sent even
        // after its tombstone is gone. A peer that EDITS that row raises its
        // clock above the mark and legitimately wins, which is the documented
        // "an edit after a delete survives it" rule, not a resurrection.
        //
        // The floor is deliberately far longer than any retention setting a
        // user is likely to pick: the cost of keeping a tombstone is one small
        // row, and the cost of dropping one too early is a deleted dictation
        // coming back.
        let tombstone_floor_days = retention_days.max(TOMBSTONE_MIN_DAYS) as i64;
        self.prune_tombstones(now_ms() - tombstone_floor_days * 86_400_000)?;
        Ok(removed)
    }

    pub fn count(&self) -> Result<i64, StoreError> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?)
    }

    // -- replication --------------------------------------------------------

    /// What to advertise to a peer: the highest clock we will not need again,
    /// per source device.
    ///
    /// The two directions are deliberately different. For OUR OWN rows this is
    /// what we hold, because that is exactly what a peer still needs from us.
    /// For every other source it is what we have RECEIVED, read from
    /// `source_marks` — never derived from the rows themselves, so neither a
    /// local edit with a fast clock nor a hostile timestamp on one row can
    /// advertise a future we have not actually seen and silence that peer.
    pub fn watermarks(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT source_machine, MAX(clock) FROM (
                 SELECT source_machine, COALESCE(updated_at, created_at) AS clock
                   FROM items
                  WHERE source_machine IS NOT NULL AND origin_id IS NOT NULL
                    AND source_machine IS ?1
                 UNION ALL
                 SELECT source_machine, deleted_at AS clock
                   FROM tombstones WHERE source_machine IS ?1
                 UNION ALL
                 SELECT source_machine, received_clock AS clock
                   FROM source_marks WHERE source_machine IS NOT ?1
             )
             GROUP BY source_machine ORDER BY source_machine",
        )?;
        let rows = stmt.query_map(params![self.source()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Record that something at `clock` arrived from `source`. Monotonic: a
    /// late or out-of-order row can never walk the mark backwards.
    ///
    /// Callers use this for rows they saw but deliberately did NOT apply — a
    /// row older than retention keeps, or of a kind the user switched off.
    /// Rows that ARE applied record themselves; see `mark_received_in`.
    pub fn note_received(&self, source: &str, clock: i64) -> Result<(), StoreError> {
        Self::mark_received_in(&self.conn, self.source(), source, clock)
    }

    /// The receipt write itself, usable inside an open transaction.
    ///
    /// `apply_remote_item` and `apply_remote_tombstone` call this in the same
    /// transaction that stores the row, so "we hold it" and "we have seen it"
    /// commit together or not at all. Leaving it to the caller would make the
    /// invariant something every call site had to remember, and forgetting it
    /// once means a peer re-sending its whole history on every exchange.
    fn mark_received_in(
        conn: &Connection,
        me: Option<&str>,
        source: &str,
        clock: i64,
    ) -> Result<(), StoreError> {
        if source.is_empty() || Some(source) == me {
            return Ok(());
        }
        // Same ceiling the rows themselves get. A peer that stamps one row
        // i64::MAX must not be able to park its own mark there and never be
        // asked for anything again.
        let clock = clock.min(now_ms() + MAX_CLOCK_SKEW_MS);
        conn.execute(
            "INSERT INTO source_marks (source_machine, received_clock) VALUES (?1, ?2)
             ON CONFLICT(source_machine)
             DO UPDATE SET received_clock = MAX(received_clock, excluded.received_clock)",
            params![source, clock],
        )?;
        Ok(())
    }

    /// Forget every receipt, so the next exchange re-offers everything.
    ///
    /// Called when the user turns a sync kind back ON. While a kind is off we
    /// still advance the mark past the rows we drop — otherwise the peer would
    /// re-send them on every exchange forever — which would leave a permanent
    /// hole the moment the switch came back on. Re-applying a whole history is
    /// idempotent and takes a second on a LAN; a hole is silent and forever.
    pub fn reset_source_marks(&self) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM source_marks", [])?;
        Ok(())
    }

    /// Every source we hold anything for, live or deleted.
    ///
    /// The serve loop iterates this rather than `watermarks()` keys so that a
    /// source with only tombstones left still gets its deletes offered.
    pub fn known_sources(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT source_machine FROM (
                 SELECT source_machine FROM items
                  WHERE source_machine IS NOT NULL AND origin_id IS NOT NULL
                 UNION
                 SELECT source_machine FROM tombstones
             ) ORDER BY source_machine",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
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
        // Clamp the peer's clock. Nothing on the wire bounds these, so a peer
        // with a dead RTC — or one that simply says so — could stamp a row
        // i64::MAX, win every future conflict under last-writer-wins, and pin
        // our watermark for that source at the ceiling so its genuine rows were
        // never requested again. A little forward skew is normal; a century is
        // not.
        let ceiling = now_ms() + MAX_CLOCK_SKEW_MS;
        let item = &RemoteItem {
            updated_at: item.updated_at.min(ceiling),
            created_at: item.created_at.min(ceiling),
            ..item.clone()
        };

        let tx = self.conn.unchecked_transaction()?;
        // Taken before the tombstone and conflict checks: we have seen this row
        // whether or not it wins, and a losing row we forget would be offered
        // again on every single exchange.
        Self::mark_received_in(&tx, self.source(), &item.source_machine, item.updated_at)?;
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
        // Same clamp as items: an unbounded deleted_at would beat every future
        // edit forever and park the source watermark at the ceiling.
        let t = &RemoteTombstone {
            deleted_at: t.deleted_at.min(now_ms() + MAX_CLOCK_SKEW_MS),
            ..t.clone()
        };
        let tx = self.conn.unchecked_transaction()?;
        Self::mark_received_in(&tx, self.source(), &t.source_machine, t.deleted_at)?;
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
    fn a_fresh_schema_matches_a_migrated_one() {
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

    #[test]
    fn clearing_history_leaves_tombstones_so_the_clear_actually_sticks() {
        // The failure this guards: clear() removed rows but wrote no
        // tombstones, the source then vanished from watermarks(), the peer
        // served from zero and handed the "cleared" history straight back.
        let mut st = Store::open_in_memory().unwrap();
        st.set_device_id("dev-a");
        st.insert_clipboard("a password", None, None).unwrap();
        st.insert_clipboard("something else", None, None).unwrap();
        assert_eq!(st.clear(None).unwrap(), 2);

        let t = st.tombstones_since("dev-a", 0, 100).unwrap();
        assert_eq!(t.len(), 2, "every cleared row leaves a tombstone");

        // And the source is still visible to replication, so the peer is told
        // about the deletes rather than being asked for everything again.
        let w = st.watermarks().unwrap();
        assert!(
            w.iter().any(|(src, _)| src == "dev-a"),
            "a source with only tombstones must still report a watermark"
        );
        assert!(st.known_sources().unwrap().iter().any(|s| s == "dev-a"));
    }

    #[test]
    fn a_pinned_row_survives_clear_and_is_not_tombstoned() {
        let mut st = Store::open_in_memory().unwrap();
        st.set_device_id("dev-a");
        let keep = st.insert_clipboard("keep me", None, None).unwrap();
        st.insert_clipboard("bin me", None, None).unwrap();
        st.set_pinned(keep, true).unwrap();
        assert_eq!(st.clear(None).unwrap(), 1);
        assert_eq!(st.tombstones_since("dev-a", 0, 100).unwrap().len(), 1);
        assert!(st.source_machine_of(keep).unwrap().is_some(), "pinned row still there");
    }

    #[test]
    fn deleting_the_last_row_from_a_source_still_propagates_the_delete() {
        // watermarks() used to see only surviving rows, so a source whose last
        // row was deleted disappeared and its tombstone was never served — the
        // peer kept the row forever.
        let mut st = Store::open_in_memory().unwrap();
        st.set_device_id("me");
        let remote = RemoteItem {
            source_machine: "peer".into(),
            origin_id: "1".into(),
            kind: "clipboard".into(),
            text: "theirs".into(),
            created_at: 1_000,
            updated_at: 1_000,
            pinned: false,
        };
        st.apply_remote_item(&remote).unwrap();
        let id: i64 = st
            .conn_for_test()
            .query_row("SELECT id FROM items WHERE source_machine='peer'", [], |r| r.get(0))
            .unwrap();
        st.delete_item_local(id).unwrap();

        assert!(
            st.known_sources().unwrap().iter().any(|s| s == "peer"),
            "the source must remain visible so its tombstone is offered"
        );
        assert_eq!(st.tombstones_since("peer", 0, 10).unwrap().len(), 1);
    }

    #[test]
    fn an_interrupted_v3_migration_can_be_re_run() {
        // A crash between the two ALTERs left user_version at 2 with origin_id
        // already added, and every later launch died on "duplicate column
        // name" — the whole history unreachable, with no repair path.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE items (
                id INTEGER PRIMARY KEY, kind TEXT, text TEXT, raw_text TEXT,
                created_at INTEGER, duration_ms INTEGER, model_id TEXT, language TEXT,
                app_id TEXT, app_name TEXT, meta TEXT, pinned INTEGER DEFAULT 0,
                source_machine TEXT
             );",
        )
        .unwrap();
        // Half-applied state.
        conn.execute("ALTER TABLE items ADD COLUMN origin_id TEXT", []).unwrap();
        conn.pragma_update(None, "user_version", 2i64).unwrap();

        let store = Store::from_connection_for_test(conn);
        assert!(store.is_ok(), "a half-applied migration must be recoverable: {:?}", store.err());
    }

    // ---- Receipts: what we advertise is what we have RECEIVED ------------

    fn peer_row(source: &str, origin: &str, text: &str, clock: i64) -> RemoteItem {
        RemoteItem {
            source_machine: source.into(),
            origin_id: origin.into(),
            kind: "transcription".into(),
            text: text.into(),
            created_at: clock,
            updated_at: clock,
            pinned: false,
        }
    }

    #[test]
    fn a_local_edit_cannot_raise_a_peers_watermark() {
        // The bug this exists for: pin a peer's row on a machine whose clock
        // runs fast, and that peer's watermark jumps into the future. Every
        // genuine row it produces afterwards falls below the mark and is never
        // offered again — silently, permanently, and not repaired by fixing
        // the clock.
        let s = store_with_device("11111111-1111-4111-8111-111111111111");
        let peer = "22222222-2222-4222-8222-222222222222";
        s.apply_remote_item(&peer_row(peer, "a", "from the peer", 1_000)).unwrap();

        let before = s.watermarks().unwrap();
        assert_eq!(before, vec![(peer.to_string(), 1_000)]);

        // A local edit stamped far in the future, exactly as a fast clock or a
        // skewed timezone would produce.
        let id: i64 = s
            .conn
            .query_row("SELECT id FROM items WHERE origin_id = 'a'", [], |r| r.get(0))
            .unwrap();
        s.conn
            .execute("UPDATE items SET pinned = 1, updated_at = ?1 WHERE id = ?2",
                     params![9_000_000_000_000i64, id])
            .unwrap();

        let after = s.watermarks().unwrap();
        assert_eq!(
            after,
            vec![(peer.to_string(), 1_000)],
            "our own edit must not tell the peer we already have its future"
        );
    }

    #[test]
    fn a_hostile_timestamp_cannot_mute_a_source_forever() {
        // A peer that stamps one row i64::MAX would, unclamped, win every
        // future conflict AND park its own watermark at the ceiling so we never
        // ask it for anything again.
        let s = store_with_device("11111111-1111-4111-8111-111111111111");
        let peer = "22222222-2222-4222-8222-222222222222";
        s.apply_remote_item(&peer_row(peer, "evil", "far future", i64::MAX)).unwrap();

        let ceiling = now_ms() + MAX_CLOCK_SKEW_MS;
        let stored: i64 = s
            .conn
            .query_row("SELECT updated_at FROM items WHERE origin_id = 'evil'", [], |r| r.get(0))
            .unwrap();
        assert!(stored <= ceiling, "stored clock must be clamped, got {stored}");

        let marks = s.watermarks().unwrap();
        assert_eq!(marks.len(), 1);
        assert!(marks[0].1 <= ceiling, "watermark must be clamped too");

        // And a genuine later row still wins, which is what "not muted" means.
        let later = now_ms() + MAX_CLOCK_SKEW_MS + 60_000;
        s.apply_remote_item(&peer_row(peer, "evil", "the real one", later)).unwrap();
        let text: String = s
            .conn
            .query_row("SELECT text FROM items WHERE origin_id = 'evil'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(text, "the real one");
    }

    #[test]
    fn receipts_outlive_the_rows_they_came_from() {
        // clear() deletes the rows and retention evicts them. If the mark were
        // derived from the rows, every exchange after a Clear History would
        // re-offer the peer's entire past.
        let s = store_with_device("11111111-1111-4111-8111-111111111111");
        let peer = "22222222-2222-4222-8222-222222222222";
        s.apply_remote_item(&peer_row(peer, "a", "one", 1_000)).unwrap();
        s.note_received(peer, 5_000).unwrap();
        s.clear(None).unwrap();

        let marks = s.watermarks().unwrap();
        let peer_mark = marks.iter().find(|(src, _)| src == peer).map(|(_, c)| *c);
        assert_eq!(peer_mark, Some(5_000), "the receipt survives the row");
    }

    #[test]
    fn a_receipt_never_walks_backwards() {
        let s = store_with_device("11111111-1111-4111-8111-111111111111");
        let peer = "22222222-2222-4222-8222-222222222222";
        s.note_received(peer, 9_000).unwrap();
        s.note_received(peer, 10).unwrap();
        let marks = s.watermarks().unwrap();
        assert_eq!(marks, vec![(peer.to_string(), 9_000)]);
    }

    #[test]
    fn our_own_watermark_is_what_we_hold_not_what_we_received() {
        // A peer needs everything we have authored, so for our own source the
        // mark must track the rows themselves.
        let me = "11111111-1111-4111-8111-111111111111";
        let s = store_with_device(me);
        s.insert_transcription(&tr("mine"), None, None).unwrap();
        let marks = s.watermarks().unwrap();
        let ours = marks.iter().find(|(src, _)| src == me);
        assert!(ours.is_some(), "we must advertise our own rows: {marks:?}");
        assert!(ours.unwrap().1 > 0);

        // And note_received refuses to touch our own source, because nothing
        // arrives from us.
        s.note_received(me, i64::MAX).unwrap();
        let after = s.watermarks().unwrap();
        assert_eq!(
            after.iter().find(|(src, _)| src == me).unwrap().1,
            ours.unwrap().1
        );
    }

    #[test]
    fn resetting_receipts_makes_the_next_exchange_refetch_everything() {
        let s = store_with_device("11111111-1111-4111-8111-111111111111");
        let peer = "22222222-2222-4222-8222-222222222222";
        s.note_received(peer, 8_000).unwrap();
        assert!(!s.watermarks().unwrap().is_empty());
        s.reset_source_marks().unwrap();
        let marks = s.watermarks().unwrap();
        assert!(
            marks.iter().all(|(src, _)| src != peer),
            "no mark means the peer sends its whole history again: {marks:?}"
        );
    }

    #[test]
    fn a_local_clear_cannot_raise_a_peers_watermark_either() {
        // clear() writes a tombstone over the peer's row stamped with OUR
        // clock. Deriving the mark from tombstones would reintroduce the same
        // silencing through a different door.
        let s = store_with_device("11111111-1111-4111-8111-111111111111");
        let peer = "22222222-2222-4222-8222-222222222222";
        s.apply_remote_item(&peer_row(peer, "a", "one", 1_000)).unwrap();
        s.clear(None).unwrap();
        s.conn
            .execute("UPDATE tombstones SET deleted_at = ?1", params![9_000_000_000_000i64])
            .unwrap();
        let marks = s.watermarks().unwrap();
        assert_eq!(marks, vec![(peer.to_string(), 1_000)]);
    }

    #[test]
    fn an_upgraded_store_keeps_its_place() {
        // A v3 store has rows but no receipts. Seeding from what it holds is
        // the only estimate available; being slightly conservative re-sends
        // idempotent rows, whereas being optimistic would open a hole.
        let s = store_with_device("11111111-1111-4111-8111-111111111111");
        let peer = "22222222-2222-4222-8222-222222222222";
        s.apply_remote_item(&peer_row(peer, "a", "one", 4_000)).unwrap();
        s.conn.execute("DELETE FROM source_marks", []).unwrap();

        // Re-run the v4 seed exactly as migration does.
        s.conn
            .execute(
                "INSERT INTO source_marks (source_machine, received_clock)
                 SELECT source_machine, MAX(clock) FROM (
                     SELECT source_machine, COALESCE(updated_at, created_at) AS clock
                       FROM items WHERE source_machine IS NOT NULL AND origin_id IS NOT NULL
                     UNION ALL
                     SELECT source_machine, deleted_at AS clock FROM tombstones
                 )
                 GROUP BY source_machine
                 ON CONFLICT(source_machine) DO NOTHING",
                [],
            )
            .unwrap();
        assert_eq!(s.watermarks().unwrap(), vec![(peer.to_string(), 4_000)]);
    }

    #[test]
    fn a_clamped_tombstone_cannot_outrank_every_future_edit() {
        let s = store_with_device("11111111-1111-4111-8111-111111111111");
        let peer = "22222222-2222-4222-8222-222222222222";
        s.apply_remote_tombstone(&RemoteTombstone {
            source_machine: peer.into(),
            origin_id: "a".into(),
            deleted_at: i64::MAX,
        })
        .unwrap();
        let stored: i64 = s
            .conn
            .query_row("SELECT deleted_at FROM tombstones WHERE origin_id='a'", [], |r| r.get(0))
            .unwrap();
        assert!(stored <= now_ms() + MAX_CLOCK_SKEW_MS);

        // A genuine later edit therefore still survives the delete.
        let later = now_ms() + MAX_CLOCK_SKEW_MS + 60_000;
        let out = s.apply_remote_item(&peer_row(peer, "a", "edited after the delete", later)).unwrap();
        assert_eq!(out, ApplyOutcome::Inserted);
    }
}
