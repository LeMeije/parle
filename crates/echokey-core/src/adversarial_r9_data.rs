//! ADVERSARIAL REVIEW, ROUND 9. Data integrity and replication correctness,
//! store side.
//!
//! Round 8's fixes are the target: the pair cursor (schema v7), the widened
//! `ORIGIN_CEILING`, the one-clock rule (`next_clock_for`), receipts made
//! atomic with rows, and the excluded-app filter moved into SQL.
//!
//! Every test here is bounded and does no I/O beyond an in-memory database.

#![cfg(test)]

use crate::history::{
    RemoteItem, RemoteTombstone, Store, MAX_CLOCK_SKEW_MS, ORIGIN_CEILING,
};
use crate::types::HistoryKind;

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

// ---------------------------------------------------------------------------
// R9-1. THE ONE-CLOCK RULE AT ITS CEILING STAMPS BELOW WHAT WE ALREADY HOLD.
//
// `next_clock_in` is "strictly above every clock held for this source, bounded
// at now + MAX_CLOCK_SKEW_MS / 2". Past that bound it returns the plain wall
// clock, which is BELOW the thing that pushed it over the bound.
//
// A peer's cursor for our source rises on EVERYTHING it receives from us,
// tombstones included. So once our own source holds a clock more than half the
// skew window ahead, every row we write afterwards is stamped below the peer's
// cursor and is never offered again. Nothing lowers a cursor.
//
// Reaching that state needs no hostility and no misconfiguration beyond one
// clock being fast inside the window the design explicitly accepts:
//   1. Laptop B's clock is 90 s fast. Still well inside MAX_CLOCK_SKEW_MS.
//   2. On B the user deletes a dictation that the Mac A recorded. B stamps the
//      tombstone with B's clock, i.e. A's now + 90 s.
//   3. B relays that delete to A. A accepts it: it is inside the skew window
//      and A holds the identity.
//   4. A now holds, for source A, a clock 90 s in A's own future.
//   5. A serves that tombstone back to B on the next exchange, so B's cursor
//      for (peer A, source A) sits at now + 90 s.
//   6. Everything A dictates for the next 90 s is stamped with A's plain wall
//      clock, below B's cursor, and B never sees it. Permanently: the cursor
//      does not come down when A's clock catches up.
// ---------------------------------------------------------------------------

#[test]
fn r9_a_relayed_delete_inside_the_skew_window_makes_our_next_rows_unservable() {
    let s = store();
    // A dictation recorded here, on A.
    let id = s.insert_clipboard("recorded on the mac", None, None).unwrap();
    let (origin, _) = s.origin_and_text_for_test(id).unwrap();

    // B deletes it while B's clock runs 90 s fast, and relays the tombstone.
    // 90_000 < MAX_CLOCK_SKEW_MS (120_000), so this is an ACCEPTED delete, not
    // a hostile one.
    let ahead = now_ms() + 90_000;
    assert!(
        90_000 < MAX_CLOCK_SKEW_MS,
        "premise: the fast clock must be inside the accepted window"
    );
    let t = RemoteTombstone {
        source_machine: ME.into(),
        origin_id: origin.clone(),
        deleted_at: ahead,
    };
    s.apply_remote_tombstone(PEER, &t).unwrap();

    // B's cursor for (peer A, source A) is now that tombstone's pair: A serves
    // it straight back, and B records the highest pair it sees.
    let cursor_clock = ahead;
    let cursor_origin = origin.clone();

    // The user dictates again on A.
    let id2 = s.insert_clipboard("dictated a moment later", None, None).unwrap();
    let (origin2, _) = s.origin_and_text_for_test(id2).unwrap();
    let stamped: i64 = s
        .conn_for_test()
        .query_row(
            "SELECT updated_at FROM items WHERE id=?1",
            rusqlite::params![id2],
            |r| r.get(0),
        )
        .unwrap();

    // What `serve` would offer B for source A, from B's cursor.
    let offered = s
        .items_from(ME, cursor_clock, &cursor_origin, 100)
        .unwrap();
    let visible = offered.iter().any(|r| r.origin_id == origin2);

    assert!(
        visible,
        "the row written after the relayed delete is stamped {stamped}, at or below \
         the peer's cursor {cursor_clock}, so it can never be served: a lost row"
    );
}

/// The same defect stated as the invariant it breaks, without the replication
/// vocabulary: this machine must never stamp one of its own rows at or below a
/// clock it already holds for its own source.
#[test]
fn r9_a_local_write_is_never_stamped_below_a_clock_we_already_hold() {
    let s = store();
    let id = s.insert_clipboard("first", None, None).unwrap();
    let (origin, _) = s.origin_and_text_for_test(id).unwrap();
    let ahead = now_ms() + 90_000;
    s.apply_remote_tombstone(
        PEER,
        &RemoteTombstone {
            source_machine: ME.into(),
            origin_id: origin,
            deleted_at: ahead,
        },
    )
    .unwrap();

    let id2 = s.insert_clipboard("second", None, None).unwrap();
    let stamped: i64 = s
        .conn_for_test()
        .query_row(
            "SELECT updated_at FROM items WHERE id=?1",
            rusqlite::params![id2],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        stamped > ahead,
        "stamped {stamped}, but this store already holds {ahead} for its own source"
    );
}

/// And the delete path, which is the worse half: a delete stamped below the
/// peer's cursor is a password the user asked to remove that never goes.
#[test]
fn r9_a_local_delete_after_a_relayed_delete_is_never_offered() {
    let s = store();
    let victim = s.insert_clipboard("password", None, None).unwrap();
    let other = s.insert_clipboard("also ours", None, None).unwrap();
    let (other_origin, _) = s.origin_and_text_for_test(other).unwrap();

    // The peer relays a delete of `other`, 90 s ahead, and we accept it.
    let ahead = now_ms() + 90_000;
    s.apply_remote_tombstone(
        PEER,
        &RemoteTombstone {
            source_machine: ME.into(),
            origin_id: other_origin.clone(),
            deleted_at: ahead,
        },
    )
    .unwrap();

    // The user now deletes the password here.
    s.delete_item_local(victim).unwrap();

    // The peer's cursor is at the relayed tombstone's pair.
    let offered = s.tombstones_from(ME, ahead, &other_origin, 100).unwrap();
    assert!(
        !offered.is_empty(),
        "the delete made after the relayed one is stamped below the peer's cursor \
         ({ahead}) and is never offered: a lost delete"
    );
}

// ---------------------------------------------------------------------------
// R9-2. `clear()` MUST AGREE WITH `next_clock_in`, INCLUDING AT THE CEILING.
// ---------------------------------------------------------------------------

/// The middle of the range: a Clear and a single delete from the same state
/// must produce the same clock.
///
/// The state is deliberately one where the two CAN disagree: this store already
/// holds a clock for its own source ABOVE the wall clock, so "strictly above
/// what we hold" and "the wall clock" are different answers. Built from a
/// fresh store they agree by accident and the test asserts nothing.
#[test]
fn r9_clear_and_a_single_delete_agree_in_the_middle() {
    let a = store();
    let b = store();
    let ahead = now_ms() + 10_000;
    let mut singles: Vec<i64> = Vec::new();
    for (ix, s) in [&a, &b].into_iter().enumerate() {
        let keeper = s.insert_clipboard("keeper", None, None).unwrap();
        let (k_origin, _) = s.origin_and_text_for_test(keeper).unwrap();
        let victim = s.insert_clipboard("victim", None, None).unwrap();
        // Something of ours already sits 10 s ahead of the wall clock, well
        // inside the ceiling of now + MAX_CLOCK_SKEW_MS / 2.
        s.apply_remote_tombstone(
            PEER,
            &RemoteTombstone {
                source_machine: ME.into(),
                origin_id: k_origin,
                deleted_at: ahead,
            },
        )
        .unwrap();
        if ix == 0 {
            s.delete_item_local(victim).unwrap();
        } else {
            s.clear(None).unwrap();
        }
        let stamped: i64 = s
            .conn_for_test()
            .query_row(
                "SELECT deleted_at FROM tombstones WHERE origin_id NOT IN
                   (SELECT origin_id FROM tombstones ORDER BY deleted_at DESC LIMIT 0)
                   AND deleted_at != ?1",
                rusqlite::params![ahead],
                |r| r.get(0),
            )
            .unwrap();
        singles.push(stamped);
    }
    assert!(
        singles[0] > ahead && singles[1] > ahead,
        "premise: both paths must be climbing above {ahead}, got {singles:?}"
    );
    assert_eq!(
        singles[0], singles[1],
        "a single delete stamped {} and a Clear stamped {} from the same state",
        singles[0], singles[1]
    );
}

/// At the ceiling. A Clear must not stamp a delete BELOW a tombstone the store
/// already holds for that source, or the delete is never offered.
#[test]
fn r9_clear_at_the_ceiling_does_not_stamp_below_what_we_hold() {
    let s = store();
    let victim = s.insert_clipboard("password", None, None).unwrap();
    let other = s.insert_clipboard("other", None, None).unwrap();
    let (other_origin, _) = s.origin_and_text_for_test(other).unwrap();
    let (victim_origin, _) = s.origin_and_text_for_test(victim).unwrap();

    let ahead = now_ms() + 90_000;
    s.apply_remote_tombstone(
        PEER,
        &RemoteTombstone {
            source_machine: ME.into(),
            origin_id: other_origin.clone(),
            deleted_at: ahead,
        },
    )
    .unwrap();

    s.clear(None).unwrap();
    let stamped: i64 = s
        .conn_for_test()
        .query_row(
            "SELECT deleted_at FROM tombstones WHERE origin_id=?1",
            rusqlite::params![victim_origin],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        stamped > ahead,
        "Clear stamped the delete at {stamped}, below the {ahead} this store already \
         holds for its own source, so the peer never asks for it"
    );
}

// ---------------------------------------------------------------------------
// R9-3. THE ORIGIN CEILING.
// ---------------------------------------------------------------------------

/// The proof in the doc comment, checked against SQLite itself rather than
/// against the comment. Every byte-wise maximum id a peer can legally send must
/// sort BELOW the ceiling, under the collation the real queries use.
#[test]
fn r9_the_origin_ceiling_out_sorts_every_legal_id_in_sqlite() {
    let s = store();
    let max_char = '\u{10FFFF}';
    let per = max_char.len_utf8();
    assert_eq!(per, 4, "U+10FFFF must be four bytes");
    // The longest legal id built from the byte-wise maximum scalar.
    let longest = max_char.to_string().repeat(128 / per);
    assert!(longest.len() <= 128);
    // And every prefix length up to it, plus the same padded with the largest
    // trailing bytes a shorter id could carry.
    let mut worst: Vec<String> = (1..=128 / per)
        .map(|n| max_char.to_string().repeat(n))
        .collect();
    // A 128-byte id that is NOT a multiple of four bytes: 31 max scalars plus
    // the largest remaining valid sequence in the leftover four bytes.
    worst.push(format!("{}{}", max_char.to_string().repeat(31), max_char));
    for w in &worst {
        let ge: i64 = s
            .conn_for_test()
            .query_row(
                "SELECT CAST(?1 AS TEXT) >= CAST(?2 AS TEXT)",
                rusqlite::params![w, ORIGIN_CEILING],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            ge,
            0,
            "an id of {} bytes sorts at or above the ceiling in SQLite",
            w.len()
        );
    }
}

/// The ceiling is only ever safe because 128 bytes is the cap. Pin the cap so
/// that raising it without raising the ceiling fails here rather than in the
/// field.
#[test]
fn r9_the_origin_ceiling_is_longer_than_the_longest_legal_id() {
    // Mirrored rather than depended on; echokey-core does not see echokey-sync.
    const MAX_ORIGIN_ID_BYTES: usize = 128;
    assert!(
        ORIGIN_CEILING.len() > MAX_ORIGIN_ID_BYTES,
        "the ceiling is {} bytes and a legal id may be {}",
        ORIGIN_CEILING.len(),
        MAX_ORIGIN_ID_BYTES
    );
}

// ---------------------------------------------------------------------------
// R9-4. THE EXCLUDED-APP FILTER, NOW IN SQL.
// ---------------------------------------------------------------------------

/// `set_excluded_apps` lower-cases in Rust (full Unicode); the SQL compares
/// with SQLite's `LOWER()`, which is ASCII-only. An app whose name carries a
/// non-ASCII capital therefore never matches, and its rows leave the machine.
#[test]
fn r9_an_excluded_app_with_a_non_ascii_name_is_still_replicated() {
    let mut s = store();
    let id = s.insert_clipboard("a secret", None, None).unwrap();
    s.conn_for_test()
        .execute(
            "UPDATE items SET app_name = 'ÉDITEUR' WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
    s.set_excluded_apps(vec!["ÉDITEUR".to_string()]);
    let page = s.items_from(ME, 0, "", 100).unwrap();
    assert!(
        page.is_empty(),
        "a row from an excluded app is still offered to peers: SQLite LOWER() is \
         ASCII-only, the exclusion list is lower-cased in Rust"
    );
}

/// The ASCII case, which must keep working: this is the guard that proves the
/// test above is testing the collation and not the plumbing.
#[test]
fn r9_an_excluded_ascii_app_is_filtered_in_sql() {
    let mut s = store();
    let id = s.insert_clipboard("a secret", None, None).unwrap();
    s.conn_for_test()
        .execute(
            "UPDATE items SET app_name = 'Bitwarden' WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
    s.set_excluded_apps(vec!["Bitwarden".to_string()]);
    assert!(s.items_from(ME, 0, "", 100).unwrap().is_empty());
}

/// Parameter numbering: with several excluded apps the LIMIT must still bind to
/// `?4`, and a full page must still be a full page.
#[test]
fn r9_the_excluded_filter_does_not_break_the_limit() {
    let mut s = store();
    for i in 0..20 {
        s.insert_clipboard(&format!("row {i}"), None, None).unwrap();
    }
    s.set_excluded_apps(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
    let page = s.items_from(ME, 0, "", 5).unwrap();
    assert_eq!(page.len(), 5, "LIMIT bound to the wrong parameter");
}

// ---------------------------------------------------------------------------
// R9-5. THE PAIR CURSOR IS MONOTONE AND CANNOT BE PARKED OR REVERSED.
// ---------------------------------------------------------------------------

#[test]
fn r9_the_pair_cursor_never_moves_backwards() {
    let s = store();
    let base = now_ms() - 1_000;
    s.note_received_at(PEER, PEER, base, "mmm").unwrap();
    // Lower clock, higher origin.
    s.note_received_at(PEER, PEER, base - 1, "zzz").unwrap();
    // Same clock, lower origin.
    s.note_received_at(PEER, PEER, base, "aaa").unwrap();
    // Out of range: must be ignored entirely.
    s.note_received_at(PEER, PEER, now_ms() + MAX_CLOCK_SKEW_MS + 5_000, "zzz")
        .unwrap();
    s.note_received_at(PEER, PEER, 0, "zzz").unwrap();
    let marks = s.watermarks_paired(PEER).unwrap();
    assert_eq!(marks, vec![(PEER.to_string(), base, "mmm".to_string())]);

    // Same clock, higher origin: this one MUST move it.
    s.note_received_at(PEER, PEER, base, "nnn").unwrap();
    let marks = s.watermarks_paired(PEER).unwrap();
    assert_eq!(marks, vec![(PEER.to_string(), base, "nnn".to_string())]);

    // Higher clock with a lower origin: the origin must follow the clock.
    s.note_received_at(PEER, PEER, base + 1, "aaa").unwrap();
    let marks = s.watermarks_paired(PEER).unwrap();
    assert_eq!(marks, vec![(PEER.to_string(), base + 1, "aaa".to_string())]);
}

/// The v6 default: an empty origin means "re-offer this whole millisecond".
/// It must terminate, i.e. the second pass must not see the same rows again.
#[test]
fn r9_an_empty_origin_cursor_terminates() {
    let s = store();
    // Several rows sharing one millisecond, written directly so the clock is
    // controlled.
    let clock = now_ms() - 5_000;
    for i in 0..5 {
        s.conn_for_test()
            .execute(
                "INSERT INTO items (kind, text, created_at, updated_at, source_machine, origin_id)
                 VALUES ('clipboard', ?1, ?2, ?2, ?3, ?4)",
                rusqlite::params![format!("r{i}"), clock, ME, format!("o{i}")],
            )
            .unwrap();
    }
    // A v6-migrated cursor: the clock, and an empty origin.
    let mut after = clock;
    let mut origin = String::new();
    let mut seen = 0usize;
    for round in 0..10 {
        let page = s.items_from(ME, after, &origin, 2).unwrap();
        if page.is_empty() {
            assert!(round > 0, "the first pass saw nothing; premise is wrong");
            assert_eq!(seen, 5, "saw {seen} of 5 rows before going quiet");
            return;
        }
        seen += page.len();
        let last = page.last().unwrap();
        after = last.updated_at;
        origin = last.origin_id.clone();
    }
    panic!("an empty-origin cursor did not go quiet within 10 pages");
}

// ---------------------------------------------------------------------------
// R9-6. MIGRATIONS v1..v6 -> v7.
// ---------------------------------------------------------------------------

fn schema_fingerprint(s: &Store) -> Vec<String> {
    let mut stmt = s
        .conn_for_test()
        .prepare(
            "SELECT type, name, COALESCE(sql,'') FROM sqlite_master
              WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
            ))
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

fn column_fingerprint(s: &Store, table: &str) -> Vec<String> {
    let mut stmt = s
        .conn_for_test()
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok(format!(
                "{}:{}:{}:{}",
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?.unwrap_or_default()
            ))
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

/// A store left at v6 with a populated `source_marks` must reach v7 with every
/// mark intact, an empty `received_origin`, and a schema identical to a fresh
/// v7 store.
#[test]
fn r9_a_v6_store_migrates_to_a_schema_identical_to_a_fresh_v7() {
    let path = std::env::temp_dir().join(format!("r9-v6-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let s = Store::open(&path).unwrap();
        s.conn_for_test()
            .execute_batch("ALTER TABLE source_marks DROP COLUMN received_origin;")
            .unwrap();
        s.conn_for_test()
            .pragma_update(None, "user_version", 6i64)
            .unwrap();
        s.conn_for_test()
            .execute(
                "INSERT INTO source_marks (peer_machine, source_machine, received_clock)
                 VALUES (?1, ?2, 12345)",
                rusqlite::params![PEER, ME],
            )
            .unwrap();
    }
    let reopened = Store::open(&path).unwrap();
    let marks = reopened.watermarks_paired(PEER).unwrap();
    assert_eq!(marks, vec![(ME.to_string(), 12345, String::new())]);

    let fresh = Store::open_in_memory().unwrap();
    let a = column_fingerprint(&reopened, "source_marks");
    let b = column_fingerprint(&fresh, "source_marks");
    assert_eq!(a, b, "migrated source_marks differs from a fresh one");
    let sa = schema_fingerprint(&reopened);
    let sb = schema_fingerprint(&fresh);
    let only_a: Vec<_> = sa.iter().filter(|x| !sb.contains(x)).take(3).collect();
    let only_b: Vec<_> = sb.iter().filter(|x| !sa.contains(x)).take(3).collect();
    assert!(
        only_a.is_empty() && only_b.is_empty(),
        "schema differs: migrated-only {only_a:?}, fresh-only {only_b:?}"
    );
    drop(reopened);
    let _ = std::fs::remove_file(&path);
}

/// The v7 ALTER re-run on a store that already has the column: idempotent, and
/// it must not reset a mark that already carries an origin.
#[test]
fn r9_the_v7_step_is_idempotent_over_an_interrupted_upgrade() {
    let path = std::env::temp_dir().join(format!("r9-v7i-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let s = Store::open(&path).unwrap();
        s.note_received_at(PEER, ME, now_ms() - 10, "origin-x").unwrap();
        // Interrupted: schema is at v7, the stamp is behind.
        s.conn_for_test()
            .pragma_update(None, "user_version", 6i64)
            .unwrap();
    }
    let reopened = Store::open(&path).unwrap();
    let marks = reopened.watermarks_paired(PEER).unwrap();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].2, "origin-x", "the interrupted upgrade lost the origin");
    let v: i64 = reopened
        .conn_for_test()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, Store::SCHEMA_VERSION_FOR_TEST);
    drop(reopened);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// R9-7. `clear()` ON A LARGE HISTORY.
//
// The Clear SQL evaluates a correlated MAX over items UNION tombstones twice
// per row. The store mutex is shared with the synchronous history commands,
// which run on the UI thread, so the cost of this statement is a freeze the
// user sees.
// ---------------------------------------------------------------------------

/// How long one Clear takes over `n` rows of our own source.
fn time_clear(n: i64) -> u128 {
    let s = store();
    {
        let tx = s.conn_for_test().unchecked_transaction().unwrap();
        for i in 0..n {
            tx.execute(
                "INSERT INTO items (kind, text, created_at, updated_at, source_machine, origin_id)
                 VALUES ('clipboard', 'x', ?1, ?1, ?2, ?3)",
                rusqlite::params![now_ms() - 100_000 + i, ME, format!("o{i}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let t0 = std::time::Instant::now();
    let removed = s.clear(None).unwrap();
    assert_eq!(removed, n as usize);
    t0.elapsed().as_micros()
}

/// Machine-independent: doubling the history must not roughly quadruple the
/// cost of clearing it. Stated as a ratio so it means the same thing on any
/// machine and in either profile, with a vacuity guard so a run too fast to
/// measure fails loudly instead of passing on noise.
#[test]
fn r9_clear_does_not_cost_the_square_of_the_history() {
    let small = time_clear(1_500);
    let large = time_clear(3_000);
    assert!(
        small > 2_000,
        "the small Clear took {small} us, too fast to compare against; raise the sizes"
    );
    let ratio = large as f64 / small as f64;
    assert!(
        ratio < 3.0,
        "clearing 3000 rows took {large} us against {small} us for 1500: a ratio of \
         {ratio:.1} where linear is 2. Clear evaluates a correlated MAX over items \
         UNION tombstones twice per row, while holding the store mutex the history \
         window shares"
    );
}

// ---------------------------------------------------------------------------
// R9-8. TWO-DEVICE CONVERGENCE OVER THE PAIR CURSOR, ENTIRELY IN THE STORE.
//
// A bounded model of the exchange: serve by pair cursor, drain by apply.
// It must go quiet, and both sides must agree.
// ---------------------------------------------------------------------------

struct Node {
    id: &'static str,
    s: Store,
}

impl Node {
    fn new(id: &'static str) -> Self {
        let mut s = Store::open_in_memory().unwrap();
        s.set_device_id(id);
        Node { id, s }
    }
}

/// One direction of an exchange, paged on the pair cursor exactly as `serve`
/// does. Returns how many messages actually crossed.
fn push(from: &Node, to: &Node, page: usize) -> usize {
    let marks: std::collections::HashMap<String, (i64, String)> = to
        .s
        .watermarks_paired(from.id)
        .unwrap()
        .into_iter()
        .map(|(src, c, o)| (src, (c, o)))
        .collect();
    let mut moved = 0usize;
    for source in from.s.known_sources().unwrap() {
        // Items: author-only.
        if source == from.id {
            let (mut after, mut origin) =
                marks.get(&source).cloned().unwrap_or((0, String::new()));
            for _ in 0..64 {
                let p = from.s.items_from(&source, after, &origin, page).unwrap();
                if p.is_empty() {
                    break;
                }
                for it in &p {
                    to.s.apply_remote_item(from.id, it).unwrap();
                    moved += 1;
                }
                let last = p.last().unwrap();
                if p.len() < page {
                    break;
                }
                after = last.updated_at;
                origin = last.origin_id.clone();
            }
        }
        // Tombstones: every source.
        let (mut after, mut origin) = marks.get(&source).cloned().unwrap_or((0, String::new()));
        for _ in 0..64 {
            let p = from.s.tombstones_from(&source, after, &origin, page).unwrap();
            if p.is_empty() {
                break;
            }
            for t in &p {
                to.s.apply_remote_tombstone(from.id, t).unwrap();
                moved += 1;
            }
            let last = p.last().unwrap();
            if p.len() < page {
                break;
            }
            after = last.deleted_at;
            origin = last.origin_id.clone();
        }
    }
    moved
}

fn live(s: &Store) -> Vec<(String, String, bool)> {
    let mut stmt = s
        .conn_for_test()
        .prepare(
            "SELECT COALESCE(origin_id,''), text, pinned FROM items
              WHERE source_machine IS NOT NULL ORDER BY origin_id",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? != 0,
            ))
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

#[test]
fn r9_two_devices_converge_and_go_quiet_including_a_clear() {
    let a = Node::new(ME);
    let b = Node::new(PEER);

    for i in 0..7 {
        a.s.insert_clipboard(&format!("a{i}"), None, None).unwrap();
        b.s.insert_clipboard(&format!("b{i}"), None, None).unwrap();
    }
    // Exchange until quiet, page size 2 so paging is exercised.
    let mut rounds = 0;
    loop {
        let moved = push(&a, &b, 2) + push(&b, &a, 2);
        rounds += 1;
        assert!(rounds < 12, "did not go quiet in 12 rounds");
        if moved == 0 {
            break;
        }
    }
    assert_eq!(live(&a.s), live(&b.s), "diverged after the first sync");
    assert_eq!(live(&a.s).len(), 14);

    // B clears. Every delete must reach A, and nothing must come back.
    b.s.clear(None).unwrap();
    let mut rounds = 0;
    loop {
        let moved = push(&b, &a, 2) + push(&a, &b, 2);
        rounds += 1;
        assert!(rounds < 12, "the clear did not go quiet in 12 rounds");
        if moved == 0 {
            break;
        }
    }
    let la = live(&a.s);
    let lb = live(&b.s);
    assert!(la.is_empty(), "{} rows survived the clear on A: {:?}", la.len(), &la[..la.len().min(3)]);
    assert!(lb.is_empty(), "{} rows survived the clear on B", lb.len());

    // And a further exchange moves nothing at all.
    assert_eq!(push(&a, &b, 2) + push(&b, &a, 2), 0, "the pair never goes quiet");
}

#[test]
fn r9_a_clear_larger_than_a_page_loses_no_delete() {
    let a = Node::new(ME);
    let b = Node::new(PEER);
    for i in 0..40 {
        b.s.insert_clipboard(&format!("b{i}"), None, None).unwrap();
    }
    let mut rounds = 0;
    while push(&b, &a, 3) + push(&a, &b, 3) > 0 {
        rounds += 1;
        assert!(rounds < 40, "first sync did not go quiet");
    }
    assert_eq!(live(&a.s).len(), 40);
    // One Clear, one clock across every tombstone, far more than a page.
    b.s.clear(None).unwrap();
    let mut rounds = 0;
    while push(&b, &a, 3) + push(&a, &b, 3) > 0 {
        rounds += 1;
        assert!(rounds < 60, "the clear did not go quiet");
    }
    let la = live(&a.s);
    assert!(
        la.is_empty(),
        "{} of 40 cleared rows are still on A, first: {:?}",
        la.len(),
        &la[..la.len().min(3)]
    );
}

// ---------------------------------------------------------------------------
// R9-9. HOSTILE VALUES.
// ---------------------------------------------------------------------------

#[test]
fn r9_extreme_peer_values_never_panic_or_corrupt() {
    let s = store();
    let big = "\u{10FFFF}".repeat(32);
    let cases = [
        (i64::MAX, i64::MAX),
        (i64::MIN, i64::MIN),
        (0, 0),
        (-1, now_ms()),
        (now_ms(), i64::MAX),
        (i64::MAX, now_ms()),
    ];
    for (u, c) in cases {
        let it = RemoteItem {
            source_machine: PEER.into(),
            origin_id: big.clone(),
            kind: "clipboard".into(),
            text: "x".into(),
            created_at: c,
            updated_at: u,
            pinned: false,
        };
        let _ = s.apply_remote_item(PEER, &it).unwrap();
        let t = RemoteTombstone {
            source_machine: PEER.into(),
            origin_id: big.clone(),
            deleted_at: u,
        };
        let _ = s.apply_remote_tombstone(PEER, &t).unwrap();
    }
    // Nothing out of range was stored, and nothing out of range reached a mark.
    let bad: i64 = s
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM source_marks WHERE received_clock <= 0
              OR received_clock > ?1",
            rusqlite::params![now_ms() + MAX_CLOCK_SKEW_MS],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bad, 0, "an out-of-range clock reached a cursor");
    // And a clear over that state still works.
    let _ = s.clear(Some(HistoryKind::Clipboard)).unwrap();
}

/// Diagnostic, ignored: how the Clear statement scales. Round 8 added the
/// `items` half of the correlated MAX; before it the subquery was
/// `MAX(deleted_at) FROM tombstones`, served by `idx_tombstones_deleted`.
#[test]
#[ignore]
fn r9_diag_clear_scaling() {
    for n in [1_000i64, 2_000, 4_000, 8_000] {
        let s = store();
        let tx = s.conn_for_test().unchecked_transaction().unwrap();
        for i in 0..n {
            tx.execute(
                "INSERT INTO items (kind, text, created_at, updated_at, source_machine, origin_id)
                 VALUES ('clipboard', 'x', ?1, ?1, ?2, ?3)",
                rusqlite::params![now_ms() - 100_000 + i, ME, format!("o{i}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let t0 = std::time::Instant::now();
        s.clear(None).unwrap();
        println!("clear {n} rows: {} ms", t0.elapsed().as_millis());
    }
}
