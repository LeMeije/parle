//! ADVERSARIAL REVIEW, ROUND 11. Data integrity, clocks and replication
//! correctness, store side.
//!
//! Round 10's fixes are the target, in this order:
//!   * `next_clock_impl` now clamps the CEILING:
//!     `now.max(newest.saturating_add(1).min(now + MAX_CLOCK_SKEW_MS))`.
//!   * `Store::clear` applies the same clamp in SQL.
//!   * `Settings::migrate` unions the stored exclusion list with the defaults.
//!   * The v4 seed is guarded by `has_col("source_marks", "peer_machine")`.
//!
//! Everything here is bounded, in-memory or a temp file, and opens no socket.

#![cfg(test)]

use crate::history::{Store, MAX_CLOCK_SKEW_MS, ORIGIN_CEILING};
use rusqlite::params;

const ME: &str = "11111111-1111-4111-8111-111111111111";
const PEER: &str = "22222222-2222-4222-8222-222222222222";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}

fn store() -> Store {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(ME);
    s
}

/// One hour. Chosen because it is the size of the two backwards steps that
/// actually happen to people: a Windows/Linux dual boot disagreeing about
/// whether the RTC is local time or UTC, and a VM or laptop resumed into a
/// clock that had been running fast and is corrected on the way back.
const STEP_BACK_MS: i64 = 60 * 60 * 1000;

/// Put this machine's own history into the state a BACKWARDS wall-clock step
/// leaves behind, and return the pair cursor a peer legitimately holds for us.
///
/// The sequence in ordinary terms:
///
///   1. Both machines agree about the time. This one dictates something. The
///      row is stamped `T`, which is the wall clock, and is synced across.
///   2. The peer records what it received: its cursor for (us, our own source)
///      is exactly that row's `(updated_at, origin_id)`. Nothing is invented
///      here — the pair is read back off the row itself, which is precisely
///      what `mark_received_in` stores for a row `serve` has just sent.
///   3. This machine's clock steps back an hour.
///
/// Only step 3 needs help from the test, and only because `now_ms()` cannot be
/// moved from inside the process: the row is written through the ordinary local
/// path and then its clock is lifted to where the wall clock used to be, which
/// is the same modelling `adversarial_r10_data::poison_own_clock` uses.
fn backwards_step(s: &Store) -> (i64, String, i64) {
    let id = s.insert_clipboard("dictated before the clock stepped back", None, None).unwrap();
    let (origin, _) = s.origin_and_text_for_test(id).unwrap();
    let t_high = now_ms() + STEP_BACK_MS;
    s.conn_for_test()
        .execute("UPDATE items SET created_at=?1, updated_at=?1 WHERE id=?2", params![t_high, id])
        .unwrap();
    // The premise, asserted rather than assumed: this is a row we would really
    // have served, so a peer really can be holding this cursor.
    let servable = s.items_from(ME, 0, "", 64).unwrap();
    assert!(
        servable.iter().any(|r| r.origin_id == origin),
        "premise: the pre-step row must be one this machine would actually serve"
    );
    assert!(
        t_high > now_ms() + MAX_CLOCK_SKEW_MS,
        "premise: the step must be larger than the accepted skew window"
    );
    (t_high, origin, id)
}

// ===========================================================================
// R11-1. THE CEILING CLAMP ASSUMES EVERY HIGH CLOCK WE HOLD WAS REFUSED BY THE
// PEER. AFTER A BACKWARDS CLOCK STEP IT WAS RECEIVED.
//
// Round 10's argument, quoted from `next_clock_impl`:
//
//   "A peer only ever banks a cursor at or below its OWN `now + skew`, so a
//    `newest` above our ceiling cannot be protecting any correctly-clocked
//    peer's cursor: those rows were refused, not received."
//
// The first half is true. The conclusion does not follow, because the cursor
// is compared against the PEER's clock at the moment it banked, and our ceiling
// against OUR clock now. Those are the same number only while our clock does
// not move backwards.
//
// Forward drift on our side: we stamp above what the peer accepts, the peer
// refuses, no cursor moves, and the clamp is exactly right. That is round 10's
// case and it still works.
//
// Backwards step on our side: the high clock was stamped when the clocks
// AGREED, so the peer accepted those rows and its cursor sits up there
// legitimately. The clamp then stamps every later row and every later delete
// BELOW that cursor, and nothing lowers a cursor.
// ===========================================================================

// ===========================================================================
// WHERE THE FIX LANDED, AND WHY THESE THREE MODEL IT RATHER THAN CALL IT.
//
// These three drive `items_from` / `tombstones_from` with the peer's cursor,
// which is exactly what `serve` did when they were written. The fix is in
// `serve` itself and cannot be reached from this crate: `serve` now refuses to
// TRUST a cursor above `now + MAX_CLOCK_SKEW_MS`, because a mark this machine's
// clock cannot reach is one it can never serve above, and offers that source
// from the beginning instead.
//
// Detecting it on the peer's advertised mark rather than on our own history is
// the load-bearing detail, and the first attempt got it wrong: looking for a
// high clock in our OWN store fails on precisely the case that matters, because
// deleting the row removes it and the evidence is destroyed by the very delete
// being propagated.
//
// So the helper below applies the same rule the production `serve` applies. The
// versions that exercise the real code path live in
// `src-tauri/src/sync/adversarial_r11_data.rs` and run over real sockets.
// ===========================================================================

/// The cursor `serve` would actually use: the peer's mark, unless it is one we
/// could never reach, in which case the beginning.
fn cursor_serve_would_use(clock: i64, origin: &str) -> (i64, String) {
    if clock > now_ms() + MAX_CLOCK_SKEW_MS {
        (0, String::new())
    } else {
        (clock, origin.to_string())
    }
}

#[test]
fn r11_a_row_written_after_a_backwards_clock_step_is_never_offered_again() {
    let s = store();
    let (cursor_clock, cursor_origin, _) = backwards_step(&s);

    // The user carries on working. Nothing about this is unusual.
    let id = s.insert_clipboard("dictated after the clock stepped back", None, None).unwrap();
    let (origin, _) = s.origin_and_text_for_test(id).unwrap();

    let (from, from_origin) = cursor_serve_would_use(cursor_clock, &cursor_origin);
    let page = s.items_from(ME, from, &from_origin, 64).unwrap();
    let visible = page.iter().any(|r| r.origin_id == origin);
    assert!(
        visible,
        "a row written after a backwards clock step is stamped below the cursor the peer \
         already holds, so it is never offered: {} of our rows are visible from that cursor",
        page.len()
    );
}

#[test]
fn r11_a_delete_made_after_a_backwards_clock_step_never_reaches_the_peer() {
    // The one failure the design says it must not have: the user deletes a
    // password on this machine and it stays on the other one for ever.
    let s = store();
    let (cursor_clock, cursor_origin, id) = backwards_step(&s);

    // Delete the very row the peer is holding.
    s.delete(id).unwrap();
    assert_eq!(
        s.tombstone_count(ME).unwrap(),
        1,
        "premise: the delete must have written a tombstone at all"
    );

    let (from, from_origin) = cursor_serve_would_use(cursor_clock, &cursor_origin);
    let offered = s.tombstones_from(ME, from, &from_origin, 64).unwrap();
    assert!(
        !offered.is_empty(),
        "the tombstone for a row the peer holds is stamped below the peer's cursor, so the \
         delete is never offered: the deleted row lives on the other machine for ever"
    );
}

#[test]
fn r11_clear_history_after_a_backwards_clock_step_is_a_silent_no_op_on_the_peer() {
    // Clear History is the product's panic button. Same clamp, same result.
    let s = store();
    let (cursor_clock, cursor_origin, _) = backwards_step(&s);
    s.insert_clipboard("and another", None, None).unwrap();

    let cleared = s.clear(None).unwrap();
    assert!(cleared >= 2, "premise: the clear must actually have removed rows, removed {cleared}");
    let n = s.tombstone_count(ME).unwrap();
    assert!(n >= 2, "premise: the clear must have written tombstones, wrote {n}");

    let (from, from_origin) = cursor_serve_would_use(cursor_clock, &cursor_origin);
    let offered = s.tombstones_from(ME, from, &from_origin, 64).unwrap();
    assert_eq!(
        offered.len(),
        n as usize,
        "Clear History wrote {n} tombstones and only {} of them are above the peer's cursor: \
         the user cleared their history and the other machine never hears about it",
        offered.len()
    );
}

// ===========================================================================
// R11-2. WHERE EXACTLY THE "STRICTLY ABOVE" PROMISE STOPS HOLDING, AND WHETHER
// THE RUST AND THE SQL AGREE THERE.
//
// `next_clock_for` documents "strictly above every clock we already hold for
// that source". The clamp breaks that promise at and above the ceiling. This
// maps the boundary rather than arguing about it, and pins that `clear` and
// `delete_item_local` break it in the SAME place — a disagreement between them
// would be a second, independent defect.
// ===========================================================================

/// The clock `delete_item_local` stamps when the newest thing we hold for our
/// own source is `newest`.
fn delete_stamp_when_newest_is(newest: i64) -> (i64, i64, i64) {
    let s = store();
    // A row we hold at `newest`, and a second row to delete.
    s.conn_for_test()
        .execute(
            "INSERT INTO items (kind, text, created_at, updated_at, pinned, source_machine, origin_id)
             VALUES ('clipboard', 'held', 1, ?1, 0, ?2, 'held')",
            params![newest, ME],
        )
        .unwrap();
    s.conn_for_test()
        .execute(
            "INSERT INTO items (kind, text, created_at, updated_at, pinned, source_machine, origin_id)
             VALUES ('clipboard', 'doomed', 1, 1, 0, ?1, 'doomed')",
            params![ME],
        )
        .unwrap();
    let id: i64 = s
        .conn_for_test()
        .query_row("SELECT id FROM items WHERE origin_id='doomed'", [], |r| r.get(0))
        .unwrap();
    let lo = now_ms();
    s.delete_item_local(id).unwrap();
    let hi = now_ms();
    let stamp: i64 = s
        .conn_for_test()
        .query_row("SELECT deleted_at FROM tombstones WHERE origin_id='doomed'", [], |r| r.get(0))
        .unwrap();
    (stamp, lo, hi)
}

/// The clock `clear` stamps in the same situation.
fn clear_stamp_when_newest_is(newest: i64) -> (i64, i64, i64) {
    let s = store();
    s.conn_for_test()
        .execute(
            "INSERT INTO items (kind, text, created_at, updated_at, pinned, source_machine, origin_id)
             VALUES ('clipboard', 'held', 1, ?1, 1, ?2, 'held')",
            params![newest, ME],
        )
        .unwrap();
    s.conn_for_test()
        .execute(
            "INSERT INTO items (kind, text, created_at, updated_at, pinned, source_machine, origin_id)
             VALUES ('clipboard', 'doomed', 1, 1, 0, ?1, 'doomed')",
            params![ME],
        )
        .unwrap();
    let lo = now_ms();
    s.clear(None).unwrap();
    let hi = now_ms();
    let stamp: i64 = s
        .conn_for_test()
        .query_row("SELECT deleted_at FROM tombstones WHERE origin_id='doomed'", [], |r| r.get(0))
        .unwrap();
    (stamp, lo, hi)
}

/// `max(now, min(newest + 1, now + MAX_CLOCK_SKEW_MS))`, which is monotone
/// non-decreasing in `now`, so bracketing `now` brackets the answer. That is
/// what makes this a deterministic assertion and not a race against the
/// millisecond tick.
fn expected(newest: i64, now: i64) -> i64 {
    now.max(newest.saturating_add(1).min(now.saturating_add(MAX_CLOCK_SKEW_MS)))
}

#[test]
fn r11_the_clear_sql_and_the_rust_clamp_agree_at_every_boundary() {
    let base = now_ms();
    let cases: Vec<(&str, i64)> = vec![
        ("nothing held", 0),
        ("well behind", base - 10_000),
        ("one below now", base - 1),
        ("two below the ceiling", base + MAX_CLOCK_SKEW_MS - 2),
        ("one below the ceiling", base + MAX_CLOCK_SKEW_MS - 1),
        ("exactly the ceiling", base + MAX_CLOCK_SKEW_MS),
        ("one past the ceiling", base + MAX_CLOCK_SKEW_MS + 1),
        ("an hour past the ceiling", base + STEP_BACK_MS),
    ];
    let mut mismatches: Vec<String> = Vec::new();
    for (name, newest) in &cases {
        let (d, dlo, dhi) = delete_stamp_when_newest_is(*newest);
        let (c, clo, chi) = clear_stamp_when_newest_is(*newest);
        if d < expected(*newest, dlo) || d > expected(*newest, dhi) {
            mismatches.push(format!("delete/{name}: {d}"));
        }
        if c < expected(*newest, clo) || c > expected(*newest, chi) {
            mismatches.push(format!("clear/{name}: {c}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "the SQL and the Rust clamp disagree: {:?}",
        &mismatches[..mismatches.len().min(4)]
    );
}

#[test]
fn r11_diag_where_the_strictly_above_promise_stops_holding() {
    // A map, not a verdict. `next_clock_for` promises "strictly above every
    // clock we already hold for that source"; this records which side of the
    // ceiling that promise survives on.
    //
    // Deliberately no offsets within a millisecond of the ceiling. `now_ms()`
    // moves between building the fixture and stamping the delete, so an offset
    // of exactly MAX_CLOCK_SKEW_MS lands on either side depending on how loaded
    // the machine is — a guard that passes alone and fails under load, which
    // this project has already paid for twice. A margin of a full second either
    // way makes the answer a property of the formula, not of the scheduler.
    const MARGIN: i64 = 1_000;
    let mut broken: Vec<i64> = Vec::new();
    for offset in [-MARGIN, 0, MAX_CLOCK_SKEW_MS - MARGIN, MAX_CLOCK_SKEW_MS + MARGIN, STEP_BACK_MS]
    {
        let newest = now_ms() + offset;
        let (stamp, _, _) = delete_stamp_when_newest_is(newest);
        if stamp <= newest {
            broken.push(offset);
        }
    }
    // The contract the code actually keeps today: strictness survives
    // comfortably below the ceiling and is given up comfortably above it.
    assert_eq!(
        broken,
        vec![MAX_CLOCK_SKEW_MS + MARGIN, STEP_BACK_MS],
        "the boundary where a local delete stops being stamped strictly above what we hold \
         has moved; offsets from now at which it is not strictly above: {broken:?}"
    );
}

// ===========================================================================
// R11-3. THE MIGRATION SWEEP THE BRIEF ASKS FOR: a database stamped at N
// carrying the schema of N+1 or later, for every N from 0 to 6.
//
// This is what an OLD build left behind whenever it was interrupted: it stamped
// `user_version` only at the very end, so any crash after a step and before the
// stamp produces exactly this shape. Criterion B: every row survives and the
// schema matches a fresh v7.
// ===========================================================================

fn schema_of(s: &Store) -> Vec<String> {
    let conn = s.conn_for_test();
    let mut st = conn
        .prepare(
            "SELECT type || ' ' || name || ' ' || COALESCE(sql, '') FROM sqlite_master
              ORDER BY type, name",
        )
        .unwrap();
    let v: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    v
}

fn columns_of(s: &Store, table: &str) -> Vec<String> {
    let conn = s.conn_for_test();
    let mut st = conn.prepare(&format!("PRAGMA table_info({table})")).unwrap();
    let v: Vec<String> = st.query_map([], |r| r.get(1)).unwrap().map(|r| r.unwrap()).collect();
    v
}

#[test]
fn r11_a_stamp_behind_a_full_v7_schema_opens_at_every_level_and_keeps_every_row() {
    let fresh = Store::open_in_memory().unwrap();
    let want_schema = schema_of(&fresh);

    let mut failures: Vec<String> = Vec::new();
    for stamp in 0..=6i64 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        // Build a real, fully migrated v7 store with content in every table.
        {
            let mut s = Store::open(&path).unwrap();
            s.set_device_id(ME);
            let a = s.insert_clipboard("kept one", None, None).unwrap();
            s.insert_clipboard("kept two", None, None).unwrap();
            s.delete(a).unwrap();
            s.note_received_at(PEER, PEER, now_ms() - 5_000, "o1").unwrap();
        }
        // Rewind the stamp, leaving the schema where it is.
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", stamp).unwrap();
        }
        let reopened = match Store::open(&path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("stamp {stamp}: would not open: {e}"));
                continue;
            }
        };
        if reopened.count().unwrap() != 1 {
            failures.push(format!("stamp {stamp}: {} rows, wanted 1", reopened.count().unwrap()));
        }
        if reopened.tombstone_count(ME).unwrap() != 1 {
            failures.push(format!("stamp {stamp}: the tombstone did not survive"));
        }
        if schema_of(&reopened) != want_schema {
            failures.push(format!("stamp {stamp}: schema differs from a fresh v7"));
        }
        let v: i64 =
            reopened.conn_for_test().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        if v != Store::SCHEMA_VERSION_FOR_TEST {
            failures.push(format!("stamp {stamp}: left stamped at {v}"));
        }
    }
    assert!(failures.is_empty(), "{:?}", &failures[..failures.len().min(5)]);
}

#[test]
fn r11_the_v4_seed_guard_is_the_only_statement_that_needed_one() {
    // The brief asks whether any OTHER statement in `Store::init` fails against
    // a partially migrated schema. The sweep above answers it for a full v7
    // schema at every stamp; this covers the shapes an interrupted upgrade
    // leaves that the sweep cannot build, where a table is at one version and
    // the stamp names another.
    let mut failures: Vec<String> = Vec::new();

    // (a) v5's source_marks with a v3 stamp: the v4 seed's ON CONFLICT names a
    //     constraint this shape does not have, so an unguarded seed fails at
    //     PREPARE time and the app never starts.
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        {
            let mut s = Store::open(&path).unwrap();
            s.set_device_id(ME);
            s.insert_clipboard("row", None, None).unwrap();
        }
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            // Back to the exact v5 shape, then rewind the stamp to 3.
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
            c.pragma_update(None, "user_version", 3i64).unwrap();
        }
        match Store::open(&path) {
            Ok(s) => {
                if !columns_of(&s, "source_marks").contains(&"received_origin".to_string()) {
                    failures.push("a: received_origin missing after the v5-shaped repair".into());
                }
                if s.count().unwrap() != 1 {
                    failures.push("a: the row did not survive".into());
                }
            }
            Err(e) => failures.push(format!("a: would not open: {e}")),
        }
    }

    // (b) The v4 shape with a v3 stamp: the seed DOES run here, and must be a
    //     no-op rather than a duplicate-key failure.
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.db");
        {
            let mut s = Store::open(&path).unwrap();
            s.set_device_id(ME);
            s.insert_clipboard("row", None, None).unwrap();
        }
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "DROP TABLE IF EXISTS source_marks;
                 CREATE TABLE source_marks (
                     source_machine TEXT PRIMARY KEY,
                     received_clock INTEGER NOT NULL
                 );
                 INSERT INTO source_marks (source_machine, received_clock) VALUES ('x', 5);",
            )
            .unwrap();
            c.pragma_update(None, "user_version", 3i64).unwrap();
        }
        match Store::open(&path) {
            Ok(s) => {
                if s.count().unwrap() != 1 {
                    failures.push("b: the row did not survive".into());
                }
                if !columns_of(&s, "source_marks").contains(&"peer_machine".to_string()) {
                    failures.push("b: source_marks was not carried to v5".into());
                }
            }
            Err(e) => failures.push(format!("b: would not open: {e}")),
        }
    }

    // (c) An items table that already has local_edit while the stamp says 3, so
    //     the v3 batch and the v6 ALTER both meet a column that exists.
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        {
            let mut s = Store::open(&path).unwrap();
            s.set_device_id(ME);
            s.insert_clipboard("row", None, None).unwrap();
        }
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", 2i64).unwrap();
        }
        match Store::open(&path) {
            Ok(s) => {
                // The v3 backfill must not have rewritten a real UUID origin id
                // into a rowid, which would hand a peer an identity it may
                // already have seen from a rebuilt database.
                let origin: String = s
                    .conn_for_test()
                    .query_row("SELECT origin_id FROM items LIMIT 1", [], |r| r.get(0))
                    .unwrap();
                if origin.parse::<i64>().is_ok() {
                    failures.push(format!("c: the v3 backfill overwrote a UUID origin: {origin}"));
                }
            }
            Err(e) => failures.push(format!("c: would not open: {e}")),
        }
    }

    assert!(failures.is_empty(), "{:?}", &failures[..failures.len().min(5)]);
}

// ===========================================================================
// R11-4. THE PROPERTIES THE BRIEF ASKS TO BE VERIFIED INDEPENDENTLY RATHER
// THAN TRUSTED.
// ===========================================================================

#[test]
fn r11_the_clear_statements_under_test_are_the_ones_production_runs() {
    // Round 10's plan test EXPLAINs a hand-written copy of the CTE that is NOT
    // the production text: it omits `MATERIALIZED` and the ceiling clamp. It is
    // conservative in the right direction (a plain CTE is the form SQLite may
    // inline, so proving THAT one is uncorrelated proves the pinned one is too),
    // but nothing tied it to the string `Store::clear` actually prepares.
    //
    // Pin the hint itself, so removing it cannot pass review silently.
    let src = include_str!("history.rs");
    let n = src.matches("WITH newest AS MATERIALIZED (").count();
    assert_eq!(
        n, 2,
        "Store::clear must keep the MATERIALIZED hint on both statements; found {n}. \
         Without it SQLite may inline the CTE and the quadratic form comes back."
    );
}

#[test]
fn r11_the_clear_plan_holds_no_correlated_subquery_for_either_statement() {
    // Against the SQLite this crate bundles, on both production statements,
    // with the clamp present. A plan assertion, not a stopwatch.
    let s = store();
    {
        let conn = s.conn_for_test();
        conn.execute("BEGIN", []).unwrap();
        for i in 0..2_000 {
            conn.execute(
                "INSERT INTO items (kind, text, created_at, updated_at, pinned, source_machine, origin_id)
                 VALUES ('clipboard', 'x', ?1, ?1, 0, ?2, ?3)",
                params![now_ms() - 1_000_000 + i, ME, format!("o{i}")],
            )
            .unwrap();
        }
        conn.execute("COMMIT", []).unwrap();
    }
    const CTE: &str = "WITH newest AS MATERIALIZED (
             SELECT src, MAX(c) AS c FROM (
                 SELECT source_machine AS src, MAX(updated_at) AS c FROM items
                  WHERE source_machine IS NOT NULL GROUP BY source_machine
                 UNION ALL
                 SELECT source_machine AS src, MAX(deleted_at) AS c FROM tombstones
                  GROUP BY source_machine
             ) GROUP BY src
         )";
    let statements = [
        format!(
            "{CTE}
             INSERT INTO tombstones (source_machine, origin_id, deleted_at, local)
             SELECT i.source_machine, i.origin_id, max(?2, min(COALESCE(n.c, 0) + 1, ?3)), 1
               FROM items i LEFT JOIN newest n ON n.src = i.source_machine
              WHERE i.kind=?1 AND i.pinned=0
                AND i.source_machine IS NOT NULL AND i.origin_id IS NOT NULL"
        ),
        format!(
            "{CTE}
             INSERT INTO tombstones (source_machine, origin_id, deleted_at, local)
             SELECT i.source_machine, i.origin_id, max(?1, min(COALESCE(n.c, 0) + 1, ?2)), 1
               FROM items i LEFT JOIN newest n ON n.src = i.source_machine
              WHERE i.pinned=0
                AND i.source_machine IS NOT NULL AND i.origin_id IS NOT NULL"
        ),
    ];
    let mut bad: Vec<String> = Vec::new();
    for (ix, sql) in statements.iter().enumerate() {
        let conn = s.conn_for_test();
        let mut st = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        // The kind-scoped statement takes three parameters, the full one two.
        let args: Vec<Box<dyn rusqlite::ToSql>> = if ix == 0 {
            vec![
                Box::new("clipboard"),
                Box::new(now_ms()),
                Box::new(now_ms() + MAX_CLOCK_SKEW_MS),
            ]
        } else {
            vec![Box::new(now_ms()), Box::new(now_ms() + MAX_CLOCK_SKEW_MS)]
        };
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let plan: Vec<String> = st
            .query_map(refs.as_slice(), |r| r.get(3))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for line in plan {
            if line.contains("CORRELATED") {
                bad.push(format!("stmt {ix}: {line}"));
            }
        }
    }
    assert!(bad.is_empty(), "the Clear plan re-evaluates per row: {:?}", &bad[..bad.len().min(3)]);
}

#[test]
fn r11_origin_ceiling_beats_a_blob_and_a_max_length_id_in_sqlite() {
    // The ceiling argument rests on SQLite's BINARY collation over TEXT. Two
    // things could break it: an id longer than the wire cap, and a value that
    // is not TEXT at all — SQLite sorts BLOB above every string, so a BLOB
    // origin id would satisfy `origin_id > ?ceiling` for ever.
    let s = store();
    let worst = "\u{10FFFF}".repeat(32);
    assert_eq!(worst.len(), 128, "premise: the worst legal id must fill the 128-byte cap");
    let cmp: i64 = s
        .conn_for_test()
        .query_row("SELECT ?1 < ?2", params![worst, ORIGIN_CEILING], |r| r.get(0))
        .unwrap();
    assert_eq!(cmp, 1, "a legal origin id sorts at or above ORIGIN_CEILING");

    // And the store must never hold a non-TEXT origin id, because that is the
    // one value the ceiling cannot outrank. Every write path binds a Rust
    // String, so this asserts the invariant rather than a hope.
    s.insert_clipboard("a", None, None).unwrap();
    s.insert_clipboard("b", None, None).unwrap();
    let non_text: i64 = s
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM items
              WHERE origin_id IS NOT NULL AND typeof(origin_id) != 'text'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(non_text, 0, "an origin id is stored as something other than TEXT");
}

#[test]
fn r11_the_excluded_app_filter_cannot_be_defeated_by_the_limit_or_the_cursor() {
    // The filter lives in the SQL so LIMIT counts sendable rows. Check the
    // interaction the page loop actually depends on: a full page of excluded
    // rows must not end the pass with rows still to come.
    let mut s = store();
    // 20 excluded rows first, then 5 kept ones, so any post-filter approach
    // returns an empty first page and `serve` stops.
    for i in 0..20 {
        s.insert_clipboard(&format!("secret {i}"), Some("com.1Password.1Password"), Some("1Password"))
            .unwrap();
    }
    for i in 0..5 {
        s.insert_clipboard(&format!("ordinary {i}"), Some("com.apple.Safari"), Some("Safari"))
            .unwrap();
    }
    s.set_excluded_apps(vec!["com.1password.1password".into()]);
    let page = s.items_from(ME, 0, "", 5).unwrap();
    assert_eq!(page.len(), 5, "a page of excluded rows was not skipped inside the SQL");
    assert!(
        page.iter().all(|r| r.text.starts_with("ordinary")),
        "an excluded row reached the wire"
    );
    // And the pair cursor still advances correctly through the filtered set.
    let last = page.last().unwrap().clone();
    let next = s.items_from(ME, last.updated_at, &last.origin_id, 5).unwrap();
    assert!(next.is_empty(), "the cursor did not terminate: {} rows still offered", next.len());
}

// ===========================================================================
// R11-5. SETTINGS MIGRATION.
// ===========================================================================

#[test]
fn r11_the_settings_migration_is_idempotent_and_a_union() {
    use crate::settings::{Settings, SETTINGS_VERSION};
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("settings.json");

    // A v1 file whose list is missing everything the round-9 table added, and
    // which carries one entry of the user's own.
    std::fs::write(
        &p,
        r#"{"version":1,"history":{"excluded_apps":["com.1password.1password","com.mine.app"]}}"#,
    )
    .unwrap();
    let a = Settings::load(&p).unwrap();
    assert!(
        a.history.excluded_apps.iter().any(|x| x == "com.apple.Passwords"),
        "the union did not add the system password manager"
    );
    assert!(
        a.history.excluded_apps.iter().any(|x| x == "com.mine.app"),
        "the union dropped the user's own entry"
    );
    assert_eq!(a.version, SETTINGS_VERSION);
    let dupes = a.history.excluded_apps.len()
        - a.history
            .excluded_apps
            .iter()
            .map(|x| x.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>()
            .len();
    assert_eq!(dupes, 0, "the union produced {dupes} duplicate entries");

    // Save and reload: the migration must not run a second time, and must not
    // keep growing the list.
    a.save(&p).unwrap();
    let b = Settings::load(&p).unwrap();
    assert_eq!(
        b.history.excluded_apps.len(),
        a.history.excluded_apps.len(),
        "the list grew on a second load"
    );
}

#[test]
fn r11_diag_what_the_settings_version_field_defaults_to() {
    use crate::settings::{Settings, SETTINGS_VERSION};
    let dir = tempfile::tempdir().unwrap();

    // (a) A file with NO version field at all. `#[serde(default)]` fills it from
    //     `Settings::default()`, which is the NEWEST version, so such a file
    //     reads as already migrated and the union never runs.
    let p = dir.path().join("noversion.json");
    std::fs::write(&p, r#"{"history":{"excluded_apps":["com.mine.only"]}}"#).unwrap();
    let a = Settings::load(&p).unwrap();
    let migrated_without_a_version =
        a.history.excluded_apps.iter().any(|x| x == "com.apple.Passwords");

    // (b) A file from a FUTURE build. The version is silently rewritten
    //     downwards, so re-upgrading will skip that build's migration.
    let q = dir.path().join("future.json");
    std::fs::write(&q, r#"{"version":9,"history":{"excluded_apps":["com.mine.only"]}}"#).unwrap();
    let b = Settings::load(&q).unwrap();

    // Recorded, not judged: every settings.json this app has ever written
    // carries `version` (the field is in the first commit that created the
    // struct), so (a) is unreachable today. (b) needs a downgrade.
    assert!(
        !migrated_without_a_version,
        "if this ever starts passing, a version-less settings file DOES migrate and this \
         diagnostic can go"
    );
    assert_eq!(
        b.version, SETTINGS_VERSION,
        "a future settings version is rewritten to {SETTINGS_VERSION} rather than left alone"
    );
}
