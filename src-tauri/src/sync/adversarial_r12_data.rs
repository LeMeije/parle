//! ADVERSARIAL REVIEW, ROUND 12. Data integrity, exchange side.
//!
//! Round 11 added `unreachable_cursor` to `serve`: a peer's advertised mark
//! above `now + MAX_SKEW_MS` is treated as unreachable and that source is
//! restarted from zero. It exists to rescue the round-10 ceiling clamp in
//! `Store::next_clock_impl` after a backwards clock step.
//!
//! Two questions are asked of it here. Does the rescue cover everything the
//! clamp strands, or only deletes? And what does restarting a source from zero
//! do to the machinery that assumed an ordinary exchange always resumes from
//! the peer's own cursor?
//!
//! Same harness shape as `adversarial_r11_data`: each exchange runs on its own
//! pair of threads under a wall-clock budget, both sockets carry read and write
//! timeouts, and every convergence loop is hard-bounded, so a stall names the
//! side that never returned instead of parking the suite.

#![cfg(test)]

use parle_core::history::Store;
use parle_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";

const BUDGET: Duration = Duration::from_secs(90);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}

fn socket_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c = TcpStream::connect(addr).unwrap();
    let (srv, _) = l.accept().unwrap();
    for sock in [&c, &srv] {
        sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(30))).unwrap();
    }
    (c, srv)
}

fn store_for(me: &str) -> Arc<Mutex<Store>> {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(me);
    Arc::new(Mutex::new(s))
}

fn both() -> Kinds {
    Kinds { dictations: true, clipboard: true }
}

/// One exchange, `x` dialling, under a wall-clock budget.
fn sync_bounded(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
) -> (RoundStats, RoundStats) {
    sync_bounded_with(x, y, None)
}

/// The same exchange, with the dialler carrying a resume debt the way
/// `manager.rs` does for a re-offer. `None` is what an ordinary exchange
/// passes, which is what every other test in this file uses.
fn sync_bounded_with(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
    debt: Option<i64>,
) -> (RoundStats, RoundStats) {
    let (resend_all, resend_from) = (debt.is_some(), debt.unwrap_or(0));
    let (sock_x, sock_y) = socket_pair();
    let key = PairedKey::from_bytes([9u8; 32]);
    let k2 = key.clone();
    let (x_store, y_store) = (x.0.clone(), y.0.clone());
    let (x_id, y_id) = (x.1, y.1);

    let (tx, rx) = mpsc::channel::<(&'static str, Result<RoundStats, String>)>();
    let tx2 = tx.clone();

    let acceptor = std::thread::spawn(move || {
        let r = (|| {
            let mut s = Session::accept(sock_y, &k2).map_err(|e| e.to_string())?;
            let known = vec![A.to_string(), B.to_string()];
            let attr = Attribution { peer_id: x_id, local_id: y_id, known: &known };
            exchange(
                &mut s,
                &y_store,
                (y_id, "peer"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
            .map_err(|e| e.to_string())
        })();
        let _ = tx2.send(("acceptor", r));
    });

    let dialler = std::thread::spawn(move || {
        let r = (|| {
            let mut s = Session::initiate(sock_x, &key).map_err(|e| e.to_string())?;
            let known = vec![A.to_string(), B.to_string()];
            let attr = Attribution { peer_id: y_id, local_id: x_id, known: &known };
            exchange(
                &mut s,
                &x_store,
                (x_id, "peer"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::First,
                resend_all,
                resend_from,
                &|| false,
            )
            .map_err(|e| e.to_string())
        })();
        let _ = tx.send(("dialler", r));
    });

    let mut got: Vec<(&'static str, Result<RoundStats, String>)> = Vec::new();
    let deadline = Instant::now() + BUDGET;
    while got.len() < 2 {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "the exchange did not finish inside {BUDGET:?}; {} of 2 sides returned",
            got.len()
        );
        match rx.recv_timeout(left) {
            Ok(r) => got.push(r),
            Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                "the exchange did not finish inside {BUDGET:?}; {} of 2 sides returned: {:?}",
                got.len(),
                got.iter().map(|(who, _)| *who).collect::<Vec<_>>()
            ),
            Err(e) => panic!("both exchange threads died without reporting: {e}"),
        }
    }
    acceptor.join().expect("acceptor thread panicked");
    dialler.join().expect("dialler thread panicked");

    let mut d = None;
    let mut a = None;
    for (who, r) in got {
        let stats = r.unwrap_or_else(|e| panic!("{who} side failed: {e}"));
        match who {
            "dialler" => d = Some(stats),
            _ => a = Some(stats),
        }
    }
    (d.expect("dialler reported"), a.expect("acceptor reported"))
}

fn text_of(store: &Arc<Mutex<Store>>, source: &str, origin: &str) -> Option<String> {
    let g = store.lock();
    let mut stmt = g
        .conn_for_test()
        .prepare(&format!(
            "SELECT text FROM items WHERE source_machine = '{source}' AND origin_id = '{origin}'"
        ))
        .unwrap();
    let mut rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.next().map(|r| r.unwrap())
}

fn holds_tombstone(store: &Arc<Mutex<Store>>, source: &str, origin: &str) -> bool {
    let g = store.lock();
    let mut stmt = g
        .conn_for_test()
        .prepare(&format!(
            "SELECT 1 FROM tombstones WHERE source_machine = '{source}' AND origin_id = '{origin}'"
        ))
        .unwrap();
    let mut rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
    rows.next().is_some()
}

fn clock_of(store: &Arc<Mutex<Store>>, source: &str, origin: &str) -> Option<i64> {
    let g = store.lock();
    let mut stmt = g
        .conn_for_test()
        .prepare(&format!(
            "SELECT updated_at FROM items WHERE source_machine = '{source}' AND origin_id = '{origin}'"
        ))
        .unwrap();
    let mut rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
    rows.next().map(|r| r.unwrap())
}

fn item_count(store: &Arc<Mutex<Store>>, source: &str) -> i64 {
    let g = store.lock();
    let mut stmt = g
        .conn_for_test()
        .prepare(&format!(
            "SELECT COUNT(*) FROM items WHERE source_machine = '{source}'"
        ))
        .unwrap();
    let mut rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
    rows.next().unwrap().unwrap()
}

/// The durable state one backwards clock step past the skew window leaves.
///
/// `now_ms()` cannot be moved from inside the process and both stores share one
/// wall clock, so the step is applied to the two durable records it changes and
/// to nothing else: the author's own row clocks, which were the wall clock when
/// they were written, and the peer's receipt for them, which `mark_received_in`
/// set to the row's `(updated_at, origin_id)` the moment it was applied.
///
/// Same modelling as `adversarial_r11_data::step_the_authors_clock_back`, which
/// is not touched.
fn step_the_authors_clock_back(
    author: &Arc<Mutex<Store>>,
    author_id: &str,
    peer: &Arc<Mutex<Store>>,
    by_ms: i64,
) -> i64 {
    let t_high = now_ms() + by_ms;
    {
        let g = author.lock();
        g.conn_for_test()
            .execute_batch(&format!(
                "UPDATE items SET created_at={t_high}, updated_at={t_high}
                  WHERE source_machine='{author_id}';"
            ))
            .unwrap();
    }
    {
        let g = peer.lock();
        g.conn_for_test()
            .execute_batch(&format!(
                "UPDATE items SET created_at={t_high}, updated_at={t_high}
                  WHERE source_machine='{author_id}';
                 UPDATE source_marks SET received_clock={t_high}
                  WHERE source_machine='{author_id}';"
            ))
            .unwrap();
    }
    t_high
}

// ===========================================================================
// R12-X1. ROUND 11 RESCUED THE DELETE AND LEFT THE CORRECTION BEHIND.
//
// In ordinary terms. The Mac and the PC agree about the time. You dictate on
// the Mac and it syncs. The Mac's clock steps back an hour. You spot a
// transcription error and fix it. The PC shows the wrong text for ever, and
// correcting the Mac's clock does not help.
//
// Round 11 proved this shape for a DELETE and fixed it in `serve`:
// `unreachable_cursor` restarts the source from zero, so the tombstone is
// offered again. That rescue is on the SENDING side only, and an edit does not
// fail on the sending side. It fails on the receiving side, where
// `apply_remote_item` is last-writer-wins on `updated_at` and the edit now
// carries a clock an hour BELOW the one the peer already holds.
//
// It is also strictly worse than the delete case, because the edit destroys
// its own evidence: `edit_stamp` writes the clamped clock onto the row, so the
// author no longer holds anything above the peer's copy and no later clock
// correction can produce one. The `sent_items` count below shows the row IS
// offered — the round-11 rescue works exactly as advertised — and the peer
// still ends up with the old text.
// ===========================================================================

#[test]
fn r12_data_a_correction_after_a_backwards_clock_step_never_reaches_the_peer() {
    let a = store_for(A);
    let b = store_for(B);

    // 1. Clocks agree. A dictates, and it syncs.
    let id = a.lock().insert_clipboard("meet me at eigth", None, None).unwrap();
    let (origin, _) = a.lock().origin_and_text_for_test(id).unwrap();
    sync_bounded((&a, A), (&b, B));
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("meet me at eigth"),
        "premise: the row must sync first"
    );

    // 2. A's clock steps back an hour.
    let t_high = step_the_authors_clock_back(&a, A, &b, 60 * 60 * 1000);
    assert!(
        t_high > now_ms() + parle_core::history::MAX_CLOCK_SKEW_MS,
        "premise: the step must exceed the skew window, or the clamp never engages"
    );

    // 3. The user corrects the transcription on A.
    a.lock().update_text(id, "meet me at eight").unwrap();

    // 4. Sync repeatedly, so a pass cannot come from the fix merely being slow.
    let mut offered = 0usize;
    for _ in 0..3 {
        let (d, _) = sync_bounded((&a, A), (&b, B));
        offered += d.sent_items;
    }
    assert!(
        offered > 0,
        "guard integrity: the corrected row was never offered at all, so the assertion \
         below would be measuring the wrong failure"
    );

    // What is asserted is that the correction is still RECOVERABLE, not that it
    // has already landed, and the distinction is the whole finding. The
    // codebase draws it itself, in `next_clock_impl`: "certain, permanent,
    // silent loss on one side, against a recoverable refusal on the other. We
    // take the recoverable one."
    //
    //   * Offered at or above the clock the peer holds, it either wins on
    //     last-writer-wins, or ties and is settled by the total, stable payload
    //     tiebreak both machines evaluate identically, or is refused for being
    //     beyond the peer's own skew ceiling — and that refusal banks no
    //     receipt, so it is re-offered every exchange and lands the moment the
    //     clocks agree.
    //   * Offered BELOW it, it loses on last-writer-wins for ever, and the same
    //     edit overwrote the author's only copy of the higher clock, so nothing
    //     will ever offer this identity higher and no clock correction reaches
    //     it.
    let landed = text_of(&b, A, &origin);
    let a_clock = clock_of(&a, A, &origin).expect("the author still holds the row");
    let b_clock = clock_of(&b, A, &origin).expect("the peer still holds the row");
    assert!(
        landed.as_deref() == Some("meet me at eight") || a_clock >= b_clock,
        "after three exchanges that offered the row {offered} times the peer still shows \
         {landed:?}, and the author is offering it at {a_clock} against the {b_clock} the \
         peer holds. `unreachable_cursor` put it back on the wire, exactly as round 11 \
         intends; last-writer-wins throws it away, because `edit_stamp` stamped it at the \
         clamped ceiling, an hour below what the peer holds. The edit overwrote the \
         author's own copy of that clock, so this is permanent and no clock correction \
         repairs it"
    );
}

// ===========================================================================
// R12-X2. WITHHOLDING THE DICTATION DOES NOT WITHHOLD THE FACT OF IT.
//
// v8 says a dictation we cannot classify is "kept on this device and never
// offered to a peer". The row is: `items_from` filters `local_only = 0` and
// the first assertion below confirms it. The DELETE of that row is not.
// Neither `delete_item_local` nor `clear()` consults the column, and the
// tombstones table has no column to filter on, so the peer is handed the
// identity and the timing of every dictation this machine decided was too
// risky to send.
// ===========================================================================

#[test]
fn r12_data_clearing_history_announces_every_withheld_dictation_to_the_peer() {
    let a = store_for(A);
    let b = store_for(B);

    let tr = parle_core::types::TranscriptionResult {
        raw_text: "hunter2".into(),
        text: "hunter2".into(),
        language: Some("en".into()),
        model_id: "test".into(),
        duration_ms: 10,
        transcribe_ms: 5,
        segments: Vec::new(),
        trimmed: Vec::new(),
        low_confidence: Vec::new(),
        cleanup_tier: 0,
        refine: None,
    };
    let secret = a.lock().insert_transcription_local_only(&tr, None, None).unwrap();
    let (secret_origin, _) = a.lock().origin_and_text_for_test(secret).unwrap();
    // An ordinary row alongside it, so every exchange below has real work and
    // an empty result cannot be mistaken for a pass.
    let ordinary = a.lock().insert_clipboard("ordinary", None, None).unwrap();
    let (ordinary_origin, _) = a.lock().origin_and_text_for_test(ordinary).unwrap();

    sync_bounded((&a, A), (&b, B));
    assert_eq!(
        text_of(&b, A, &ordinary_origin).as_deref(),
        Some("ordinary"),
        "guard integrity: nothing synced at all, so the checks below prove nothing"
    );
    assert!(
        text_of(&b, A, &secret_origin).is_none(),
        "the withheld dictation itself must not cross, and this is the part that works"
    );

    // The user hits Clear History.
    a.lock().clear(None).unwrap();
    for _ in 0..2 {
        sync_bounded((&a, A), (&b, B));
    }

    assert!(
        holds_tombstone(&b, A, &ordinary_origin),
        "guard integrity: the clear did not propagate at all, so the check below is empty"
    );
    assert!(
        !holds_tombstone(&b, A, &secret_origin),
        "the peer now holds a durable, absorbing tombstone for a dictation it was never \
         allowed to see: identity {secret_origin}, with the time it was taken and the time \
         it was deleted. That is the existence and the timing of every dictation this \
         machine withheld because it could not rule out a password field"
    );
}

// ===========================================================================
// R12-X3. AN UNREACHABLE CURSOR TURNS A TRUNCATED PASS INTO A PERMANENT HOLE.
//
// `serve` truncates the item pass after MAX_BATCHES full pages and sets
// `stats.truncated`. `manager.rs` only ever acts on that flag when `resend_all`
// is already true ("if resend_all"), and until round 11 it did not need to: an
// ordinary truncated exchange still moved the peer's cursor to wherever the
// pass stopped, so the next exchange resumed above it.
//
// `unreachable_cursor` is the first thing that breaks that assumption. It
// ignores the peer's cursor and restarts the source at zero on EVERY exchange
// with `resend_all` false, so every exchange re-sends the same prefix, stops in
// the same place, banks no debt, and nothing past the cap is ever delivered.
//
// Round 11's comment prices this as bandwidth only: "A hostile peer can
// advertise an absurd mark to make us re-offer in full. That costs us bandwidth
// to a device we have already paired with and told our whole history to, so it
// buys an attacker nothing it did not have." It buys more than that. Nothing
// validates an incoming watermark — `recv_watermarks` takes `w.clock as i64`
// straight off the wire — so one line in one message silently stops this
// machine ever delivering another row to that peer, deletes included.
// ===========================================================================

/// Advertise `clock` as the peer's mark for `source`, which is what arrives on
/// the wire. Written into the peer's own receipt table because that is what
/// `send_watermarks` reads; `mark_received_in`'s range check guards the write
/// path only, and `recv_watermarks` applies no check of its own.
fn peer_advertises(peer: &Arc<Mutex<Store>>, source: &str, clock: i64) {
    let g = peer.lock();
    g.conn_for_test()
        .execute_batch(&format!(
            "UPDATE source_marks SET received_clock={clock} WHERE source_machine='{source}';"
        ))
        .unwrap();
}

/// The sibling below simulates `manager.rs`'s bookkeeping rather than calling
/// it, because `exchange` is the entry point a test can drive. That simulation
/// is only worth anything while it still matches the real rule, so this pins
/// the real rule to it.
///
/// The original form of this test asserted the DEFECT: it drove the same
/// scenario through the old rule and watched the fresh row strand behind the
/// batch cap for ever. Round 12 fixed the rule, so that assertion now measures
/// the test's own copy of the old code and nothing in production. What has to
/// stay true is that the two cannot drift.
#[test]
fn r12_data_the_manager_banks_a_debt_on_any_truncated_pass() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let code: String = std::fs::read_to_string(root.join("src/sync/manager.rs"))
        .expect("manager.rs is readable")
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("stats.resend_progress.unwrap_or(0)")
            && code.contains("i.resend_owed.insert("),
        "premise: the manager still banks a resume debt from a truncated pass at all"
    );
    assert!(
        !code.contains("if resend_all {"),
        "the manager banks a resume debt only when the pass was ALREADY a re-offer. \
         `unreachable_cursor` restarts a source at zero on every exchange with `resend_all` \
         false, so without a debt the same prefix is re-sent for ever and everything past \
         the batch cap is stranded, deletes included. The sibling below proves banking on \
         any truncation closes it; this asserts production actually does that"
    );
    assert!(
        code.contains("if resend_all || stats.truncated {"),
        "the debt is banked on some other condition than 'this pass truncated', so the \
         sibling's simulation no longer models the production rule"
    );
}

// ---------------------------------------------------------------------------
// The other side of R12-X3, so the diagnosis is pinned rather than asserted.
//
// Identical scenario, one difference: the resume debt is banked whenever a pass
// truncates, not only when the pass was already a re-offer. That is the whole
// of the proposed fix, expressed through the same `exchange` entry point, and
// it closes the hole in two rounds. If this test and the one above ever both
// pass or both fail, the pair is no longer measuring the `if resend_all` gate.
// ---------------------------------------------------------------------------

#[test]
fn r12_data_the_same_hole_closes_when_an_ordinary_truncated_pass_banks_a_debt() {
    const ROWS: usize = 17_000;

    let a = store_for(A);
    let b = store_for(B);
    {
        let g = a.lock();
        for i in 0..ROWS {
            g.insert_clipboard(&format!("bulk {i}"), None, None).unwrap();
        }
    }
    let mut rounds = 0;
    loop {
        sync_bounded((&a, A), (&b, B));
        rounds += 1;
        if item_count(&b, A) as usize >= ROWS {
            break;
        }
        assert!(rounds < 8, "guard integrity: the ordinary path never converged");
    }
    assert!(rounds >= 2, "guard integrity: the pass never truncated, so the cap is untested");

    peer_advertises(&b, A, now_ms() + 60 * 60 * 1000);
    let fresh = a.lock().insert_clipboard("written afterwards", None, None).unwrap();
    let (fresh_origin, _) = a.lock().origin_and_text_for_test(fresh).unwrap();

    // `manager.rs`'s bookkeeping, with the `if resend_all` gate removed: any
    // truncated pass owes a resume, and the next exchange starts from it.
    let mut debt: Option<i64> = None;
    let mut truncations = 0usize;
    for _ in 0..4 {
        let (d, _) = sync_bounded_with((&a, A), (&b, B), debt);
        if d.truncated {
            truncations += 1;
            debt = Some(d.resend_progress.unwrap_or(0));
        } else {
            debt = None;
        }
        if text_of(&b, A, &fresh_origin).is_some() {
            break;
        }
    }
    assert!(
        truncations > 0,
        "guard integrity: no pass truncated, so the debt path was never exercised and a \
         pass here would mean nothing"
    );
    assert_eq!(
        text_of(&b, A, &fresh_origin).as_deref(),
        Some("written afterwards"),
        "banking a debt on an ordinary truncated pass did not close the hole either, so the \
         `if resend_all` gate in manager.rs is not the whole cause"
    );
}
