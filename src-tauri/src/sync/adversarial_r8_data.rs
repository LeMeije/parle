//! ADVERSARIAL REVIEW, ROUND 8. Data integrity and replication correctness,
//! exchange side.
//!
//! Scope: `replicate.rs` — serve, drain, authority, paging, the one-shot
//! re-offer. The store-side half is in
//! `crates/parle-core/src/adversarial_r8_data.rs`.
//!
//! Every socket here has read AND write timeouts, every loop has a hard bound,
//! and every exchange runs under a wall-clock budget with both sides on their
//! own thread, copied from `adversarial_r7_scale::sync_bounded`, so a stall
//! fails with a message naming the side that never returned.

#![cfg(test)]

use parle_core::history::{RemoteItem, RemoteTombstone, Store};
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
        sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
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
///
/// `x_abort` is the number of abort-hook calls the DIALLING side will allow
/// before it starts returning true; `usize::MAX` means never abort. Errors are
/// returned rather than unwrapped, because an aborted exchange is expected to
/// end in one on both sides — the point of the test is what each store is left
/// holding afterwards.
fn sync_bounded(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
    x_abort_after: usize,
) -> (Result<RoundStats, String>, Result<RoundStats, String>) {
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
        let calls = AtomicUsize::new(0);
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
                &|| calls.fetch_add(1, Ordering::SeqCst) >= x_abort_after,
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
        match who {
            "dialler" => d = Some(r),
            _ => a = Some(r),
        }
    }
    (d.expect("dialler reported"), a.expect("acceptor reported"))
}

/// `n` rows authored by `source`, seeded into `store` through the same
/// `apply_remote_item` the drain path uses.
fn seed(store: &Arc<Mutex<Store>>, source: &str, n: usize, clock: i64) {
    let g = store.lock();
    for i in 0..n {
        g.apply_remote_item(
            source,
            &RemoteItem {
                source_machine: source.into(),
                origin_id: format!("row-{i:06}"),
                kind: "clipboard".into(),
                text: format!("secret {i}"),
                created_at: clock,
                updated_at: clock,
                pinned: false,
            },
        )
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// R8-S1. A STOP BETWEEN TOMBSTONE PAGES LOSES EVERY DELETE IN THE BOUNDARY
//        MILLISECOND, PERMANENTLY.
//
// `serve`'s ITEM loop trims every full page back to a millisecond boundary, and
// the comment there states exactly why:
//
//   "The peer records the highest clock it sees, and the next exchange asks
//    strictly above it. So a run that stops BETWEEN pages — the user unpairs
//    mid-exchange, sync is switched off, the network drops, the abort hook
//    fires — left the peer's cursor inside a millisecond, and the rest of that
//    millisecond sat below it forever. Nothing lowers a cursor."
//
// The TOMBSTONE loop, twenty lines below it, pages identically and has no trim.
// And tombstones are the one stream where a whole millisecond routinely holds
// far more than a page: `clear()` stamps every tombstone for a source with a
// single clock.
//
// In ordinary terms. You have a laptop and a desktop, sharing history. You
// paste a password on the laptop, panic, and hit Clear History, which is the
// feature's whole promise. The sync starts, and while it is running you switch
// sync off, or unpair, or close the lid, or walk out of Wi-Fi range. The
// desktop applied the first 256 deletes and recorded the clock they all share.
// Every later exchange asks for deletes strictly above that clock and there are
// none. The remaining rows — the password among them — stay on the desktop for
// ever, and no later sync, restart or re-pairing will ever remove them.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_clear_interrupted_between_tombstone_pages_loses_the_rest_of_that_millisecond() {
    let rows = 600usize; // more than two pages, one millisecond
    let a = store_for(A);
    let b = store_for(B);
    let clock = now_ms() - 500_000;

    // B authored the history; A holds a replicated copy of all of it.
    seed(&b, B, rows, clock);
    seed(&a, B, rows, clock);
    assert_eq!(a.lock().count().unwrap(), rows as i64);
    assert_eq!(b.lock().count().unwrap(), rows as i64);

    // The user clears history on A. One clock for the whole batch.
    a.lock().clear(None).unwrap();
    assert_eq!(a.lock().tombstone_count(B).unwrap(), rows as i64);
    let mut clocks: Vec<i64> = a
        .lock()
        .tombstones_since(B, 0, 10_000)
        .unwrap()
        .into_iter()
        .map(|t| t.deleted_at)
        .collect();
    clocks.sort_unstable();
    clocks.dedup();
    // Recorded, not asserted. Today a Clear stamps ONE clock for the whole
    // source, which is what makes the boundary millisecond unpageable. If a fix
    // makes it stamp a chain the interruption resumes correctly and the final
    // assertion below simply passes, which is the outcome we want — a guard
    // that dies on its own precondition proves nothing either way.
    let distinct_clear_clocks = clocks.len();

    // Exchange one, interrupted after the first tombstone batch: A dials, so it
    // serves before it drains, and the abort hook fires on its second call.
    let (dial, _acc) = sync_bounded((&a, A), (&b, B), 1);
    assert!(
        dial.is_err(),
        "the premise: the interrupted side must abort, not complete"
    );
    let after_first = b.lock().count().unwrap();
    assert!(
        after_first > 0 && after_first < rows as i64,
        "the premise: the interruption must land PART way through, {after_first} of {rows} left"
    );

    // Now let them talk as long as they like, uninterrupted and bounded.
    for _ in 0..8 {
        let (d, c) = sync_bounded((&a, A), (&b, B), usize::MAX);
        let moved = d.map(|s| s.applied_tombstones + s.applied_items).unwrap_or(0)
            + c.map(|s| s.applied_tombstones + s.applied_items).unwrap_or(0);
        if moved == 0 {
            break;
        }
    }

    // The count is taken into a local FIRST. `assert_eq!(b.lock()..., 0, "..",
    // b.lock()..)` deadlocks on failure: the guard from the left operand is a
    // temporary that lives to the end of the statement, and parking_lot's mutex
    // is not reentrant, so the failure path blocks for ever instead of
    // reporting. The same shape exists in `adversarial_r7_scale`, where it has
    // simply never fired.
    let left_on_b = b.lock().count().unwrap();
    assert_eq!(
        left_on_b, 0,
        "the user cleared history and {left_on_b} rows are still on the other \
         machine, unreachable to every future exchange. The clear stamped \
         {distinct_clear_clocks} distinct clock(s) across {rows} tombstones, so \
         the cursor the peer banked sits INSIDE that millisecond and every later \
         exchange asks strictly above it."
    );
}

// ---------------------------------------------------------------------------
// R8-S2. THE EXCHANGE NEVER GOES QUIET WHEN AN ORIGIN ID SORTS ABOVE THE
//        SENTINEL.
//
// `ORIGIN_CEILING` is "\u{FFFF}" and is used as "strictly after this whole
// millisecond", justified by "origin ids are UUIDs — ASCII". The wire does not
// enforce that: `validate_origin_id` checks length only, so any UTF-8 up to 128
// bytes is legal and every scalar above U+FFFF sorts above the sentinel under
// SQLite's BINARY collation.
//
// A device running a different or later build of Parle, or simply a buggy one,
// that mints such an id has its tombstone re-served on EVERY exchange for the
// life of the pairing. The receiving side records the clock it already had, so
// nothing can ever advance the cursor past it.
//
// The control half of this test runs the identical sequence with a UUID origin
// id and requires it to go quiet, so the test cannot pass by measuring nothing.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_tombstone_with_an_origin_id_above_the_sentinel_is_re_served_for_ever() {
    // Control first: an ordinary UUID origin must go quiet.
    let quiet_rounds = tombstone_traffic_tail("6f9619ff-8b86-d011-b42d-00c04fc964ff");
    assert_eq!(
        quiet_rounds, 0,
        "the control did not go quiet, so this test measures nothing"
    );

    let noisy = tombstone_traffic_tail("\u{1F600}-origin");
    assert_eq!(
        noisy, 0,
        "after the history had settled, one tombstone was still served on every \
         exchange ({noisy} in the last round). Its origin id sorts above \
         ORIGIN_CEILING, so `deleted_at = cursor AND origin_id > sentinel` \
         matches for ever and no cursor can move past it."
    );
}

/// Seed one row on both sides under `origin`, delete it on the author, let the
/// pair exchange until it should have settled, and report how many tombstones
/// were still being sent on the LAST round.
fn tombstone_traffic_tail(origin: &str) -> usize {
    let a = store_for(A);
    let b = store_for(B);
    let clock = now_ms() - 60_000;
    let item = RemoteItem {
        source_machine: B.into(),
        origin_id: origin.into(),
        kind: "clipboard".into(),
        text: "hunter2".into(),
        created_at: clock,
        updated_at: clock,
        pinned: false,
    };
    // Both machines hold it, and B is its author.
    a.lock().apply_remote_item(B, &item).unwrap();
    b.lock().apply_remote_item(B, &item).unwrap();

    // B deletes it, through the ordinary local delete path.
    let id = b.lock().recent(None, 10).unwrap()[0].id;
    b.lock().delete_item_local(id).unwrap();

    let mut tail = 0usize;
    for _ in 0..5 {
        // B dials, so B serves its own tombstone first.
        let (d, c) = sync_bounded((&b, B), (&a, A), usize::MAX);
        tail = d.map(|s| s.sent_tombstones).unwrap_or(0)
            + c.map(|s| s.sent_tombstones).unwrap_or(0);
    }
    tail
}

// ---------------------------------------------------------------------------
// R8-S3. The ordinary case still converges, so S1 and S2 are not measuring a
// pair that never worked. Same shape as S1, small enough to fit in one page.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_clear_that_fits_in_one_page_reaches_the_author_and_goes_quiet() {
    let a = store_for(A);
    let b = store_for(B);
    let clock = now_ms() - 500_000;
    seed(&b, B, 40, clock);
    seed(&a, B, 40, clock);
    a.lock().clear(None).unwrap();

    let mut last_moved = usize::MAX;
    for _ in 0..6 {
        let (d, c) = sync_bounded((&a, A), (&b, B), usize::MAX);
        last_moved = d.map(|s| s.applied_items + s.applied_tombstones).unwrap_or(0)
            + c.map(|s| s.applied_items + s.applied_tombstones).unwrap_or(0);
    }
    assert_eq!(b.lock().count().unwrap(), 0, "the clear did not reach the author");
    assert_eq!(last_moved, 0, "the pair never went quiet");
}

// ---------------------------------------------------------------------------
// R8-S4. Hostile origin ids and clocks through the whole exchange: no panic, no
// hang, and nothing a peer sends can make the store hold a row it cannot later
// offer.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_hostile_peer_cannot_crash_or_corrupt_the_other_side() {
    let a = store_for(A);
    let b = store_for(B);
    let now = now_ms();

    // B is the hostile one. Everything here is legal on the wire.
    let nasties: Vec<(String, i64)> = vec![
        ("\u{10FFFF}".to_string(), now - 1_000),
        ("\u{0}embedded-nul".to_string(), now - 2_000),
        ("'); DROP TABLE items;--".to_string(), now - 3_000),
        ("z".repeat(128), now - 4_000),
        ("\u{202E}rtl".to_string(), now - 5_000),
    ];
    {
        let g = b.lock();
        for (origin, clock) in &nasties {
            g.apply_remote_item(
                B,
                &RemoteItem {
                    source_machine: B.into(),
                    origin_id: origin.clone(),
                    kind: "transcription".into(),
                    text: "\u{0}\u{FFFD}payload".into(),
                    created_at: *clock,
                    updated_at: *clock,
                    pinned: true,
                },
            )
            .unwrap();
        }
    }

    for _ in 0..3 {
        let _ = sync_bounded((&b, B), (&a, A), usize::MAX);
    }

    // Whatever A kept must be reachable from a zero cursor: a row the store
    // holds but replication can never offer is a silent permanent divergence.
    let g = a.lock();
    let held = g
        .recent(None, 1_000)
        .unwrap()
        .into_iter()
        .filter(|r| g.source_machine_of(r.id).unwrap().as_deref() == Some(B))
        .count();
    let reachable = g.items_since(B, 0, 1_000).unwrap().len();
    assert_eq!(held, reachable, "A holds a row no cursor can reach");
    assert!(held > 0, "nothing arrived, so this test measured nothing");
}

// ---------------------------------------------------------------------------
// R8-S5. A peer may not author rows for a third device, nor for us, however it
// dresses them up. Pinned here as a regression guard on `Attribution`, which
// three previous rounds each had to re-fix.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_peer_still_cannot_author_for_a_third_device_or_for_us() {
    const C: &str = "33333333-3333-4333-8333-333333333333";
    let a = store_for(A);
    let b = store_for(B);
    let now = now_ms();

    {
        let g = b.lock();
        for (src, origin) in [(C, "third-1"), (A, "ours-1")] {
            g.apply_remote_item(
                src,
                &RemoteItem {
                    source_machine: src.into(),
                    origin_id: origin.into(),
                    kind: "clipboard".into(),
                    text: "forged".into(),
                    created_at: now - 1_000,
                    updated_at: now - 1_000,
                    pinned: false,
                },
            )
            .unwrap();
        }
        // And a forged delete for a row A does hold.
        g.apply_remote_tombstone(
            C,
            &RemoteTombstone {
                source_machine: C.into(),
                origin_id: "never-existed".into(),
                deleted_at: now - 900,
            },
        )
        .unwrap();
    }

    for _ in 0..2 {
        let _ = sync_bounded((&b, B), (&a, A), usize::MAX);
    }

    let g = a.lock();
    let forged = g
        .recent(None, 1_000)
        .unwrap()
        .into_iter()
        .filter(|r| r.text == "forged")
        .count();
    assert_eq!(forged, 0, "a paired peer authored rows for a device that is not it");
    assert!(
        !g.holds_identity(C, "never-existed").unwrap(),
        "a paired peer planted a tombstone for an identity this machine never had"
    );
}
