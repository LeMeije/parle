//! ADVERSARIAL REVIEW, ROUND 10. Data integrity, clocks and replication
//! correctness, store side.
//!
//! Round 9's fixes are the target:
//!   * `next_clock_in` lost its ceiling fallback and now returns
//!     `max(now, newest + 1)` unconditionally.
//!   * `Store::clear` moved from a correlated subquery to a CTE.
//!   * `next_clock_in` dropped `COALESCE(updated_at, created_at)`.
//!   * `drain` range-checks `created_at` (asserted from the sync side).
//!
//! Every test here is bounded and does no I/O beyond an in-memory database or
//! a temp file. Nothing sleeps and nothing opens a socket.

#![cfg(test)]

use crate::history::{RemoteItem, RemoteTombstone, Store, MAX_CLOCK_SKEW_MS, ORIGIN_CEILING};
use crate::types::HistoryKind;

const ME: &str = "11111111-1111-4111-8111-111111111111";
const PEER: &str = "22222222-2222-4222-8222-222222222222";
const THIRD: &str = "33333333-3333-4333-8333-333333333333";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}

fn store_as(id: &str) -> Store {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(id);
    s
}

fn store() -> Store {
    store_as(ME)
}

/// The clock actually stamped on a row, read straight out of the table.
fn updated_at_of(s: &Store, id: i64) -> i64 {
    s.conn_for_test()
        .query_row("SELECT updated_at FROM items WHERE id=?1", rusqlite::params![id], |r| r.get(0))
        .unwrap()
}

fn clocks_for(s: &Store, source: &str) -> Vec<i64> {
    let mut st = s
        .conn_for_test()
        .prepare("SELECT deleted_at FROM tombstones WHERE source_machine=?1 ORDER BY origin_id")
        .unwrap();
    let v: Vec<i64> =
        st.query_map(rusqlite::params![source], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    v
}

// ===========================================================================
// R10-1. A CLOCK THAT WAS ONCE WRONG NEVER COMES BACK DOWN.
//
// `next_clock_in` used to fall back to the plain wall clock once
// `newest + 1` passed a ceiling. Round 9 removed the fallback outright
// because it stamped BELOW what the store already held. What replaced it is
// unconditional: `max(now, newest + 1)`, warn if it is out of range, stamp it
// anyway.
//
// Nothing ever lowers `newest`. It is the maximum over `items.updated_at` and
// `tombstones.deleted_at` for our own source, tombstones are never pruned by
// age (`prune_tombstones` has no caller anywhere in the app), and a delete or
// a Clear stamps `newest + 1`, which RAISES it.
//
// So one episode of a wrong system clock poisons this device's own source for
// the whole duration of the drift, and it survives every repair the user has:
// fixing the clock, deleting the rows, and Clear History all leave it in
// place, and the last two make it worse.
//
// In ordinary terms: the Windows box has a flat CMOS battery, or dual-boots
// and gets the RTC-vs-UTC mix-up, or the user set the date forward once to
// test something. It runs three days fast for an afternoon. The user notices
// and fixes it. From that moment every dictation and every clipboard entry on
// that machine is stamped three days in the future, so the Mac refuses all of
// them, silently, for three days. Sync still connects, still handshakes, still
// reports a successful exchange. Nothing arrives.
// ===========================================================================

/// The state a backwards clock correction leaves behind.
///
/// There is no production path that can be driven from a test, because it needs
/// `now_ms()` to move: a row is written while the clock reads T + drift, and
/// the clock is then corrected to T. What is written is exactly this — the row
/// `insert_clipboard` produces with `created_at = updated_at = now_ms()` — so
/// the row is stamped directly rather than pretending otherwise.
fn poison_own_clock(s: &Store, drift_ms: i64) {
    let id = s.insert_clipboard("written while the clock was wrong", None, None).unwrap();
    let ahead = now_ms() + drift_ms;
    s.conn_for_test()
        .execute(
            "UPDATE items SET created_at=?1, updated_at=?1 WHERE id=?2",
            rusqlite::params![ahead, id],
        )
        .unwrap();
}

#[test]
fn r10_a_corrected_clock_never_recovers_every_later_row_is_refused_by_a_correct_peer() {
    // INVERTED: correcting the clock now DOES recover the machine.
    //
    // The finding was real and was a round-9 regression. Round 9 removed the
    // ceiling entirely, so a machine whose clock had once been days fast kept
    // its own `newest` up there for ever: nothing lowers it, tombstones are
    // never pruned by age, and both delete and Clear stamp above it. Every row
    // it wrote afterwards was stamped days ahead, a correctly-clocked peer
    // refused all of them, and fixing the clock changed nothing.
    //
    // The clamp is on the CEILING now, not the floor. A peer only ever banks a
    // cursor at or below its own `now + skew`, so a `newest` above our ceiling
    // is not protecting any real cursor: those rows were refused, not received.
    // Stamping at the ceiling is above every cursor that exists and inside what
    // a correct peer accepts, so the machine recovers immediately.
    let s = store();
    let now = now_ms();

    // The machine wrote rows while its clock was three days fast.
    let poisoned = now + 3 * 86_400_000;
    s.conn_for_test()
        .execute(
            "INSERT INTO items (kind, text, created_at, updated_at, source_machine, origin_id)
             VALUES ('clipboard', 'from the bad old days', ?1, ?1, ?2, 'poison')",
            rusqlite::params![poisoned, ME],
        )
        .unwrap();

    // The user notices and fixes the clock. The next thing they write must be
    // acceptable to a peer whose clock is correct.
    let id = s.insert_clipboard("after the fix", None, None).unwrap();
    let clock: i64 = s
        .conn_for_test()
        .query_row("SELECT updated_at FROM items WHERE id=?1", rusqlite::params![id], |r| r.get(0))
        .unwrap();

    let peer_ceiling = now_ms() + MAX_CLOCK_SKEW_MS;
    assert!(
        clock <= peer_ceiling,
        "a row written after the clock was corrected is stamped {clock}, past the {peer_ceiling} \
         a correctly-clocked peer will accept. The device syncs nothing and fixing the clock \
         does not help."
    );
    // And it is still strictly above the wall clock, so it cannot fall below a
    // cursor an honest peer legitimately holds.
    assert!(clock >= now_ms() - 1_000, "and it must not be stamped in the past: {clock}");
}

#[test]
fn r10_no_user_action_repairs_a_poisoned_clock_delete_and_clear_both_make_it_worse() {
    // INVERTED: delete and Clear no longer push the clock further out.
    //
    // They used to: `delete_item_local` stamped `newest + 1` and `clear`
    // stamped `max(now, n.c + 1)`, both unbounded, so the two actions a user
    // reaches for when something looks wrong each made the poisoning worse and
    // moved it into a table nothing prunes.
    let s = store();
    let now = now_ms();
    let poisoned = now + 3 * 86_400_000;
    for (origin, text) in [("poison", "from the bad old days"), ("other", "also old")] {
        s.conn_for_test()
            .execute(
                "INSERT INTO items (kind, text, created_at, updated_at, source_machine, origin_id)
                 VALUES ('clipboard', ?3, ?1, ?1, ?2, ?4)",
                rusqlite::params![poisoned, ME, text, origin],
            )
            .unwrap();
    }
    let ceiling = || now_ms() + MAX_CLOCK_SKEW_MS;

    // Delete one.
    let id: i64 = s
        .conn_for_test()
        .query_row("SELECT id FROM items WHERE origin_id='poison'", [], |r| r.get(0))
        .unwrap();
    s.delete_item_local(id).unwrap();
    let d: i64 = s
        .conn_for_test()
        .query_row("SELECT deleted_at FROM tombstones WHERE origin_id='poison'", [], |r| r.get(0))
        .unwrap();
    assert!(
        d <= ceiling(),
        "deleting a row stamped the tombstone at {d}, past what any peer accepts: the delete \
         never reaches the other machine and the poisoning is now in a table nothing prunes"
    );

    // And Clear the rest.
    s.clear(None).unwrap();
    let worst: i64 = s
        .conn_for_test()
        .query_row("SELECT MAX(deleted_at) FROM tombstones", [], |r| r.get(0))
        .unwrap();
    assert!(
        worst <= ceiling(),
        "Clear History stamped {worst}, past what any peer accepts: the user cleared their \
         history and the other machine never hears about it"
    );
}

// ===========================================================================
// R10-2. THE POISON IS REACHABLE FROM A PEER, NOT ONLY FROM OUR OWN CLOCK.
//
// A tombstone from a peer is accepted up to `now + MAX_CLOCK_SKEW_MS`, and a
// relayed delete for an identity we hold is accepted from ANY paired device.
// So a paired peer sets our own source's clock to our ceiling at will.
//
// This is the ratchet the brief asks about. It is BOUNDED: the ceiling is
// computed against the live wall clock, so the offset cannot grow past the skew
// window, and it does not compound across exchanges. That is the good news and
// this test pins it, so a future change that removes the bound is caught.
// ===========================================================================

#[test]
fn r10_a_paired_peer_can_ratchet_our_own_clock_but_only_to_the_skew_ceiling() {
    let a = store();
    let mut highest_offset = 0i64;

    for round in 0..25 {
        let id = a.insert_clipboard(&format!("row {round}"), None, None).unwrap();
        let (origin, _) = a.origin_and_text_for_test(id).unwrap();
        // The peer relays a delete of our row, stamped as high as we will take.
        let t = RemoteTombstone {
            source_machine: ME.into(),
            origin_id: origin,
            deleted_at: now_ms() + MAX_CLOCK_SKEW_MS,
        };
        a.apply_remote_tombstone(PEER, &t).unwrap();

        let next = a.insert_clipboard(&format!("after {round}"), None, None).unwrap();
        let offset = updated_at_of(&a, next) - now_ms();
        highest_offset = highest_offset.max(offset);
    }

    assert!(
        highest_offset >= MAX_CLOCK_SKEW_MS,
        "premise: the ratchet must actually have run; best offset was {highest_offset}"
    );
    // 25 rounds of +1 each plus the window itself. If this ever grows with the
    // round count the ratchet has become unbounded.
    assert!(
        highest_offset <= MAX_CLOCK_SKEW_MS + 1_000,
        "the ratchet compounded: {highest_offset} ms ahead after 25 rounds, ceiling is {}",
        MAX_CLOCK_SKEW_MS
    );
}

// ===========================================================================
// R10-3. THE `clear` CTE. Does the aggregate observe the rows the same
// statement inserts, is the GROUP BY right for a source present in only one
// table, and does a source with no history get a sane clock?
//
// Against the SQLite this crate actually bundles, not the one on the PATH.
// ===========================================================================

/// Put a row from `source` into the store without going through replication,
/// so a test can build any starting shape it likes.
fn seed_item(s: &Store, source: &str, origin: &str, clock: i64, pinned: bool) {
    s.conn_for_test()
        .execute(
            "INSERT INTO items (kind, text, created_at, updated_at, pinned, source_machine, origin_id)
             VALUES ('clipboard', ?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![format!("t-{origin}"), clock.min(now_ms()), clock, pinned as i64, source, origin],
        )
        .unwrap();
}

#[test]
fn r10_clear_stamps_exactly_one_clock_per_source_and_does_not_chain() {
    let s = store();
    // ME: three rows, newest at now-1000.
    let base = now_ms() - 100_000;
    seed_item(&s, ME, "m1", base + 10, false);
    seed_item(&s, ME, "m2", base + 11, false);
    seed_item(&s, ME, "m3", base + 12, false);
    // PEER: two rows plus a tombstone that is NEWER than either row, so the
    // union arm has to be consulted.
    seed_item(&s, PEER, "p1", base + 20, false);
    seed_item(&s, PEER, "p2", base + 21, false);
    s.conn_for_test()
        .execute(
            "INSERT INTO tombstones (source_machine, origin_id, deleted_at, local) VALUES (?1,'p0',?2,0)",
            rusqlite::params![PEER, base + 500],
        )
        .unwrap();
    // THIRD: tombstones only, no live rows at all.
    s.conn_for_test()
        .execute(
            "INSERT INTO tombstones (source_machine, origin_id, deleted_at, local) VALUES (?1,'x0',?2,0)",
            rusqlite::params![THIRD, base + 900],
        )
        .unwrap();

    s.clear(None).unwrap();

    let mine = clocks_for(&s, ME);
    let theirs: Vec<i64> = clocks_for(&s, PEER);
    assert_eq!(mine.len(), 3, "every unpinned row of ours must be tombstoned");
    // ONE clock per source, not a chain.
    let mine_distinct: std::collections::BTreeSet<i64> = mine.iter().copied().collect();
    assert_eq!(
        mine_distinct.len(),
        1,
        "a Clear must stamp ONE clock per source; got {} distinct values, first few {:?}",
        mine_distinct.len(),
        mine_distinct.iter().take(4).collect::<Vec<_>>()
    );
    // PEER has 3 tombstones now: p0 (pre-existing, moved up or not) plus p1/p2.
    let peer_new: std::collections::BTreeSet<i64> = theirs.iter().copied().collect();
    assert!(
        peer_new.len() <= 2,
        "PEER should hold at most the pre-existing clock plus one new one; got {:?}",
        peer_new
    );

    // And strictly above everything that source held before, including the
    // tombstone that outranked every row.
    let peer_stamp = *peer_new.iter().max().unwrap();
    assert!(
        peer_stamp > base + 500,
        "the Clear clock for PEER must beat its newest tombstone: {peer_stamp} vs {}",
        base + 500
    );

    // THIRD had no live rows, so nothing was inserted for it and its existing
    // tombstone is untouched.
    assert_eq!(clocks_for(&s, THIRD), vec![base + 900], "a source with no live rows must be left alone");
}

#[test]
fn r10_clear_never_stamps_below_a_future_clock_it_already_holds() {
    // The no-ceiling rule, applied to Clear: if the store already holds a clock
    // past the skew window for a source, the Clear must still beat it, or the
    // delete sits below the peer's cursor and never travels.
    let s = store();
    seed_item(&s, ME, "m1", now_ms() - 5_000, false);
    // (a) INSIDE the window: the Clear must beat what we already hold, so a
    //     peer sitting at that cursor still hears about the deletes.
    let held = now_ms() + MAX_CLOCK_SKEW_MS / 2;
    s.conn_for_test()
        .execute(
            "INSERT INTO tombstones (source_machine, origin_id, deleted_at, local) VALUES (?1,'m0',?2,1)",
            rusqlite::params![ME, held],
        )
        .unwrap();
    s.clear(None).unwrap();
    let inside = clocks_for(&s, ME).into_iter().max().unwrap();
    assert!(
        inside > held,
        "inside the window the Clear must beat the clock we already hold: {inside} vs {held}"
    );

    // (b) BEYOND the window: it must NOT keep climbing.
    //
    // This half changed, deliberately, and it is the trade round 10 forced. A
    // clock five skew-windows out cannot be protecting any correctly-clocked
    // peer's cursor, because that peer refused the rows that put it there. So
    // climbing above it buys nothing and costs everything: the deletes are
    // stamped where no peer will accept them, and Clear History becomes a
    // permanent no-op on the other machine that fixing the clock does not undo.
    //
    // Stamping at the ceiling is above every cursor that really exists and
    // inside what a correct peer accepts.
    let s2 = store();
    seed_item(&s2, ME, "a", now_ms() - 30_000, false);
    let poison = now_ms() + 5 * MAX_CLOCK_SKEW_MS;
    s2.conn_for_test()
        .execute(
            "INSERT INTO tombstones (source_machine, origin_id, deleted_at, local) VALUES (?1,'m0',?2,1)",
            rusqlite::params![ME, poison],
        )
        .unwrap();
    s2.clear(None).unwrap();
    let beyond = clocks_for(&s2, ME)
        .into_iter()
        .filter(|c| *c != poison)
        .max()
        .expect("the clear wrote a tombstone");
    assert!(
        beyond <= now_ms() + MAX_CLOCK_SKEW_MS,
        "the Clear chased a clock five windows out to {beyond}: no peer accepts that, so the \
         user cleared their history and the other machine never hears about it"
    );
    assert!(beyond > now_ms() - 1_000, "and it must not be stamped in the past: {beyond}");
}

#[test]
fn r10_a_kind_scoped_clear_beats_clocks_held_under_the_other_kind() {
    // The CTE deliberately has no kind filter. If it grew one, a Clear of
    // clipboard history could be stamped below a transcription row of the same
    // source, which is a delete the peer never asks for again.
    let s = store();
    let base = now_ms() - 50_000;
    seed_item(&s, ME, "c1", base, false);
    s.conn_for_test()
        .execute("UPDATE items SET kind='clipboard' WHERE origin_id='c1'", [])
        .unwrap();
    seed_item(&s, ME, "t1", base + 40_000, false);
    s.conn_for_test()
        .execute("UPDATE items SET kind='transcription' WHERE origin_id='t1'", [])
        .unwrap();

    s.clear(Some(HistoryKind::Clipboard)).unwrap();
    let stamped = clocks_for(&s, ME);
    assert_eq!(stamped.len(), 1, "only the clipboard row is tombstoned");
    assert!(
        stamped[0] > base + 40_000,
        "the clipboard Clear must beat the transcription row's clock: {} vs {}",
        stamped[0],
        base + 40_000
    );
}

#[test]
fn r10_clear_is_not_quadratic_and_the_plan_holds_no_correlated_scan() {
    // A plan assertion, not a stopwatch: the old form was O(N^2) because each
    // inserted row re-scanned `items`. If the CTE is ever materialised per row
    // the plan says CORRELATED.
    let s = store();
    let n = 20_000;
    {
        let conn = s.conn_for_test();
        conn.execute("BEGIN", []).unwrap();
        for i in 0..n {
            conn.execute(
                "INSERT INTO items (kind, text, created_at, updated_at, pinned, source_machine, origin_id)
                 VALUES ('clipboard', 'x', ?1, ?1, 0, ?2, ?3)",
                rusqlite::params![now_ms() - 1_000_000 + i, ME, format!("o{i}")],
            )
            .unwrap();
        }
        conn.execute("COMMIT", []).unwrap();
    }

    let plan: Vec<String> = {
        let conn = s.conn_for_test();
        let mut st = conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 WITH newest AS (
                     SELECT src, MAX(c) AS c FROM (
                         SELECT source_machine AS src, MAX(updated_at) AS c FROM items
                          WHERE source_machine IS NOT NULL GROUP BY source_machine
                         UNION ALL
                         SELECT source_machine AS src, MAX(deleted_at) AS c FROM tombstones
                          GROUP BY source_machine
                     ) GROUP BY src
                 )
                 INSERT INTO tombstones (source_machine, origin_id, deleted_at, local)
                 SELECT i.source_machine, i.origin_id, max(?1, COALESCE(n.c,0)+1), 1
                   FROM items i LEFT JOIN newest n ON n.src = i.source_machine
                  WHERE i.pinned=0 AND i.source_machine IS NOT NULL AND i.origin_id IS NOT NULL",
            )
            .unwrap();
        let v = st
            .query_map(rusqlite::params![now_ms()], |r| r.get::<_, String>(3))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    let correlated: Vec<&String> = plan.iter().filter(|l| l.contains("CORRELATED")).collect();
    assert!(
        correlated.is_empty(),
        "the Clear plan re-evaluates per row: {:?}",
        correlated
    );

    let t0 = std::time::Instant::now();
    let removed = s.clear(None).unwrap();
    let ms = t0.elapsed().as_millis();
    assert_eq!(removed, n as usize, "every row must be cleared");
    // Deliberately loose. This is a smoke bound against a return to O(N^2),
    // where 20,000 rows measured in the tens of seconds; it is not a
    // performance assertion and must not become one.
    assert!(ms < 30_000, "Clear of {n} rows took {ms} ms, which smells quadratic again");
    eprintln!("r10: clear of {n} rows took {ms} ms");
}

// ===========================================================================
// R10-4. THE `COALESCE(updated_at, created_at)` THAT WAS DROPPED.
//
// The claim is that `items.updated_at` is never NULL. The column has no
// NOT NULL constraint, so the claim rests entirely on every write path and
// every migration path. These two tests pin the claim, and the third shows
// what it costs if it ever stops being true.
// ===========================================================================

#[test]
fn r10_no_production_write_path_leaves_updated_at_null() {
    let s = store();
    let a = s.insert_clipboard("one", None, None).unwrap();
    let _ = s.insert_clipboard("one", None, None).unwrap(); // dedupe branch
    let b = s.insert_clipboard("two", Some("com.x"), Some("X")).unwrap();
    s.set_pinned(a, true).unwrap();
    s.update_text(b, "two edited").unwrap();
    // and the replication path, both the insert and the update arms
    let wire = RemoteItem {
        source_machine: PEER.into(),
        origin_id: "p1".into(),
        kind: "clipboard".into(),
        text: "from the peer".into(),
        created_at: now_ms() - 1_000,
        updated_at: now_ms() - 1_000,
        pinned: false,
    };
    s.apply_remote_item(PEER, &wire).unwrap();
    let mut newer = wire.clone();
    newer.updated_at += 5;
    newer.text = "corrected".into();
    s.apply_remote_item(PEER, &newer).unwrap();

    let nulls: i64 = s
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM items WHERE updated_at IS NULL", [], |r| r.get(0))
        .unwrap();
    let total: i64 =
        s.conn_for_test().query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)).unwrap();
    assert!(total >= 3, "premise: the paths under test must have written rows, got {total}");
    assert_eq!(nulls, 0, "a production write left updated_at NULL");
}

/// The v1 schema as shipped. A private copy, because `history::tests` is not
/// visible from here and this file must not edit that one.
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

#[test]
fn r10_migrating_from_v1_leaves_no_null_clock_behind() {
    let dir = std::env::temp_dir().join(format!("parle-r10-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v1.db");
    let _ = std::fs::remove_file(&path);
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(V1_SCHEMA).unwrap();
        c.execute(
            "INSERT INTO items (kind, text, created_at, pinned, source_machine)
             VALUES ('clipboard','legacy attributed',500,0,?1)",
            rusqlite::params![ME],
        )
        .unwrap();
        c.execute(
            "INSERT INTO items (kind, text, created_at) VALUES ('transcription','legacy orphan',600)",
            [],
        )
        .unwrap();
        c.pragma_update(None, "user_version", 1).unwrap();
    }
    let s = Store::open(&path).unwrap();
    let nulls: i64 = s
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM items WHERE updated_at IS NULL", [], |r| r.get(0))
        .unwrap();
    let total: i64 =
        s.conn_for_test().query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)).unwrap();
    assert_eq!(total, 2, "the migration must preserve every row");
    assert_eq!(nulls, 0, "the v3 backfill must leave no NULL clock");
    drop(s);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn r10_if_a_null_clock_ever_existed_the_next_local_write_would_go_backwards() {
    // Not reachable through any path in the tree today (the two tests above
    // pin that). Recorded because `items.updated_at` is still NULLABLE, three
    // other readers still hedge with COALESCE, and this is what a single NULL
    // would cost: the clock walks BACKWARDS past a row we still hold and still
    // serve, which is a permanently unservable row.
    let s = store();
    let base = now_ms() + 30_000; // inside the skew window
    seed_item(&s, ME, "keep", base, false);
    // One row of ours with no clock at all.
    seed_item(&s, ME, "hole", base + 5_000, false);
    s.conn_for_test()
        .execute("UPDATE items SET updated_at=NULL WHERE origin_id='hole'", [])
        .unwrap();

    let id = s.insert_clipboard("next", None, None).unwrap();
    let stamped = updated_at_of(&s, id);
    assert!(
        stamped > base,
        "sanity: the visible clock is still respected ({stamped} vs {base})"
    );
    // The point: `MAX(updated_at)` silently skipped the NULL row, so the new
    // row's clock is BELOW the one the NULL row would have carried.
    assert!(
        stamped < base + 5_000,
        "premise failed: MAX(updated_at) apparently did see the NULL row"
    );
}

// ===========================================================================
// R10-5. THE ROUND-9 PROPERTIES THAT WERE SAID TO HOLD. Verified here rather
// than trusted.
// ===========================================================================

#[test]
fn r10_the_pair_cursor_takes_a_lexicographic_max_and_never_walks_back() {
    let s = store();
    s.note_received_at(PEER, PEER, 100, "b").unwrap();
    let read = || s.watermarks_paired(PEER).unwrap().into_iter().next().unwrap();
    assert_eq!((read().1, read().2), (100, "b".to_string()));

    // A lower clock with a higher origin must not move either field.
    s.note_received_at(PEER, PEER, 50, "zzz").unwrap();
    assert_eq!((read().1, read().2), (100, "b".to_string()), "a lower clock moved the cursor");

    // Same clock, lower origin: no move.
    s.note_received_at(PEER, PEER, 100, "a").unwrap();
    assert_eq!((read().1, read().2), (100, "b".to_string()), "a lower origin moved the cursor");

    // Same clock, higher origin: the origin advances, the clock does not.
    s.note_received_at(PEER, PEER, 100, "c").unwrap();
    assert_eq!((read().1, read().2), (100, "c".to_string()), "a higher origin must advance");

    // Higher clock with a lower origin: both take the new value, or the rest of
    // the new millisecond is stranded below the cursor.
    s.note_received_at(PEER, PEER, 101, "a").unwrap();
    assert_eq!((read().1, read().2), (101, "a".to_string()), "a higher clock must reset the origin");

    // Out of range: refused entirely, so nothing can park the cursor.
    s.note_received_at(PEER, PEER, now_ms() + 10 * MAX_CLOCK_SKEW_MS, "zzz").unwrap();
    assert_eq!((read().1, read().2), (101, "a".to_string()), "an out-of-range clock parked the cursor");
}

#[test]
fn r10_origin_ceiling_outranks_every_origin_id_the_wire_can_carry() {
    // The wire cap is 128 bytes. The worst case a peer can mint is 32 copies of
    // U+10FFFF (4 bytes each), which is byte-for-byte the largest valid UTF-8
    // sequence there is.
    const WIRE_CAP: usize = 128;
    let worst = "\u{10FFFF}".repeat(WIRE_CAP / 4);
    assert_eq!(worst.len(), WIRE_CAP, "premise: the worst case must fill the cap exactly");
    assert!(
        worst.as_str() < ORIGIN_CEILING,
        "a peer can mint an origin at or above ORIGIN_CEILING"
    );
    assert!(ORIGIN_CEILING.len() > WIRE_CAP, "the ceiling must be longer than anything sendable");

    // And SQLite must agree with Rust, because `items_from` does the comparison
    // in SQL and `mark_received_in` does it in SQL too.
    let s = store();
    let agrees: i64 = s
        .conn_for_test()
        .query_row("SELECT ?1 < ?2", rusqlite::params![worst, ORIGIN_CEILING], |r| r.get(0))
        .unwrap();
    assert_eq!(agrees, 1, "SQLite orders the ceiling differently from Rust");

    // Which is what makes items_since exclude the whole millisecond.
    seed_item(&s, ME, &worst, 1_000, false);
    assert!(
        s.items_since(ME, 1_000, 50).unwrap().is_empty(),
        "items_since must exclude every origin sharing the cursor's millisecond"
    );
    assert_eq!(
        s.items_from(ME, 1_000, "", 50).unwrap().len(),
        1,
        "and items_from with an empty origin must still find it"
    );
}

#[test]
fn r10_a_receipt_is_atomic_with_the_row_and_a_refusal_banks_nothing() {
    let s = store();
    // Accepted: row AND receipt land.
    let good = RemoteItem {
        source_machine: PEER.into(),
        origin_id: "g1".into(),
        kind: "clipboard".into(),
        text: "fine".into(),
        created_at: now_ms() - 10,
        updated_at: now_ms() - 10,
        pinned: false,
    };
    s.apply_remote_item(PEER, &good).unwrap();
    assert!(s.holds_identity(PEER, "g1").unwrap());
    assert_eq!(s.watermarks(PEER).unwrap().len(), 1, "an accepted row must bank a receipt");

    // Refused for a future clock: neither.
    let bad = RemoteItem {
        origin_id: "b1".into(),
        updated_at: now_ms() + 10 * MAX_CLOCK_SKEW_MS,
        ..good.clone()
    };
    s.apply_remote_item(PEER, &bad).unwrap();
    assert!(!s.holds_identity(PEER, "b1").unwrap(), "a future row must not be stored");
    let mark = s.watermarks(PEER).unwrap()[0].1;
    assert_eq!(mark, good.updated_at, "a refused row moved the cursor to {mark}");

    // Refused for a non-positive clock: the same.
    let zero = RemoteItem { origin_id: "z1".into(), updated_at: 0, created_at: 0, ..good.clone() };
    s.apply_remote_item(PEER, &zero).unwrap();
    assert!(!s.holds_identity(PEER, "z1").unwrap());
    assert_eq!(s.watermarks(PEER).unwrap()[0].1, good.updated_at);
}

#[test]
fn r10_the_excluded_app_filter_is_applied_inside_the_page_not_after_it() {
    let mut s = store();
    // Ten rows, alternating between an excluded app and a kept one.
    for i in 0..10 {
        let app = if i % 2 == 0 { "com.Excluded" } else { "com.kept" };
        s.insert_clipboard(&format!("row {i}"), Some(app), Some(app)).unwrap();
    }
    s.set_excluded_apps(vec!["com.excluded".into()]);
    // A limit of 5 must return 5 KEPT rows, not 5 rows of which 2 survive.
    let page = s.items_from(ME, 0, "", 5).unwrap();
    assert_eq!(page.len(), 5, "the LIMIT must count rows that will actually be sent");
    assert!(
        page.iter().all(|r| !r.text.contains("row 0")),
        "an excluded row reached the page"
    );
    // Total across the whole source: exactly the five kept ones.
    let all = s.items_from(ME, 0, "", 100).unwrap();
    assert_eq!(all.len(), 5, "the excluded app's rows must never be offered");

    // app_name is filtered as well as app_id, and both are ASCII-folded.
    s.set_excluded_apps(vec!["com.kept".into()]);
    let all = s.items_from(ME, 0, "", 100).unwrap();
    assert_eq!(all.len(), 5, "swapping the exclusion must swap which half is offered");
}

// ===========================================================================
// R10-6. CRITERION D. Peer-controlled values at the extremes.
// ===========================================================================

#[test]
fn r10_extreme_peer_clocks_never_panic_or_overflow() {
    let s = store();
    for clock in [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX] {
        let it = RemoteItem {
            source_machine: PEER.into(),
            origin_id: format!("o{clock}"),
            kind: "clipboard".into(),
            text: "x".into(),
            created_at: clock,
            updated_at: clock,
            pinned: false,
        };
        // Must not panic and must not store anything out of range.
        let _ = s.apply_remote_item(PEER, &it).unwrap();
        let t = RemoteTombstone {
            source_machine: PEER.into(),
            origin_id: format!("t{clock}"),
            deleted_at: clock,
        };
        let _ = s.apply_remote_tombstone(PEER, &t).unwrap();
    }
    let stored: i64 = s
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM items WHERE updated_at <= 0 OR updated_at > ?1",
            rusqlite::params![now_ms() + MAX_CLOCK_SKEW_MS],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, 0, "an out-of-range clock was stored");
    let marks = s.watermarks(PEER).unwrap();
    assert!(
        marks.iter().all(|(_, c)| *c > 0 && *c <= now_ms() + MAX_CLOCK_SKEW_MS),
        "an out-of-range clock reached the cursor table"
    );

    // And the local clock must survive whatever did get stored.
    let id = s.insert_clipboard("still writable", None, None).unwrap();
    assert!(updated_at_of(&s, id) > 0, "the local clock overflowed");
}

#[test]
fn r10_a_saturating_clock_would_stop_being_strictly_above_itself() {
    // The saturation edge is now unreachable, and the ceiling clamp is why.
    //
    // `newest.saturating_add(1)` at `i64::MAX` returns `i64::MAX`, so "strictly
    // above" would quietly become "equal to" and two rows could share a clock.
    // The clamp removes the question: whatever `newest` is, the result is at
    // most `now + MAX_CLOCK_SKEW_MS`, which is nowhere near the saturation
    // point, so the `+1` never has to carry the invariant on its own.
    let s = store();
    s.conn_for_test()
        .execute(
            "INSERT INTO items (kind, text, created_at, updated_at, source_machine, origin_id)
             VALUES ('clipboard', 'x', ?1, ?1, ?2, 'max')",
            rusqlite::params![i64::MAX, ME],
        )
        .unwrap();

    let id = s.insert_clipboard("after", None, None).unwrap();
    let clock: i64 = s
        .conn_for_test()
        .query_row("SELECT updated_at FROM items WHERE id=?1", rusqlite::params![id], |r| r.get(0))
        .unwrap();
    assert!(
        clock < i64::MAX,
        "a row we hold at i64::MAX made the next local write saturate to the same value, so two \
         rows share a clock and one of them is unreachable to paging"
    );
    assert!(
        clock <= now_ms() + MAX_CLOCK_SKEW_MS,
        "and the clamp must hold even against an absurd stored value: {clock}"
    );
}

// ===========================================================================
// R10-7. CRITERION B. MIGRATIONS, INCLUDING WHEN INTERRUPTED.
//
// The migration now runs in ONE transaction, so a database this build writes
// can never have a version stamp behind its schema. A database damaged by the
// build BEFORE that fix can, and the guards exist precisely so those still
// open. This walks every stamp from 0 to 7 over a full v7 schema and asserts
// the store opens, keeps every row, and lands on the same schema.
// ===========================================================================

/// Everything that describes the shape of the database, ordered, so two of
/// them can be compared directly.
fn schema_of(conn: &rusqlite::Connection) -> Vec<String> {
    let mut st = conn
        .prepare(
            "SELECT type || ' ' || name || ' :: ' || COALESCE(sql,'')
               FROM sqlite_master ORDER BY type, name",
        )
        .unwrap();
    let v: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    v
}

fn seed_a_full_store(path: &std::path::Path) {
    let mut s = Store::open(path).unwrap();
    s.set_device_id(ME);
    s.insert_clipboard("one", None, None).unwrap();
    s.insert_clipboard("two", None, None).unwrap();
    let id = s.insert_clipboard("three", None, None).unwrap();
    s.set_pinned(id, true).unwrap();
    s.apply_remote_item(
        PEER,
        &RemoteItem {
            source_machine: PEER.into(),
            origin_id: "p1".into(),
            kind: "clipboard".into(),
            text: "from the laptop".into(),
            created_at: now_ms() - 1_000,
            updated_at: now_ms() - 1_000,
            pinned: false,
        },
    )
    .unwrap();
    s.apply_remote_tombstone(
        PEER,
        &RemoteTombstone {
            source_machine: PEER.into(),
            origin_id: "p2".into(),
            deleted_at: now_ms() - 500,
        },
    )
    .unwrap();
    s.dict_upsert("parle", &["parlay".into()], false).unwrap();
}

#[test]
fn r10_a_version_stamp_behind_the_schema_still_opens_and_keeps_every_row() {
    let dir = std::env::temp_dir().join(format!("parle-r10-mig-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // The reference: a store built by this code, untouched.
    let reference_path = dir.join("reference.db");
    let _ = std::fs::remove_file(&reference_path);
    seed_a_full_store(&reference_path);
    let (reference_schema, reference_rows) = {
        let s = Store::open(&reference_path).unwrap();
        let rows: i64 =
            s.conn_for_test().query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)).unwrap();
        (schema_of(s.conn_for_test()), rows)
    };
    assert_eq!(reference_rows, 4, "premise: the seed must have written four rows");

    let mut failures: Vec<String> = Vec::new();
    for stamp in 0..=7i64 {
        let path = dir.join(format!("stamp{stamp}.db"));
        let _ = std::fs::remove_file(&path);
        std::fs::copy(&reference_path, &path).unwrap();
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", stamp).unwrap();
        }
        match Store::open(&path) {
            Err(e) => failures.push(format!("stamp {stamp}: open failed: {e}")),
            Ok(s) => {
                let rows: i64 = s
                    .conn_for_test()
                    .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
                    .unwrap();
                if rows != reference_rows {
                    failures.push(format!("stamp {stamp}: {rows} rows, expected {reference_rows}"));
                }
                let got = schema_of(s.conn_for_test());
                if got != reference_schema {
                    let diff: Vec<&String> =
                        got.iter().filter(|l| !reference_schema.contains(l)).collect();
                    failures.push(format!("stamp {stamp}: schema differs, e.g. {:?}", diff.first()));
                }
                let nulls: i64 = s
                    .conn_for_test()
                    .query_row("SELECT COUNT(*) FROM items WHERE updated_at IS NULL", [], |r| {
                        r.get(0)
                    })
                    .unwrap();
                if nulls != 0 {
                    failures.push(format!("stamp {stamp}: {nulls} rows lost their clock"));
                }
            }
        }
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::remove_file(&reference_path);
    assert!(
        failures.is_empty(),
        "{} of 8 version stamps did not survive re-opening; first few: {:?}",
        failures.len(),
        failures.iter().take(8).collect::<Vec<_>>()
    );
}

/// The same defect, constructed the way it actually happens rather than by
/// setting a pragma on a finished store.
///
/// The build BEFORE the migration became one transaction ran every step
/// un-transacted and stamped `user_version` only at the very end. A crash or a
/// force quit after the v5 `CREATE TABLE source_marks` therefore leaves the
/// v5-shaped table on disk with the stamp still where it started. Every ALTER
/// in `init` is guarded against exactly that. The v4 seed INSERT is not.
#[test]
fn r10_a_pre_transaction_crash_at_the_v5_step_makes_the_database_unopenable() {
    let dir = std::env::temp_dir().join(format!("parle-r10-v5crash-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("crashed.db");
    let _ = std::fs::remove_file(&path);
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(V1_SCHEMA).unwrap();
        c.execute(
            "INSERT INTO items (kind, text, created_at) VALUES ('clipboard','a password',10)",
            [],
        )
        .unwrap();
        c.pragma_update(None, "user_version", 1i64).unwrap();
        // What the old build had autocommitted by the time it died: the v3
        // columns and the v5-shaped receipts table. The stamp is still 1.
        c.execute_batch(
            "ALTER TABLE items ADD COLUMN origin_id TEXT;
             ALTER TABLE items ADD COLUMN updated_at INTEGER;
             CREATE TABLE tombstones (
                 source_machine TEXT NOT NULL,
                 origin_id TEXT NOT NULL,
                 deleted_at INTEGER NOT NULL,
                 PRIMARY KEY (source_machine, origin_id)
             );
             CREATE TABLE source_marks (
                 peer_machine   TEXT NOT NULL,
                 source_machine TEXT NOT NULL,
                 received_clock INTEGER NOT NULL,
                 PRIMARY KEY (peer_machine, source_machine)
             );",
        )
        .unwrap();
    }
    let reopened = Store::open(&path);
    let err = reopened.as_ref().err().map(|e| e.to_string());
    let _ = std::fs::remove_file(&path);
    assert!(
        reopened.is_ok(),
        "an interrupted upgrade must be recoverable, exactly as the v1, v2, v3 and v6 steps \
         already are. The v4 seed INSERT is unguarded: {err:?}"
    );
}

#[test]
fn r10_rewinding_the_stamp_to_v4_destroys_every_receipt() {
    // The milder half of the same problem, and the reason the v4 seed cannot
    // simply be deleted: v5 DROPs `source_marks` unconditionally. A stamp of 4
    // on a live store therefore throws away every cursor, so every peer
    // re-offers its whole history. Idempotent, but it is a full resync, and it
    // is worth knowing it is what a stale stamp costs.
    let dir = std::env::temp_dir().join(format!("parle-r10-v4-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v4.db");
    let _ = std::fs::remove_file(&path);
    {
        let mut s = Store::open(&path).unwrap();
        s.set_device_id(ME);
        s.note_received_at(PEER, PEER, now_ms() - 1_000, "p1").unwrap();
        assert_eq!(s.watermarks(PEER).unwrap().len(), 1, "premise: one receipt must exist");
    }
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.pragma_update(None, "user_version", 4i64).unwrap();
    }
    let s = Store::open(&path).unwrap();
    let left = s.watermarks(PEER).unwrap().len();
    drop(s);
    let _ = std::fs::remove_file(&path);
    assert_eq!(left, 0, "documenting the cost: a stamp of 4 wipes the receipts");
}
