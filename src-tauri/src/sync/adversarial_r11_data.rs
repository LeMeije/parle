//! ADVERSARIAL REVIEW, ROUND 11. Data integrity, clocks and replication
//! correctness, exchange side.
//!
//! Round 10's fixes are the target, in particular the CEILING clamp in
//! `Store::next_clock_impl` and the matching clamp in `Store::clear`, plus the
//! `created_at` receipt that round 9 added and round 10 removed.
//!
//! Every exchange runs on its own threads under a wall-clock budget, both
//! sockets carry read AND write timeouts, and every loop has a hard bound, so a
//! stall fails with a message naming the side that never returned rather than
//! parking the suite.

#![cfg(test)]

use echokey_core::history::{Store, MAX_CLOCK_SKEW_MS};
use echokey_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";
const C: &str = "33333333-3333-4333-8333-333333333333";

const BUDGET: Duration = Duration::from_secs(60);
/// Hard bound on every convergence loop in this file.
const MAX_ROUNDS: usize = 12;

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

/// One exchange, `x` dialling, under a wall-clock budget. The structure is
/// `adversarial_r7_scale::sync_bounded`'s, for the reason section 4.4 of the
/// handover gives; the only change is that the paired roster is the whole
/// three-device mesh, because these tests need one.
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
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
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
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
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

/// Symmetric difference, capped, so a failure stays readable.
fn diff(a: &[String], b: &[String]) -> Vec<String> {
    let sa: std::collections::BTreeSet<&String> = a.iter().collect();
    let sb: std::collections::BTreeSet<&String> = b.iter().collect();
    sa.symmetric_difference(&sb).take(5).map(|s| (*s).clone()).collect()
}

fn traffic(s: &RoundStats) -> usize {
    s.sent_items + s.sent_tombstones
}

// ===========================================================================
// R11-S1. THE BACKWARDS CLOCK STEP, END TO END.
//
// In ordinary terms. The Mac and the PC agree about the time. You dictate a
// password on the Mac and it syncs to the PC. The Mac's clock then steps back
// an hour — a dual-booted machine disagreeing about whether the RTC is local
// time or UTC, a VM resumed from a snapshot, an NTP correction after the clock
// ran fast. You delete the password on the Mac.
//
// The PC keeps it. For ever. The exchange succeeds and reports a clean round.
//
// Why. `next_clock_impl` stamps `now.max(min(newest + 1, now + skew))`. After
// the step, `newest` (the clock on the row the PC already has) is an hour above
// `now`, so the delete is stamped at the ceiling, an hour BELOW the cursor the
// PC banked when it received the row. `serve` offers strictly above the cursor.
// Nothing lowers a cursor, and correcting the clock does not move the tombstone.
//
// Round 10's argument for the clamp is that "a peer only ever banks a cursor at
// or below its OWN now + skew, so a newest above our ceiling cannot be
// protecting a real cursor". True of a clock that drifted FORWARD, where the
// peer refused the rows. False of one that stepped BACKWARDS, where the peer
// accepted them while the clocks still agreed.
// ===========================================================================

/// The on-disk state one backwards clock step leaves behind, on both machines.
///
/// `now_ms()` cannot be moved from inside the process and both stores share one
/// wall clock, so the step is applied to the two durable records it changes,
/// and to nothing else:
///
///   * on the author, the row's own clock, which was the wall clock when it was
///     written — this is the same modelling `adversarial_r10_data::poison_own_clock`
///     uses;
///   * on the peer, the receipt for that row, which `mark_received_in` sets to
///     the row's `(updated_at, origin_id)` the moment it is applied.
///
/// Both values are read back off the row rather than invented.
fn step_the_authors_clock_back(
    author: &Arc<Mutex<Store>>,
    author_id: &str,
    peer: &Arc<Mutex<Store>>,
    origin: &str,
    by_ms: i64,
) -> i64 {
    let t_high = now_ms() + by_ms;
    for (store, who) in [(author, author_id), (peer, author_id)] {
        let g = store.lock();
        g.conn_for_test()
            .execute_batch(&format!(
                "UPDATE items SET created_at={t_high}, updated_at={t_high}
                  WHERE source_machine='{who}' AND origin_id='{origin}';
                 UPDATE source_marks SET received_clock={t_high}, received_origin='{origin}'
                  WHERE source_machine='{who}';"
            ))
            .unwrap();
    }
    t_high
}

#[test]
fn r11_a_delete_after_a_backwards_clock_step_never_reaches_the_other_machine() {
    let a = store_for(A);
    let b = store_for(B);

    // 1. Clocks agree. A dictates, and it syncs.
    let id = a.lock().insert_clipboard("the password", None, None).unwrap();
    let (origin, _) = a.lock().origin_and_text_for_test(id).unwrap();
    sync_bounded((&a, A), (&b, B));
    assert!(texts(&b).iter().any(|t| t == "the password"), "premise: the row must sync first");
    let banked = b.lock().watermarks_paired(A).unwrap();
    assert!(
        banked.iter().any(|(src, _, o)| src == A && o == &origin),
        "premise: B must have banked a real receipt naming that row: {banked:?}"
    );

    // 2. A's clock steps back an hour.
    let t_high = step_the_authors_clock_back(&a, A, &b, &origin, 60 * 60 * 1000);
    assert!(t_high > now_ms() + MAX_CLOCK_SKEW_MS, "premise: the step exceeds the skew window");

    // 3. The user deletes the password on A.
    a.lock().delete(id).unwrap();
    assert_eq!(a.lock().tombstone_count(A).unwrap(), 1, "premise: a tombstone must exist");

    // 4. Sync, more than once, to be sure it is not merely slow.
    //
    // What is asserted is that the delete is OFFERED, not that it lands. That
    // distinction is the whole finding, and it is what separates the two clock
    // rules — both of which leave B still holding the password at this instant:
    //
    //   * Round 9 stamped `newest + 1`, an hour ahead. B is offered it and
    //     refuses it for being in the future, banking no receipt, so it is
    //     re-offered every exchange and lands the moment B's own clock passes
    //     that stamp. Noisy, self-announcing, and it repairs itself.
    //   * Round 10 stamps the ceiling, an hour BELOW the cursor B banked when
    //     it received the row. It is never offered, by anyone, ever. No amount
    //     of waiting and no clock correction reaches it, because nothing lowers
    //     a cursor and nothing rewrites the tombstone.
    //
    // A delete that is never offered cannot arrive. That is the failure.
    let mut offered = 0usize;
    for _ in 0..3 {
        let (d, _) = sync_bounded((&a, A), (&b, B));
        offered += d.sent_tombstones;
    }
    assert!(
        offered > 0,
        "the tombstone for a password the peer holds was never offered to it at all across \
         three exchanges: it is stamped below the cursor the peer banked before the clock \
         stepped back, so no later exchange and no clock correction can ever deliver it"
    );
}

#[test]
fn r11_a_row_written_after_a_backwards_clock_step_never_reaches_the_other_machine() {
    let a = store_for(A);
    let b = store_for(B);

    let id = a.lock().insert_clipboard("before the step", None, None).unwrap();
    let (origin, _) = a.lock().origin_and_text_for_test(id).unwrap();
    sync_bounded((&a, A), (&b, B));
    assert!(texts(&b).iter().any(|t| t == "before the step"), "premise: the first row must sync");

    step_the_authors_clock_back(&a, A, &b, &origin, 60 * 60 * 1000);

    a.lock().insert_clipboard("after the step", None, None).unwrap();
    // Same distinction as the delete above: offered-and-refused is recoverable,
    // never-offered is not.
    let mut offered = 0usize;
    for _ in 0..3 {
        let (d, _) = sync_bounded((&a, A), (&b, B));
        offered += d.sent_items;
    }
    assert!(
        offered > 0,
        "a row written after the clock stepped back was never offered to the other machine \
         at all across three exchanges: it is stamped below the cursor the peer already \
         holds, so nothing will ever offer it again"
    );
}

// ===========================================================================
// R11-S2. THE THREE-DEVICE MESH. Criterion A end to end, including the relayed
// delete the brief asks about: C deletes a row A wrote, and the news has to
// reach A through B as well as directly.
// ===========================================================================

/// Every pair, once, in a fixed order. One "round" of a full mesh.
fn mesh_round(
    a: &Arc<Mutex<Store>>,
    b: &Arc<Mutex<Store>>,
    c: &Arc<Mutex<Store>>,
) -> usize {
    let mut moved = 0;
    for (x, xi, y, yi) in [(a, A, b, B), (b, B, c, C), (a, A, c, C)] {
        let (d, s) = sync_bounded((x, xi), (y, yi));
        moved += traffic(&d) + traffic(&s);
    }
    moved
}

/// Run the mesh until nothing moves, bounded. Returns the number of rounds it
/// took; panics rather than looping if it never goes quiet.
fn mesh_until_quiet(
    a: &Arc<Mutex<Store>>,
    b: &Arc<Mutex<Store>>,
    c: &Arc<Mutex<Store>>,
    what: &str,
) -> usize {
    for round in 1..=MAX_ROUNDS {
        if mesh_round(a, b, c) == 0 {
            return round;
        }
    }
    panic!("the mesh never went quiet after {what}: still moving rows after {MAX_ROUNDS} rounds");
}

#[test]
fn r11_a_three_device_mesh_converges_and_goes_quiet() {
    let (a, b, c) = (store_for(A), store_for(B), store_for(C));
    for i in 0..4 {
        a.lock().insert_clipboard(&format!("a{i}"), None, None).unwrap();
        b.lock().insert_clipboard(&format!("b{i}"), None, None).unwrap();
        c.lock().insert_clipboard(&format!("c{i}"), None, None).unwrap();
    }

    let rounds = mesh_until_quiet(&a, &b, &c, "twelve rows across three devices");
    assert!(rounds >= 2, "premise: quiet on the first round means nothing was ever exchanged");

    let (ta, tb, tc) = (texts(&a), texts(&b), texts(&c));
    assert_eq!(ta.len(), 12, "A holds {} of 12 rows", ta.len());
    assert!(ta == tb, "A and B diverged: {:?}", diff(&ta, &tb));
    assert!(ta == tc, "A and C diverged: {:?}", diff(&ta, &tc));
}

#[test]
fn r11_a_delete_relayed_through_a_third_device_sticks_everywhere() {
    let (a, b, c) = (store_for(A), store_for(B), store_for(C));
    for i in 0..3 {
        a.lock().insert_clipboard(&format!("a{i}"), None, None).unwrap();
        b.lock().insert_clipboard(&format!("b{i}"), None, None).unwrap();
        c.lock().insert_clipboard(&format!("c{i}"), None, None).unwrap();
    }
    mesh_until_quiet(&a, &b, &c, "the initial fill");
    assert_eq!(texts(&c).len(), 9, "premise: C must hold all nine rows first");

    // C deletes a row A wrote. C is not the author, so this is a relayed delete
    // the whole way: it has to reach A, and it has to reach B, and it must not
    // be undone by A re-offering its own row afterwards.
    // Literal SQL: this crate does not depend on rusqlite, and every value here
    // is a test constant.
    let doomed: i64 = {
        let g = c.lock();
        g.conn_for_test()
            .query_row(
                &format!("SELECT id FROM items WHERE source_machine='{A}' AND text='a1'"),
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    c.lock().delete(doomed).unwrap();

    let rounds = mesh_until_quiet(&a, &b, &c, "a relayed delete");
    assert!(rounds >= 2, "premise: the delete must actually have travelled");

    for (name, s) in [("A", &a), ("B", &b), ("C", &c)] {
        let t = texts(s);
        assert!(!t.iter().any(|x| x == "a1"), "{name} still holds the deleted row");
        assert_eq!(t.len(), 8, "{name} holds {} rows, wanted 8", t.len());
    }

    // And it stays gone: another few rounds must not resurrect it, and the
    // author writing something new must not carry it back either.
    a.lock().insert_clipboard("a-new", None, None).unwrap();
    mesh_until_quiet(&a, &b, &c, "a write after the delete");
    for (name, s) in [("A", &a), ("B", &b), ("C", &c)] {
        let t = texts(s);
        assert!(!t.iter().any(|x| x == "a1"), "{name} resurrected the deleted row");
        assert!(t.iter().any(|x| x == "a-new"), "{name} never got the row written after it");
    }
}

#[test]
fn r11_a_clear_on_one_device_of_a_three_device_mesh_reaches_both_others() {
    let (a, b, c) = (store_for(A), store_for(B), store_for(C));
    for i in 0..3 {
        a.lock().insert_clipboard(&format!("a{i}"), None, None).unwrap();
        b.lock().insert_clipboard(&format!("b{i}"), None, None).unwrap();
        c.lock().insert_clipboard(&format!("c{i}"), None, None).unwrap();
    }
    mesh_until_quiet(&a, &b, &c, "the initial fill");
    assert_eq!(texts(&b).len(), 9, "premise: B must hold all nine rows first");

    // The panic button, pressed on B, over a history that is mostly other
    // devices' rows.
    let cleared = b.lock().clear(None).unwrap();
    assert_eq!(cleared, 9, "premise: the clear must have removed all nine");

    mesh_until_quiet(&a, &b, &c, "a Clear History");
    for (name, s) in [("A", &a), ("B", &b), ("C", &c)] {
        let t = texts(s);
        assert!(t.is_empty(), "{name} kept {} rows after a Clear on B: {:?}", t.len(), diff(&t, &[]));
    }
}

// ===========================================================================
// R11-S3. CRITERION C FOR THE `created_at` REFUSAL ROUND 10 RESTORED.
//
// Round 9 banked a receipt for an out-of-range `created_at`; round 10 reverted
// it, accepting "one re-offer per exchange, ending the moment the clock is
// fixed". The question the brief asks is whether that loop is really bounded.
//
// It is, and this pins WHY, because the reason is not the one the comment
// gives. It is not that the clock gets fixed — `created_at` is stored data and
// fixing a clock does not change it. It is that the cursor is compared against
// `updated_at`, so any later row from the same source lifts the cursor over the
// bad one and it stops being offered. The cost is exactly one row per exchange
// until that happens.
// ===========================================================================

#[test]
fn r11_a_created_at_refusal_costs_one_row_per_exchange_and_no_more() {
    let a = store_for(A);
    let b = store_for(B);
    let ahead = now_ms() + 4 * MAX_CLOCK_SKEW_MS;

    // Twenty ordinary rows and one whose created_at is out of range, stamped
    // ABOVE all of them so no ordinary row can lift the cursor past it.
    for i in 0..20 {
        a.lock().insert_clipboard(&format!("ordinary {i}"), None, None).unwrap();
    }
    let top = now_ms();
    // Seeded with literal SQL, because no API can build it: `apply_remote_item`
    // range-checks `created_at` on the way in too, so A cannot be handed one.
    //
    // It is nonetheless a shape an HONEST author now reaches, which is worth
    // recording: `insert_clipboard` on a machine whose clock is fast writes
    // `created_at = updated_at = ahead`, and a later pin or correction, after
    // the clock is fixed, rewrites `updated_at` through `edit_stamp` ->
    // `local_clock_at`, which round 10 clamps to `now + skew`. That leaves
    // `updated_at` BELOW `created_at`, which the comment in `drain` says an
    // honest author cannot produce.
    {
        let g = a.lock();
        g.conn_for_test()
            .execute_batch(&format!(
                "INSERT INTO items (kind, text, created_at, updated_at, pinned, source_machine, origin_id)
                 VALUES ('clipboard', 'crafted', {ahead}, {top}, 0, '{A}', 'crafted');"
            ))
            .unwrap();
    }

    let first = sync_bounded((&a, A), (&b, B));
    assert!(traffic(&first.0) >= 21, "premise: the first exchange must carry everything, carried {}", traffic(&first.0));
    assert_eq!(texts(&b).len(), 20, "the crafted row must be refused, not stored");

    // Every later exchange must carry that one row and nothing else. Growth
    // here, or a full re-offer, is criterion C.
    let mut per_round: Vec<usize> = Vec::new();
    for _ in 0..4 {
        let (d, s) = sync_bounded((&a, A), (&b, B));
        per_round.push(traffic(&d) + traffic(&s));
    }
    assert!(
        per_round.iter().all(|n| *n <= 1),
        "a refused row costs more than itself on every later exchange: {per_round:?}"
    );

    // And it goes quiet entirely as soon as the source produces anything newer.
    a.lock().insert_clipboard("newer than the crafted row", None, None).unwrap();
    let mut tail: Vec<usize> = Vec::new();
    for _ in 0..3 {
        let (d, s) = sync_bounded((&a, A), (&b, B));
        tail.push(traffic(&d) + traffic(&s));
    }
    assert_eq!(
        tail.iter().rev().take(2).sum::<usize>(),
        0,
        "the exchange never goes quiet after a newer row lifts the cursor: {tail:?}"
    );
}
