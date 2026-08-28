//! ADVERSARIAL REVIEW, ROUND 10. Data integrity, clocks and replication
//! correctness, exchange side.
//!
//! Round 9's fixes are the target: `next_clock_in` losing its ceiling
//! fallback, the `clear` CTE, and `drain`'s new `created_at` range check.
//!
//! Every exchange here runs on its own threads under a wall-clock budget, both
//! sockets carry read AND write timeouts, and every loop has a hard bound, so a
//! stall fails with a message naming the side that never returned.

#![cfg(test)]

use parle_core::history::{RemoteItem, Store, MAX_CLOCK_SKEW_MS};
use parle_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";

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

/// A row with independently chosen clocks, which an honest author cannot
/// produce: every local write sets `created_at = now` and `updated_at >= created_at`.
fn crafted(source: &str, origin: &str, created_at: i64, updated_at: i64) -> RemoteItem {
    RemoteItem {
        source_machine: source.into(),
        origin_id: origin.into(),
        kind: "clipboard".into(),
        text: format!("text for {origin}"),
        created_at,
        updated_at,
        pinned: false,
    }
}

fn both() -> Kinds {
    Kinds { dictations: true, clipboard: true }
}

/// One exchange, `x` dialling, under a wall-clock budget. Copied from
/// `adversarial_r7_scale::sync_bounded` for the reason section 4.4 of the
/// handover gives: a stall must name the side that never returned rather than
/// parking the suite.
fn sync_bounded(
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

fn texts(store: &Arc<Mutex<Store>>) -> Vec<String> {
    let g = store.lock();
    let mut stmt = g.conn_for_test().prepare("SELECT text FROM items ORDER BY text").unwrap();
    let v: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    v
}

/// The state a backwards system-clock correction leaves behind: one row of our
/// OWN source carrying a clock `drift_ms` in the future, exactly as
/// `insert_clipboard` wrote it while the clock was wrong.
fn poison_own_clock(store: &Arc<Mutex<Store>>, me: &str, drift_ms: i64) {
    let g = store.lock();
    let id = g.insert_clipboard("written while the clock was wrong", None, None).unwrap();
    let ahead = now_ms() + drift_ms;
    // Literal SQL: this crate does not depend on rusqlite. Every value here is
    // test-controlled.
    g.conn_for_test()
        .execute_batch(&format!(
            "UPDATE items SET created_at={ahead}, updated_at={ahead}
              WHERE id={id} AND source_machine='{me}';"
        ))
        .unwrap();
}

// ===========================================================================
// R10-S1. THE POISONED CLOCK, END TO END.
//
// A's clock ran three days fast for one afternoon and was then corrected.
// `next_clock_in` has no ceiling any more, so every row A writes from now on
// is stamped `newest + 1`, three days ahead, and B refuses all of them.
//
// The exchange succeeds. Both sides report a clean round. Nothing arrives, and
// nothing ever will until the wall clock catches up with the old drift.
// ===========================================================================

#[test]
fn r10_a_device_whose_clock_was_once_wrong_syncs_nothing_and_never_goes_quiet() {
    // INVERTED: the machine recovers as soon as its clock is corrected.
    //
    // The finding was a round-9 regression. Round 9 removed the ceiling so a
    // device whose clock had once been days fast kept its own `newest` up
    // there for ever, every row it wrote was stamped days ahead, a correct peer
    // refused all of them, and fixing the clock changed nothing.
    //
    // The clamp is on the ceiling now: a `newest` above `now + skew` cannot be
    // protecting any correctly-clocked peer's cursor, because that peer refused
    // the rows that put it there.
    let a = store_for(A);
    let b = store_for(B);

    // A wrote rows while its clock was three days fast.
    let poisoned = now_ms() + 3 * 86_400_000;
    a.lock()
        .conn_for_test()
        .execute(
            "INSERT INTO items (kind, text, created_at, updated_at, source_machine, origin_id)
             VALUES ('clipboard','from the bad old days',?1,?1,?2,'poison')",
            (poisoned, A),
        )
        .unwrap();

    // The clock is fixed. Everything written from here must reach B.
    for i in 0..3 {
        a.lock().insert_clipboard(&format!("after the fix {i}"), None, None).unwrap();
    }
    sync_bounded((&a, A), (&b, B));

    let landed = texts(&b);
    let arrived = landed.iter().filter(|t| t.starts_with("after the fix")).count();
    assert_eq!(
        arrived, 3,
        "only {arrived} of 3 rows written after the clock was corrected reached the peer. The \
         device syncs nothing for as long as the original drift lasted and fixing the clock \
         does not help. B holds {landed:?}"
    );

    // And it goes quiet: the poisoned row is refused, banks nothing, and is
    // re-offered at most once per exchange rather than growing.
    let second = sync_bounded((&a, A), (&b, B)).0;
    assert_eq!(
        second.applied_items, 0,
        "the pair did not go quiet after everything had arrived"
    );
}

#[test]
fn r10_the_same_device_synced_perfectly_before_its_clock_was_ever_wrong() {
    // The control for the test above. Without it the first test proves nothing
    // about the clock: it could be any reason B holds nothing.
    let a = store_for(A);
    let b = store_for(B);
    for i in 0..3 {
        a.lock().insert_clipboard(&format!("dictated after the fix {i}"), None, None).unwrap();
    }
    sync_bounded((&a, A), (&b, B));
    assert_eq!(texts(&b).len(), 3, "an unpoisoned device must deliver its rows");
    // And the pair goes quiet.
    let (d, acc) = sync_bounded((&a, A), (&b, B));
    assert_eq!(
        d.sent_items + acc.sent_items,
        0,
        "an ordinary pair must go quiet on the second exchange"
    );
}

// ===========================================================================
// R10-S2. `drain`'s NEW `created_at` RECEIPT.
//
// Round 9 made an out-of-range `created_at` bank a receipt so the row stops
// being re-offered. But that refusal is TEMPORARY: the wall clock moves, and a
// `created_at` two minutes ahead is acceptable two minutes from now. The
// receipt makes the row unreachable for ever, which is the exact rule the
// surrounding comment says receipts must obey ("never banked for a refusal we
// might reverse").
//
// Reachability, stated plainly: no honest author can produce it, because every
// local write path sets `created_at = now_ms()` and `updated_at >= created_at`,
// so `created_at` out of range implies `updated_at` out of range too. It takes
// a peer that crafts the pair, and the loss then falls on that peer's own rows.
// So this is a correctness wart with a hostile-only, self-inflicted trigger,
// not a live data-loss path.
// ===========================================================================

#[test]
fn r10_a_row_refused_for_a_future_created_at_is_banked_and_so_lost_for_ever() {
    // INVERTED: the refusal banks nothing, so the row is not stranded.
    //
    // Round 9 banked a receipt here, because `apply_remote_item` returns before
    // opening its transaction on that path and so nothing else would. Round 10
    // showed that is the wrong call: a `created_at` two minutes ahead becomes
    // acceptable two minutes from now, so the refusal is TEMPORARY, and this
    // file states in three other places that a receipt is never banked for a
    // refusal we might reverse. Banking it excluded that exact row from every
    // future page, permanently.
    //
    // The loop that returns is the one `apply_remote_item` already accepts for
    // an out-of-range `updated_at`: bounded, self-inflicted waste from a
    // machine whose clock is wrong, one re-offer per exchange, ending when it
    // is fixed. An honest author cannot even produce the pair, because every
    // local write sets `created_at = now` and `updated_at >= created_at`.
    let a = store_for(A);
    let b = store_for(B);
    let ahead = now_ms() + 2 * MAX_CLOCK_SKEW_MS;

    // An ordinary row EARLIER than the crafted one, so the crafted row is not
    // stranded on its own and the cursor question is real.
    a.lock()
        .apply_remote_item(A, &crafted(A, "row-ok", now_ms() - 10_000, now_ms() - 10_000))
        .unwrap();
    a.lock().apply_remote_item(A, &crafted(A, "row-0", ahead, now_ms())).unwrap();

    sync_bounded((&a, A), (&b, B));

    let cursor = b.lock().watermarks_paired(A).unwrap();
    let named = cursor.iter().any(|(_, _, o)| o == "row-0");
    assert!(
        !named,
        "the refusal banked a receipt naming the refused row: that refusal is temporary, so the \
         row is now excluded from every future page permanently. cursor {cursor:?}"
    );
    // The ordinary row still arrives, so the refusal is confined to its own row.
    assert!(texts(&b).iter().any(|t| t.contains("row-ok")), "the ordinary row must still land");
}

// ===========================================================================
// R10-S3. THE BOUNDED RATCHET STILL GOES QUIET.
//
// A paired peer relaying deletes at our ceiling pushes our own source's clock
// up to (but not past) the skew window. Criterion C says every exchange must
// still go quiet. This is the positive control for R10-2 in the core file.
// ===========================================================================

#[test]
fn r10_a_ratcheted_clock_inside_the_window_still_reaches_quiet() {
    let a = store_for(A);
    let b = store_for(B);

    // Ordinary traffic first.
    for i in 0..5 {
        a.lock().insert_clipboard(&format!("mac row {i}"), None, None).unwrap();
    }
    sync_bounded((&a, A), (&b, B));
    assert_eq!(texts(&b).len(), 5, "premise: the ordinary rows must have landed");

    // B deletes one of A's rows while B's clock runs 110 s fast: inside the
    // window, so A accepts it, and A's own source clock goes with it.
    let ids: Vec<i64> = {
        let g = b.lock();
        let mut st = g.conn_for_test().prepare("SELECT id FROM items LIMIT 1").unwrap();
        let v = st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        v
    };
    {
        let g = b.lock();
        g.delete(ids[0]).unwrap();
        let fast = now_ms() + 110_000;
        g.conn_for_test()
            .execute_batch(&format!(
                "UPDATE tombstones SET deleted_at={fast} WHERE source_machine='{A}';"
            ))
            .unwrap();
    }
    sync_bounded((&a, A), (&b, B));

    let ahead: i64 = {
        let g = a.lock();
        g.conn_for_test()
            .query_row(
                &format!(
                    "SELECT COALESCE(MAX(deleted_at),0) FROM tombstones WHERE source_machine='{A}'"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(
        ahead > now_ms(),
        "premise: A's own source clock must actually have been ratcheted forward ({} ms)",
        ahead - now_ms()
    );

    // More ordinary traffic, then the pair must go quiet.
    for i in 0..3 {
        a.lock().insert_clipboard(&format!("mac row after {i}"), None, None).unwrap();
    }
    let mut rounds = 0;
    for r in 1..=6 {
        let (d, acc) = sync_bounded((&a, A), (&b, B));
        if d.sent_items + d.sent_tombstones + acc.sent_items + acc.sent_tombstones == 0 {
            rounds = r;
            break;
        }
    }
    assert!(rounds > 0, "a ratcheted-but-in-window pair never went quiet in 6 exchanges");
    // And nothing was lost: the four surviving originals plus three new ones.
    assert_eq!(texts(&a).len(), texts(&b).len(), "the two devices diverged");
    assert_eq!(texts(&a), texts(&b), "the two devices hold different rows");
}

// ===========================================================================
// R10-S4. THE NEW `clear` CTE, END TO END.
//
// A Clear must stamp one clock per source, above everything the peer's cursor
// already holds, so every delete travels and the exchange goes quiet.
// ===========================================================================

#[test]
fn r10_clear_history_propagates_completely_and_the_pair_goes_quiet() {
    let a = store_for(A);
    let b = store_for(B);
    for i in 0..40 {
        a.lock().insert_clipboard(&format!("secret {i:03}"), None, None).unwrap();
    }
    for i in 0..10 {
        b.lock().insert_clipboard(&format!("laptop {i:03}"), None, None).unwrap();
    }
    // Converge.
    for _ in 0..4 {
        sync_bounded((&a, A), (&b, B));
    }
    assert_eq!(texts(&a).len(), 50, "premise: A must hold both histories");
    assert_eq!(texts(&b).len(), 50, "premise: B must hold both histories");

    // Panic button on A.
    a.lock().clear(None).unwrap();
    assert!(texts(&a).is_empty(), "premise: the Clear must empty A");

    for _ in 0..4 {
        sync_bounded((&a, A), (&b, B));
    }
    let left = texts(&b);
    assert!(
        left.is_empty(),
        "{} rows survived Clear History on the other machine; first few {:?}",
        left.len(),
        left.iter().take(3).collect::<Vec<_>>()
    );

    // Quiet, and it stays cleared.
    let (d, acc) = sync_bounded((&a, A), (&b, B));
    assert_eq!(
        d.sent_items + acc.sent_items,
        0,
        "rows were still crossing after the Clear had settled"
    );
    assert!(texts(&a).is_empty() && texts(&b).is_empty(), "a cleared row came back");
}

#[test]
fn r10_a_clear_of_a_history_larger_than_one_page_still_lands_every_delete() {
    // A Clear stamps ONE clock per source, so every tombstone shares a
    // millisecond. That is the case the pair cursor exists for: if it regressed
    // to a bare clock, a Clear larger than a page would strand the rest.
    let a = store_for(A);
    let b = store_for(B);
    for i in 0..600 {
        a.lock().insert_clipboard(&format!("row {i:04}"), None, None).unwrap();
    }
    for _ in 0..8 {
        sync_bounded((&a, A), (&b, B));
    }
    assert_eq!(texts(&b).len(), 600, "premise: the whole history must have crossed");

    a.lock().clear(None).unwrap();
    let stamps: usize = {
        let g = a.lock();
        g.conn_for_test()
            .query_row(
                &format!(
                    "SELECT COUNT(DISTINCT deleted_at) FROM tombstones WHERE source_machine='{A}'"
                ),
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap() as usize
    };
    assert_eq!(stamps, 1, "premise: a Clear must stamp exactly one clock, got {stamps}");

    for _ in 0..10 {
        sync_bounded((&a, A), (&b, B));
    }
    let left = texts(&b);
    assert!(
        left.is_empty(),
        "{} of 600 deletes never arrived; first few {:?}",
        left.len(),
        left.iter().take(3).collect::<Vec<_>>()
    );
}
