//! ADVERSARIAL REVIEW — ROUND 7. Scale, and the exchange that would not finish.
//!
//! Round 6 left one test quarantined with `#[ignore = "HANGS"]`: a Clear
//! History over a replicated history larger than `MAX_TOMBSTONES_PER_SOURCE`,
//! exchanged in one round. It blocked the whole suite rather than failing.
//!
//! This file re-derives it from scratch, as the handover asks: hard iteration
//! bounds, read AND write timeouts on every socket, and a wall-clock budget on
//! the exchange itself so a stall is a FAILED assertion naming where it stalled
//! rather than a suite that never returns.

#![cfg(test)]

use echokey_core::history::{RemoteItem, Store};
use echokey_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";

/// Every exchange in this file must finish inside this, or the test fails and
/// says which side was still running.
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
/// Both sides run on their own thread and report through a channel, so a side
/// that never returns is reported as a stall instead of parking the harness.
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
        assert!(!left.is_zero(), "the exchange did not finish inside {BUDGET:?}; {} of 2 sides returned, the other is stalled", got.len());
        match rx.recv_timeout(left) {
            Ok(r) => got.push(r),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!(
                    "the exchange did not finish inside {BUDGET:?}; {} of 2 sides returned. \
                     Returned so far: {:?}",
                    got.len(),
                    got.iter().map(|(who, _)| *who).collect::<Vec<_>>()
                );
            }
            Err(e) => panic!("both exchange threads died without reporting: {e}"),
        }
    }
    acceptor.join().expect("acceptor thread panicked");
    dialler.join().expect("dialler thread panicked");

    let mut dialler_stats = None;
    let mut acceptor_stats = None;
    for (who, r) in got {
        let stats = r.unwrap_or_else(|e| panic!("{who} side failed: {e}"));
        match who {
            "dialler" => dialler_stats = Some(stats),
            _ => acceptor_stats = Some(stats),
        }
    }
    (
        dialler_stats.expect("the dialling side reported"),
        acceptor_stats.expect("the accepting side reported"),
    )
}

/// Seed `store` with `n` rows authored by `source`.
fn seed(store: &Arc<Mutex<Store>>, source: &str, n: usize, base: i64) {
    let g = store.lock();
    for i in 0..n {
        g.apply_remote_item(
            source,
            &RemoteItem {
                source_machine: source.into(),
                origin_id: format!("row-{i:06}"),
                kind: "clipboard".into(),
                text: format!("secret {i}"),
                created_at: base + i as i64,
                updated_at: base + i as i64,
                pinned: false,
            },
        )
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// R7-1. THE QUARANTINED TEST, RE-DERIVED.
//
// B authored a history larger than MAX_TOMBSTONES_PER_SOURCE and A holds all of
// it. The user clears history on A. One exchange, with B dialling, so A accepts
// and therefore drains before it serves — the order the round-6 reviewer
// believed put the tombstone-cap eviction before the serve.
//
// Two separate claims are checked, because the original test conflated them:
//   1. the exchange completes at all (it used to hang), and
//   2. every one of the user's deletes reaches B.
// ---------------------------------------------------------------------------
#[test]
fn r7_clear_history_over_a_large_replicated_history_completes_and_loses_no_delete() {
    let over = echokey_core::history::MAX_TOMBSTONES_PER_SOURCE as usize + 50;
    let a = store_for(A);
    let b = store_for(B);
    let base = now_ms() - 10_000_000;

    seed(&b, B, over, base);
    seed(&a, B, over, base);
    assert_eq!(a.lock().count().unwrap(), over as i64);
    assert_eq!(b.lock().count().unwrap(), over as i64);

    a.lock().clear(None).unwrap();
    assert_eq!(a.lock().count().unwrap(), 0);
    assert_eq!(a.lock().tombstone_count(B).unwrap(), over as i64);

    // B deletes one of its own rows too — an entirely ordinary thing, and the
    // tombstone that used to trigger the eviction on A.
    let b_id: i64 = b.lock().recent(None, 1).unwrap()[0].id;
    b.lock().delete_item_local(b_id).unwrap();

    // Bounded: a fixed number of exchanges, each under a wall-clock budget.
    // One is not enough on purpose — a single exchange is capped at
    // MAX_BATCHES * PAGE per source, and demanding that the whole job fit in
    // one round is a different (and wrong) requirement from "no delete is
    // lost".
    for round in 0..6 {
        let (b_stats, _a_stats) = sync_bounded((&b, B), (&a, A));
        if b_stats.applied_tombstones == 0 && b_stats.applied_items == 0 {
            break;
        }
        assert!(round < 5, "the pair had not converged after six exchanges");
    }

    assert_eq!(
        b.lock().count().unwrap(),
        0,
        "the user cleared history and B kept {} of the cleared rows",
        b.lock().count().unwrap()
    );
}

// ---------------------------------------------------------------------------
// R7-2. The same shape, small enough to isolate a stall from a data defect.
// If R7-1 stalls and this passes, the problem is scale, not sequence.
// ---------------------------------------------------------------------------
#[test]
fn r7_a_small_clear_history_reaches_the_author_in_one_exchange() {
    let a = store_for(A);
    let b = store_for(B);
    let base = now_ms() - 1_000_000;

    seed(&b, B, 20, base);
    seed(&a, B, 20, base);
    a.lock().clear(None).unwrap();

    let (_b_stats, _a_stats) = sync_bounded((&b, B), (&a, A));
    assert_eq!(b.lock().count().unwrap(), 0, "a small clear must land in one exchange");
}
