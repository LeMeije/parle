//! ADVERSARIAL REVIEW, ROUND 14. Data and convergence.
//!
//! Round 13 changed three things this file can reach:
//!
//!   * `manager.rs` compare-and-swaps `resend_owed` on a TRUNCATED pass.
//!   * `Store::edit_stamp` still stamps above the ceiling and now warns.
//!   * the `local_only` filters added in round 12 now sit alongside both.
//!
//! Harness shape is the one rounds 11 to 13 settled on: every exchange runs on
//! its own pair of threads under a wall-clock budget, both sockets carry read
//! and write timeouts, every convergence loop is hard-bounded, and no assertion
//! re-locks anything it names in its own message.

#![cfg(test)]

use parle_core::history::{RemoteItem, Store, MAX_CLOCK_SKEW_MS};
use parle_core::types::{HistoryKind, TranscriptionResult};
use parle_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";
const C: &str = "33333333-3333-4333-8333-333333333333";

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

fn tr(text: &str) -> TranscriptionResult {
    TranscriptionResult {
        raw_text: text.to_string(),
        text: text.to_string(),
        language: Some("en".into()),
        model_id: "test".into(),
        duration_ms: 100,
        transcribe_ms: 10,
        segments: vec![],
        trimmed: vec![],
        low_confidence: vec![],
        cleanup_tier: 0,
    }
}

/// One exchange, `x` dialling, under a wall-clock budget. `debt` is what
/// `manager.rs` passes as `(resend_all, resend_from)`.
///
/// `known` names all three devices, because this file runs a three-peer mesh
/// and `Attribution::may_create` is the rule under test in the convergence
/// simulation, not the roster.
fn run_exchange(
    x: (&Arc<Mutex<Store>>, &'static str, Kinds),
    y: (&Arc<Mutex<Store>>, &'static str, Kinds),
    debt: Option<i64>,
) -> (RoundStats, RoundStats) {
    let (resend_all, resend_from) = (debt.is_some(), debt.unwrap_or(0));
    let (sock_x, sock_y) = socket_pair();
    let key = PairedKey::from_bytes([9u8; 32]);
    let k2 = key.clone();
    let (x_store, y_store) = (x.0.clone(), y.0.clone());
    let (x_id, y_id) = (x.1, y.1);
    let (x_kinds, y_kinds) = (x.2, y.2);

    let (tx, rx) = mpsc::channel::<(&'static str, Result<RoundStats, String>)>();
    let tx2 = tx.clone();

    let acceptor = std::thread::spawn(move || {
        let r = (|| {
            let mut s = Session::accept(sock_y, &k2).map_err(|e| e.to_string())?;
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: x_id, local_id: y_id, known: &known };
            exchange(
                &mut s,
                &y_store,
                (y_id, "peer"),
                y_kinds,
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
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: y_id, local_id: x_id, known: &known };
            exchange(
                &mut s,
                &x_store,
                (x_id, "peer"),
                x_kinds,
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

fn sync(x: (&Arc<Mutex<Store>>, &'static str), y: (&Arc<Mutex<Store>>, &'static str)) -> (RoundStats, RoundStats) {
    run_exchange((x.0, x.1, both()), (y.0, y.1, both()), None)
}

// -- reading a store, without going through replication ---------------------

/// `src-tauri` does not depend on `rusqlite`, so every read here goes through
/// `Store::conn_for_test` with the literals inlined. Nothing user-supplied
/// reaches these strings: they are device ids and origin ids this file minted.
fn text_of(store: &Arc<Mutex<Store>>, source: &str, origin: &str) -> Option<String> {
    let g = store.lock();
    g.conn_for_test()
        .query_row(
            &format!(
                "SELECT text FROM items WHERE source_machine='{source}' AND origin_id='{origin}'"
            ),
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
}

fn clock_of(store: &Arc<Mutex<Store>>, source: &str, origin: &str) -> Option<i64> {
    let g = store.lock();
    g.conn_for_test()
        .query_row(
            &format!(
                "SELECT updated_at FROM items WHERE source_machine='{source}' AND origin_id='{origin}'"
            ),
            [],
            |r| r.get::<_, i64>(0),
        )
        .ok()
}

fn mark_for(store: &Arc<Mutex<Store>>, peer: &str, source: &str) -> Option<i64> {
    let g = store.lock();
    g.conn_for_test()
        .query_row(
            &format!(
                "SELECT received_clock FROM source_marks
                  WHERE peer_machine='{peer}' AND source_machine='{source}'"
            ),
            [],
            |r| r.get::<_, i64>(0),
        )
        .ok()
}

fn origin_of(store: &Arc<Mutex<Store>>, id: i64) -> String {
    store.lock().origin_and_text_for_test(id).unwrap().0
}

fn live_count(store: &Arc<Mutex<Store>>) -> i64 {
    store.lock().count().unwrap()
}

/// Every replicable identity a store holds live, with the whole payload that
/// last-writer-wins compares.
#[derive(Debug, PartialEq, Eq, Clone)]
struct Payload {
    kind: String,
    text: String,
    pinned: bool,
    created_at: i64,
    updated_at: i64,
}

fn live_rows(store: &Arc<Mutex<Store>>) -> BTreeMap<(String, String), Payload> {
    let g = store.lock();
    let mut stmt = g
        .conn_for_test()
        .prepare(
            "SELECT source_machine, origin_id, kind, text, pinned, created_at, updated_at
               FROM items
              WHERE source_machine IS NOT NULL AND origin_id IS NOT NULL AND local_only = 0",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                Payload {
                    kind: r.get(2)?,
                    text: r.get(3)?,
                    pinned: r.get::<_, i64>(4)? != 0,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                },
            ))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn tombstone_ids(store: &Arc<Mutex<Store>>) -> BTreeSet<(String, String)> {
    let g = store.lock();
    let mut stmt = g
        .conn_for_test()
        .prepare("SELECT source_machine, origin_id FROM tombstones")
        .unwrap();
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn local_only_ids(store: &Arc<Mutex<Store>>) -> BTreeSet<(String, String)> {
    let g = store.lock();
    let mut stmt = g
        .conn_for_test()
        .prepare(
            "SELECT source_machine, origin_id FROM items
              WHERE local_only = 1 AND source_machine IS NOT NULL AND origin_id IS NOT NULL",
        )
        .unwrap();
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn sql(store: &Arc<Mutex<Store>>, statement: &str) {
    let g = store.lock();
    g.conn_for_test().execute_batch(statement).unwrap();
}

/// The durable state one backwards clock step past the skew window leaves.
///
/// `now_ms()` cannot be moved from inside the process and every store here
/// shares one wall clock, so the step is applied to the durable records it
/// changes: the author's own row clocks, and the peer's copies plus the receipt
/// `mark_received_in` wrote when it applied them.
///
/// Same modelling as `adversarial_r11_data::step_the_authors_clock_back` and
/// the copies in rounds 12 and 13. None of them is touched.
fn step_the_authors_clock_back(
    author: &Arc<Mutex<Store>>,
    author_id: &str,
    peer: &Arc<Mutex<Store>>,
    by_ms: i64,
) -> i64 {
    let t_high = now_ms() + by_ms;
    sql(
        author,
        &format!(
            "UPDATE items SET created_at={t_high}, updated_at={t_high}
              WHERE source_machine='{author_id}';"
        ),
    );
    sql(
        peer,
        &format!(
            "UPDATE items SET created_at={t_high}, updated_at={t_high}
              WHERE source_machine='{author_id}';
             UPDATE source_marks SET received_clock={t_high}
              WHERE source_machine='{author_id}';"
        ),
    );
    t_high
}

/// `manager.rs` with every comment removed, so a source assertion measures the
/// code and not the prose that defends it.
fn manager_code() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join("src/sync/manager.rs"))
        .expect("manager.rs is readable")
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

// ===========================================================================
// R14-1. THE COMPARE-AND-SWAP ROUND 13 ADDED GUARDS ONE OF THE TWO ARMS.
//
// The race round 13 names is: `set_kinds` primes `resend_owed[peer] = 0` while
// an exchange for that peer is already in flight, and the exchange's own write
// lands afterwards and destroys the 0. Round 13 put a compare-and-swap on the
// TRUNCATED arm.
//
// The other arm still runs `i.resend_owed.remove(&peer_id)` unconditionally,
// and it is the arm an ordinary complete exchange takes. So the identical race,
// with the identical consequence, is still open: turn a sync kind on while an
// exchange is running, that exchange completes, and the promise to re-offer is
// deleted before anything served under it.
//
// The comparison also cannot see the case it was written for when the debt it
// read was ALREADY 0, because 0 is the only value `set_kinds` ever writes. That
// is a plain ABA: the value is equal, the meaning is not.
// ===========================================================================

/// R14-1a. The complete-pass arm has no guard at all.
#[test]
fn r14_data_a_complete_pass_still_deletes_the_debt_without_comparing_anything() {
    let code = manager_code();

    // Premise: round 13's compare-and-swap is the one in production.
    assert!(
        code.contains("let current = i.resend_owed.get(&peer_id).copied();"),
        "premise: round 13's read-back is still there"
    );
    assert!(
        code.contains("if current == resend_from {"),
        "premise: and it is compared against the value read before the exchange"
    );

    // The other arm.
    let remove = "i.resend_owed.remove(&peer_id);";
    let at = code.find(remove).expect("premise: the clearing arm still removes the debt");
    let else_at = code[..at]
        .rfind("} else {")
        .expect("premise: the removal is the else arm of the truncation test");
    let between = &code[else_at + "} else {".len()..at];
    assert!(
        between.trim().is_empty(),
        "the complete-pass arm deletes the debt as its FIRST statement, with nothing between \
         it and the `else`, so it cannot have compared anything. Round 13 guarded the truncated \
         arm against a `set_kinds` landing mid-exchange and left the arm an ordinary complete \
         exchange takes wide open. Found between the else and the remove: {between:?}"
    );

    // And what makes the guard blind even where it exists: the only value
    // `set_kinds` ever primes is a literal 0, so when the debt already read 0
    // the compare succeeds and the fresh promise is overwritten anyway.
    assert!(
        code.contains(".map(|d| (d.id.clone(), 0))"),
        "premise: `set_kinds` primes the key with a literal 0 for every paired device, so a \
         debt that already read Some(0) is indistinguishable from one primed mid-exchange"
    );
    // INVERTED. A generation is what makes the guard work: `set_kinds` only
    // ever writes a literal 0, so a value comparison cannot tell a 0 primed
    // mid-flight from the 0 that was read. A counter cannot be confused with
    // itself.
    assert!(
        code.contains("resend_epoch"),
        "nothing carries a generation, so the comparison is on the value alone"
    );

    // What the user is told when the comparison DOES fire. The arm returns
    // `current`, so the resume log names the debt that won rather than the one
    // this pass earned, and when `current` is None — `unpair` clears this key
    // mid-session — it takes the "complete" branch for a re-offer that did not
    // complete. Neither loses data; both are recorded here because the brief
    // for this round asks what the log says.
    let cas = code.find("if current == resend_from {").expect("premise: the compare is there");
    let arm = &code[cas..cas + 240.min(code.len() - cas)];
    assert!(
        arm.contains("} else {") && arm.contains("current\n"),
        "premise: the failure path returns `current`, which is what the resume log then \
         reports: {arm:?}"
    );
    assert!(
        code.contains("hit the batch cap; resuming from {from}")
            && code.contains("re-offer to {peer_id} complete"),
        "premise: those two are the only two things the resume log can say"
    );
}

/// R14-1b. What losing the debt costs, on two real stores.
///
/// Round 13's own pair proves that a debt of 0 reaches a row a disabled kind
/// hid, and that a debt ABOVE it does not. This is the third leg, and it is the
/// one the unguarded arm produces: no debt at all. Ordinary exchanges never
/// offer a row below the peer's own mark, so the row is gone for good.
#[test]
fn r14_data_a_debt_deleted_by_a_complete_pass_strands_the_row_for_ever() {
    let a = store_for(A);
    let b = store_for(B);

    // Clipboard sync is off here, so `serve`'s kind filter drops the row and
    // the peer never sees it. The peer's mark is then lifted OVER it by a
    // dictation stamped later, which is exactly how a disabled kind strands a
    // row: beneath a cursor nothing lowers.
    let hidden = a.lock().insert_clipboard("captured while the switch was off", None, None).unwrap();
    let origin = origin_of(&a, hidden);
    let dictations_only = Kinds { dictations: true, clipboard: false };
    run_exchange((&a, A, dictations_only), (&b, B, both()), None);
    assert!(
        text_of(&b, A, &origin).is_none(),
        "premise: the row must not have reached the peer while the kind was off"
    );
    a.lock().insert_transcription(&tr("a later dictation"), None, None).unwrap();
    run_exchange((&a, A, dictations_only), (&b, B, both()), None);
    assert!(
        mark_for(&b, A, A).unwrap_or(0) > clock_of(&a, A, &origin).unwrap(),
        "premise: the peer's mark for us is now above the hidden row"
    );

    // The kind is back on. The user's promise of a re-offer was deleted by a
    // complete pass that landed after `set_kinds` primed it, so every exchange
    // from here is an ordinary one.
    for _ in 0..6 {
        sync((&a, A), (&b, B));
    }
    assert!(
        text_of(&b, A, &origin).is_none(),
        "with the debt gone nothing ever offers a row below the peer's mark again"
    );
    assert!(
        clock_of(&a, A, &origin).is_some(),
        "guard integrity: the author still holds the row it never managed to send, so the \
         assertion above is about delivery and not about the row having been pruned"
    );

    // And the debt is the only thing that could have delivered it, which is
    // what makes the deletion a permanent loss rather than a delay.
    run_exchange((&a, A, both()), (&b, B, both()), Some(0));
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("captured while the switch was off"),
        "guard integrity: a re-offer from zero DOES reach it, so the loop above measured the \
         missing debt and not some other refusal"
    );
}

// ===========================================================================
// R14-2. `edit_stamp` ABOVE THE CEILING, PRICED AGAINST EVERY ALTERNATIVE.
//
// Round 13 kept the stamp above the ceiling and called it "the least-bad of
// three options". These four tests are that comparison, run rather than argued,
// over two real stores after one backwards clock step of six minutes.
//
// The fourth option, which round 13 did not consider, is to rewrite the row's
// clock DOWNWARDS for the whole source once the machine notices its own clock
// went backwards. It is tested last, because it is the only one that could have
// been better and it is not.
// ===========================================================================

/// A row whose REPLICATION clock is six minutes past the ceiling on both
/// machines, with B's receipt sitting there too. Returns the origin id and the
/// high clock.
///
/// Only `updated_at` is moved. `created_at` is deliberately left in range,
/// because `apply_remote_item` and `drain` BOTH refuse on `created_at` as well,
/// and a fixture that moves it makes every test below pass on that second gate
/// whatever the clock rule under test does. R14-2e is the test for that gate;
/// these four are about the one `edit_stamp` governs.
///
/// The state is ordinary: a dictation captured on Monday, pinned on Tuesday
/// while this machine's clock was six minutes fast, and corrected on Wednesday
/// after NTP put the clock right.
fn a_row_written_while_the_clock_was_fast() -> (Arc<Mutex<Store>>, Arc<Mutex<Store>>, String, i64) {
    let a = store_for(A);
    let b = store_for(B);
    let id = a.lock().insert_clipboard("before", None, None).unwrap();
    let origin = origin_of(&a, id);
    sync((&a, A), (&b, B));
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("before"),
        "premise: the row reached the peer while the clocks agreed"
    );
    let t_high = now_ms() + 6 * 60 * 1000;
    for store in [&a, &b] {
        sql(store, &format!("UPDATE items SET updated_at={t_high} WHERE source_machine='{A}';"));
    }
    sql(&b, &format!("UPDATE source_marks SET received_clock={t_high} WHERE source_machine='{A}';"));
    assert!(
        t_high > now_ms() + MAX_CLOCK_SKEW_MS,
        "premise: six minutes is outside the two-minute window"
    );
    let created: i64 = {
        let g = a.lock();
        g.conn_for_test()
            .query_row(
                &format!("SELECT created_at FROM items WHERE origin_id='{origin}'"),
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(
        created <= now_ms() + MAX_CLOCK_SKEW_MS,
        "premise: created_at stays in range, so it cannot be the thing that refuses the row"
    );
    (a, b, origin, t_high)
}

/// The same shape, but only thirty seconds fast, so every clock involved is
/// INSIDE the accepted window and the receiver's range check is not what
/// decides the outcome.
fn a_row_written_while_the_clock_was_slightly_fast(
) -> (Arc<Mutex<Store>>, Arc<Mutex<Store>>, String, i64) {
    let a = store_for(A);
    let b = store_for(B);
    let id = a.lock().insert_clipboard("before", None, None).unwrap();
    let origin = origin_of(&a, id);
    sync((&a, A), (&b, B));
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("before"),
        "premise: the row reached the peer"
    );
    let t = step_the_authors_clock_back(&a, A, &b, 30_000);
    assert!(t < now_ms() + MAX_CLOCK_SKEW_MS, "premise: thirty seconds is inside the window");
    (a, b, origin, t)
}

/// Overwrite the stamp an alternative `edit_stamp` would have produced. The
/// edit itself is real; only the clock rule is being swapped.
fn edit_with_stamp(store: &Arc<Mutex<Store>>, origin: &str, text: &str, clock: i64) {
    sql(
        store,
        &format!("UPDATE items SET text='{text}', updated_at={clock} WHERE origin_id='{origin}';"),
    );
}

/// R14-2a. PRODUCTION, option three: above the ceiling. Refused, and the
/// refusal banks nothing, which is the whole reason it recovers by itself.
#[test]
fn r14_data_a_stamp_above_the_ceiling_is_refused_and_banks_no_receipt() {
    let (a, b, origin, t_high) = a_row_written_while_the_clock_was_fast();
    let id: i64 = {
        let g = a.lock();
        g.conn_for_test()
            .query_row("SELECT id FROM items WHERE origin_id=?1", [&origin], |r| r.get(0))
            .unwrap()
    };
    a.lock().update_text(id, "corrected").unwrap();
    let stamp = clock_of(&a, A, &origin).unwrap();
    assert_eq!(stamp, t_high + 1, "production stamps one past the row, which is above the ceiling");
    assert!(stamp > now_ms() + MAX_CLOCK_SKEW_MS, "premise: no correctly clocked peer accepts it");

    let before = mark_for(&b, A, A);
    let (d, _) = sync((&a, A), (&b, B));
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("before"),
        "the correction is refused for as long as the wall clock is behind the old stamp"
    );
    assert_eq!(
        mark_for(&b, A, A),
        before,
        "a refusal must bank no receipt: that is the ONLY thing that makes this recoverable, \
         because a receipt at the refused clock would hide the row for ever"
    );
    // The author still holds the winning clock, so the correction is postponed
    // and not destroyed. That is the property the other three options lose.
    assert!(
        clock_of(&a, A, &origin).unwrap() > clock_of(&b, A, &origin).unwrap(),
        "the author must keep a clock strictly above the peer's copy, or nothing can ever \
         beat it again"
    );
    assert!(d.sent_items >= 1, "guard integrity: the row was actually offered: {d:?}");
}

/// R14-2b. OPTION ONE, below the row: the clamped clock alone. The row's own
/// clock walks DOWN and the author no longer holds anything that can beat the
/// peer's copy, so the correction is not delayed, it is gone.
#[test]
fn r14_data_a_stamp_below_the_row_destroys_the_only_clock_that_could_have_won() {
    let (a, b, origin, t_high) = a_row_written_while_the_clock_was_fast();
    let clamped = now_ms() + MAX_CLOCK_SKEW_MS;
    assert!(clamped < t_high, "premise: the clamp really is below the row");
    edit_with_stamp(&a, &origin, "corrected", clamped);

    let mut offered = 0usize;
    for _ in 0..4 {
        let (d, _) = sync((&a, A), (&b, B));
        offered += d.sent_items;
    }
    assert!(
        offered >= 1,
        "guard integrity: the row must actually be OFFERED, or this test measures a cursor \
         and not last-writer-wins"
    );
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("before"),
        "last-writer-wins is strictly greater, so a stamp below the peer's copy never lands"
    );
    assert!(
        clock_of(&a, A, &origin).unwrap() < clock_of(&b, A, &origin).unwrap(),
        "and the author has thrown away the high clock, so no later exchange and no clock \
         correction can produce one: the loss is permanent, not postponed"
    );
    // Pinned from the other side so the test cannot pass by the row simply
    // never being offered.
    assert!(
        text_of(&a, A, &origin).as_deref() == Some("corrected"),
        "guard integrity: the edit really was made locally"
    );
}

/// R14-2c. OPTION TWO, exactly at the row. It is the worst of the four, and for
/// a reason neither of its neighbours has.
///
/// Three parts.
///
/// 1. An ordinary exchange does not OFFER it at all. The peer's cursor is a
///    `(clock, origin)` pair sitting exactly on that row, and `items_from`
///    takes `updated_at > c OR (updated_at = c AND origin_id > o)`. An equal
///    stamp on the same origin satisfies neither. The edit is not refused, it
///    is invisible.
/// 2. Where it IS offered — a one-shot re-offer, which is what a kind or
///    retention widening banks — it falls through last-writer-wins into the
///    payload tiebreak, so whether the user's correction survives depends on
///    where its bytes sort against the text it replaces. Both directions are
///    run, so neither half is vacuous.
/// 3. Six minutes past the ceiling, which is where this rule is actually
///    reached from, it is refused by the range check exactly as production's
///    `current + 1` is. It pays production's whole cost and keeps none of the
///    benefit.
#[test]
fn r14_data_a_stamp_equal_to_the_row_is_never_even_offered() {
    let (a, b, origin, t) = a_row_written_while_the_clock_was_slightly_fast();
    edit_with_stamp(&a, &origin, "zzz corrected", t);
    let mut offered = 0usize;
    for _ in 0..3 {
        let (d, _) = sync((&a, A), (&b, B));
        offered += d.sent_items;
    }
    assert_eq!(
        offered, 0,
        "an equal stamp is not offered at all: the peer's cursor is the PAIR (clock, origin) \
         and it is sitting on this exact row, so the correction never reaches the peer to be \
         judged. Stamping at the row is not a weaker win, it is silence"
    );
    assert_eq!(text_of(&b, A, &origin).as_deref(), Some("before"), "so the peer keeps the old text");
}

#[test]
fn r14_data_a_stamp_equal_to_the_row_then_depends_on_byte_order() {
    // Sorts ABOVE "before": under the one re-offer that ignores the cursor,
    // the correction survives.
    {
        let (a, b, origin, t) = a_row_written_while_the_clock_was_slightly_fast();
        edit_with_stamp(&a, &origin, "zzz corrected", t);
        run_exchange((&a, A, both()), (&b, B, both()), Some(0));
        assert_eq!(
            text_of(&b, A, &origin).as_deref(),
            Some("zzz corrected"),
            "guard integrity: an equal clock CAN land under a re-offer, so the other half is \
             not vacuous"
        );
    }
    // Sorts BELOW "before": the identical edit, offered identically, is
    // silently discarded.
    {
        let (a, b, origin, t) = a_row_written_while_the_clock_was_slightly_fast();
        edit_with_stamp(&a, &origin, "aaa corrected", t);
        for _ in 0..3 {
            run_exchange((&a, A, both()), (&b, B, both()), Some(0));
        }
        assert_eq!(
            text_of(&b, A, &origin).as_deref(),
            Some("before"),
            "stamping at the row makes whether a user's correction replicates depend on where \
             its first byte sorts against the text it replaces"
        );
    }
    // And at the clock the rule is really reached from, it buys nothing at all.
    {
        let (a, b, origin, t_high) = a_row_written_while_the_clock_was_fast();
        edit_with_stamp(&a, &origin, "zzz corrected", t_high);
        let (d, _) = sync((&a, A), (&b, B));
        assert!(d.sent_items >= 1, "guard integrity: the row was offered: {d:?}");
        assert_eq!(
            text_of(&b, A, &origin).as_deref(),
            Some("before"),
            "six minutes past the ceiling an equal stamp is refused by the range check just as \
             `current + 1` is"
        );
    }
}

/// R14-2d. THE FOURTH OPTION: rewrite the whole source's clocks downwards when
/// the machine notices its own clock went backwards.
///
/// It sounds like the real fix and it is not, for one reason that no amount of
/// local rewriting can touch: the PEER's copy still carries the high clock, and
/// last-writer-wins is strictly greater. Lowering our side cannot lower theirs.
///
/// The honest other half is asserted too, because it is the one thing the
/// rewrite genuinely buys: a row the peer never RECEIVED does arrive
/// immediately under it, where production leaves it stranded until the wall
/// clock catches up. So the trade is real, and it still loses: the case
/// `edit_stamp` governs is by construction a row the peer already holds.
#[test]
fn r14_data_rewriting_the_source_downwards_cannot_beat_a_copy_the_peer_already_holds() {
    let (a, b, origin, t_high) = a_row_written_while_the_clock_was_fast();
    // A second row, written while the clock was still fast, that never reached
    // the peer.
    let undelivered = a.lock().insert_clipboard("never delivered", None, None).unwrap();
    let undelivered_origin = origin_of(&a, undelivered);
    sql(
        &a,
        &format!("UPDATE items SET updated_at={t_high} WHERE origin_id='{undelivered_origin}';"),
    );
    assert!(
        text_of(&b, A, &undelivered_origin).is_none(),
        "premise: the peer has never seen the second row"
    );

    // The rewrite: every replication clock this machine holds for its own
    // source is rebased to the corrected wall clock, preserving order.
    let base = now_ms();
    sql(
        &a,
        &format!(
            "UPDATE items SET updated_at={base} + (updated_at - {t_high})
              WHERE source_machine='{A}';"
        ),
    );
    // And the edit, stamped by the rebased clock.
    edit_with_stamp(&a, &origin, "corrected", base + 5);

    for _ in 0..4 {
        sync((&a, A), (&b, B));
    }
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("before"),
        "the rewrite delivers nothing for a row the peer already holds: its copy still carries \
         the pre-correction clock and nothing in the protocol can lower it"
    );
    assert_eq!(
        text_of(&b, A, &undelivered_origin).as_deref(),
        Some("never delivered"),
        "guard integrity: the rebased rows ARE being offered and accepted, so the assertion \
         above measures last-writer-wins and not a silent serve"
    );
}

/// R14-2e. THE CHOICE `edit_stamp` MAKES IS MOOT FOR A ROW CREATED WHILE THE
/// CLOCK WAS FAST, BECAUSE `created_at` IS REFUSED ON THE SAME CEILING AND
/// NOTHING EVER RESTAMPS IT.
///
/// Round 13's comment prices the trade entirely in terms of the replication
/// clock: "the edit is not lost, it is postponed until the wall clock climbs
/// past the row's pre-correction stamp". Both `drain` and `apply_remote_item`
/// apply the identical ceiling to `created_at`, and no write path in the store
/// ever changes `created_at` after the insert. So for the rows that a
/// backwards clock step actually produces — the ones captured while the clock
/// was fast — every candidate rule for `edit_stamp`, including one that stamps
/// perfectly in range, is refused anyway and on the same deadline.
///
/// That does not overturn round 13's conclusion. It does mean the comment
/// defending it describes only half of what is holding the row back.
#[test]
fn r14_data_a_row_created_while_the_clock_was_fast_is_refused_whatever_the_edit_stamp_says() {
    let a = store_for(A);
    let b = store_for(B);
    let id = a.lock().insert_clipboard("captured while the clock was fast", None, None).unwrap();
    let origin = origin_of(&a, id);
    let t_high = now_ms() + 6 * 60 * 1000;
    // Created while the clock was six minutes fast. Its replication clock has
    // since been stamped perfectly in range: this is the BEST any alternative
    // `edit_stamp` could do.
    let in_range = now_ms() + 1_000;
    sql(
        &a,
        &format!("UPDATE items SET created_at={t_high}, updated_at={in_range} WHERE id={id};"),
    );
    assert!(in_range < now_ms() + MAX_CLOCK_SKEW_MS, "premise: the replication clock is acceptable");

    let (d, ac) = sync((&a, A), (&b, B));
    assert!(d.sent_items >= 1, "guard integrity: the row was offered: {d:?}");
    assert!(
        text_of(&b, A, &origin).is_none(),
        "the row is refused on `created_at` alone. Every rule `edit_stamp` could follow lands \
         on the same deadline, so round 13's three-way trade decides nothing for the rows a \
         backwards clock step actually produces"
    );
    assert!(ac.ignored >= 1, "guard integrity: the peer counted a refusal: {ac:?}");
    assert_eq!(
        mark_for(&b, A, A),
        None,
        "and, correctly, it banks no receipt, so it is offered again when the clock catches up"
    );
}

// ===========================================================================
// R14-3. THE `local_only` FILTERS, AGAINST PAGING AND AGAINST CONVERGENCE.
// ===========================================================================

/// R14-3a. The page LIMIT counts rows that can actually be sent.
///
/// The filter is in the SQL for exactly this reason: `serve` decides whether to
/// keep paging from `page.len() >= PAGE`, so a filter applied in Rust would
/// make a full page read short and end the pass with rows still to come.
#[test]
fn r14_data_a_page_limit_counts_only_the_rows_that_can_be_sent() {
    let a = store_for(A);
    let mut sendable: Vec<String> = Vec::new();
    for i in 0..40 {
        if i % 2 == 0 {
            let id = a.lock().insert_clipboard(&format!("row {i}"), None, None).unwrap();
            sendable.push(origin_of(&a, id));
        } else {
            let id = a.lock().insert_transcription_local_only(&tr(&format!("secret {i}")), None, None).unwrap();
            let _ = origin_of(&a, id);
        }
    }
    assert_eq!(sendable.len(), 20, "premise: half the rows are withheld");

    let page = a.lock().items_from(A, 0, "", 8).unwrap();
    assert_eq!(
        page.len(),
        8,
        "a LIMIT of 8 over a history that is half withheld must still return 8 SENDABLE rows. \
         Filtering after the query returns 4 and `serve` reads that as the end of the source"
    );
    assert!(
        page.iter().all(|r| !r.text.starts_with("secret")),
        "guard integrity: no withheld row is in the page"
    );

    // And walking the keyset covers every sendable row exactly once.
    let mut seen: Vec<String> = Vec::new();
    let (mut at, mut origin) = (0i64, String::new());
    for _ in 0..20 {
        let page = a.lock().items_from(A, at, &origin, 3).unwrap();
        if page.is_empty() {
            break;
        }
        at = page.last().unwrap().updated_at;
        origin = page.last().unwrap().origin_id.clone();
        seen.extend(page.into_iter().map(|r| r.origin_id));
    }
    seen.sort();
    let mut want = sendable.clone();
    want.sort();
    assert_eq!(seen, want, "the keyset walk must reach every sendable row and no withheld one");
}

/// R14-3b. A store that is ENTIRELY withheld rows finishes an exchange, offers
/// nothing, and does not spin.
#[test]
fn r14_data_a_store_that_is_entirely_local_only_finishes_and_stays_quiet() {
    let a = store_for(A);
    let b = store_for(B);
    for i in 0..12 {
        a.lock().insert_transcription_local_only(&tr(&format!("withheld {i}")), None, None).unwrap();
    }
    assert_eq!(live_count(&a), 12, "premise: the store holds rows");
    assert_eq!(
        a.lock().known_sources().unwrap(),
        vec![A.to_string()],
        "premise: the source is still named, so `serve` really does iterate it"
    );

    for round in 0..4 {
        let (d, ac) = sync((&a, A), (&b, B));
        assert_eq!(d.sent_items, 0, "round {round}: a withheld row was offered: {d:?}");
        assert_eq!(d.sent_tombstones, 0, "round {round}: a withheld row minted a tombstone: {d:?}");
        assert_eq!(ac.applied_items, 0, "round {round}: the peer stored something: {ac:?}");
    }
    assert_eq!(live_count(&b), 0, "nothing withheld may ever reach the peer");
    assert!(
        tombstone_ids(&b).is_empty(),
        "and the peer must not learn the identities either"
    );
    assert_eq!(mark_for(&b, A, A), None, "no receipt is invented for a source that sent nothing");
}

/// R14-3c. Rows counted on one side and not the other move no cursor, no
/// receipt and no watermark, through the delete and Clear paths as well.
#[test]
fn r14_data_withheld_rows_skew_no_cursor_receipt_or_watermark_on_either_side() {
    let a = store_for(A);
    let b = store_for(B);
    let shared = a.lock().insert_clipboard("shared", None, None).unwrap();
    let shared_origin = origin_of(&a, shared);
    sync((&a, A), (&b, B));
    let mark_after_share = mark_for(&b, A, A).expect("premise: the peer banked a receipt");

    // Three withheld rows, then a delete of one and a Clear over the rest.
    let mut withheld = Vec::new();
    for i in 0..3 {
        let id = a.lock().insert_transcription_local_only(&tr(&format!("withheld {i}")), None, None).unwrap();
        withheld.push((id, origin_of(&a, id)));
    }
    a.lock().delete(withheld[0].0).unwrap();
    a.lock().clear(None).unwrap();

    let local_tombs = tombstone_ids(&a);
    for (_, origin) in &withheld {
        assert!(
            !local_tombs.contains(&(A.to_string(), origin.clone())),
            "a withheld row must not mint a tombstone even locally; the tombstone is what \
             announces the identity and the timing to every peer"
        );
    }
    assert!(
        local_tombs.contains(&(A.to_string(), shared_origin.clone())),
        "guard integrity: the Clear DID mint tombstones, so the assertion above is not vacuous"
    );

    for _ in 0..3 {
        sync((&a, A), (&b, B));
    }
    assert_eq!(
        tombstone_ids(&b),
        BTreeSet::from([(A.to_string(), shared_origin.clone())]),
        "the peer must learn exactly the one delete it was entitled to and no other identity"
    );
    assert!(
        mark_for(&b, A, A).unwrap() >= mark_after_share,
        "a receipt never walks backwards"
    );
    // Our own (peer, our id) mark exists on purpose — it gates the deletes a
    // peer relays back about our rows — so what matters is that it can only
    // have been lifted by something the peer really sent us. The only thing B
    // could relay about source A is the tombstone for the shared row, so the
    // mark must be exactly that clock and not the clock of any withheld row.
    let shared_tomb: i64 = {
        let g = a.lock();
        g.conn_for_test()
            .query_row(
                &format!("SELECT deleted_at FROM tombstones WHERE origin_id='{shared_origin}'"),
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        mark_for(&a, B, A),
        Some(shared_tomb),
        "the cursor we keep for what a peer relays about our own source must sit exactly on \
         the one delete it was told about; a withheld row must not be able to move it"
    );
    assert_eq!(live_count(&b), 0, "the shared row is gone from the peer");
}

/// R14-3d. Withheld rows do not shorten a TRUNCATED pass, and do not cross it.
///
/// 16,384 rows is one full exchange, so this is the interaction the round-12
/// filter and round-13's resume debt share: the filter decides what a page
/// contains, and the debt decides where the next pass starts.
#[test]
fn r14_data_withheld_rows_neither_shorten_nor_cross_a_truncated_pass() {
    let a = store_for(A);
    let b = store_for(B);
    // `MAX_BATCHES` is private to `replicate.rs`, so its value is restated
    // here. `d1.truncated` below is the premise that catches it drifting.
    let sendable = 64 * parle_sync::MAX_BATCH_LEN + 300;
    let base = now_ms() - 5_000_000;
    {
        let g = a.lock();
        for i in 0..sendable {
            g.apply_remote_item(C, &RemoteItem {
                source_machine: A.into(),
                origin_id: format!("s{i:06}"),
                kind: "clipboard".into(),
                text: "x".into(),
                created_at: base + i as i64,
                updated_at: base + i as i64,
                pinned: false,
            })
            .unwrap();
        }
        // Withheld rows interleaved through the same clock range.
        for i in 0..(sendable / 4) {
            g.apply_remote_item(C, &RemoteItem {
                source_machine: A.into(),
                origin_id: format!("w{i:06}"),
                kind: "clipboard".into(),
                text: "withheld".into(),
                created_at: base + (i * 4) as i64,
                updated_at: base + (i * 4) as i64,
                pinned: false,
            })
            .unwrap();
        }
        g.conn_for_test()
            .execute_batch("UPDATE items SET local_only = 1 WHERE origin_id LIKE 'w%';")
            .unwrap();
    }
    let withheld_here = local_only_ids(&a).len();
    assert_eq!(withheld_here, sendable / 4, "premise: the withheld rows are there");

    let (d1, _) = run_exchange((&a, A, both()), (&b, B, both()), Some(0));
    assert!(d1.truncated, "premise: a history this size must truncate: {d1:?}");
    assert!(
        (live_count(&b) as usize) < sendable,
        "premise: the cap really did bite"
    );

    let mut from = d1.resend_progress.unwrap_or(0);
    let mut rounds = 0;
    for _ in 0..10 {
        if live_count(&b) as usize == sendable {
            break;
        }
        rounds += 1;
        let (r, _) = run_exchange((&a, A, both()), (&b, B, both()), Some(from));
        if r.truncated {
            from = r.resend_progress.unwrap_or(from);
        }
    }
    assert!(rounds >= 1, "guard integrity: the resume loop actually ran");
    assert_eq!(
        live_count(&b) as usize,
        sendable,
        "the resume must deliver every sendable row: a withheld row inside a page must not \
         make the page read short and end the source early"
    );
    assert!(
        local_only_ids(&b).is_empty() && live_count(&b) as usize == sendable,
        "and not one withheld row may cross, however the pass was cut"
    );
}

// ===========================================================================
// R14-4. THREE PEERS, A SEEDED RANDOM WORKLOAD, RUN TO A FIXED POINT.
//
// This is the point of the whole feature, so it is the one result worth having.
// Everything the design names is in the workload: inserts of both kinds, edits,
// pins, single deletes, Clear History, per-machine exclusion lists, withheld
// rows, clocks up to the edge of the accepted skew window, a peer that goes
// away and comes back, and one-shot re-offers standing in for a kind widening.
//
// Two things are deliberately NOT in it, because the design rules them out and
// including them would assert a convergence nobody promised:
//
//   * edits and pins on a row this machine did NOT author, which are local by
//     design and documented as such;
//   * clocks BEYOND the skew window, which are refused by design. R14-4b covers
//     those on their own and asserts what they must not do, which is stop
//     everything else converging.
// ===========================================================================

/// xorshift64*, written out here because the harness must not depend on the
/// wall clock or on a crate for its entropy: the same seed must produce the
/// same sequence on every machine and in every run, or a failure cannot be
/// minimised.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

struct Peer {
    id: &'static str,
    store: Arc<Mutex<Store>>,
    /// How far ahead of the wall clock this machine's own stamps run, inside
    /// the accepted window.
    fast_by: i64,
    away_until: usize,
}

fn ids_authored_by(store: &Arc<Mutex<Store>>, source: &str) -> Vec<i64> {
    let g = store.lock();
    let mut stmt = g
        .conn_for_test()
        .prepare(
            "SELECT id FROM items
              WHERE source_machine=?1 AND origin_id IS NOT NULL AND local_only=0
              ORDER BY id",
        )
        .unwrap();
    let rows = stmt.query_map([source], |r| r.get::<_, i64>(0)).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn all_ids(store: &Arc<Mutex<Store>>) -> Vec<i64> {
    let g = store.lock();
    let mut stmt = g.conn_for_test().prepare("SELECT id FROM items ORDER BY id").unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

/// Apply this machine's clock offset to the row it just wrote. Faithful to what
/// a machine whose clock is `fast_by` ahead actually stores, and never past the
/// ceiling, so the receiving side is supposed to accept it.
fn apply_clock_offset(peer: &Peer, id: i64) {
    if peer.fast_by == 0 {
        return;
    }
    let want = now_ms() + peer.fast_by;
    sql(
        &peer.store,
        &format!(
            "UPDATE items SET created_at = MAX(created_at, {want}),
                              updated_at = MAX(updated_at + 1, {want})
              WHERE id = {id};"
        ),
    );
}

/// Everything the three stores must agree on once the dust settles.
fn assert_converged(peers: &[Peer], what: &str) {
    let rows: Vec<_> = peers.iter().map(|p| live_rows(&p.store)).collect();
    let tombs: Vec<_> = peers.iter().map(|p| tombstone_ids(&p.store)).collect();
    let withheld: BTreeSet<(String, String)> =
        peers.iter().flat_map(|p| local_only_ids(&p.store)).collect();

    // Every identity anyone has an opinion about.
    let mut every: BTreeSet<(String, String)> = BTreeSet::new();
    for r in &rows {
        every.extend(r.keys().cloned());
    }
    for t in &tombs {
        every.extend(t.iter().cloned());
    }
    assert!(!every.is_empty(), "{what}: guard integrity: the workload produced nothing to check");

    let mut agreed = 0usize;
    let mut buried = 0usize;
    for id in &every {
        assert!(
            !withheld.contains(id),
            "{what}: a withheld identity {id:?} reached replication"
        );
        let deleted = tombs.iter().any(|t| t.contains(id));
        if deleted {
            for (i, r) in rows.iter().enumerate() {
                assert!(
                    !r.contains_key(id),
                    "{what}: {} still holds {id:?} after a tombstone for it exists somewhere; \
                     a tombstone is absorbing",
                    peers[i].id
                );
            }
            for (i, t) in tombs.iter().enumerate() {
                assert!(
                    t.contains(id),
                    "{what}: {} never learned the delete of {id:?}",
                    peers[i].id
                );
            }
            buried += 1;
            continue;
        }
        // Alive. Every peer must hold it, with the same payload.
        let mut seen: Option<&Payload> = None;
        for (i, r) in rows.iter().enumerate() {
            let got = r.get(id).unwrap_or_else(|| {
                panic!("{what}: {} is missing the live row {id:?}", peers[i].id)
            });
            match seen {
                None => seen = Some(got),
                Some(first) => assert_eq!(
                    first, got,
                    "{what}: {} and {} disagree about {id:?}",
                    peers[0].id, peers[i].id
                ),
            }
        }
        agreed += 1;
    }
    assert!(
        agreed > 0 && buried > 0,
        "{what}: guard integrity: the workload must produce both surviving rows ({agreed}) and \
         tombstoned ones ({buried}), or half this assertion is vacuous"
    );
}

/// Exchange every pair until nothing is applied anywhere. Bounded, and the
/// bound failing IS a finding.
fn settle(peers: &[Peer], what: &str) {
    let pairs = [(0usize, 1usize), (1, 2), (0, 2)];
    for sweep in 0..12 {
        let mut moved = 0usize;
        let mut offered = 0usize;
        for (i, j) in pairs {
            let (d, a) = run_exchange(
                (&peers[i].store, peers[i].id, both()),
                (&peers[j].store, peers[j].id, both()),
                None,
            );
            moved += d.applied_items + d.applied_tombstones + a.applied_items + a.applied_tombstones;
            offered += d.sent_items + d.sent_tombstones + a.sent_items + a.sent_tombstones;
        }
        if moved == 0 {
            // ONE more full sweep, because "nothing was applied" is not yet
            // "nothing is offered": the last productive sweep still carries the
            // no-op echo each side owes the other, and it is that echo which
            // lifts the cursors. Asserting silence on the sweep that first
            // reported `moved == 0` fails on a mesh that is converging
            // perfectly well, and it cost this file a false finding.
            //
            // The confirming sweep is where a spin shows: if anything is still
            // offered once every cursor has settled, it will be offered again
            // on every exchange for the life of the pairing.
            let _ = offered;
            let mut again = 0usize;
            let mut still_offered = 0usize;
            for (i, j) in pairs {
                let (d, a) = run_exchange(
                    (&peers[i].store, peers[i].id, both()),
                    (&peers[j].store, peers[j].id, both()),
                    None,
                );
                again += d.applied_items + d.applied_tombstones + a.applied_items + a.applied_tombstones;
                still_offered += d.sent_items + d.sent_tombstones + a.sent_items + a.sent_tombstones;
            }
            assert_eq!(again, 0, "{what}: sweep {sweep} applied nothing and the next one applied {again}");
            assert_eq!(
                still_offered, 0,
                "{what}: the mesh went quiet on sweep {sweep} but is still offering \
                 {still_offered} rows or tombstones on every exchange after that, for ever. \
                 That is a spin"
            );
            return;
        }
    }
    panic!("{what}: three peers did not reach a fixed point in 12 full sweeps");
}

fn run_one_seed(seed: u64) {
    let mut rng = Rng::new(seed);
    let mut peers = vec![
        Peer { id: A, store: store_for(A), fast_by: 0, away_until: 0 },
        Peer { id: B, store: store_for(B), fast_by: 45_000, away_until: 0 },
        Peer { id: C, store: store_for(C), fast_by: MAX_CLOCK_SKEW_MS - 10_000, away_until: 0 },
    ];
    // One machine keeps a password manager out of its history. Set once and
    // never changed, because changing it mid-run asserts a convergence the
    // design deliberately does not offer: a row already replicated stays on the
    // peer that holds it.
    {
        let mut g = peers[1].store.lock();
        g.set_excluded_apps(vec!["com.vault.app".into()]);
    }

    let pairs = [(0usize, 1usize), (1, 2), (0, 2)];
    let rounds = 26;
    for round in 0..rounds {
        // 1 to 3 local operations somewhere.
        for _ in 0..(1 + rng.below(3)) {
            let who = rng.below(3);
            let op = rng.below(100);
            let text = format!("s{seed}-r{round}-{}", rng.next() % 1000);
            let store = peers[who].store.clone();
            let id = if op < 30 {
                Some(store.lock().insert_transcription(&tr(&text), None, None).unwrap())
            } else if op < 55 {
                Some(store.lock().insert_clipboard(&text, None, None).unwrap())
            } else if op < 62 {
                // A dictation the accessibility probe could not classify.
                store.lock().insert_transcription_local_only(&tr(&text), None, None).unwrap();
                None
            } else if op < 68 {
                // A row from the excluded app. Only peer B excludes it.
                Some(
                    store
                        .lock()
                        .insert_clipboard(&text, Some("com.vault.app"), Some("Vault"))
                        .unwrap(),
                )
            } else if op < 80 {
                // An edit, only on a row this machine authored.
                let mine = ids_authored_by(&store, peers[who].id);
                if !mine.is_empty() {
                    let pick = mine[rng.below(mine.len())];
                    store.lock().update_text(pick, &format!("edited {text}")).unwrap();
                    Some(pick)
                } else {
                    None
                }
            } else if op < 88 {
                // A pin or unpin, again only on a row this machine authored.
                let mine = ids_authored_by(&store, peers[who].id);
                if !mine.is_empty() {
                    let pick = mine[rng.below(mine.len())];
                    let pinned = rng.below(2) == 0;
                    store.lock().set_pinned(pick, pinned).unwrap();
                    Some(pick)
                } else {
                    None
                }
            } else if op < 97 {
                // A delete, of ANY row this machine holds. Deletes travel for
                // every source, which is the rule most of the design exists to
                // protect.
                let any = all_ids(&store);
                if !any.is_empty() {
                    store.lock().delete(any[rng.below(any.len())]).unwrap();
                }
                None
            } else if round < rounds * 2 / 3 {
                // Clear History, the product's panic button.
                let kind = match rng.below(3) {
                    0 => None,
                    1 => Some(HistoryKind::Transcription),
                    _ => Some(HistoryKind::Clipboard),
                };
                store.lock().clear(kind).unwrap();
                None
            } else {
                None
            };
            if let Some(id) = id {
                apply_clock_offset(&peers[who], id);
            }
        }

        // One machine goes away for a stretch and comes back.
        if rng.below(100) < 12 {
            let who = rng.below(3);
            peers[who].away_until = round + 2 + rng.below(4);
        }

        // One or two exchanges, skipping anyone who is away.
        for _ in 0..(1 + rng.below(2)) {
            let (i, j) = pairs[rng.below(3)];
            if round < peers[i].away_until || round < peers[j].away_until {
                continue;
            }
            // Every so often the dialler owes a full re-offer, which is what a
            // kind widening banks.
            let debt = if rng.below(100) < 15 { Some(0) } else { None };
            run_exchange(
                (&peers[i].store, peers[i].id, both()),
                (&peers[j].store, peers[j].id, both()),
                debt,
            );
        }
    }

    // Everyone is back.
    for p in peers.iter_mut() {
        p.away_until = 0;
    }
    let what = format!("seed {seed}");
    settle(&peers, &what);

    // The excluded rows are the one class that legitimately does not travel, so
    // they are checked separately and then removed from the picture.
    let excluded: Vec<(String, String)> = {
        let g = peers[1].store.lock();
        let mut stmt = g
            .conn_for_test()
            .prepare(
                "SELECT source_machine, origin_id FROM items
                  WHERE app_id='com.vault.app' AND source_machine IS NOT NULL AND origin_id IS NOT NULL",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    for id in &excluded {
        for other in [0usize, 2] {
            assert!(
                !live_rows(&peers[other].store).contains_key(id),
                "{what}: an excluded row {id:?} reached {}",
                peers[other].id
            );
        }
        sql(
            &peers[1].store,
            &format!("DELETE FROM items WHERE origin_id='{}';", id.1),
        );
    }

    assert_converged(&peers, &what);
}

#[test]
fn r14_data_three_peers_converge_under_a_seeded_random_workload() {
    for seed in [1u64, 7, 42, 1337, 90210, 20260828] {
        run_one_seed(seed);
    }
}

/// R14-4b. A row stamped beyond the skew window is refused, banks nothing, and
/// does not stop the rest of the mesh converging.
#[test]
fn r14_data_a_row_beyond_the_window_does_not_stop_the_rest_converging() {
    let peers = vec![
        Peer { id: A, store: store_for(A), fast_by: 0, away_until: 0 },
        Peer { id: B, store: store_for(B), fast_by: 0, away_until: 0 },
        Peer { id: C, store: store_for(C), fast_by: 0, away_until: 0 },
    ];
    // Ordinary traffic, plus one delete so the tombstone half of the
    // convergence assertion is not vacuous.
    for (i, p) in peers.iter().enumerate() {
        for j in 0..3 {
            p.store.lock().insert_clipboard(&format!("p{i} row {j}"), None, None).unwrap();
        }
    }
    let doomed = peers[0].store.lock().insert_clipboard("deleted later", None, None).unwrap();
    settle(&peers, "before the bad clock");
    peers[0].store.lock().delete(doomed).unwrap();

    // One row from a machine whose clock is six minutes fast.
    let bad = peers[2].store.lock().insert_clipboard("from a very fast clock", None, None).unwrap();
    let bad_origin = origin_of(&peers[2].store, bad);
    let t_high = now_ms() + 6 * 60 * 1000;
    sql(
        &peers[2].store,
        &format!("UPDATE items SET created_at={t_high}, updated_at={t_high} WHERE id={bad};"),
    );

    let pairs = [(0usize, 1usize), (1, 2), (0, 2)];
    let mut refusals = 0usize;
    for _ in 0..6 {
        for (i, j) in pairs {
            let (d, a) = run_exchange(
                (&peers[i].store, peers[i].id, both()),
                (&peers[j].store, peers[j].id, both()),
                None,
            );
            refusals += d.ignored + a.ignored;
        }
    }
    assert!(refusals > 0, "guard integrity: the out-of-window row must actually be refused");
    for other in [0usize, 1] {
        assert!(
            text_of(&peers[other].store, C, &bad_origin).is_none(),
            "a row beyond the window must not be stored"
        );
        assert!(
            mark_for(&peers[other].store, C, C).unwrap_or(0) < t_high,
            "and it must bank no receipt, or the machine is muted for as long as the drift \
             lasted even after its clock is fixed"
        );
    }

    // Now remove it, the way a user would, and prove the rest of the mesh had
    // converged all along rather than merely being stuck behind the bad row.
    sql(&peers[2].store, &format!("DELETE FROM items WHERE id={bad};"));
    settle(&peers, "after the bad row is gone");
    assert_converged(&peers, "after the bad row is gone");
}
