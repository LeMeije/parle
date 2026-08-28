//! ADVERSARIAL REVIEW, ROUND 9. Concurrency, lifecycle and resource management.
//!
//! Round 8 fixed two criticals in this scope: the dial is now spawned OUTSIDE
//! `inner`, and a receipt is now taken inside the same transaction as the row
//! it describes. The brief for this round is to break those fixes, so every
//! test here points at the newest code first.
//!
//! House rules this file obeys, from `docs/SYNC_HANDOVER.md` section 4:
//!
//! - every socket has a read AND a write timeout, every loop a hard bound, and
//!   any exchange runs on two threads under a wall-clock budget so a stall
//!   FAILS naming the stalled side (`bounded`, below, is `sync_bounded` from
//!   `adversarial_r7_scale`);
//! - a guard that can find nothing asserts that it found something first;
//! - failure output is a count and a couple of entries, never two big vectors.
//!
//! Following the house convention (see `adversarial_r8_conc`), a test that
//! demonstrates a LIVE defect asserts the defective behaviour and says
//! CONFIRMED in the message, so the suite stays green and a fix flips the test.

#![cfg(test)]

use parle_core::history::{ApplyOutcome, RemoteItem, RemoteTombstone, Store};
use parle_core::types::TranscriptionResult;
use parle_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";

/// Wall-clock budget on any exchange in this file.
const BUDGET: Duration = Duration::from_secs(60);

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
        sock.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(15))).unwrap();
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

/// One exchange, `x` dialling, under a wall-clock budget, both sides on their
/// own thread. A side that never returns is a FAILED assertion naming it.
fn bounded(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
) -> (RoundStats, RoundStats) {
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
                false,
                0,
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
                "the exchange did not finish inside {BUDGET:?}; only these sides returned: {:?}",
                got.iter().map(|(w, _)| *w).collect::<Vec<_>>()
            ),
            Err(e) => panic!("both exchange threads died without reporting: {e}"),
        }
    }
    acceptor.join().expect("acceptor thread panicked");
    dialler.join().expect("dialler thread panicked");

    let (mut d, mut a) = (None, None);
    for (who, r) in got {
        let stats = r.unwrap_or_else(|e| panic!("{who} side failed: {e}"));
        if who == "dialler" {
            d = Some(stats)
        } else {
            a = Some(stats)
        }
    }
    (d.expect("dialler reported"), a.expect("acceptor reported"))
}

fn dictation(text: &str) -> TranscriptionResult {
    TranscriptionResult {
        raw_text: text.to_string(),
        text: text.to_string(),
        language: Some("en".into()),
        model_id: "test".into(),
        duration_ms: 1_000,
        transcribe_ms: 50,
        segments: Vec::new(),
        trimmed: Vec::new(),
        low_confidence: Vec::new(),
        cleanup_tier: 0,
    }
}

/// Fill `store` with `n` rows attributed to `source`, clocks ascending from
/// `base`, through the replication path so each row carries a real identity.
fn seed(store: &Store, source: &str, n: usize, base: i64) {
    for i in 0..n {
        store
            .apply_remote_item(
                "seeder",
                &RemoteItem {
                    source_machine: source.into(),
                    origin_id: format!("row-{i:07}"),
                    kind: "clipboard".into(),
                    text: format!("row {i}"),
                    created_at: base + i as i64,
                    updated_at: base + i as i64,
                    pinned: false,
                },
            )
            .unwrap();
    }
}

fn mark_for(store: &Store, peer: &str, source: &str) -> Option<i64> {
    store
        .watermarks(peer)
        .unwrap()
        .into_iter()
        .find(|(s, _)| s == source)
        .map(|(_, c)| c)
}

// ---------------------------------------------------------------------------
// 1. THE HOT PATH. `next_clock_for` runs a UNION over items and tombstones on
//    every local insert, edit and delete, under the mutex the history window
//    shares.
// ---------------------------------------------------------------------------

/// Median of `runs` local dictation inserts, in microseconds.
fn median_insert_us(store: &Store, runs: usize) -> u128 {
    let mut samples: Vec<u128> = Vec::with_capacity(runs);
    for i in 0..runs {
        let t = Instant::now();
        store.insert_transcription(&dictation(&format!("hot path {i}")), None, None).unwrap();
        samples.push(t.elapsed().as_micros());
    }
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// R9-1. Does an ordinary dictation get slower as the history grows?
///
/// `insert_transcription` calls `local_clock` -> `next_clock_for(me)`, which is
///
///     SELECT MAX(c) FROM (
///       SELECT COALESCE(updated_at, created_at) FROM items WHERE source_machine=?
///       UNION ALL
///       SELECT deleted_at FROM tombstones WHERE source_machine=? )
///
/// `COALESCE(updated_at, created_at)` needs a column that the
/// `(source_machine, updated_at)` index does not carry, so the index cannot
/// answer the MAX on its own.
///
/// This measures rather than guessing, and the assertion is a factor rather
/// than a millisecond count so it means the same thing on a slow box.
#[test]
fn r9_a_local_dictation_does_not_slow_down_as_history_grows() {
    let base_clock = now_ms() - 10_000_000;

    let mut small = Store::open_in_memory().unwrap();
    small.set_device_id(A);
    seed(&small, A, 200, base_clock);
    let base_us = median_insert_us(&small, 40);

    // The shipped default cap (`HistorySettings::max_items` = 10_000).
    let mut capped = Store::open_in_memory().unwrap();
    capped.set_device_id(A);
    seed(&capped, A, 10_000, base_clock);
    let capped_us = median_insert_us(&capped, 40);

    let mut big = Store::open_in_memory().unwrap();
    big.set_device_id(A);
    seed(&big, A, 25_000, base_clock);
    let big_us = median_insert_us(&big, 40);

    // CONTROL, and the whole point of it: the same 40k rows, but attributed to
    // ANOTHER device. The table, the indexes and the insert are identical; only
    // `next_clock_for(me)`'s `WHERE source_machine = me` scan is empty. If this
    // one is fast, the cost above is that scan and nothing else.
    let mut foreign = Store::open_in_memory().unwrap();
    foreign.set_device_id(A);
    seed(&foreign, B, 25_000, base_clock);
    let foreign_us = median_insert_us(&foreign, 40);

    // Guard that can find nothing: prove the stores really are the sizes this
    // test needs before believing anything about the timings.
    assert!(
        big.count().unwrap() > 24_000
            && foreign.count().unwrap() > 24_000
            && small.count().unwrap() < 1_000,
        "the stores are not the sizes this test needs: big={} foreign={} small={}",
        big.count().unwrap(),
        foreign.count().unwrap(),
        small.count().unwrap()
    );

    eprintln!(
        "R9-1 median local insert: 200 rows = {base_us}us, 10k (shipped cap) = {capped_us}us, \
         25k = {big_us}us, 25k owned by ANOTHER device = {foreign_us}us"
    );
    // INVERTED, as the message on the old assertion asked.
    //
    // The finding was real: `next_clock_for` took a single `MAX` over a
    // `UNION ALL` of items and tombstones, and SQLite plans that as a
    // co-routine feeding an aggregate, so it VISITED every row for our own
    // source. Measured then: 80us at 200 rows, 1,240us at the shipped 10,000
    // cap, 3,080us at 25,000, against 61us for a store holding 25,000 rows
    // owned by somebody else. Fifty times, on the app's hottest path, under the
    // mutex the history window shares. And the scanned set had no ceiling,
    // because tombstones are never pruned by age.
    //
    // Two plain `MAX` queries instead: each is a covering-index seek to the
    // last entry, so the cost no longer depends on how much history we hold.
    //
    // The assertion is a RATIO against the small store, not an absolute
    // microsecond figure, because this suite runs in parallel and an absolute
    // threshold would fail on a loaded machine while the code was correct. A
    // scan would show up here as hundreds of times the cost, so the bound is
    // generous and still catches the regression it exists for.
    assert!(
        big_us <= base_us.max(1) * 8,
        "an insert into a 25k-row store costs {big_us}us against {base_us}us into a 200-row one. \
         `next_clock_for` is scanning our own history again, on every dictation, edit and \
         delete, under the store mutex the history window shares. \
         (10k = {capped_us}us, 25k owned by another device = {foreign_us}us.)"
    );
}

/// R9-1b. REGRESSION GUARD. Clear History must not become quadratic again.
///
/// FOUND QUADRATIC IN THIS ROUND, and fixed while the round was running.
/// `Store::clear` wrote every tombstone in one `INSERT ... SELECT` whose clock
/// was a CORRELATED subquery, written out twice inside a CASE:
///
///     COALESCE((SELECT MAX(c) FROM (
///        SELECT COALESCE(updated_at, created_at) FROM items     WHERE source_machine = i.source_machine
///        UNION ALL
///        SELECT deleted_at                       FROM tombstones WHERE source_machine = i.source_machine)), 0) + 1
///
/// That is a scan of the whole source per row on a column no index covers, so
/// the statement was O(rows^2) inside one transaction holding the store mutex.
/// `clear_history` is a NON-ASYNC `#[tauri::command]`, so it runs on the MAIN
/// thread; nothing in the app responds until it returns.
///
/// MEASURED on this machine, in an in-memory store (the friendly case):
///
/// | rows   | before      | after   |
/// |--------|-------------|---------|
/// | 250    | 20 ms       | 1.4 ms  |
/// | 2,000  | 1,073 ms    | 10.4 ms |
/// | 10,000 | **56,663 ms** | measured below |
///
/// 10,000 is the shipped default `HistorySettings::max_items`, and Clear
/// History is the product's panic button: the design doc's own scenario is a
/// user who pasted a password and wants it gone now. Replacing the correlated
/// subquery with a constant measured a ratio of 4.7x for 8x the rows, which is
/// what identified the cause.
#[test]
fn r9_clear_history_does_not_get_quadratically_slower() {
    let mut small = Store::open_in_memory().unwrap();
    small.set_device_id(A);
    seed(&small, A, 250, now_ms() - 10_000_000);
    let t = Instant::now();
    let n_small = small.clear(None).unwrap();
    let small_us = t.elapsed().as_micros();

    let mut bigger = Store::open_in_memory().unwrap();
    bigger.set_device_id(A);
    seed(&bigger, A, 2_000, now_ms() - 10_000_000);
    let t = Instant::now();
    let n_bigger = bigger.clear(None).unwrap();
    let bigger_us = t.elapsed().as_micros();

    // And the case the user actually meets: a full default history.
    let mut capped = Store::open_in_memory().unwrap();
    capped.set_device_id(A);
    seed(&capped, A, 10_000, now_ms() - 10_000_000);
    let t = Instant::now();
    let n_capped = capped.clear(None).unwrap();
    let capped_ms = t.elapsed().as_millis();

    // Guard that can find nothing: the clears must actually have cleared.
    assert_eq!(n_small, 250, "the small clear removed {n_small} rows, not 250");
    assert_eq!(n_bigger, 2_000, "the larger clear removed {n_bigger} rows, not 2000");
    assert_eq!(n_capped, 10_000, "the capped clear removed {n_capped} rows, not 10000");

    let ratio = bigger_us as f64 / small_us.max(1) as f64;
    eprintln!(
        "R9-1b Clear History: 250 rows = {small_us}us, 2000 rows = {bigger_us}us, \
         ratio {ratio:.1}x for 8x the rows (linear = 8x, quadratic = 64x); \
         10k rows (shipped default cap) = {capped_ms}ms"
    );
    assert!(
        ratio < 20.0,
        "R9-1b REGRESSION: 8x the rows cost {ratio:.1}x the time. Clear History is \
         quadratic again. It runs on the MAIN thread, under the store mutex, in one \
         unbounded transaction, and 10k rows is the shipped default cap."
    );
    assert!(
        capped_ms < 3_000,
        "R9-1b REGRESSION: clearing a full default history took {capped_ms}ms on the main \
         thread. The app is frozen for that long and the user is offered Force Quit, which \
         rolls the transaction back and leaves the history they asked to clear intact."
    );
}

/// R9-2. `items_from` now builds its SQL as a string and filters excluded apps
/// with two `NOT IN` lists over `LOWER(app_id)` and `LOWER(app_name)`, neither
/// of which any index covers. `serve` calls it once per page, under the store
/// mutex the history UI shares.
#[test]
fn r9_serving_a_page_with_an_exclusion_list_stays_cheap() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(A);
    seed(&s, A, 15_000, now_ms() - 60_000_000);

    let plain = {
        let t = Instant::now();
        let page = s.items_from(A, 0, "", 200).unwrap();
        assert_eq!(page.len(), 200, "the unfiltered page came back short");
        t.elapsed().as_micros()
    };

    let excluded: Vec<String> = (0..12).map(|i| format!("com.example.password{i}")).collect();
    s.set_excluded_apps(excluded);
    let filtered = {
        let t = Instant::now();
        let page = s.items_from(A, 0, "", 200).unwrap();
        assert_eq!(page.len(), 200, "the filtered page came back short");
        t.elapsed().as_micros()
    };

    eprintln!("R9-2 items_from page of 200: plain = {plain}us, 12 exclusions = {filtered}us");
    assert!(
        filtered < plain.max(200) * 25,
        "R9-2: the exclusion filter costs {filtered}us against {plain}us unfiltered, and \
         `serve` runs it under the mutex the history window shares"
    );
}

// ---------------------------------------------------------------------------
// 2. RECEIPTS ARE ATOMIC WITH ROWS. Attack: a path that applies something and
//    banks nothing (endless re-offer), or refuses reversibly and banks nothing.
// ---------------------------------------------------------------------------

/// R9-3. An accepted row must bank its receipt, and so must a row that reaches
/// the transaction and then loses. This is the invariant round 8 moved inside
/// `apply_remote_item`; delete the `mark_received_in(&tx, ...)` call there and
/// this test fails on its first assertion.
#[test]
fn r9_every_row_that_reaches_the_transaction_banks_its_receipt() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(B);
    let now = now_ms();

    let win = RemoteItem {
        source_machine: A.into(),
        origin_id: "won".into(),
        kind: "clipboard".into(),
        text: "hello".into(),
        created_at: now - 5_000,
        updated_at: now - 5_000,
        pinned: false,
    };
    s.apply_remote_item(A, &win).unwrap();
    assert_eq!(
        mark_for(&s, A, A),
        Some(now - 5_000),
        "an accepted row banked no receipt, so the peer re-offers it on every exchange"
    );

    // A row that loses to a tombstone still counts as seen.
    s.apply_remote_tombstone(
        A,
        &RemoteTombstone {
            source_machine: A.into(),
            origin_id: "buried".into(),
            deleted_at: now - 1_000,
        },
    )
    .unwrap();
    let lose = RemoteItem { origin_id: "buried".into(), updated_at: now - 4_000, ..win.clone() };
    assert_eq!(
        s.apply_remote_item(A, &lose).unwrap(),
        ApplyOutcome::Ignored,
        "the tombstone should have won"
    );
    assert!(
        mark_for(&s, A, A).unwrap() >= now - 1_000,
        "a row refused by a tombstone banked nothing, so it is re-offered every exchange"
    );
}

/// R9-4. THE HOLE THE ROUND-8 FIX OPENED. Found in this round, and fixed while
/// the round was running: `drain` now range-checks `created_at` itself and
/// calls `note_refused`, at the top of `replicate.rs`'s item arm.
///
/// The defect: round 8 removed `drain`'s standalone receipt, leaving
/// `apply_remote_item` as the only thing that banks. But `apply_remote_item`
/// refuses a row whose `created_at` is past the skew ceiling and returns BEFORE
/// opening its transaction, so nothing was banked at all and the row was
/// re-offered on every exchange until the wall clock caught up.
///
/// It needed no hostile peer. A machine whose clock ran fast at capture and was
/// corrected afterwards holds exactly this row: `created_at` in the future,
/// `updated_at` ordinary. Before round 8 `drain` banked it and the exchange went
/// quiet; after round 8 it never went quiet.
///
/// This test keeps the STORE-level property, because that is what makes the
/// compensating check in `drain` load-bearing: `apply_remote_item` still banks
/// nothing here, so if anyone deletes `drain`'s range check the loop comes back.
#[test]
fn r9_a_row_stamped_ahead_is_refused_and_banks_nothing() {
    let mut b = Store::open_in_memory().unwrap();
    b.set_device_id(B);
    let now = now_ms();

    let ahead = RemoteItem {
        source_machine: A.into(),
        origin_id: "fast-clock".into(),
        kind: "clipboard".into(),
        text: "written while the clock was fast".into(),
        // Past B's ceiling of now + MAX_CLOCK_SKEW_MS (two minutes)...
        created_at: now + 10 * 60 * 1000,
        // ...while the replication clock is perfectly ordinary.
        updated_at: now - 1_000,
        pinned: false,
    };
    assert_eq!(
        b.apply_remote_item(A, &ahead).unwrap(),
        ApplyOutcome::Ignored,
        "the ceiling check did not fire, so this test proves nothing"
    );
    assert_eq!(b.count().unwrap(), 0, "B stored nothing, as designed");
    assert_eq!(
        mark_for(&b, A, A),
        None,
        "`apply_remote_item` now banks a receipt for a row it refuses on the ceiling. \
         If that is deliberate, `drain`'s compensating range check is redundant; if it \
         is not, it parks the cursor at a clock we never accepted."
    );

    // And prove it is a MISSING CALL, not the range check, that would leave the
    // loop open: the very same clock is perfectly bankable.
    b.note_received_at(A, A, ahead.updated_at, &ahead.origin_id).unwrap();
    assert_eq!(
        mark_for(&b, A, A),
        Some(now - 1_000),
        "the clock is not bankable, so the re-offer loop has a different cause"
    );
}

/// R9-5. Both halves of the tombstone receipt rule, pinned from opposite sides
/// so neither extreme passes: an applied tombstone MUST bank, a tombstone past
/// the skew ceiling MUST NOT.
#[test]
fn r9_an_applied_tombstone_banks_and_a_future_one_does_not() {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(B);
    let now = now_ms();

    s.apply_remote_tombstone(
        A,
        &RemoteTombstone {
            source_machine: A.into(),
            origin_id: "gone".into(),
            deleted_at: now - 10,
        },
    )
    .unwrap();
    assert_eq!(
        mark_for(&s, A, A),
        Some(now - 10),
        "an applied tombstone banked nothing, so the delete is re-offered every exchange"
    );

    let before = mark_for(&s, A, A);
    s.apply_remote_tombstone(
        A,
        &RemoteTombstone {
            source_machine: A.into(),
            origin_id: "ahead".into(),
            deleted_at: now + 10 * 60 * 1000,
        },
    )
    .unwrap();
    assert_eq!(
        mark_for(&s, A, A),
        before,
        "a tombstone past the skew ceiling moved the cursor; everything below is now hidden"
    );
}

// ---------------------------------------------------------------------------
// 3. QUIT MID-EXCHANGE. `stop()` does not wait for an in-flight exchange and
//    the app exits with libc::_exit(0), so the store must never be left holding
//    a receipt for a row it does not have.
// ---------------------------------------------------------------------------

/// R9-6. Cut the exchange off partway through and check the invariant that has
/// to survive a quit: no receipt may stand at or above a row we never stored.
///
/// The abort closure is the one `run_session` passes, so this is the real
/// mid-exchange stop path rather than a model of it.
#[test]
fn r9_an_aborted_exchange_leaves_no_receipt_without_its_row() {
    let a = store_for(A);
    let b = store_for(B);
    let base = now_ms() - 5_000_000;
    seed(&a.lock(), A, 2_000, base);

    let (sock_x, sock_y) = socket_pair();
    let key = PairedKey::from_bytes([7u8; 32]);
    let k2 = key.clone();
    let b_store = b.clone();
    let a_store = a.clone();

    let (tx, rx) = mpsc::channel::<&'static str>();
    let tx2 = tx.clone();

    // B drains and then aborts, a few messages in.
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = calls.clone();
    let receiver = std::thread::spawn(move || {
        if let Ok(mut s) = Session::accept(sock_y, &k2) {
            let known = vec![A.to_string(), B.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            let _ = exchange(
                &mut s,
                &b_store,
                (B, "peer"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| c2.fetch_add(1, Ordering::SeqCst) > 3,
            );
        }
        let _ = tx2.send("receiver");
    });
    let sender = std::thread::spawn(move || {
        if let Ok(mut s) = Session::initiate(sock_x, &key) {
            let known = vec![A.to_string(), B.to_string()];
            let attr = Attribution { peer_id: B, local_id: A, known: &known };
            let _ = exchange(
                &mut s,
                &a_store,
                (A, "peer"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::First,
                false,
                0,
                &|| false,
            );
        }
        let _ = tx.send("sender");
    });

    let deadline = Instant::now() + BUDGET;
    let mut seen = 0;
    while seen < 2 {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "the aborted exchange stalled; {seen} of 2 sides returned");
        match rx.recv_timeout(left) {
            Ok(_) => seen += 1,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("the aborted exchange stalled; {seen} of 2 sides returned")
            }
            Err(e) => panic!("both sides died without reporting: {e}"),
        }
    }
    receiver.join().expect("receiver panicked");
    sender.join().expect("sender panicked");

    // Guard that can find nothing: the abort must have landed mid-stream, or
    // this asserts about an exchange that simply completed.
    let held = b.lock().count().unwrap();
    assert!(
        held > 0 && held < 2_000,
        "the abort did not land mid-stream: B holds {held} of 2000, so this proves nothing"
    );

    // The invariant. Every row at or below the banked cursor must be present,
    // because the receipt and the row commit together.
    let mark = mark_for(&b.lock(), A, A).expect("B banked something for A");
    let missing: Vec<String> = {
        let g = b.lock();
        (0..2_000usize)
            .map(|i| (format!("row-{i:07}"), base + i as i64))
            .filter(|(_, clock)| *clock <= mark)
            .filter(|(origin, _)| !g.holds_identity(A, origin).unwrap())
            .map(|(origin, _)| origin)
            .collect()
    };
    assert!(
        missing.is_empty(),
        "{} row(s) sit at or below B's cursor {mark} but were never stored, so nothing will \
         ever offer them again. First few: {:?}",
        missing.len(),
        missing.iter().take(3).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// 4. RE-ENTRANCY. `parking_lot::Mutex` is not reentrant, and this crate is
//    edition 2021, where an `if let` scrutinee temporary lives to the END of
//    the body.
// ---------------------------------------------------------------------------

/// R9-7. Pin the two language rules the whole re-entrancy audit rests on, using
/// the same mutex type the app uses, so an edition bump that changes them fails
/// a test rather than silently retiring the argument.
///
/// - an `if let` scrutinee guard is STILL ALIVE inside the body (edition 2021),
///   which is what made the round-8 `kind_of` deadlock real;
/// - a `while` CONDITION guard is NOT alive inside the body, which is the only
///   reason `stop()`'s `while self.inner.lock().starting { sleep(10ms) }` does
///   not hold `inner` for five seconds and freeze the UI behind `sync_status`.
#[test]
fn r9_the_temporary_scope_rules_this_audit_depends_on_still_hold() {
    let m: Mutex<Option<u32>> = Mutex::new(Some(1));
    let mut inside_if_let = None;
    if let Some(_v) = *m.lock() {
        inside_if_let = Some(m.try_lock().is_none());
    }
    assert_eq!(
        inside_if_let,
        Some(true),
        "an `if let` scrutinee guard is now released before the body: the edition changed, \
         and every 'bound to a local first' comment in the sync code can be revisited"
    );

    let n: Mutex<u32> = Mutex::new(3);
    let mut held_in_body = Vec::new();
    while *n.lock() > 0 {
        held_in_body.push(n.try_lock().is_none());
        *n.lock() -= 1;
    }
    assert_eq!(
        held_in_body,
        vec![false, false, false],
        "a `while` condition guard survives into the body: stop()'s wait would then hold \
         `inner` across a five-second sleep and freeze the UI behind sync_status"
    );
}

// ---------------------------------------------------------------------------
// 5. THE DIAL SLOT. `DialGuard`, `ReleasesDial`, `MAX_DIALS`, `admit_inbound`,
//    `make_room_for_peer`, `note_peer_record` and `stop_claim` are all PRIVATE
//    to `manager`, so a sibling module cannot reach them. What IS reachable is
//    the property the round-8 fix now depends on: the claim is still atomic
//    even though the action is not.
// ---------------------------------------------------------------------------

/// R9-8. SHAPE ONLY, and labelled as such. The decision (`dialing.insert`) and
/// the action (`Builder::spawn`) are no longer one step. The claim itself is
/// still a single `HashSet::insert` under the lock, so racing threads must
/// still produce exactly one dial per peer.
///
/// The real thing cannot be driven from here: see the report for what needs
/// `pub(crate)`.
#[test]
fn r9_a_claim_under_the_lock_cannot_be_double_spent() {
    let dialing: Arc<Mutex<std::collections::HashSet<String>>> =
        Arc::new(Mutex::new(std::collections::HashSet::new()));
    let won = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(std::sync::Barrier::new(8));

    let mut hs = Vec::new();
    for _ in 0..8 {
        let (d, w, b) = (dialing.clone(), won.clone(), start.clone());
        hs.push(std::thread::spawn(move || {
            b.wait();
            if d.lock().insert("peer".to_string()) {
                w.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }
    for h in hs {
        h.join().expect("claimant panicked");
    }
    assert_eq!(
        won.load(Ordering::SeqCst),
        1,
        "eight threads claimed one peer more than once; that device would be dialled twice"
    );
    assert_eq!(dialing.lock().len(), 1, "the set under-counted against MAX_DIALS");
}
