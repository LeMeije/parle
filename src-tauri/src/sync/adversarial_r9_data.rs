//! ADVERSARIAL REVIEW, ROUND 9. Data integrity and replication correctness,
//! exchange side.
//!
//! Round 8's fixes are the target: the pair cursor, the one-clock rule, and
//! receipts made atomic with the rows they describe.
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

fn both() -> Kinds {
    Kinds { dictations: true, clipboard: true }
}

/// One exchange, `x` dialling, under a wall-clock budget.
fn sync_bounded(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
) -> (RoundStats, RoundStats) {
    let (sock_x, sock_y) = socket_pair();
    let key = PairedKey::from_bytes([7u8; 32]);
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

/// Exchange until neither side sends anything, or fail. Returns the number of
/// exchanges it took, and how much crossed on the LAST one.
fn sync_until_quiet(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
    max: usize,
) -> usize {
    for round in 1..=max {
        let (d, a) = sync_bounded(x, y);
        let moved = d.sent_items + d.sent_tombstones + a.sent_items + a.sent_tombstones;
        if moved == 0 {
            return round;
        }
    }
    panic!("the pair did not go quiet in {max} exchanges");
}

/// A row with independently chosen clocks, which an honest author cannot make.
fn crafted_row(source: &str, origin: &str, created_at: i64, updated_at: i64) -> RemoteItem {
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

fn texts(store: &Arc<Mutex<Store>>) -> Vec<String> {
    let g = store.lock();
    let mut stmt = g
        .conn_for_test()
        .prepare("SELECT text FROM items ORDER BY text")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

// ---------------------------------------------------------------------------
// R9-E1. A DELETE RELAYED BY A PEER WHOSE CLOCK IS FAST, BUT LEGAL, SILENCES
//        THIS MACHINE FOR THE LENGTH OF THE SKEW.
//
// In ordinary terms:
//   The Mac (A) records a dictation. It syncs to the laptop (B). The laptop's
//   clock is 90 seconds fast, which is inside the two minutes the design
//   accepts and needs no misconfiguration beyond ordinary NTP drift on a
//   machine that has just woken. On the laptop the user deletes that dictation.
//   The delete travels back to the Mac and is applied, correctly.
//   From that moment, for the next 90 seconds, everything the user dictates on
//   the Mac is invisible to the laptop, and stays invisible for ever.
//
// In code terms: `apply_remote_tombstone` stores a clock for source A that is
// `now + 90_000` on A. `next_clock_in` is bounded at `now + MAX_CLOCK_SKEW_MS/2`
// (`now + 60_000`), so past that bound it stops climbing and returns the plain
// wall clock — which is BELOW the tombstone. The laptop's cursor for
// (peer A, source A) has meanwhile risen to that tombstone's pair, because A
// serves the tombstone straight back. Nothing lowers a cursor.
// ---------------------------------------------------------------------------
#[test]
fn r9_a_legal_but_fast_peer_delete_makes_every_later_row_on_this_machine_unservable() {
    let a = store_for(A);
    let b = store_for(B);

    // A records a dictation and it reaches B. This is also the vacuity guard:
    // if this row does not arrive, the harness proves nothing.
    let id = a.lock().insert_clipboard("first dictation", None, None).unwrap();
    let (origin, _) = a.lock().origin_and_text_for_test(id).unwrap();
    sync_until_quiet((&a, A), (&b, B), 6);
    assert_eq!(
        texts(&b),
        vec!["first dictation".to_string()],
        "premise failed: the first row never reached B"
    );

    // On B, whose clock reads 90 s ahead, the user deletes it. That is exactly
    // what `delete_item_local` writes on a machine 90 s fast.
    let ahead = now_ms() + 90_000;
    {
        let g = b.lock();
        // Written as literal SQL because this crate does not depend on
        // rusqlite; every value here is test-controlled.
        g.conn_for_test()
            .execute_batch(&format!(
                "INSERT INTO tombstones (source_machine, origin_id, deleted_at, local)
                 VALUES ('{A}', '{origin}', {ahead}, 1);
                 DELETE FROM items WHERE source_machine='{A}' AND origin_id='{origin}';"
            ))
            .unwrap();
    }

    // The delete reaches A, and A hands it back, so B's cursor for A rises to
    // it. Both are ordinary, correct behaviour.
    sync_until_quiet((&a, A), (&b, B), 6);
    assert!(texts(&a).is_empty(), "the delete did not reach A");

    // The user carries on dictating on the Mac.
    a.lock().insert_clipboard("dictated after the delete", None, None).unwrap();
    a.lock().insert_clipboard("and again", None, None).unwrap();
    sync_until_quiet((&a, A), (&b, B), 8);

    let on_b = texts(&b);
    let on_a = texts(&a);
    assert_eq!(
        on_b, on_a,
        "A holds {} rows and B holds {}: rows written after a legal relayed \
         delete are stamped below B's cursor and can never be served",
        on_a.len(),
        on_b.len()
    );
}

// ---------------------------------------------------------------------------
// R9-E2. A ROW REFUSED BY `apply_remote_item` FOR ITS `created_at` BANKS NO
//        RECEIPT AND IS RE-OFFERED ON EVERY EXCHANGE, FOR EVER.
//
// In ordinary terms:
//   The Mac's clock was ten minutes fast when a clipboard entry was captured,
//   then NTP corrected it, and the user later edited that entry. The entry now
//   carries a sane `updated_at` and a `created_at` ten minutes in the future.
//   Every single sync from then on carries that one row across the wire and the
//   laptop throws it away. The pair never goes quiet, for the life of the
//   pairing, with nothing able to say stop.
//
// In code terms: round 8 removed drain's standalone receipt on the ground that
// "a row that is APPLIED takes its receipt inside `apply_remote_item`'s own
// transaction". That is true of every row `apply_remote_item` reaches. It is
// not true of the rows it refuses BEFORE opening that transaction: an
// out-of-range `created_at` returns `Ignored` with no receipt at all, and no
// branch in `drain` banks one. `updated_at` is in range, so the receipt the old
// code took was legal and did close the loop.
// ---------------------------------------------------------------------------
#[test]
fn r9_a_row_refused_for_its_created_at_is_re_offered_on_every_exchange() {
    // REVERSED BY ROUND 10, and the disagreement is recorded rather than
    // quietly resolved, because two rounds reached opposite conclusions about
    // the same line and the reasoning matters more than the verdict.
    //
    // Round 9 (this test, as written): a row refused for an out-of-range
    // `created_at` banks nothing, so it is re-offered on every exchange for
    // ever. Criterion C. Fixed by banking a receipt.
    //
    // Round 10: that receipt is banked for a refusal that is TEMPORARY. A
    // `created_at` two minutes ahead becomes acceptable two minutes from now,
    // and banking excluded that exact row from every future page permanently.
    // Criterion A, which is the more serious of the two.
    //
    // Round 10 wins on two counts. Losing a row for good is worse than
    // re-offering it. And the loop is bounded and self-inflicted: the same one
    // `apply_remote_item` already accepts for an out-of-range `updated_at`, one
    // re-offer per exchange from a machine whose clock is wrong, ending the
    // moment it is fixed. An honest author cannot even produce the pair,
    // because every local write sets `created_at = now` and
    // `updated_at >= created_at`, so it takes a peer crafting it deliberately.
    //
    // What this test pins now is that the waste stays BOUNDED: one re-offer per
    // exchange, not a growing one, and confined to the crafted row.
    let a = store_for(A);
    let b = store_for(B);
    let ahead = now_ms() + 2 * MAX_CLOCK_SKEW_MS;

    a.lock().apply_remote_item(A, &crafted_row(A, "ordinary", now_ms() - 5_000, now_ms() - 5_000)).unwrap();
    a.lock().apply_remote_item(A, &crafted_row(A, "future-created", ahead, now_ms())).unwrap();

    let mut carried = Vec::new();
    for _ in 0..4 {
        carried.push(sync_bounded((&a, A), (&b, B)).0.sent_items);
    }

    // The ordinary row lands and stops being sent; the crafted one is re-offered
    // at a steady one per exchange rather than accumulating.
    assert!(
        texts(&b).iter().any(|t| t.contains("ordinary")),
        "the ordinary row must still arrive alongside the crafted one"
    );
    let tail = &carried[1..];
    assert!(
        tail.iter().all(|&n| n <= 1),
        "the waste is supposed to be one re-offer per exchange from a broken peer, not a \
         growing one: sent_items per round was {carried:?}"
    );
}

// ---------------------------------------------------------------------------
// R9-E3. THE ORDINARY PATHS STILL GO QUIET, AND CONVERGE.
//
// The counterweight to E2: this must keep passing whatever is done about it.
// ---------------------------------------------------------------------------
#[test]
fn r9_an_ordinary_two_device_history_converges_and_goes_quiet() {
    let a = store_for(A);
    let b = store_for(B);
    // Deliberately more than one wire page each (PAGE == MAX_BATCH_LEN == 256),
    // so the Clear below spans several pages of tombstones that all share one
    // clock. Under a clock-only cursor that is exactly the case that strands
    // the rest of a millisecond for ever.
    for i in 0..300 {
        a.lock().insert_clipboard(&format!("a{i:04}"), None, None).unwrap();
        b.lock().insert_clipboard(&format!("b{i:04}"), None, None).unwrap();
    }
    let rounds = sync_until_quiet((&a, A), (&b, B), 10);
    assert!(rounds <= 5, "took {rounds} exchanges to go quiet");
    assert_eq!(texts(&a), texts(&b), "the two devices diverged");
    assert_eq!(texts(&a).len(), 600);

    // A clears. A Clear stamps one clock per source across every tombstone it
    // writes, which is the premise this test needs; assert it rather than
    // assume it.
    a.lock().clear(None).unwrap();
    let distinct: i64 = {
        let g = a.lock();
        g.conn_for_test()
            .query_row(
                "SELECT COUNT(*) FROM (SELECT DISTINCT source_machine, deleted_at FROM tombstones)",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(
        distinct <= 2,
        "premise: a Clear should stamp one clock per source, got {distinct} distinct pairs"
    );

    let rounds = sync_until_quiet((&a, A), (&b, B), 10);
    assert!(rounds <= 5, "the clear took {rounds} exchanges to go quiet");
    let left = texts(&b);
    assert!(
        left.is_empty(),
        "{} of 600 cleared rows survived on B, first: {:?}",
        left.len(),
        &left[..left.len().min(3)]
    );
}
