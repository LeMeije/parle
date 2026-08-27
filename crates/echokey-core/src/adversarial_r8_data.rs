//! ADVERSARIAL REVIEW, ROUND 8. Data integrity and replication correctness,
//! store side.
//!
//! Scope: `history.rs`. Divergence, lost rows, resurrected deletes, lost
//! deletes, unbounded repeated transfer, migrations, keyset paging, and
//! arithmetic on peer-controlled values.
//!
//! Every test here is bounded and does no I/O beyond a temp directory.

#![cfg(test)]

use crate::history::{
    ApplyOutcome, RemoteItem, RemoteTombstone, Store, MAX_CLOCK_SKEW_MS, ORIGIN_CEILING,
};
use crate::types::HistoryKind;
use rusqlite::Connection;

const ME: &str = "11111111-1111-4111-8111-111111111111";
const PEER: &str = "22222222-2222-4222-8222-222222222222";
const THIRD: &str = "33333333-3333-4333-8333-333333333333";

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

fn peer_item(origin: &str, text: &str, clock: i64) -> RemoteItem {
    RemoteItem {
        source_machine: PEER.into(),
        origin_id: origin.into(),
        kind: "clipboard".into(),
        text: text.into(),
        created_at: clock,
        updated_at: clock,
        pinned: false,
    }
}

// ---------------------------------------------------------------------------
// R8-1. THE ORIGIN CEILING IS NOT A CEILING.
//
// `ORIGIN_CEILING` is "\u{FFFF}", chosen so that `(clock, ORIGIN_CEILING)`
// reads as "strictly after this whole millisecond". The doc comment justifies
// it with "origin ids are UUIDs — ASCII". That is true of the ids WE mint. It
// is not true of the ids a PEER mints: `wire::validate_origin_id` checks only
// that the string is non-empty and at most 128 bytes, so any UTF-8 is legal,
// and every scalar above U+FFFF sorts ABOVE the sentinel under SQLite's BINARY
// collation.
//
// A tombstone carrying such an origin id therefore satisfies
// `deleted_at = cursor AND origin_id > ORIGIN_CEILING` for ever: it is served
// again on every single exchange, the receiving side records the same clock it
// already had, and nothing can ever move the cursor past it.
// ---------------------------------------------------------------------------
/// The wire's cap on an origin id, mirrored so this crate does not need to
/// depend on `echokey-sync` for one number.
fn echokey_sync_max_origin_id_bytes() -> usize {
    128
}

#[test]
fn r8_an_origin_id_above_the_sentinel_is_re_offered_at_its_own_clock_for_ever() {
    let s = store();
    let clock = now_ms() - 1_000;

    // The peer authors a row of its own and deletes it, naming the row with an
    // origin id no UUID would produce. Both are things `wire.rs` accepts.
    let hostile = "\u{1F600}row";
    // The premise is now UNREACHABLE, and that is the fix.
    //
    // `ORIGIN_CEILING` was `"\u{FFFF}"`, justified by "origin ids are UUIDs".
    // True of the ids we mint, false of the ids a peer mints: the wire accepts
    // any UTF-8 up to 128 bytes, and any scalar above the BMP encodes to bytes
    // above U+FFFF's. Such an id satisfied `deleted_at = cursor AND origin_id >
    // sentinel` for ever, so the pair never went quiet.
    //
    // The sentinel is now 33 copies of U+10FFFF: 132 bytes of the byte-wise
    // maximum valid UTF-8 sequence, against a 128-byte cap on any legal id. So
    // every legal id is either smaller at the first differing byte or a proper
    // prefix, and a prefix sorts below. This assertion is what stops anyone
    // shortening it back.
    assert!(
        hostile < ORIGIN_CEILING,
        "an origin id a PEER can legally send sorts above the sentinel, so a row at the cursor's \
         own clock is re-served on every exchange for ever"
    );
    assert!(
        ORIGIN_CEILING.len() > echokey_sync_max_origin_id_bytes(),
        "the sentinel must be longer than the longest legal origin id, or a maximal id equals it"
    );
    s.apply_remote_tombstone(
        PEER,
        &RemoteTombstone {
            source_machine: PEER.into(),
            origin_id: hostile.into(),
            deleted_at: clock,
        },
    )
    .unwrap();

    // A well-formed neighbour at the same clock, as a control.
    s.apply_remote_tombstone(
        PEER,
        &RemoteTombstone {
            source_machine: PEER.into(),
            origin_id: "aaaa-normal".into(),
            deleted_at: clock,
        },
    )
    .unwrap();

    // `serve` asks for everything strictly above the peer's cursor. The peer's
    // cursor is `clock`: it has already been handed both of these.
    let again = s.tombstones_since(PEER, clock, 256).unwrap();
    let ids: Vec<&str> = again.iter().map(|t| t.origin_id.as_str()).collect();

    assert!(
        !ids.contains(&"aaaa-normal"),
        "the control must not come back; if it does this test proves nothing"
    );
    assert!(
        !ids.contains(&hostile),
        "a tombstone at or below the peer's cursor was offered again: {ids:?}. \
         Nothing can ever advance the cursor past it, so this repeats on every \
         exchange for the life of the pairing."
    );
}

// ---------------------------------------------------------------------------
// R8-2. Same defect, reached through the ordinary `items_since` door, to show
// the sentinel is the problem rather than the tombstone table.
//
// This one is currently unreachable in production — `serve` offers items only
// for `source == me`, and our own origin ids are UUIDs — so it is recorded as
// a latent trap rather than a live defect. It fails today for the same reason
// R8-1 does.
// ---------------------------------------------------------------------------
#[test]
fn r8_an_item_with_an_origin_id_above_the_sentinel_is_also_re_offered() {
    let s = store();
    let clock = now_ms() - 1_000;
    let hostile = "\u{10FFFF}";
    s.apply_remote_item(PEER, &peer_item(hostile, "x", clock)).unwrap();
    s.apply_remote_item(PEER, &peer_item("aaaa-normal", "y", clock)).unwrap();

    let again = s.items_since(PEER, clock, 256).unwrap();
    let ids: Vec<&str> = again.iter().map(|r| r.origin_id.as_str()).collect();
    assert!(!ids.contains(&"aaaa-normal"), "control leaked");
    assert!(
        !ids.contains(&hostile),
        "an item at the cursor was offered again: {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// R8-3. TOMBSTONE PAGING HAS NO MILLISECOND BOUNDARY.
//
// `items_from` is paged by `serve` with an explicit trim: every FULL page is
// cut back to a millisecond boundary, because "the peer records the highest
// clock it sees, and the next exchange asks strictly above it", so a run that
// stops between pages parks the cursor inside a millisecond and the rest of
// that millisecond is below it for ever.
//
// The tombstone loop in `serve` does the same paging and has no such trim.
// This test establishes the store-side precondition that makes that reachable:
// `clear()` stamps every tombstone for one source with ONE clock, and a single
// `clear` of more than PAGE (256) rows therefore produces a millisecond that
// cannot be paged out of without landing inside it.
//
// The consequence — a genuinely lost delete — is proved over real sockets in
// `src-tauri/src/sync/adversarial_r8_data.rs`.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_clear_stamps_one_millisecond_across_far_more_rows_than_a_page_holds() {
    let s = store();
    let base = now_ms() - 100_000;
    for i in 0..600 {
        s.apply_remote_item(PEER, &peer_item(&format!("row-{i:04}"), "secret", base + 1))
            .unwrap();
    }
    assert_eq!(s.count().unwrap(), 600);
    s.clear(None).unwrap();
    assert_eq!(s.tombstone_count(PEER).unwrap(), 600);

    // How many distinct clocks did the clear produce?
    let distinct: i64 = s
        .conn_for_test()
        .query_row(
            "SELECT COUNT(DISTINCT deleted_at) FROM tombstones WHERE source_machine=?1",
            rusqlite::params![PEER],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        distinct, 1,
        "a Clear History stamps one clock per source; if this ever changes the \
         paging hazard below changes with it"
    );

    // One page of 256. The caller banks the highest clock it saw, which is
    // that single clock, and the next request is strictly above it.
    let page = s.tombstones_since(PEER, 0, 256).unwrap();
    assert_eq!(page.len(), 256);
    let banked = page.iter().map(|t| t.deleted_at).max().unwrap();
    let next = s.tombstones_since(PEER, banked, 256).unwrap();
    assert_eq!(
        next.len(),
        0,
        "sanity: strictly-above returns nothing, which is exactly why a stop \
         between pages loses the other 344 deletes"
    );
    // 344 of the user's deletes are now unreachable to any cursor at `banked`.
    assert_eq!(s.tombstone_count(PEER).unwrap() - 256, 344);
}

// ---------------------------------------------------------------------------
// R8-4. Hostile clocks and payloads: no panic, no overflow, no unreachable row.
//
// Every value here is one a peer can put on the wire. The store must refuse or
// absorb each without raising, and anything it DOES store must be reachable to
// `items_since` / `tombstones_since` from a cursor of 0.
// ---------------------------------------------------------------------------
#[test]
fn r8_hostile_clocks_never_panic_and_never_produce_an_unreachable_row() {
    let s = store();
    let now = now_ms();

    let clocks: [i64; 10] = [
        i64::MIN,
        i64::MIN + 1,
        -1,
        0,
        1,
        now,
        now + MAX_CLOCK_SKEW_MS,
        now + MAX_CLOCK_SKEW_MS + 1,
        i64::MAX - 1,
        i64::MAX,
    ];

    // `created_at` is held at a plainly valid value here. Pairing it with the
    // hostile list as well made the test flaky rather than thorough: the
    // refusal ceiling is `now_ms() + skew`, which MOVES, so a created_at of
    // exactly `now + skew + 1` is refused on one millisecond and accepted on
    // the next, and the "is it idempotent" assertion then compared two calls
    // made under different ceilings. The created_at door is exercised on its
    // own below.
    let safe_created = now - 60_000;

    for (i, c) in clocks.iter().enumerate() {
        let mut it = peer_item(&format!("hostile-{i}"), "payload", *c);
        it.created_at = safe_created;
        let out = s.apply_remote_item(PEER, &it).unwrap();
        let t = RemoteTombstone {
            source_machine: PEER.into(),
            origin_id: format!("hostile-t-{i}"),
            deleted_at: *c,
        };
        let _ = s.apply_remote_tombstone(PEER, &t).unwrap();
        // Re-applying the identical message must be a no-op, or the row is
        // rewritten on every exchange for ever. Only asserted for clocks well
        // clear of the moving ceiling.
        if *c <= now + MAX_CLOCK_SKEW_MS / 2 {
            let again = s.apply_remote_item(PEER, &it).unwrap();
            assert_eq!(
                again,
                ApplyOutcome::Ignored,
                "re-applying an identical row at clock {c} (first pass {out:?}) \
                 changed the store again: that is a rewrite on every exchange"
            );
        }
    }

    // Hostile `created_at` with a perfectly ordinary `updated_at`.
    for (i, c) in clocks.iter().enumerate() {
        let mut it = peer_item(&format!("created-{i}"), "payload", now - 30_000);
        it.created_at = *c;
        let _ = s.apply_remote_item(PEER, &it).unwrap();
    }

    // Everything the store kept must be reachable from a zero cursor.
    let held: i64 = s
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM items WHERE source_machine=?1",
            rusqlite::params![PEER],
            |r| r.get(0),
        )
        .unwrap();
    let reachable = s.items_since(PEER, 0, 1_000).unwrap().len() as i64;
    assert_eq!(
        held, reachable,
        "a row is stored but no cursor can ever reach it: replication would \
         never offer it and the two machines diverge silently"
    );

    let tombs = s.tombstone_count(PEER).unwrap();
    let t_reachable = s.tombstones_since(PEER, 0, 1_000).unwrap().len() as i64;
    assert_eq!(tombs, t_reachable, "a tombstone is stored but unreachable");

    // A local delete on top of whatever clocks got in must still be stampable.
    let id = s.insert_clipboard("mine", None, None).unwrap();
    s.delete_item_local(id).unwrap();
}

// ---------------------------------------------------------------------------
// R8-5. Hostile text: NUL bytes, lone surrogates' escapes, very long strings,
// and control characters must round-trip or be refused, never corrupt.
// ---------------------------------------------------------------------------
#[test]
fn r8_hostile_text_round_trips_or_is_refused_but_never_corrupts() {
    let s = store();
    let now = now_ms() - 1_000;
    let texts = [
        "plain",
        "with\0nul",
        "\u{FFFD}replacement",
        "'); DROP TABLE items;--",
        "\u{202E}rtl-override",
        &"x".repeat(100_000),
    ];
    for (i, t) in texts.iter().enumerate() {
        let mut it = peer_item(&format!("text-{i}"), t, now + i as i64);
        it.kind = "transcription".into();
        assert_eq!(
            s.apply_remote_item(PEER, &it).unwrap(),
            ApplyOutcome::Inserted,
            "text {i} refused"
        );
        let back = s.items_since(PEER, now + i as i64 - 1, 10).unwrap();
        let found = back
            .iter()
            .find(|r| r.origin_id == format!("text-{i}"))
            .unwrap_or_else(|| panic!("text {i} stored but unreachable"));
        assert_eq!(&found.text, t, "text {i} came back changed");
    }
    // Search must not fall over on any of it.
    let _ = s.search("plain", None, 10).unwrap();
    let _ = s.search("\0", None, 10).unwrap();
    let _ = s.search("*", None, 10).unwrap();
}

// ---------------------------------------------------------------------------
// R8-6. A tombstone is absorbing across every order the wire can deliver.
//
// Six orderings of {row, later row, delete} for one identity, each in a fresh
// store. The row must be gone in every one, and must stay gone when the row is
// re-offered afterwards.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_delete_is_absorbing_in_every_arrival_order() {
    let base = now_ms() - 10_000;
    let row = |clock: i64| peer_item("one", "hunter2", clock);
    let tomb = |clock: i64| RemoteTombstone {
        source_machine: PEER.into(),
        origin_id: "one".into(),
        deleted_at: clock,
    };

    // (row clock, later-row clock, delete clock) permutations of arrival.
    let orders: [&[&str]; 6] = [
        &["row", "edit", "del"],
        &["row", "del", "edit"],
        &["del", "row", "edit"],
        &["del", "edit", "row"],
        &["edit", "row", "del"],
        &["edit", "del", "row"],
    ];
    for (n, order) in orders.iter().enumerate() {
        let s = store();
        for step in order.iter() {
            match *step {
                "row" => {
                    s.apply_remote_item(PEER, &row(base)).unwrap();
                }
                "edit" => {
                    let mut e = row(base + 100);
                    e.text = "hunter2 corrected".into();
                    s.apply_remote_item(PEER, &e).unwrap();
                }
                _ => {
                    s.apply_remote_tombstone(PEER, &tomb(base + 50)).unwrap();
                }
            }
        }
        assert!(
            !s.items_since(PEER, 0, 10).unwrap().iter().any(|r| r.origin_id == "one"),
            "order {n} ({order:?}) left the deleted row alive"
        );
        // And a straggling re-offer, at every clock, cannot resurrect it.
        for c in [base, base + 50, base + 100, base + 1_000] {
            let mut e = row(c);
            e.text = "resurrected".into();
            s.apply_remote_item(PEER, &e).unwrap();
        }
        assert!(
            !s.items_since(PEER, 0, 10).unwrap().iter().any(|r| r.origin_id == "one"),
            "order {n}: a later re-offer resurrected a deleted row"
        );
    }
}

// ---------------------------------------------------------------------------
// R8-7. The v6 half-migration. `r6_interrupted_migrations_are_re_runnable`
// predates v6 and covers v3 and v5 only.
//
// v6 is two independent guarded ALTERs. A crash between them leaves
// `tombstones.local` present and `items.local_edit` absent with the stamp still
// at 5. Re-opening twice must land on a fresh v6 schema and keep every row.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_v6_interrupted_between_its_two_alters_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    let fresh_path = dir.path().join("fresh.db");
    let fresh = Store::open(&fresh_path).unwrap();
    let fresh_shape = shape(fresh.conn_for_test());
    drop(fresh);

    let path = dir.path().join("half6.db");
    {
        // Build a real v6-capable store, then wind it back to a half-applied v6.
        let mut s = Store::open(&path).unwrap();
        s.set_device_id(ME);
        s.insert_clipboard("survivor", None, None).unwrap();
        s.apply_remote_item(PEER, &peer_item("p1", "peer row", now_ms() - 5_000))
            .unwrap();
        s.apply_remote_tombstone(
            PEER,
            &RemoteTombstone {
                source_machine: PEER.into(),
                origin_id: "gone".into(),
                deleted_at: now_ms() - 4_000,
            },
        )
        .unwrap();
        drop(s);
        let c = Connection::open(&path).unwrap();
        // Undo the second half of v6 and the stamp, keeping the first half.
        // SQLite cannot DROP COLUMN before 3.35; rusqlite's bundled build can,
        // and if it cannot the test says so rather than passing vacuously.
        c.execute_batch("ALTER TABLE items DROP COLUMN local_edit;")
            .expect("bundled SQLite must support DROP COLUMN for this test to mean anything");
        c.pragma_update(None, "user_version", 5i64).unwrap();
    }

    for pass in 1..=2 {
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.conn_for_test()
                .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            Store::SCHEMA_VERSION_FOR_TEST,
            "pass {pass}"
        );
        assert_eq!(shape(s.conn_for_test()), fresh_shape, "pass {pass}: schema drifted");
        assert_eq!(s.count().unwrap(), 2, "pass {pass}: a row was lost");
        assert_eq!(s.tombstone_count(PEER).unwrap(), 1, "pass {pass}: a delete was lost");
    }
}

/// Column shape of every table, the same comparison `adversarial_r6_data` uses.
fn shape(conn: &Connection) -> Vec<(String, Vec<(String, String, i64, Option<String>, i64)>)> {
    let names: Vec<String> = {
        let mut st = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' \
                   AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap();
        st.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    };
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

// ---------------------------------------------------------------------------
// R8-8. `clear()` and the tombstone clock it is allowed to stamp.
//
// Two halves, and the second is the interesting one.
//
// (a) Inside the climb zone — a tombstone already held for the source, below
//     `now + MAX_CLOCK_SKEW_MS / 2` — the clear must stamp STRICTLY above it,
//     or a peer whose cursor sits at that tombstone never hears about the
//     clear. This is `delete_clock`'s rule expressed in SQL and it holds.
//
// (b) Past the climb zone the code deliberately drops back to the plain wall
//     clock, which is BELOW the tombstone already held. `delete_clock` does the
//     same thing and says so, and it is the right trade: climbing further would
//     stamp a delete the receiving side refuses outright. What is asserted here
//     is that the fallback really is taken, so a future change is caught, and
//     that it is a lost delete when it happens.
//
//     The gap worth reporting is not the trade, it is that `delete_clock`
//     `tracing::warn!`s when it takes this branch and the SQL in `clear` takes
//     it in complete silence.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_clear_climbs_above_a_held_tombstone_but_gives_up_past_half_the_skew() {
    // (a) inside the climb zone.
    {
        let s = store();
        let now = now_ms();
        let held = now + MAX_CLOCK_SKEW_MS / 4; // a peer a little under a minute fast
        s.apply_remote_tombstone(
            PEER,
            &RemoteTombstone {
                source_machine: PEER.into(),
                origin_id: "already".into(),
                deleted_at: held,
            },
        )
        .unwrap();
        for i in 0..5 {
            s.apply_remote_item(PEER, &peer_item(&format!("r{i}"), "secret", now - 1_000))
                .unwrap();
        }
        s.clear(None).unwrap();
        let lowest = min_clear_clock(&s);
        assert!(
            lowest > held,
            "Clear History stamped {lowest}, at or below the {held} already \
             delivered for that source: a peer at that cursor never hears about it"
        );
    }

    // (b) past the climb zone: the documented fallback, and it DOES lose the
    //     delete to any peer whose cursor already sits at the held tombstone.
    {
        let s = store();
        let now = now_ms();
        let held = now + MAX_CLOCK_SKEW_MS - 1_000; // a peer nearly two minutes fast
        s.apply_remote_tombstone(
            PEER,
            &RemoteTombstone {
                source_machine: PEER.into(),
                origin_id: "already".into(),
                deleted_at: held,
            },
        )
        .unwrap();
        for i in 0..5 {
            s.apply_remote_item(PEER, &peer_item(&format!("r{i}"), "secret", now - 1_000))
                .unwrap();
        }
        s.clear(None).unwrap();
        let lowest = min_clear_clock(&s);
        assert!(
            lowest < held,
            "the fallback is no longer taken; if `clear` now climbs past half \
             the skew window it may be stamping deletes the peer will refuse"
        );
        // And this is what it costs: unreachable to a cursor at `held`.
        assert!(
            s.tombstones_since(PEER, held, 100).unwrap().is_empty(),
            "the cleared deletes are below the cursor and will never be offered"
        );
    }
}

fn min_clear_clock(s: &Store) -> i64 {
    s.conn_for_test()
        .query_row(
            "SELECT MIN(deleted_at) FROM tombstones \
              WHERE source_machine=?1 AND origin_id != 'already'",
            rusqlite::params![PEER],
            |r| r.get(0),
        )
        .unwrap()
}

// ---------------------------------------------------------------------------
// R8-9a. THE MINIMAL CASE THE FUZZ BELOW REDUCES TO.
//
// A cursor is a bare millisecond, and `serve` asks for everything STRICTLY
// above it on the first page of every exchange (`after_origin` starts at
// `ORIGIN_CEILING`). So any row written in the same millisecond the peer's
// cursor already sits at is, from that moment, below the cursor for ever.
// Nothing lowers a cursor.
//
// `Store::items_from`'s own doc comment says the opposite is intended:
//
//   "`at_or_after` is inclusive so a millisecond can never be split across
//    exchanges either. Re-offering the rows that share the highest clock we
//    have already seen is idempotent and costs one millisecond's worth."
//
// That protection is written into `items_from` and then bypassed at the only
// call site that matters, because `serve` passes the sentinel on the first
// page. The sequence in ordinary terms: you copy something, your machines
// sync, and you copy something else inside the same millisecond. The second
// one never reaches the other machine, silently, for ever.
//
// The loop retries until it lands two writes in one millisecond, and FAILS if
// it never manages to — a guard that cannot find its own precondition must say
// so rather than pass.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_row_written_in_the_cursors_own_millisecond_is_never_offered_again() {
    // The original loop tried 2,000 times to get two local writes into one
    // millisecond, and asserted the second was still offered. It could not pass
    // vacuously, and it does not now: it can no longer set up its own premise,
    // because `Store::local_clock` makes our own source's clocks strictly
    // increasing. Two local writes in one millisecond are unreachable.
    //
    // That closes the local half. The other half is NOT ours to control: a peer
    // can hand us two rows at the same clock, and the pair cursor is what stops
    // the second being lost. Both are pinned here.

    // 1. Our own writes never share a millisecond, however fast they come.
    let mut a = Store::open_in_memory().unwrap();
    a.set_device_id(ME);
    let mut clocks = Vec::new();
    for i in 0..50 {
        let id = a.insert_clipboard(&format!("row {i}"), None, None).unwrap();
        clocks.push(
            a.conn_for_test()
                .query_row(
                    "SELECT updated_at FROM items WHERE id=?1",
                    rusqlite::params![id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap(),
        );
    }
    let mut sorted = clocks.clone();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        clocks.len(),
        "two of our own rows share a replication clock, so a peer's cursor landing between them \
         hides the later one for ever: {clocks:?}"
    );
    assert!(clocks.windows(2).all(|w| w[1] > w[0]), "and they must be strictly increasing");

    // 2. A PEER's two rows at one clock. We cannot stop a peer doing this, so
    //    the cursor has to survive it: the first is received, the mark records
    //    (clock, origin), and the second must still be reachable.
    let mut b = Store::open_in_memory().unwrap();
    b.set_device_id(PEER);
    let shared = now_ms() - 5_000;
    // Deliberately out of origin order, so this cannot pass by luck of sorting.
    for origin in ["bbb-second", "aaa-first"] {
        b.apply_remote_item(
            ME,
            &RemoteItem {
                source_machine: ME.into(),
                origin_id: origin.into(),
                kind: "clipboard".into(),
                text: origin.into(),
                created_at: shared,
                updated_at: shared,
                pinned: false,
            },
        )
        .unwrap();
    }
    let marks = b.watermarks_paired(ME).unwrap();
    let (_, clock, origin) = marks
        .into_iter()
        .find(|(src, _, _)| src == ME)
        .expect("a receipt was banked for the peer's source");
    assert_eq!(clock, shared, "the mark holds the shared clock");
    assert_eq!(
        origin, "bbb-second",
        "the mark must hold the GREATEST origin at that clock, not the last one applied, or \
         resuming from it re-offers rows already taken"
    );

    // Resuming from the pair sees nothing more, and resuming from the clock
    // alone still sees the whole millisecond, which is the safe direction.
    let after_pair = b.items_from(ME, clock, &origin, 10).unwrap();
    assert!(after_pair.is_empty(), "resuming from the exact pair must not re-offer what we hold");
    let after_clock_only = b.items_from(ME, clock, "", 10).unwrap();
    assert_eq!(
        after_clock_only.len(),
        2,
        "an empty origin must mean 're-offer this whole millisecond', which is what a cursor \
         migrated from v6 carries"
    );
}

// ---------------------------------------------------------------------------
// R8-9c. OUR OWN CLOCK CAN RUN AHEAD OF OUR OWN WALL CLOCK, AND NOTHING BOUNDS
// IT — after which every row we write until the wall clock catches up is
// permanently invisible to any peer that has already synced.
//
// `delete_clock` solved exactly this problem for tombstones: it stamps a delete
// "strictly above every tombstone already held for that source", precisely
// because "a delete stamped in a millisecond we have already delivered sits
// below the peer's mark and is never offered. That is a lost delete." It even
// bounds itself at half the skew window so the cure cannot become the disease.
//
// The ITEM path has no equivalent. `edit_stamp` stamps `now.max(current + 1)`
// for a row we authored, so an edit can push a row's clock above our wall
// clock, and there is no ceiling on it at all. `insert_clipboard` and
// `insert_transcription` then stamp the bare `now_ms()`, which is BELOW the
// cursor the peer just banked. Nothing lowers a cursor, so those rows are gone.
//
// In ordinary terms, the realistic trigger is not the repetition below: it is a
// backwards clock step on the authoring machine — an NTP correction after a bad
// RTC, a VM or laptop resume. Every dictation and every copy made until the
// clock catches up simply never appears on the other machine, silently, with no
// warning and no repair short of the user toggling a sync kind off and on. The
// two-minute skew window does not help: it only ever judges clocks arriving
// FROM a peer, never our own, and these rows never arrive anywhere to be judged.
//
// The repetition here reaches the same state through nothing but the production
// edit path, so no clock has to be moved to prove it.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_clock_pushed_ahead_by_our_own_edits_hides_every_later_row_we_write() {
    let mut a = Store::open_in_memory().unwrap();
    a.set_device_id(ME);
    let mut b = Store::open_in_memory().unwrap();
    b.set_device_id(PEER);

    let id = a.insert_clipboard("early", None, None).unwrap();
    // Each edit of a row we authored stamps `now.max(current + 1)`, and an edit
    // is far faster than a millisecond, so the row's clock climbs away from the
    // wall clock. No ceiling applies.
    for i in 0..3_000 {
        a.set_pinned(id, i % 2 == 0).unwrap();
    }
    let ahead = clock_of(&a, id);
    let wall = now_ms();
    assert!(
        ahead > wall + 500,
        "the premise failed: our own row is only {} ms ahead of the wall clock, \
         so this test proves nothing",
        ahead - wall
    );

    // The peer syncs and banks that clock as its cursor for us.
    push(&a, &b, ME, PEER);
    let banked = b
        .watermarks(ME)
        .unwrap()
        .into_iter()
        .find(|(src, _)| src == ME)
        .map(|(_, c)| c)
        .expect("B recorded a receipt for A");
    assert_eq!(banked, ahead, "B's cursor for A is the inflated clock");

    // An entirely ordinary capture, stamped with the real wall clock.
    let later = a.insert_clipboard("the next thing you copy", None, None).unwrap();
    let later_clock = clock_of(&a, later);
    if later_clock > banked {
        // The defect is gone: local inserts now stamp above what we have
        // already published for our own source, the way `delete_clock` does for
        // tombstones. Nothing left to prove.
        return;
    }

    for _ in 0..5 {
        push(&a, &b, ME, PEER);
        push(&b, &a, PEER, ME);
    }
    let on_b: Vec<String> = b.recent(None, 100).unwrap().into_iter().map(|r| r.text).collect();
    assert!(
        on_b.iter().any(|t| t == "the next thing you copy"),
        "a row written at {later_clock} is below the peer's cursor of {banked} \
         and was never offered. B holds {on_b:?}"
    );
}

fn clock_of(s: &Store, id: i64) -> i64 {
    s.conn_for_test()
        .query_row(
            "SELECT updated_at FROM items WHERE id=?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap()
}

// ---------------------------------------------------------------------------
// R8-9b. Two devices, ordinary sequence, no hostility: they must agree.
//
// Store-level model of a two-device exchange. Every row is applied through the
// same `apply_remote_*` the wire uses, with the author-only rule enforced by
// only ever handing each store the OTHER store's own rows.
//
// This is the fuzz that found R8-9a. It writes fast enough that many rows share
// a millisecond, which is exactly the condition R8-9a isolates, so it fails for
// the same single reason.
// ---------------------------------------------------------------------------
#[test]
fn r8_two_stores_converge_over_a_long_random_sequence() {
    let mut a = Store::open_in_memory().unwrap();
    a.set_device_id(ME);
    let mut b = Store::open_in_memory().unwrap();
    b.set_device_id(PEER);

    // A cheap deterministic PRNG; no dev-dependency needed.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    // How often two local writes landed in the same millisecond. If that never
    // happens the run cannot exercise the boundary at all, and a pass would be
    // vacuous — so it is asserted, not assumed. Under a loaded test binary the
    // wall clock ticks between writes far more often, and an earlier version of
    // this test passed for exactly that reason.
    let mut collisions = 0usize;
    let mut last = (0i64, 0i64);
    // Every clock each side has stamped on its own source, to prove they are
    // strictly increasing.
    let (mut seen_a, mut seen_b): (Vec<i64>, Vec<i64>) = (Vec::new(), Vec::new());
    for round in 0..200 {
        // Each side does a little local work.
        for (ix, (s, me)) in [(&a, ME), (&b, PEER)].into_iter().enumerate() {
            match next() % 4 {
                0 => {
                    // Counted on the WALL clock, not the stored clock.
                    //
                    // The guard's purpose is to prove the run went fast enough
                    // to reach the same-millisecond boundary, and it used to
                    // read that off the row's stored clock. It cannot any more:
                    // `Store::local_clock` stamps strictly above everything this
                    // machine holds for its own source, so two local writes
                    // NEVER share a stored clock by construction. That is the
                    // fix for the defect this test found, and measuring it
                    // would make the guard permanently unsatisfiable.
                    //
                    // The wall clock still collides, which is exactly the
                    // condition the boundary needs, so the guard keeps its
                    // meaning: it fails if the machine was too slow to probe.
                    let before = now_ms();
                    let id = s.insert_clipboard(&format!("{me}-{round}"), None, None).unwrap();
                    let stored = clock_of(s, id);
                    let prev = if ix == 0 { &mut last.0 } else { &mut last.1 };
                    if before == *prev {
                        collisions += 1;
                    }
                    *prev = before;
                    // And the stored clock must never repeat or go backwards,
                    // which is what makes the collision above harmless.
                    let seen = if ix == 0 { &mut seen_a } else { &mut seen_b };
                    assert!(
                        seen.iter().all(|c| *c < stored),
                        "a local write reused or went below a clock this machine had already \
                         stamped for its own source: {stored} against {seen:?}"
                    );
                    seen.push(stored);
                }
                1 => {
                    if let Some(r) = s.recent(None, 20).unwrap().first() {
                        s.set_pinned(r.id, !r.pinned).unwrap();
                    }
                }
                2 => {
                    if let Some(r) = s.recent(None, 20).unwrap().first() {
                        s.delete_item_local(r.id).unwrap();
                    }
                }
                _ => {}
            }
        }
        // Exchange, author-only for items, every source for tombstones.
        push(&a, &b, ME, PEER);
        push(&b, &a, PEER, ME);
    }

    // Settle: keep exchanging until nothing moves, bounded.
    let mut quiet = 0;
    for _ in 0..40 {
        let before = (fingerprint(&a), fingerprint(&b));
        push(&a, &b, ME, PEER);
        push(&b, &a, PEER, ME);
        if (fingerprint(&a), fingerprint(&b)) == before {
            quiet += 1;
            if quiet >= 2 {
                break;
            }
        } else {
            quiet = 0;
        }
    }
    assert!(quiet >= 2, "the pair never went quiet");
    // The boundary is FORCED above, not hoped for.
    //
    // This used to assert that two local writes happened to land in the same
    // wall-clock millisecond. That is a timing coincidence, and the reviewer's
    // own comment records it cutting both ways: under a loaded test binary the
    // clock ticks between writes and the guard fails, while on an idle one it
    // passes without proving much. It fails in the full suite and passes alone,
    // which is the worst of both.
    //
    // What replaces it is deterministic and stronger: every local write
    // asserted, in the loop above, that its clock was strictly above every
    // clock this machine had already stamped for its own source. That is the
    // invariant which makes a same-millisecond collision harmless, and it is
    // checked on all 200 rounds rather than on whichever ones happened to race.
    //
    // The peer-side case, two of a PEER's rows arriving at ONE clock, is
    // covered deterministically by
    // `r8_a_row_written_in_the_cursors_own_millisecond_is_never_offered_again`,
    // which constructs it directly instead of hoping a fuzz run produces it.
    let _ = collisions;
    assert_eq!(
        fingerprint(&a),
        fingerprint(&b),
        "two devices ended a plain sequence holding different history. The \
         deterministic reductions are R8-9a (a row written in the cursor's own \
         millisecond) and R8-9c (our own clock pushed above our wall clock by an \
         edit); {collisions} same-millisecond writes occurred in this run"
    );
}

/// One direction of an exchange, modelled exactly as `serve` + `drain` do it.
///
/// The cursors are the RECEIVER's own `source_marks`, read back through
/// `watermarks(from_id)`, which is precisely what the receiver advertises and
/// what `floor_for` consumes — one per (peer, source), never one shared
/// counter. Items go author-only, tombstones for every source, and the
/// receiving side applies `drain`'s authored / already-held gate.
fn push(from: &Store, to: &Store, from_id: &str, to_id: &str) {
    // The cursor is a PAIR, as production's is.
    //
    // This helper used to read `watermarks()` and resume with `items_since` /
    // `tombstones_since`, which resume after the WHOLE millisecond. That was a
    // faithful model of `serve` at the time and it is not any more: the cursor
    // now carries the origin id of the newest row received, so a run resumes
    // exactly where it stopped rather than skipping the remainder of a
    // millisecond it had only partly delivered. Modelling the old shape here
    // would keep reporting a defect that no longer exists.
    let marks: std::collections::HashMap<String, (i64, String)> = to
        .watermarks_paired(from_id)
        .unwrap()
        .into_iter()
        .map(|(src, clock, origin)| (src, (clock, origin)))
        .collect();
    let floor = |src: &str| -> (i64, String) {
        marks.get(src).cloned().unwrap_or((0, String::new()))
    };

    // ITEMS: only rows `from` authored.
    let (after, after_origin) = floor(from_id);
    for it in from.items_from(from_id, after, &after_origin, 1_000).unwrap() {
        to.apply_remote_item(from_id, &it).unwrap();
    }

    // TOMBSTONES: every source `from` holds anything for.
    for src in from.known_sources().unwrap() {
        let (after, after_origin) = floor(&src);
        for t in from.tombstones_from(&src, after, &after_origin, 1_000).unwrap() {
            let authored = src == from_id && src != to_id;
            let held = to.holds_identity(&t.source_machine, &t.origin_id).unwrap();
            if authored || held {
                to.apply_remote_tombstone(from_id, &t).unwrap();
            }
            // A relayed delete for an identity we do not hold is refused. In a
            // two-device mesh both sources are always reachable, so no receipt
            // is banked — the same branch `drain` takes.
        }
    }
}

/// What a user would see: the live rows, and every delete we know about.
fn fingerprint(s: &Store) -> (Vec<(String, bool)>, Vec<(String, String)>) {
    let mut rows: Vec<(String, bool)> = s
        .recent(None, 10_000)
        .unwrap()
        .into_iter()
        .map(|r| (r.text, r.kind == HistoryKind::Clipboard))
        .collect();
    rows.sort();
    let mut tombs: Vec<(String, String)> = {
        let mut st = s
            .conn_for_test()
            .prepare("SELECT source_machine, origin_id FROM tombstones")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    tombs.sort();
    (rows, tombs)
}

// ---------------------------------------------------------------------------
// R8-10. A third device's rows must never be brought into existence by the
// store on behalf of a peer that did not author them — checked here at the
// STORE boundary, since `Attribution` lives one crate up and a future caller
// could reasonably assume the store defends itself.
//
// This is a read-only observation turned into an assertion of what IS true, so
// that a change in either layer is caught: the store deliberately does NOT
// check authority, and everything therefore rests on `drain`.
// ---------------------------------------------------------------------------
#[test]
fn r8_the_store_itself_enforces_no_authority_which_is_load_bearing_upstream() {
    let s = store();
    let now = now_ms() - 1_000;
    let mut forged = peer_item("forged", "not mine", now);
    forged.source_machine = THIRD.into();
    assert_eq!(
        s.apply_remote_item(PEER, &forged).unwrap(),
        ApplyOutcome::Inserted,
        "the store applies whatever it is given; `Attribution::may_create` in \
         replicate.rs is the only thing that stops this, and must never be \
         bypassed by a new call site"
    );
    // And the receipt lands under the PEER that handed it over, not the source.
    let marks = s.watermarks(PEER).unwrap();
    assert!(marks.iter().any(|(src, _)| src == THIRD));
    assert!(s.watermarks(THIRD).unwrap().is_empty());
}
