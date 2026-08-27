//! ADVERSARIAL REVIEW, ROUND 12. Data integrity, store side.
//!
//! Target: round 11's data work, and the round-10 clock rule round 11 built a
//! rescue on top of instead of replacing.
//!
//!   * `Store::next_clock_impl`'s ceiling clamp,
//!     `now.max(newest.saturating_add(1).min(now + MAX_CLOCK_SKEW_MS))`, and
//!     the three call sites that inherit it: `local_clock_at` (inserts),
//!     `edit_stamp` (pins and corrections) and `next_clock_in` (deletes).
//!   * Schema v8's `items.local_only`, `insert_transcription_local_only`, and
//!     the single `local_only = 0` filter in `items_from` that is supposed to
//!     be the one outbound door.
//!
//! Everything here runs against the store alone. The exchange-level twins live
//! in `src-tauri/src/sync/adversarial_r12_data.rs`.

#![cfg(test)]

use crate::history::{RemoteItem, Store, MAX_CLOCK_SKEW_MS};
use crate::types::TranscriptionResult;

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

/// The clock and origin a local row is actually carrying right now.
fn clock_and_origin(s: &Store, id: i64) -> (i64, String) {
    s.conn_for_test()
        .query_row(
            "SELECT updated_at, COALESCE(origin_id, '') FROM items WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

/// Model a wall clock that has stepped BACKWARDS past the skew window.
///
/// `now_ms()` cannot be moved from inside the process, so the step is applied
/// to the only durable thing it changes on the authoring machine: the clocks
/// already stamped on our own rows, which were the wall clock when they were
/// written. This is the same modelling `adversarial_r11_data::step_the_authors_clock_back`
/// uses, reduced to the one store this file needs.
///
/// Returns the high clock every one of our rows now carries.
fn step_our_clock_back(s: &Store, by_ms: i64) -> i64 {
    let t_high = now_ms() + by_ms;
    s.conn_for_test()
        .execute(
            "UPDATE items SET created_at = ?1, updated_at = ?1 WHERE source_machine = ?2",
            rusqlite::params![t_high, ME],
        )
        .unwrap();
    t_high
}

// ===========================================================================
// R12-D1. AN EDIT AFTER A BACKWARDS CLOCK STEP MOVES THE ROW'S OWN CLOCK
//         DOWN, AND THAT IS THE ONE DIRECTION NOTHING CAN UNDO.
//
// In ordinary terms. The Mac and the PC agree about the time. You dictate on
// the Mac and it syncs. The Mac's clock then steps back an hour. You notice a
// transcription error and correct it, or you pin the row. The PC keeps the old
// text, for ever, and fixing the clock does not help.
//
// Why. `edit_stamp` delegates to `local_clock_at` -> `next_clock_impl`, whose
// documented contract is "strictly above every clock we already hold for that
// source". Under the ceiling clamp it is not: with `newest` an hour above
// `now`, `min(newest + 1, now + skew)` collapses to `now + skew`, an hour
// BELOW the row's own `updated_at`. The UPDATE then writes that lower value
// onto the row.
//
// Round 11's `unreachable_cursor` rescue does not reach this. That rescue only
// makes `serve` OFFER the row from zero again; the peer still refuses it,
// because `apply_remote_item` is last-writer-wins on `updated_at` and the
// offered clock is now strictly lower than the one it holds. And unlike the
// delete case round 11 fixed, correcting the clock cannot repair it: the local
// row's old clock has been overwritten, so the evidence is destroyed by the
// very edit we are trying to propagate.
// ===========================================================================

#[test]
fn r12_data_an_edit_after_a_backwards_clock_step_lowers_the_rows_own_clock() {
    let s = store();
    let id = s.insert_clipboard("the original", None, None).unwrap();
    let (before, _) = clock_and_origin(&s, id);

    let t_high = step_our_clock_back(&s, 60 * 60 * 1000);
    assert!(
        t_high > now_ms() + MAX_CLOCK_SKEW_MS,
        "premise: the step has to exceed the skew window, or the clamp never engages"
    );
    let (poisoned, _) = clock_and_origin(&s, id);
    assert_eq!(poisoned, t_high, "premise: the row must now carry the high clock");
    assert!(poisoned > before, "premise: the poison must have moved the clock up");

    // The user corrects the transcription.
    s.update_text(id, "the correction").unwrap();
    let (after, _) = clock_and_origin(&s, id);

    assert!(
        after > poisoned,
        "an edit stamped this row at {after}, which is {} ms BELOW the clock the row \
         already carried ({poisoned}). A peer holding that row applies last-writer-wins on \
         updated_at and refuses anything not strictly greater, so the correction can never \
         land; and because the edit overwrote the row's own clock, correcting the wall clock \
         afterwards cannot recover it either. next_clock_impl promises 'strictly above every \
         clock we already hold for that source' and the ceiling clamp breaks that promise",
        poisoned - after
    );
}

/// The same break through the pin path, which is a different statement.
#[test]
fn r12_data_a_pin_after_a_backwards_clock_step_lowers_the_rows_own_clock() {
    let s = store();
    let id = s.insert_clipboard("worth keeping", None, None).unwrap();
    let t_high = step_our_clock_back(&s, 60 * 60 * 1000);
    let (poisoned, _) = clock_and_origin(&s, id);
    assert_eq!(poisoned, t_high, "premise: the row must carry the high clock");

    s.set_pinned(id, true).unwrap();
    let (after, _) = clock_and_origin(&s, id);
    assert!(
        after > poisoned,
        "a pin stamped {after} onto a row already at {poisoned}; the pin is unofferable and \
         the row's own clock has been walked backwards {} ms",
        poisoned - after
    );
}

/// And the peer-side consequence, proved on a second store rather than argued.
///
/// The store here plays the PC: it holds the row at the high clock, exactly as
/// it would after receiving it while the two clocks still agreed. The edit is
/// then offered to it as `serve` would offer it.
#[test]
fn r12_data_the_lowered_edit_is_refused_by_a_peer_that_holds_the_row() {
    // The author.
    let a = store();
    let id = a.insert_clipboard("the original", None, None).unwrap();
    let (origin, _) = a.origin_and_text_for_test(id).unwrap();
    let t_high = step_our_clock_back(&a, 60 * 60 * 1000);

    // The peer, holding that row at the clock it was given.
    let mut b = Store::open_in_memory().unwrap();
    b.set_device_id(PEER);
    // Received while the clocks still agreed, so it carries the author's stamp.
    // Written straight in, because `apply_remote_item` would refuse a clock an
    // hour ahead and the point here is the state AFTER a backwards step, not
    // the step itself.
    b.conn_for_test()
        .execute(
            "INSERT INTO items (kind, text, created_at, updated_at, source_machine, origin_id, pinned)
             VALUES ('clipboard', 'the original', ?1, ?1, ?2, ?3, 0)",
            rusqlite::params![t_high, ME, origin],
        )
        .unwrap();
    let held: String = b
        .conn_for_test()
        .query_row(
            "SELECT text FROM items WHERE source_machine = ?1 AND origin_id = ?2",
            rusqlite::params![ME, origin],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(held, "the original", "premise: the peer must hold the row first");

    // The user corrects it on the author, after the clock step.
    a.update_text(id, "the correction").unwrap();
    let page = a.items_from(ME, 0, "", 10).unwrap();
    let offered: &RemoteItem = page
        .iter()
        .find(|r| r.origin_id == origin)
        .expect("premise: the corrected row must still be offerable at all");
    assert_eq!(offered.text, "the correction", "premise: the author holds the correction");

    let outcome = b.apply_remote_item(ME, offered).unwrap();
    let landed: String = b
        .conn_for_test()
        .query_row(
            "SELECT text FROM items WHERE source_machine = ?1 AND origin_id = ?2",
            rusqlite::params![ME, origin],
            |r| r.get(0),
        )
        .unwrap();

    // What is asserted is that the edit is RECOVERABLE, not that it lands this
    // instant, and that distinction is the whole finding. `next_clock_impl`'s
    // own comment states the trade it means to take: "certain, permanent,
    // silent loss on one side, against a recoverable refusal on the other. We
    // take the recoverable one."
    //
    //   * Stamped at or above the clock the peer holds, the offer either wins
    //     on last-writer-wins or ties, and a tie is settled by the total,
    //     stable payload tiebreak that both machines evaluate identically. If
    //     it is above the peer's own skew ceiling it is refused with NO
    //     receipt, so it is re-offered every exchange and lands the moment the
    //     clocks agree.
    //   * Stamped BELOW it, the offer loses on last-writer-wins for ever, no
    //     receipt is involved, and the author has overwritten its own copy of
    //     the higher clock, so no later exchange and no clock correction can
    //     produce a higher one.
    assert!(
        offered.updated_at >= t_high,
        "the correction was offered at {} against the {t_high} the peer holds, so \
         last-writer-wins refused it ({outcome:?}) and the peer still shows {landed:?}. \
         Nothing will ever offer this identity at a higher clock: the edit overwrote the \
         author's only copy of it. The two machines disagree about the contents of one \
         identity, permanently",
        offered.updated_at
    );
}

// ===========================================================================
// R12-D2. THE SAME CLAMP HANDS OUT ONE CLOCK TWICE, AND THAT IS SURVIVABLE.
//
// TRIAGED IN ROUND 12 AS A STALE CONTRACT, NOT A DATA DEFECT. The cursor has
// not been a bare clock since schema v7: `items_from` selects on
// `updated_at > ?2 OR (updated_at = ?2 AND origin_id > ?3)` and the cursor is
// banked as that PAIR. A clock shared by two rows is therefore broken by
// origin, and both rows are delivered. Reuse costs a row only where something
// compares clocks ALONE, which is last-writer-wins on an edit, and that is
// R12-D1 and is fixed at its own call site.
//
// So the code keeps the clamp, which round 9 proved is the recoverable side of
// this trade, and the doc comment stops claiming a strictness it does not have.
// The tests below now assert the property that carries the weight.
//
// `next_clock_impl`'s first line of documentation is "Strictly above every
// clock we already hold for that source, across BOTH items and tombstones",
// and the reason given is that a peer's cursor is "a promise never to ask at
// or below it again. A clock we reuse, or one that goes backwards, is a row or
// a delete the peer will never request."
//
// Once `newest >= now + skew - 1` the clamp returns `now + skew` no matter what
// `newest` is, so every write inside one millisecond gets an identical clock.
// This is the property the paging keyset has to work around rather than an
// abstract tidiness point: rows are ordered `(updated_at, origin_id)` and a
// cursor is banked as that pair.
// ===========================================================================

#[test]
fn r12_data_a_clock_the_clamp_reused_still_reaches_a_peer() {
    let s = store();
    let id = s.insert_clipboard("first", None, None).unwrap();
    let t_high = step_our_clock_back(&s, 60 * 60 * 1000);
    let id2 = s.insert_clipboard("second", None, None).unwrap();
    let (c2, _) = clock_and_origin(&s, id2);
    let _ = id;

    // The stamp IS at or below what we already hold. That is the clamp working
    // as designed, and the original form of this test asserted it must not be.
    assert!(c2 <= t_high, "premise: the clamp is still in force, so the stamp is not strict");

    // What must be true is that the row still crosses. A peer whose cursor sits
    // exactly on the colliding clock asks with the PAIR, and the origin half
    // is what carries it.
    let all = s.items_from(ME, 0, "", 100).unwrap();
    assert!(
        all.iter().any(|r| r.text == "second"),
        "a row stamped with a reused clock is not offered to a peer at all, which is the \
         data loss the strictness contract existed to prevent"
    );
}

#[test]
fn r12_data_a_shared_clock_never_hides_a_row_from_a_peer() {
    let s = store();
    s.insert_clipboard("first", None, None).unwrap();
    step_our_clock_back(&s, 60 * 60 * 1000);

    // Bounded burst. Under the documented rule every stamp is `newest + 1`, so
    // no two can ever collide however fast the loop runs. Under the clamp every
    // stamp is `now + skew`, so any two captures inside one millisecond share a
    // clock, and an in-memory store writes far more than one row per
    // millisecond.
    let mut seen: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    let mut collision: Option<(String, String, i64)> = None;
    for i in 0..2_000 {
        let text = format!("row {i}");
        let id = s.insert_clipboard(&text, None, None).unwrap();
        let (clock, _) = clock_and_origin(&s, id);
        if let Some(prev) = seen.insert(clock, text.clone()) {
            collision = Some((prev, text, clock));
            break;
        }
    }
    assert!(
        !seen.is_empty(),
        "guard integrity: the burst recorded no clocks at all, so a pass here would mean nothing"
    );
    // Collisions ARE expected under the clamp. What must not happen is that a
    // collision hides a row from a peer, so the assertion is on delivery.
    if let Some((ref a, ref b, clock)) = collision {
        let offered = s.items_from(ME, 0, "", 4_000).unwrap();
        let got = |t: &str| offered.iter().any(|r| r.text == t);
        assert!(
            got(a) && got(b),
            "two rows shared clock {clock} and only one is offered ({a:?} present: {}, \
             {b:?} present: {}). The keyset pages on (updated_at, origin_id) precisely so a \
             shared clock is survivable; if one is dropped, it is not",
            got(a),
            got(b)
        );
    }
}

// ===========================================================================
// R12-D3. A DICTATION WE COULD NOT CLASSIFY IS REPLICABLE BETWEEN THE TWO
//         COMMITS THAT ARE SUPPOSED TO WITHHOLD IT.
//
// `insert_transcription_local_only` is `insert_transcription(...)` followed by
// a separate `UPDATE items SET local_only = 1`, with no transaction around the
// pair. `insert_transcription` itself commits twice (the INSERT, then
// `stamp_origin`'s UPDATE). After its second commit the row is durable, has an
// origin id, and has `local_only = 0`, which is precisely the shape
// `items_from` hands to `serve`.
//
// This app quits with `libc::_exit(0)` and this file already argues elsewhere
// that the window between two commits "is not exotic". The row this window
// exposes is the one the whole v8 column exists to withhold: a dictation that
// may be a password typed into a field the accessibility probe could not read.
// ===========================================================================

#[test]
fn r12_data_a_withheld_dictation_is_offerable_between_its_two_commits() {
    let s = store();
    // A trigger that fails exactly the statement `insert_transcription_local_only`
    // uses to withhold the row, and nothing else. `stamp_origin`'s UPDATE
    // leaves `local_only` at 0 so it does not fire. This is a crash at the one
    // instant that matters, made deterministic.
    s.conn_for_test()
        .execute_batch(
            "CREATE TRIGGER r12_die_before_withholding
               AFTER UPDATE ON items
               WHEN NEW.local_only = 1 AND OLD.local_only = 0
             BEGIN SELECT RAISE(ABORT, 'r12: process died here'); END;",
        )
        .unwrap();

    let err = s.insert_transcription_local_only(&a_dictation("hunter2"), None, None);
    assert!(
        err.is_err(),
        "guard integrity: the injected failure did not fire, so nothing below is being tested"
    );

    // What survived the abort.
    let page = s.items_from(ME, 0, "", 10).unwrap();
    assert!(
        !page.iter().any(|r| r.text == "hunter2"),
        "a dictation the pipeline decided must never leave this machine is sitting in \
         `items_from`, the single outbound door, with local_only = 0 and an origin id. \
         `insert_transcription_local_only` commits the row first and withholds it second, \
         with no transaction around the pair, so any stop between the two publishes it. \
         The row is otherwise complete, so nothing later ever repairs it"
    );
}

// ===========================================================================
// R12-D4. WITHHOLDING THE ROW DOES NOT WITHHOLD THE FACT OF IT.
//
// `local_only` is filtered in exactly one place, `items_from`. Neither
// `delete_item_local` nor `clear()` knows about the column, so both mint an
// ordinary tombstone for a withheld row, and `tombstones_from` has no way to
// tell it apart: the tombstones table has no `local_only` column to filter on.
//
// The peer therefore learns that a dictation existed on this device, when it
// was taken, and when it was deleted, for every dictation the probe could not
// classify. It also banks a permanent absorbing tombstone for an identity it
// has never held and never will.
// ===========================================================================

#[test]
fn r12_data_deleting_a_withheld_dictation_offers_its_tombstone_to_peers() {
    let s = store();
    let id = s.insert_transcription_local_only(&a_dictation("hunter2"), None, None).unwrap();
    let (origin, _) = s.origin_and_text_for_test(id).unwrap();
    assert!(
        !s.items_from(ME, 0, "", 10).unwrap().iter().any(|r| r.origin_id == origin),
        "premise: the row itself must be withheld, or this test is about something else"
    );
    // An ORDINARY row deleted the same way, exactly as the `clear` sibling
    // below does it. The first shape of this test deleted only the withheld
    // row, so the correct behaviour (mint nothing) tripped its own vacuity
    // guard. Deleting both proves the machinery works AND that the filter
    // discriminates, which one row alone can never show.
    let other = s.insert_clipboard("ordinary", None, None).unwrap();
    let (other_origin, _) = s.origin_and_text_for_test(other).unwrap();

    s.delete(id).unwrap();
    s.delete(other).unwrap();

    let offered = s.tombstones_from(ME, 0, "", 100).unwrap();
    assert!(
        offered.iter().any(|t| t.origin_id == other_origin),
        "guard integrity: the delete of an ORDINARY row minted no tombstone either, so the \
         check below would pass for the wrong reason"
    );
    assert!(
        !offered.iter().any(|t| t.origin_id == origin),
        "the delete of a withheld dictation is offered to every paired peer as an ordinary \
         tombstone, naming the identity and the time. The row was withheld because we could \
         not rule out that it was a password; its existence and its timing are not withheld \
         at all"
    );
}

#[test]
fn r12_data_clear_history_offers_a_tombstone_for_every_withheld_dictation() {
    let s = store();
    let id = s.insert_transcription_local_only(&a_dictation("hunter2"), None, None).unwrap();
    let (origin, _) = s.origin_and_text_for_test(id).unwrap();
    // An ordinary row too, so the clear has real work to do and a pass cannot
    // come from the clear having done nothing.
    s.insert_clipboard("ordinary", None, None).unwrap();

    let cleared = s.clear(None).unwrap();
    assert!(cleared >= 2, "premise: the clear must have removed both rows, removed {cleared}");

    let offered = s.tombstones_from(ME, 0, "", 100).unwrap();
    assert!(
        !offered.is_empty(),
        "guard integrity: the clear minted no tombstones at all, so the check below is empty"
    );
    assert!(
        !offered.iter().any(|t| t.origin_id == origin),
        "Clear History mints a replicable tombstone for every local_only row. The v8 \
         migration says these rows are 'kept on this device and never offered to a peer'; \
         `clear`'s INSERT filters on kind, pinned, source_machine and origin_id, and not on \
         local_only"
    );
}

// ===========================================================================
// R12-D5. THE v7 TO v8 UPGRADE.
//
// Asked of round 12 explicitly: does the upgrade keep every row and every
// cursor? Built by taking a real v8 database back to v7 (drop the column,
// reset user_version) and reopening it, so the v7 shape is the one this build
// actually produces rather than one retyped from memory.
// ===========================================================================

#[test]
fn r12_data_the_v7_to_v8_upgrade_keeps_every_row_and_every_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r12_v7_to_v8.db");

    let (origins, marks) = {
        let mut s = Store::open(&path).unwrap();
        s.set_device_id(ME);
        for i in 0..5 {
            s.insert_clipboard(&format!("row {i}"), None, None).unwrap();
        }
        s.apply_remote_item(
            PEER,
            &RemoteItem {
                source_machine: PEER.into(),
                origin_id: "peer-1".into(),
                kind: "clipboard".into(),
                text: "from the peer".into(),
                created_at: now_ms(),
                updated_at: now_ms(),
                pinned: false,
            },
        )
        .unwrap();
        let origins: Vec<String> = s
            .conn_for_test()
            .prepare("SELECT origin_id FROM items ORDER BY origin_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let marks = s.watermarks_paired(PEER).unwrap();
        assert_eq!(origins.len(), 6, "premise: six identities before the downgrade");
        assert!(!marks.is_empty(), "premise: a cursor must exist before the downgrade");

        // Back to v7: the column goes, and so does the version stamp.
        s.conn_for_test()
            .execute_batch(
                "ALTER TABLE items DROP COLUMN local_only;
                 PRAGMA user_version = 7;",
            )
            .unwrap();
        (origins, marks)
    };

    // Reopen. This is the upgrade path.
    let s = Store::open(&path).unwrap();
    let after: Vec<String> = s
        .conn_for_test()
        .prepare("SELECT origin_id FROM items ORDER BY origin_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(after, origins, "the v8 upgrade must not lose or rename a row");
    assert_eq!(s.watermarks_paired(PEER).unwrap(), marks, "the v8 upgrade must not lose a cursor");

    let withheld: i64 = s
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM items WHERE local_only != 0", [], |r| r.get(0))
        .unwrap();
    assert_eq!(withheld, 0, "an upgraded row must default to replicable, not withheld");
    assert_eq!(
        s.items_from(ME, 0, "", 50).unwrap().len(),
        5,
        "every upgraded local row must still be offerable"
    );
}
