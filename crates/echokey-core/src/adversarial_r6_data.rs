//! ADVERSARIAL REVIEW — ROUND 6, store-level pass. Demonstrations, NOT fixes.
//!
//! Own file: `history.rs` is edited concurrently by other reviewers.
//! Every loop here is hard bounded; nothing opens a socket.

#![cfg(test)]

use crate::history::{RemoteItem, RemoteTombstone, Store};
use rusqlite::Connection;
use std::path::Path;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}

const ME: &str = "11111111-1111-4111-8111-111111111111";
const PEER: &str = "22222222-2222-4222-8222-222222222222";

/// The v1 schema exactly as shipped. Copied rather than imported because
/// `history::tests` is a private module; kept byte-identical to it so these
/// migrations run against the real historical shape.
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

// -- schema snapshots -------------------------------------------------------

/// Every column of every table, with its declared type, NOT NULL flag, default
/// and primary-key position. Compared instead of the raw CREATE text because
/// `ALTER TABLE ADD COLUMN` appends to the stored SQL, so a migrated table is
/// semantically identical to a fresh one while differing cosmetically.
fn table_shapes(conn: &Connection) -> Vec<(String, Vec<(String, String, i64, Option<String>, i64)>)> {
    let mut names: Vec<String> = {
        let mut st = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' \
                   AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap();
        let r = st.query_map([], |r| r.get(0)).unwrap();
        r.collect::<Result<Vec<String>, _>>().unwrap()
    };
    names.sort();
    names
        .into_iter()
        .map(|t| {
            let mut st = conn.prepare(&format!("PRAGMA table_info({t})")).unwrap();
            let cols = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (t, cols)
        })
        .collect()
}

/// Indexes and triggers, with their exact SQL. Both paths create these with
/// identical statements, so any difference here is a real one.
fn index_and_trigger_sql(conn: &Connection) -> Vec<(String, String, String)> {
    let mut st = conn
        .prepare(
            "SELECT type, name, COALESCE(sql,'') FROM sqlite_master \
              WHERE type IN ('index','trigger') AND name NOT LIKE 'sqlite_%' \
              ORDER BY type, name",
        )
        .unwrap();
    st.query_map([], |r| {
        let sql: String = r.get(2)?;
        // Whitespace only: the seeded v1 schema in this file is indented
        // differently from the one `init` writes, and that is an artefact of
        // the test, not a schema difference.
        let normalised = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        Ok((r.get(0)?, r.get(1)?, normalised))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap()
}

// -- historical databases ---------------------------------------------------

fn seed_v1(path: &Path) {
    let c = Connection::open(path).unwrap();
    let v1_only = V1_SCHEMA.replace("ALTER TABLE items ADD COLUMN source_machine TEXT;", "");
    c.execute_batch(&v1_only).unwrap();
    c.execute(
        "INSERT INTO items (kind, text, created_at) VALUES ('clipboard','v1 row',100)",
        [],
    )
    .unwrap();
    c.pragma_update(None, "user_version", 1i64).unwrap();
}

fn seed_v2(path: &Path) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(V1_SCHEMA).unwrap();
    c.execute(
        "INSERT INTO items (kind, text, created_at, source_machine) \
         VALUES ('clipboard','v2 row',200,'11111111-1111-4111-8111-111111111111')",
        [],
    )
    .unwrap();
    c.pragma_update(None, "user_version", 2i64).unwrap();
}

/// A real v3 database: the v3 DDL exactly as `init` writes it, plus rows and
/// tombstones, stamped at 3.
fn seed_v3(path: &Path) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(V1_SCHEMA).unwrap();
    c.execute_batch(
        r#"
        ALTER TABLE items ADD COLUMN origin_id TEXT;
        ALTER TABLE items ADD COLUMN updated_at INTEGER;
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
    )
    .unwrap();
    c.execute(
        "INSERT INTO items (kind, text, created_at, updated_at, source_machine, origin_id) \
         VALUES ('transcription','v3 row',300,300,?1,'origin-3')",
        rusqlite::params![PEER],
    )
    .unwrap();
    c.execute(
        "INSERT INTO tombstones (source_machine, origin_id, deleted_at) VALUES (?1,'gone-3',350)",
        rusqlite::params![PEER],
    )
    .unwrap();
    c.pragma_update(None, "user_version", 3i64).unwrap();
}

/// A real v4 database: v3 plus the single-key `source_marks` table, with a mark
/// already in it, stamped at 4.
fn seed_v4(path: &Path) {
    seed_v3(path);
    let c = Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS source_marks (
             source_machine TEXT PRIMARY KEY,
             received_clock INTEGER NOT NULL
         );",
    )
    .unwrap();
    c.execute(
        "INSERT INTO source_marks (source_machine, received_clock) VALUES (?1, 350)",
        rusqlite::params![PEER],
    )
    .unwrap();
    c.pragma_update(None, "user_version", 4i64).unwrap();
}

fn seed_v5(path: &Path) {
    seed_v4(path);
    let c = Connection::open(path).unwrap();
    c.execute_batch(
        "DROP TABLE IF EXISTS source_marks;
         CREATE TABLE source_marks (
             peer_machine   TEXT NOT NULL,
             source_machine TEXT NOT NULL,
             received_clock INTEGER NOT NULL,
             PRIMARY KEY (peer_machine, source_machine)
         );",
    )
    .unwrap();
    c.pragma_update(None, "user_version", 5i64).unwrap();
}

// ---------------------------------------------------------------------------
// R6-M1. Every historical version reaches a schema identical to a fresh one.
//
// Asserted against `Store::SCHEMA_VERSION_FOR_TEST` rather than a literal. The
// literal was `5`, so adding the v6 migration failed this test for no reason
// beyond its own staleness: which is noise that trains you to edit the test
// whenever it complains, exactly the reflex that lets a real migration bug
// through. The property under test is "every path lands where a fresh one
// does", and that is version-independent.
// ---------------------------------------------------------------------------
#[test]
fn r6_every_migration_path_lands_on_the_same_schema_as_a_fresh_store() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = Store::open(&dir.path().join("fresh.db")).unwrap();
    let fresh_tables = table_shapes(fresh.conn_for_test());
    let fresh_objects = index_and_trigger_sql(fresh.conn_for_test());
    let current = Store::SCHEMA_VERSION_FOR_TEST;
    assert_eq!(user_version(fresh.conn_for_test()), current);

    let cases: [(&str, fn(&Path)); 5] = [
        ("v1", seed_v1),
        ("v2", seed_v2),
        ("v3", seed_v3),
        ("v4", seed_v4),
        ("v5", seed_v5),
    ];
    for (name, seed) in cases {
        let p = dir.path().join(format!("{name}.db"));
        seed(&p);
        let s = Store::open(&p).unwrap();
        assert_eq!(user_version(s.conn_for_test()), current, "{name}: version stamp");
        assert_eq!(table_shapes(s.conn_for_test()), fresh_tables, "{name}: table shapes differ");
        assert_eq!(
            index_and_trigger_sql(s.conn_for_test()),
            fresh_objects,
            "{name}: indexes/triggers differ"
        );
    }
}

// ---------------------------------------------------------------------------
// R6-M2. Every migration path preserves every row and every tombstone.
// ---------------------------------------------------------------------------
#[test]
fn r6_migrations_preserve_every_row_and_tombstone() {
    let dir = tempfile::tempdir().unwrap();

    let p3 = dir.path().join("rows3.db");
    seed_v3(&p3);
    let s3 = Store::open(&p3).unwrap();
    assert_eq!(s3.count().unwrap(), 1, "v3 -> v5 lost a row");
    assert_eq!(s3.tombstone_count(PEER).unwrap(), 1, "v3 -> v5 lost a tombstone");
    assert!(s3.holds_identity(PEER, "gone-3").unwrap(), "the tombstone identity survived");
    assert_eq!(s3.items_from(PEER, 0, "", 10).unwrap().len(), 1, "the row is visible to sync");

    let p4 = dir.path().join("rows4.db");
    seed_v4(&p4);
    let s4 = Store::open(&p4).unwrap();
    assert_eq!(s4.count().unwrap(), 1, "v4 -> v5 lost a row");
    assert_eq!(s4.tombstone_count(PEER).unwrap(), 1, "v4 -> v5 lost a tombstone");
    // Documented: v4 marks are dropped because we never recorded which peer
    // supplied them. Assert the documented behaviour so a silent change fails.
    assert!(
        s4.watermarks(PEER).unwrap().is_empty(),
        "v4 marks must be dropped, not migrated under a guessed peer"
    );
}

// ---------------------------------------------------------------------------
// R6-M3. Interrupted migrations. Each is re-opened TWICE: a repair that only
// works once is not a repair.
// ---------------------------------------------------------------------------
#[test]
fn r6_interrupted_migrations_are_re_runnable() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = Store::open(&dir.path().join("fresh.db")).unwrap();
    let fresh_tables = table_shapes(fresh.conn_for_test());

    // (a) The v5 DDL landed but the stamp did not.
    let a = dir.path().join("a.db");
    seed_v4(&a);
    {
        let c = Connection::open(&a).unwrap();
        c.execute_batch(
            "DROP TABLE IF EXISTS source_marks;
             CREATE TABLE source_marks (
                 peer_machine   TEXT NOT NULL,
                 source_machine TEXT NOT NULL,
                 received_clock INTEGER NOT NULL,
                 PRIMARY KEY (peer_machine, source_machine)
             );",
        )
        .unwrap();
        // stamp deliberately left at 4
    }
    for pass in 1..=2 {
        let s = Store::open(&a).unwrap();
        assert_eq!(user_version(s.conn_for_test()), Store::SCHEMA_VERSION_FOR_TEST, "pass {pass}");
        assert_eq!(table_shapes(s.conn_for_test()), fresh_tables, "pass {pass}");
        assert_eq!(s.count().unwrap(), 1, "pass {pass}: row survived");
    }

    // (b) Half of the v3 ALTERs landed, stamp still at 2.
    let b = dir.path().join("b.db");
    seed_v2(&b);
    {
        let c = Connection::open(&b).unwrap();
        c.execute("ALTER TABLE items ADD COLUMN origin_id TEXT", []).unwrap();
    }
    for pass in 1..=2 {
        let s = Store::open(&b).unwrap();
        assert_eq!(user_version(s.conn_for_test()), Store::SCHEMA_VERSION_FOR_TEST, "pass {pass}");
        assert_eq!(table_shapes(s.conn_for_test()), fresh_tables, "pass {pass}");
        assert_eq!(s.count().unwrap(), 1, "pass {pass}: row survived");
    }

    // (c) Tables exist, stamp never written at all (the original build's
    //     interrupted first run).
    let c_path = dir.path().join("c.db");
    seed_v1(&c_path);
    {
        let c = Connection::open(&c_path).unwrap();
        c.pragma_update(None, "user_version", 0i64).unwrap();
    }
    for pass in 1..=2 {
        let s = Store::open(&c_path).unwrap();
        assert_eq!(user_version(s.conn_for_test()), Store::SCHEMA_VERSION_FOR_TEST, "pass {pass}");
        assert_eq!(table_shapes(s.conn_for_test()), fresh_tables, "pass {pass}");
        assert_eq!(s.count().unwrap(), 1, "pass {pass}: row survived");
    }
}

// ---------------------------------------------------------------------------
// R6-H1. Hostile input at the store boundary: nothing raises, nothing is
// stored that replication cannot later reach.
// ---------------------------------------------------------------------------
#[test]
fn r6_hostile_remote_rows_never_raise_and_never_hide() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(ME);
    let base = now_ms();

    let nasties: Vec<(&str, RemoteItem)> = vec![
        ("sql in the origin id", RemoteItem {
            source_machine: PEER.into(),
            origin_id: "'); DROP TABLE items;--".into(),
            kind: "clipboard".into(),
            text: "x".into(),
            created_at: base,
            updated_at: base,
            pinned: false,
        }),
        ("sql in the source", RemoteItem {
            source_machine: "'; DELETE FROM tombstones;--".into(),
            origin_id: "o".into(),
            kind: "clipboard".into(),
            text: "x".into(),
            created_at: base,
            updated_at: base,
            pinned: false,
        }),
        ("nul and invalid utf16 pairs in text", RemoteItem {
            source_machine: PEER.into(),
            origin_id: "nul".into(),
            kind: "transcription".into(),
            text: "a\u{0}b\u{FFFF}\u{FEFF}c".into(),
            created_at: base,
            updated_at: base,
            pinned: false,
        }),
        ("origin id above the paging ceiling", RemoteItem {
            source_machine: PEER.into(),
            origin_id: "\u{FFFF}\u{FFFF}".into(),
            kind: "clipboard".into(),
            text: "above the ceiling".into(),
            created_at: base,
            updated_at: base,
            pinned: false,
        }),
        ("i64::MAX clocks", RemoteItem {
            source_machine: PEER.into(),
            origin_id: "max".into(),
            kind: "clipboard".into(),
            text: "x".into(),
            created_at: i64::MAX,
            updated_at: i64::MAX,
            pinned: false,
        }),
        ("i64::MIN clocks", RemoteItem {
            source_machine: PEER.into(),
            origin_id: "min".into(),
            kind: "clipboard".into(),
            text: "x".into(),
            created_at: i64::MIN,
            updated_at: i64::MIN,
            pinned: false,
        }),
        ("empty everything", RemoteItem {
            source_machine: "".into(),
            origin_id: "".into(),
            kind: "".into(),
            text: "".into(),
            created_at: 0,
            updated_at: 0,
            pinned: false,
        }),
        ("one megabyte of text", RemoteItem {
            source_machine: PEER.into(),
            origin_id: "big".into(),
            kind: "clipboard".into(),
            text: "z".repeat(1024 * 1024),
            created_at: base,
            updated_at: base,
            pinned: false,
        }),
    ];
    for (what, item) in &nasties {
        s.apply_remote_item(PEER, item).unwrap_or_else(|e| panic!("{what} raised: {e}"));
        // Applying twice must be a no-op, never a duplicate.
        s.apply_remote_item(PEER, item).unwrap_or_else(|e| panic!("{what} raised on re-apply: {e}"));
    }
    for (what, item) in &nasties {
        let t = RemoteTombstone {
            source_machine: item.source_machine.clone(),
            origin_id: item.origin_id.clone(),
            deleted_at: item.updated_at,
        };
        s.apply_remote_tombstone(PEER, &t)
            .unwrap_or_else(|e| panic!("{what} tombstone raised: {e}"));
    }

    // The store is still usable and self-consistent.
    assert!(s.count().unwrap() >= 0);
    assert!(s.search("above", None, 10).is_ok());
    assert!(s.known_sources().is_ok());
    let integrity: String = s
        .conn_for_test()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok", "the database is damaged");

    // EVERY stored row must be reachable by the replication cursor. A row a
    // peer can put in but nobody can ever page out is a silent hole.
    let mut unreachable = Vec::new();
    for src in s.known_sources().unwrap() {
        let stored: i64 = s
            .conn_for_test()
            .query_row(
                "SELECT COUNT(*) FROM items WHERE source_machine=?1 AND origin_id IS NOT NULL",
                rusqlite::params![src],
                |r| r.get(0),
            )
            .unwrap();
        let paged = s.items_from(&src, 0, "", 100_000).unwrap().len() as i64;
        if paged != stored {
            unreachable.push((src, stored, paged));
        }
    }
    assert!(unreachable.is_empty(), "rows stored but unreachable to sync: {unreachable:?}");
}

// ---------------------------------------------------------------------------
// R6-C1. The tombstone ceiling drops deletes that have not been delivered yet.
//
// `cap_tombstones` runs only inside `apply_remote_tombstone`, so LOCAL deletes
// are not capped: Clear History over a large replicated history writes as many
// tombstones as there were rows. The very next tombstone that arrives from a
// peer then trims the table back to MAX_TOMBSTONES_PER_SOURCE by dropping the
// OLDEST — which, right after a Clear, are the user's own undelivered deletes.
//
// Once dropped, the identity is no longer held (`holds_identity` is false) and
// `apply_remote_item` has nothing to lose to: the row is resurrected the next
// time the author offers it, which a re-offer after a kind toggle
// (`resend_all`, replicate.rs:390) does from clock zero.
// ---------------------------------------------------------------------------
#[test]
fn r6_the_tombstone_ceiling_drops_undelivered_local_deletes_and_the_row_returns() {
    let over = crate::history::MAX_TOMBSTONES_PER_SOURCE as usize + 50;
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(ME);
    let base = now_ms() - 10_000_000;

    // A large replicated history from one peer.
    for i in 0..over {
        s.apply_remote_item(
            PEER,
            &RemoteItem {
                source_machine: PEER.into(),
                origin_id: format!("row-{i:06}"),
                kind: "clipboard".into(),
                text: format!("secret {i}"),
                created_at: base + i as i64,
                updated_at: base + i as i64,
                pinned: false,
            },
        )
        .unwrap();
    }
    assert_eq!(s.count().unwrap(), over as i64);

    // The user clears history. Local deletes are NOT capped.
    s.clear(None).unwrap();
    assert_eq!(
        s.tombstone_count(PEER).unwrap(),
        over as i64,
        "precondition: Clear History wrote one tombstone per row, uncapped"
    );
    assert!(s.holds_identity(PEER, "row-000000").unwrap());

    // One ordinary tombstone arrives from the peer. That single apply trims the
    // table — dropping the oldest, i.e. deletes we have not yet delivered.
    s.apply_remote_tombstone(
        PEER,
        &RemoteTombstone {
            source_machine: PEER.into(),
            origin_id: "row-from-the-peer".into(),
            deleted_at: now_ms(),
        },
    )
    .unwrap();

    assert!(
        s.holds_identity(PEER, "row-000000").unwrap(),
        "a delete the user made and we never delivered was silently dropped; \
         tombstones now {}",
        s.tombstone_count(PEER).unwrap()
    );

    // And with the tombstone gone there is nothing to refuse the row with.
    let back = s
        .apply_remote_item(
            PEER,
            &RemoteItem {
                source_machine: PEER.into(),
                origin_id: "row-000000".into(),
                kind: "clipboard".into(),
                text: "secret 0".into(),
                created_at: base,
                updated_at: base,
                pinned: false,
            },
        )
        .unwrap();
    assert_eq!(
        back,
        crate::history::ApplyOutcome::Ignored,
        "the cleared row came straight back"
    );
}
