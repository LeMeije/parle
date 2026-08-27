//! ADVERSARIAL REVIEW — ROUND 7. The store rules round 6 introduced.
//!
//! Round 6 added three things to the store: a `local` flag on tombstones so the
//! per-source cap cannot evict a delete the user made, a `local_edit` flag on
//! items so an echo cannot revert an edit the user made, and `delete_clock` so
//! a delete's clock can never fall below one already delivered.
//!
//! Every one of those replaced a rule that looked right, which is the pattern
//! this project keeps hitting, so they are attacked here rather than assumed.
//! Two of these tests failed against the first version of the round-6 code.

#![cfg(test)]

use crate::history::{
    ApplyOutcome, RemoteItem, RemoteTombstone, Store, MAX_CLOCK_SKEW_MS,
    MAX_TOMBSTONES_PER_SOURCE,
};
use crate::types::{HistoryKind, TranscriptionResult};

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

fn peer_row(origin: &str, text: &str, clock: i64) -> RemoteItem {
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

fn rowid_of(s: &Store, origin: &str) -> i64 {
    s.conn_for_test()
        .query_row("SELECT id FROM items WHERE origin_id=?1", rusqlite::params![origin], |r| r.get(0))
        .unwrap()
}

fn tomb_clock(s: &Store, origin: &str) -> i64 {
    s.conn_for_test()
        .query_row(
            "SELECT deleted_at FROM tombstones WHERE origin_id=?1",
            rusqlite::params![origin],
            |r| r.get(0),
        )
        .unwrap()
}

// ---------------------------------------------------------------------------
// R7-S1. `clear()` and `delete_item_local()` must stamp the same clock from the
// same state.
//
// They are two SQL paths writing one field, and they disagreed: the Rust one
// dropped back to the wall clock when the monotonic chain would pass the
// ceiling, while the SQL one clamped to the ceiling itself — a full minute in
// the future, on every row of a Clear, for no reason. Two writers of one field
// that disagree is how a cursor ends up somewhere neither path intended.
// ---------------------------------------------------------------------------
#[test]
fn r7_clear_and_single_delete_stamp_the_same_clock_from_the_same_state() {
    // A tombstone already sitting near the ceiling, which is what triggers the
    // fallback in both paths.
    let near_ceiling = now_ms() + MAX_CLOCK_SKEW_MS - 1_000;

    let a = store();
    a.apply_remote_item(PEER, &peer_row("keep-1", "x", now_ms() - 5_000)).unwrap();
    a.apply_remote_tombstone(
        PEER,
        &RemoteTombstone {
            source_machine: PEER.into(),
            origin_id: "high".into(),
            deleted_at: near_ceiling,
        },
    )
    .unwrap();

    let b = store();
    b.apply_remote_item(PEER, &peer_row("keep-1", "x", now_ms() - 5_000)).unwrap();
    b.apply_remote_tombstone(
        PEER,
        &RemoteTombstone {
            source_machine: PEER.into(),
            origin_id: "high".into(),
            deleted_at: near_ceiling,
        },
    )
    .unwrap();

    a.delete_item_local(rowid_of(&a, "keep-1")).unwrap();
    b.clear(None).unwrap();

    let single = tomb_clock(&a, "keep-1");
    let cleared = tomb_clock(&b, "keep-1");
    assert!(
        (single - cleared).abs() <= 50,
        "a single delete stamped {single} and a Clear stamped {cleared} from identical state; \
         two paths writing one field must agree"
    );
    // And neither may exceed what a peer will accept.
    for (what, clock) in [("single delete", single), ("clear", cleared)] {
        assert!(
            clock <= now_ms() + MAX_CLOCK_SKEW_MS,
            "{what} stamped {clock}, past the ceiling a peer refuses"
        );
    }
}

// ---------------------------------------------------------------------------
// R7-S2. A delete's clock never falls at or below one already stamped for that
// source, however the two were made.
// ---------------------------------------------------------------------------
#[test]
fn r7_delete_clocks_for_one_source_never_go_backwards() {
    let s = store();
    // A row from a peer whose clock runs fast, well inside the accepted skew.
    let fast = now_ms() + 90_000;
    s.apply_remote_item(PEER, &peer_row("fast", "x", fast)).unwrap();
    s.apply_remote_item(PEER, &peer_row("normal", "y", now_ms() - 10_000)).unwrap();

    s.delete_item_local(rowid_of(&s, "fast")).unwrap();
    let first = tomb_clock(&s, "fast");
    s.delete_item_local(rowid_of(&s, "normal")).unwrap();
    let second = tomb_clock(&s, "normal");

    assert!(
        second > first,
        "the second delete is stamped {second}, at or below the first at {first}: \
         a peer's cursor sits at the first, so the second is never offered again"
    );
    assert!(second <= now_ms() + MAX_CLOCK_SKEW_MS, "and it must still be deliverable");
}

// ---------------------------------------------------------------------------
// R7-S3. The per-source tombstone ceiling evicts REPLICATED entries only, and
// keeps evicting them: the protection must not become a leak that stops the cap
// working at all.
//
// The ORDERING here is the whole test and the first version of it got the
// ordering wrong. It flooded with replicated tombstones dated in the past, so
// they were the oldest and `ORDER BY deleted_at ASC` evicted them first with or
// without the `local = 0` filter — the test passed against the unfixed code and
// proved nothing.
//
// The real sequence is the reverse, and it is the one that hurts: Clear History
// writes local tombstones at NOW, and the tombstones that arrive afterwards from
// a peer are NEWER. The user's own undelivered deletes are then the oldest rows
// in the table, and they are exactly what an unfiltered eviction takes.
// ---------------------------------------------------------------------------
#[test]
fn r7_the_tombstone_cap_evicts_replicated_entries_and_never_local_ones() {
    let s = store();
    let base = now_ms() - 10_000_000;

    // 60 rows from the peer, which the user then clears. These are stamped now.
    for i in 0..60 {
        s.apply_remote_item(PEER, &peer_row(&format!("mine-{i:04}"), "x", base + i)).unwrap();
    }
    s.clear(None).unwrap();
    assert_eq!(s.tombstone_count(PEER).unwrap(), 60);
    let oldest_local = s
        .conn_for_test()
        .query_row(
            "SELECT MIN(deleted_at) FROM tombstones WHERE local = 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();

    // Then far more replicated tombstones than the ceiling allows, every one of
    // them NEWER than the local ones, which is what a live peer produces.
    let flood = MAX_TOMBSTONES_PER_SOURCE as usize + 200;
    for i in 0..flood {
        let clock = oldest_local + 1 + (i as i64 % (MAX_CLOCK_SKEW_MS / 2));
        s.apply_remote_tombstone(
            PEER,
            &RemoteTombstone {
                source_machine: PEER.into(),
                origin_id: format!("theirs-{i:06}"),
                deleted_at: clock,
            },
        )
        .unwrap();
    }
    // The cap holds the TOTAL at the ceiling, so "still over it" is the wrong
    // precondition. What must be true is that eviction actually ran — a test
    // that can only ever find nothing is not a test.
    let replicated: i64 = s
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM tombstones WHERE local = 0", [], |r| r.get(0))
        .unwrap();
    assert!(
        replicated < flood as i64,
        "precondition: the flood must trigger eviction; {replicated} of {flood} survived untouched"
    );

    // Every local delete survives.
    let surviving_local: i64 = s
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM tombstones WHERE local = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        surviving_local, 60,
        "{} of 60 local deletes were evicted by the cap; nothing re-creates a tombstone, \
         so those rows walk back in on the author's next re-offer",
        60 - surviving_local
    );
    for i in 0..60 {
        assert!(
            s.holds_identity(PEER, &format!("mine-{i:04}")).unwrap(),
            "local delete mine-{i:04} was evicted"
        );
    }

    // And the cap still did its job on the entries it IS allowed to drop, or
    // protecting local deletes has simply disabled the ceiling a peer controls.
    assert!(
        replicated <= MAX_TOMBSTONES_PER_SOURCE,
        "{replicated} replicated tombstones survived a ceiling of {MAX_TOMBSTONES_PER_SOURCE}: \
         a paired device has unbounded control of our disk"
    );
}

// ---------------------------------------------------------------------------
// R7-S4. `local_edit` protects a user's edit from an echo, and NOTHING else.
//
// The TEXTS matter and the first version of this test got them wrong. The tie
// is broken by the payload order when `local_edit` is clear, so a test whose
// edited text happens to sort ABOVE the original passes with the protection
// removed — it was "as recorded" against "as the user fixed it", and 'r' sorts
// below 't', so the echo lost on its own and proved nothing.
//
// Here the original sorts ABOVE the edit, so without the flag the echo wins the
// tiebreak and reverts the user. That is the case that has to be pinned.
// ---------------------------------------------------------------------------
#[test]
fn r7_a_local_edit_survives_an_echo_but_loses_to_a_newer_author_change() {
    let s = store();
    let c = now_ms() - 10_000;
    let row = peer_row("r", "zebra, as recorded", c);
    assert_eq!(s.apply_remote_item(PEER, &row).unwrap(), ApplyOutcome::Inserted);

    let id = rowid_of(&s, "r");
    s.set_pinned(id, true).unwrap();
    s.update_text(id, "aardvark, as the user fixed it").unwrap();
    assert!(
        "zebra, as recorded" > "aardvark, as the user fixed it",
        "precondition: the echo must WIN the payload tiebreak, or this test proves nothing"
    );

    // The clock must NOT have moved: it is the author's, and moving it swallows
    // the author's own later correction.
    let clock: i64 = s
        .conn_for_test()
        .query_row("SELECT updated_at FROM items WHERE id=?1", rusqlite::params![id], |r| r.get(0))
        .unwrap();
    assert_eq!(clock, c, "a local edit on a peer's row moved the author's clock");

    // An unchanged echo must not revert it.
    s.apply_remote_item(PEER, &row).unwrap();
    let got = s.get(id).unwrap().unwrap();
    assert_eq!(
        (got.text.as_str(), got.pinned),
        ("aardvark, as the user fixed it", true),
        "an unchanged echo reverted the user's edit"
    );

    // A genuinely newer change from the author wins, and clears the protection
    // so the row is not immune to ties for ever.
    let newer = peer_row("r", "the author's own correction", c + 1);
    assert_eq!(s.apply_remote_item(PEER, &newer).unwrap(), ApplyOutcome::Updated);
    assert_eq!(s.get(id).unwrap().unwrap().text, "the author's own correction");
    let flag: i64 = s
        .conn_for_test()
        .query_row("SELECT local_edit FROM items WHERE id=?1", rusqlite::params![id], |r| r.get(0))
        .unwrap();
    assert_eq!(flag, 0, "local_edit must be cleared once the author has overwritten the edit");

    // With the flag cleared, the payload tiebreak governs again, so a row that
    // was once locally edited is not permanently immune to a tie.
    let tie = peer_row("r", "zzz a later tie", c + 1);
    assert_eq!(s.apply_remote_item(PEER, &tie).unwrap(), ApplyOutcome::Updated);
}

// ---------------------------------------------------------------------------
// R7-S5. Our OWN rows are untouched by all of the above: a local edit still
// bumps the clock, because that edit has to travel.
// ---------------------------------------------------------------------------
#[test]
fn r7_a_local_edit_of_our_own_row_still_wins_and_still_travels() {
    let s = store();
    s.insert_transcription(&tr("mine"), None, None).unwrap();
    let id: i64 = s.recent(None, 1).unwrap()[0].id;
    let before: i64 = s
        .conn_for_test()
        .query_row("SELECT updated_at FROM items WHERE id=?1", rusqlite::params![id], |r| r.get(0))
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(3));
    s.set_pinned(id, true).unwrap();

    let after: i64 = s
        .conn_for_test()
        .query_row("SELECT updated_at FROM items WHERE id=?1", rusqlite::params![id], |r| r.get(0))
        .unwrap();
    assert!(after > before, "an edit of our OWN row must bump the clock, or it never replicates");

    let flag: i64 = s
        .conn_for_test()
        .query_row("SELECT local_edit FROM items WHERE id=?1", rusqlite::params![id], |r| r.get(0))
        .unwrap();
    assert_eq!(flag, 0, "our own edits are not 'local edits'; they are the authoritative ones");

    // And it is visible to replication above the old clock.
    let served = s.items_from(ME, before, "\u{FFFF}", 10).unwrap();
    assert_eq!(served.len(), 1, "the edited row must be offered to peers");
}

// ---------------------------------------------------------------------------
// R7-S6. A kind-scoped Clear obeys the same clock rule as a full one.
//
// It is a SEPARATE SQL statement from the full clear, so it can regress on its
// own. This asserts monotonicity, not just plausibility.
// ---------------------------------------------------------------------------
#[test]
fn r7_a_kind_scoped_clear_stamps_a_deliverable_monotonic_clock() {
    let s = store();
    // A delete already stamped for this source, which the clear must clear.
    s.apply_remote_item(PEER, &peer_row("earlier", "x", now_ms() - 20_000)).unwrap();
    s.delete_item_local(rowid_of(&s, "earlier")).unwrap();
    let earlier = tomb_clock(&s, "earlier");

    s.apply_remote_item(PEER, &peer_row("clip", "y", now_ms() - 5_000)).unwrap();
    s.clear(Some(HistoryKind::Clipboard)).unwrap();

    let c = tomb_clock(&s, "clip");
    assert!(
        c > earlier,
        "a kind-scoped clear stamped {c}, at or below the {earlier} already delivered for \
         this source: a peer's cursor sits there and this delete is never offered"
    );
    assert!(c <= now_ms() + MAX_CLOCK_SKEW_MS, "a kind clear stamped {c}, past what a peer accepts");
    assert!(s.holds_identity(PEER, "clip").unwrap(), "the clear must write a tombstone");

    // And it must be a LOCAL one, or the cap may drop it.
    let local: i64 = s
        .conn_for_test()
        .query_row("SELECT local FROM tombstones WHERE origin_id='clip'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(local, 1, "a kind-scoped clear wrote a tombstone the cap is allowed to evict");
}
