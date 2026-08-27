//! ADVERSARIAL REVIEW, ROUND 13. Data integrity, store side.
//!
//! Target: the three things round 12 changed in `history.rs`.
//!
//!   * `edit_stamp` now returns
//!     `local_clock_at(now).max(current.saturating_add(1))`, which is the
//!     first and only stamp in this file allowed to sit ABOVE the ceiling
//!     `next_clock_impl` clamps every other stamp to.
//!   * `insert_transcription_local_only` now wraps an `unchecked_transaction`
//!     around a call that used to commit on its own.
//!   * `delete_item_local` and `clear` now match on `local_only = 0`, so a
//!     withheld dictation mints no tombstone.
//!
//! The exchange-level twins live in `src-tauri/src/sync/adversarial_r13_data.rs`.

#![cfg(test)]

use crate::history::{ApplyOutcome, RemoteItem, Store, MAX_CLOCK_SKEW_MS};
use crate::types::{HistoryKind, TranscriptionResult};

const ME: &str = "11111111-1111-4111-8111-111111111111";
const PEER: &str = "22222222-2222-4222-8222-222222222222";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}

fn store_as(me: &str) -> Store {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(me);
    s
}

fn store() -> Store {
    store_as(ME)
}

fn a_dictation(text: &str) -> TranscriptionResult {
    TranscriptionResult {
        raw_text: text.to_string(),
        text: text.to_string(),
        language: Some("en".into()),
        model_id: "test".into(),
        duration_ms: 10,
        transcribe_ms: 5,
        segments: Vec::new(),
        trimmed: Vec::new(),
        low_confidence: Vec::new(),
        cleanup_tier: 0,
    }
}

fn clock_of(s: &Store, id: i64) -> i64 {
    s.conn_for_test()
        .query_row("SELECT updated_at FROM items WHERE id = ?1", rusqlite::params![id], |r| r.get(0))
        .unwrap()
}

fn local_only_of(s: &Store, id: i64) -> Option<i64> {
    s.conn_for_test()
        .query_row("SELECT local_only FROM items WHERE id = ?1", rusqlite::params![id], |r| r.get(0))
        .ok()
}

fn row_count(s: &Store) -> i64 {
    s.conn_for_test().query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)).unwrap()
}

fn tombstone_total(s: &Store) -> i64 {
    s.conn_for_test().query_row("SELECT COUNT(*) FROM tombstones", [], |r| r.get(0)).unwrap()
}

fn text_held_for(s: &Store, source: &str, origin: &str) -> Option<String> {
    s.conn_for_test()
        .query_row(
            "SELECT text FROM items WHERE source_machine = ?1 AND origin_id = ?2",
            rusqlite::params![source, origin],
            |r| r.get(0),
        )
        .ok()
}

/// Model a wall clock that has stepped BACKWARDS by `by_ms`.
///
/// `now_ms()` cannot be moved from inside the process, so the step is applied
/// to the only durable thing it changes on the authoring machine: the clocks
/// already stamped on our own rows, which were the wall clock when they were
/// written. Identical modelling to `adversarial_r11_data::step_the_authors_clock_back`
/// and `adversarial_r12_data::step_our_clock_back`, neither of which is touched.
fn step_our_clock_back(s: &Store, me: &str, by_ms: i64) -> i64 {
    let t_high = now_ms() + by_ms;
    s.conn_for_test()
        .execute(
            "UPDATE items SET created_at = ?1, updated_at = ?1 WHERE source_machine = ?2",
            rusqlite::params![t_high, me],
        )
        .unwrap();
    t_high
}

/// The row exactly as `serve` would put it on the wire.
fn as_offered(s: &Store, source: &str, id: i64) -> RemoteItem {
    let (origin, _) = s.origin_and_text_for_test(id).unwrap();
    let (kind, text, created, updated, pinned): (String, String, i64, i64, i64) = s
        .conn_for_test()
        .query_row(
            "SELECT kind, text, created_at, updated_at, pinned FROM items WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    RemoteItem {
        source_machine: source.to_string(),
        origin_id: origin,
        kind,
        text,
        created_at: created,
        updated_at: updated,
        pinned: pinned != 0,
    }
}

// ===========================================================================
// R13-D1. THE EDIT STAMP IS THE ONE STAMP IN THE STORE ALLOWED ABOVE THE
//         CEILING, AND `apply_remote_item` REFUSES EXACTLY THAT.
//
// Round 12 changed `edit_stamp` to `max(clamped, current + 1)` because the
// clamped value alone walked the row's own clock DOWN after a backwards clock
// step, and the peer then refused the correction on last-writer-wins for ever.
//
// The half round 12 did not state is that `current + 1` is bounded by nothing.
// Where `current` is already above `now + MAX_CLOCK_SKEW_MS`, the edit is
// stamped above it too, and `apply_remote_item` refuses a row above the
// receiver's ceiling.
//
// Two cases, landing on opposite sides, so neither extreme passes both.
// ===========================================================================

/// A backwards step INSIDE the skew window. Round 12's win, and the test that
/// fails if you put round 11's `edit_stamp` back.
#[test]
fn r13_data_a_correction_inside_the_skew_window_now_reaches_the_peer() {
    let a = store();
    let b = store_as(PEER);

    let id = a.insert_transcription(&a_dictation("the original"), None, None).unwrap();
    let offered = as_offered(&a, ME, id);
    assert_eq!(
        b.apply_remote_item(ME, &offered).unwrap(),
        ApplyOutcome::Inserted,
        "premise: the peer has the row before the clock moves"
    );
    // The peer's copy carries the clock it was given, which is the pre-step one.
    let high = step_our_clock_back(&a, ME, MAX_CLOCK_SKEW_MS / 2);
    b.conn_for_test()
        .execute(
            "UPDATE items SET created_at = ?1, updated_at = ?1 WHERE source_machine = ?2",
            rusqlite::params![high, ME],
        )
        .unwrap();

    a.update_text(id, "the correction").unwrap();
    let stamped = clock_of(&a, id);
    assert!(
        stamped > high,
        "an edit must move the row's own clock UP; it went from {high} to {stamped}"
    );
    assert!(
        stamped <= now_ms() + MAX_CLOCK_SKEW_MS,
        "guard integrity: this case is supposed to stay INSIDE the ceiling, or it is \
         measuring the other case; stamped {stamped}"
    );

    assert_eq!(
        b.apply_remote_item(ME, &as_offered(&a, ME, id)).unwrap(),
        ApplyOutcome::Updated,
        "the correction must reach the peer after a backwards step inside the skew window"
    );
    assert_eq!(
        text_held_for(&b, ME, &offered.origin_id).as_deref(),
        Some("the correction"),
        "the peer kept the old text"
    );
}

/// A backwards step LARGER than the skew window. Round 11 refused this on
/// last-writer-wins, so "the peer refuses" alone discriminates nothing. What
/// discriminates is the CLOCK: round 11 could not stamp above the ceiling and
/// round 12 does.
#[test]
fn r13_data_a_correction_past_the_skew_window_is_stamped_above_every_peers_ceiling() {
    let a = store();
    let b = store_as(PEER);

    let id = a.insert_transcription(&a_dictation("the original"), None, None).unwrap();
    let offered = as_offered(&a, ME, id);
    assert_eq!(
        b.apply_remote_item(ME, &offered).unwrap(),
        ApplyOutcome::Inserted,
        "premise: the peer has the row before the clock moves"
    );

    // Three skew windows back: six minutes, which is an ordinary NTP
    // correction after a flat RTC. The peer's copy moved with it, because it
    // accepted the row while the clocks still agreed.
    let high = step_our_clock_back(&a, ME, 3 * MAX_CLOCK_SKEW_MS);
    b.conn_for_test()
        .execute(
            "UPDATE items SET created_at = ?1, updated_at = ?1 WHERE source_machine = ?2",
            rusqlite::params![high, ME],
        )
        .unwrap();

    a.update_text(id, "the correction").unwrap();
    let stamped = clock_of(&a, id);

    // THE DISCRIMINATOR.
    let ceiling = now_ms() + MAX_CLOCK_SKEW_MS;
    assert!(
        stamped > ceiling,
        "round 12 stamps an edit above the ceiling every other stamp in this file is \
         clamped to; stamped {stamped}, ceiling {ceiling}"
    );

    // And that is precisely what `apply_remote_item` throws away.
    assert_eq!(
        b.apply_remote_item(ME, &as_offered(&a, ME, id)).unwrap(),
        ApplyOutcome::Ignored,
        "a peer with a correct clock must refuse a row stamped past its ceiling"
    );
    assert_eq!(
        text_held_for(&b, ME, &offered.origin_id).as_deref(),
        Some("the original"),
        "guard integrity: if the peer had taken the row there would be nothing to refuse"
    );

    // The refusal is NOT the permanent one round 11 produced, and this is the
    // difference between the two rules stated as an assertion rather than as a
    // paragraph. Shift the whole picture down by the size of the clock error,
    // which is what "the wall clock caught up" looks like at the receiving end,
    // and offer both stamps against the copy the peer holds.
    let shift = 3 * MAX_CLOCK_SKEW_MS;
    b.conn_for_test()
        .execute(
            "UPDATE items SET created_at = ?1, updated_at = ?1 WHERE source_machine = ?2",
            rusqlite::params![high - shift, ME],
        )
        .unwrap();

    // Round 11's stamp was the clamped value, which after this backwards step
    // is BELOW the row's own clock. It loses on last-writer-wins whatever the
    // wall clock does.
    let mut round_eleven = as_offered(&a, ME, id);
    round_eleven.updated_at = high - shift - 1;
    round_eleven.created_at = high - shift - 1;
    assert_eq!(
        b.apply_remote_item(ME, &round_eleven).unwrap(),
        ApplyOutcome::Ignored,
        "guard integrity: round 11's lowered stamp loses last-writer-wins, permanently"
    );

    // Round 12's stamp is above it, so once it is reachable it lands.
    let mut round_twelve = as_offered(&a, ME, id);
    round_twelve.updated_at = stamped - shift;
    round_twelve.created_at = stamped - shift;
    assert_eq!(
        b.apply_remote_item(ME, &round_twelve).unwrap(),
        ApplyOutcome::Updated,
        "round 12's stamp must land once the wall clock has caught up to it"
    );
}

// ===========================================================================
// R13-D2. ONCE A ROW IS ABOVE THE CEILING, EVERY FURTHER EDIT CLIMBS BY ONE
//         WITH NOTHING TO STOP IT.
//
// The walk is bounded in practice, because the ceiling climbs a millisecond
// per millisecond and so does the edit. What it proves is that `edit_stamp`
// has no ceiling of its own, and that both of the history window's ordinary
// actions reach it.
// ===========================================================================

#[test]
fn r13_data_every_further_edit_climbs_by_one_with_no_ceiling() {
    let a = store();
    let id = a.insert_transcription(&a_dictation("v0"), None, None).unwrap();
    let high = step_our_clock_back(&a, ME, 3 * MAX_CLOCK_SKEW_MS);

    let mut seen = Vec::new();
    for i in 0..5 {
        a.update_text(id, &format!("v{}", i + 1)).unwrap();
        seen.push(clock_of(&a, id));
    }
    assert_eq!(
        seen,
        (1..=5).map(|k| high + k).collect::<Vec<_>>(),
        "each edit past the ceiling adds exactly one, unclamped, starting from {high}"
    );

    // A pin takes the same stamp, so the walk is reachable from the two most
    // ordinary actions in the history window and not from editing alone.
    a.set_pinned(id, true).unwrap();
    assert_eq!(clock_of(&a, id), high + 6, "a pin walks the clock too");
}

#[test]
fn r13_data_a_row_above_the_ceiling_does_not_drag_the_rest_of_the_source_with_it() {
    let a = store();
    let id = a.insert_transcription(&a_dictation("v0"), None, None).unwrap();
    step_our_clock_back(&a, ME, 3 * MAX_CLOCK_SKEW_MS);
    a.update_text(id, "v1").unwrap();
    assert!(
        clock_of(&a, id) > now_ms() + MAX_CLOCK_SKEW_MS,
        "guard integrity: the premise of this test is a row above the ceiling"
    );

    // Everything that still goes through `next_clock_impl` is still clamped,
    // so the poison does not spread from the edited row to anything else.
    let fresh = a.insert_clipboard("written afterwards", None, None).unwrap();
    let fresh_clock = clock_of(&a, fresh);
    assert!(
        fresh_clock <= now_ms() + MAX_CLOCK_SKEW_MS,
        "a fresh row must stay inside the ceiling; got {fresh_clock}"
    );
    assert!(
        fresh_clock < clock_of(&a, id),
        "guard integrity: the fresh row must sit BELOW the poisoned one, or the clamp \
         is not being exercised"
    );

    // And the delete of the poisoned row is still stamped inside the ceiling,
    // so the delete travels even where the edit could not.
    a.delete_item_local(id).unwrap();
    let t = a.tombstones_since(ME, 0, 10).unwrap();
    assert_eq!(t.len(), 1, "the delete must still mint a tombstone");
    assert!(
        t[0].deleted_at <= now_ms() + MAX_CLOCK_SKEW_MS,
        "the tombstone must stay inside the ceiling; got {}",
        t[0].deleted_at
    );
}

// ===========================================================================
// R13-D3. THE WITHHELD DICTATION AND ITS ONE TRANSACTION.
//
// Round 12 wrapped `insert_transcription` in an `unchecked_transaction` so the
// row is never durable with `local_only = 0`. `insert_transcription` issues
// plain `execute`s and opens no transaction of its own, so there is no inner
// commit to close the outer one early. Checked from both ends.
// ===========================================================================

#[test]
fn r13_data_a_withheld_dictation_is_marked_and_never_offered() {
    let a = store();
    let id = a.insert_transcription_local_only(&a_dictation("hunter2"), None, None).unwrap();
    assert_eq!(local_only_of(&a, id), Some(1), "the withheld flag must survive the transaction");

    let (origin, text) = a.origin_and_text_for_test(id).unwrap();
    assert_eq!(text, "hunter2", "the user still has their dictation");
    assert!(!origin.is_empty(), "the row still gets an origin id inside the same transaction");

    assert!(
        a.items_from(ME, 0, "", 50).unwrap().is_empty(),
        "the single outbound door must not offer a withheld row"
    );
    // An ordinary insert on the same store IS offered, so the emptiness above
    // is the filter and not an empty store.
    a.insert_transcription(&a_dictation("ordinary"), None, None).unwrap();
    let offered = a.items_from(ME, 0, "", 50).unwrap();
    assert_eq!(offered.len(), 1, "guard integrity: exactly the ordinary row is offerable");
    assert_eq!(offered[0].text, "ordinary");
}

#[test]
fn r13_data_a_withheld_dictation_is_all_or_nothing_when_the_mark_fails() {
    let a = store();
    let before = row_count(&a);

    // Abort the UPDATE that applies the mark, exactly where a crash between the
    // two old commits landed. Without one transaction the INSERT survives:
    // durable, carrying an origin id, with `local_only = 0`, which is the exact
    // shape `items_from` hands to `serve`.
    a.conn_for_test()
        .execute_batch(
            "CREATE TRIGGER r13_block_the_mark BEFORE UPDATE OF local_only ON items
             BEGIN SELECT RAISE(ABORT, 'r13: the mark never landed'); END;",
        )
        .unwrap();

    let attempt = a.insert_transcription_local_only(&a_dictation("hunter2"), None, None);
    assert!(attempt.is_err(), "guard integrity: the trigger must actually fire");

    a.conn_for_test().execute_batch("DROP TRIGGER r13_block_the_mark;").unwrap();

    assert_eq!(
        row_count(&a),
        before,
        "a withheld dictation whose mark failed must leave NO row behind; a surviving \
         row is durable, has an origin id and has local_only = 0"
    );
    assert!(
        a.items_from(ME, 0, "", 50).unwrap().is_empty(),
        "and nothing may be offerable afterwards"
    );

    // The store is still usable afterwards: an aborted statement inside the
    // transaction must not have left one open.
    let ok = a.insert_transcription_local_only(&a_dictation("later"), None, None).unwrap();
    assert_eq!(local_only_of(&a, ok), Some(1), "the next withheld insert still works");
}

// ===========================================================================
// R13-D4. DELETING AND CLEARING A WITHHELD DICTATION.
//
// Round 12 made `local_only = 0` part of the row MATCH in `delete_item_local`
// and part of the WHERE in both `clear` arms. The row must still go; only the
// announcement must not.
// ===========================================================================

#[test]
fn r13_data_deleting_a_withheld_dictation_removes_it_and_announces_nothing() {
    let a = store();
    let withheld = a.insert_transcription_local_only(&a_dictation("hunter2"), None, None).unwrap();
    let ordinary = a.insert_transcription(&a_dictation("ordinary"), None, None).unwrap();

    a.delete_item_local(withheld).unwrap();
    assert_eq!(local_only_of(&a, withheld), None, "the withheld row must still be DELETED");
    assert_eq!(tombstone_total(&a), 0, "no tombstone may be minted for a row no peer ever saw");

    // The same call on an ordinary row does mint one, so the zero above is the
    // filter and not a store that cannot write tombstones at all.
    a.delete_item_local(ordinary).unwrap();
    assert_eq!(
        tombstone_total(&a),
        1,
        "guard integrity: an ordinary delete must still announce itself"
    );
}

#[test]
fn r13_data_clear_removes_withheld_rows_counts_them_and_announces_none_of_them() {
    let a = store();
    for i in 0..3 {
        a.insert_transcription_local_only(&a_dictation(&format!("secret {i}")), None, None).unwrap();
    }
    for i in 0..2 {
        a.insert_transcription(&a_dictation(&format!("ordinary {i}")), None, None).unwrap();
    }
    assert_eq!(row_count(&a), 5, "premise");

    let removed = a.clear(None).unwrap();
    assert_eq!(removed, 5, "Clear reports every row it removed, withheld ones included");
    assert_eq!(row_count(&a), 0, "and removes them");
    assert_eq!(
        tombstone_total(&a),
        2,
        "exactly the two ordinary rows are announced; rows removed is deliberately NOT \
         equal to tombstones written"
    );
}

#[test]
fn r13_data_a_kind_scoped_clear_withholds_the_same_way() {
    let a = store();
    a.insert_transcription_local_only(&a_dictation("secret"), None, None).unwrap();
    a.insert_transcription(&a_dictation("ordinary"), None, None).unwrap();
    a.insert_clipboard("a clipboard row", None, None).unwrap();

    let removed = a.clear(Some(HistoryKind::Transcription)).unwrap();
    assert_eq!(removed, 2, "both dictations go, the clipboard row stays");
    assert_eq!(row_count(&a), 1, "guard integrity: the clipboard row survived");
    assert_eq!(
        tombstone_total(&a),
        1,
        "only the ordinary dictation is announced by a kind-scoped Clear"
    );
}

/// The origin id of a withheld row cannot be handed out and then reused, so a
/// peer cannot be holding a tombstone that would swallow a legitimate future
/// row under the same identity. Asserted rather than assumed, because the brief
/// for this round asked the question directly.
#[test]
fn r13_data_a_withheld_rows_origin_id_is_never_reused_by_a_later_row() {
    let a = store();
    let mut seen = std::collections::HashSet::new();
    for i in 0..40 {
        let id = if i % 2 == 0 {
            a.insert_transcription_local_only(&a_dictation(&format!("s{i}")), None, None).unwrap()
        } else {
            a.insert_transcription(&a_dictation(&format!("o{i}")), None, None).unwrap()
        };
        let (origin, _) = a.origin_and_text_for_test(id).unwrap();
        assert!(!origin.is_empty(), "every row carrying a source needs an identity");
        assert!(seen.insert(origin.clone()), "origin id {origin} was handed out twice");
        // Deleting between rows is what would let a rowid be recycled. The
        // origin id must not follow it.
        a.delete_item_local(id).unwrap();
    }
    assert_eq!(seen.len(), 40, "guard integrity: forty distinct identities were actually minted");
}
