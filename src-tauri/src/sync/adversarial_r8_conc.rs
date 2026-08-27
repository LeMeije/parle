//! ADVERSARIAL REVIEW, ROUND 8. Concurrency, lifecycle and resource management.
//!
//! Demonstrations of live findings. NOT fixes. Nothing in this file edits
//! production code.
//!
//! Constraints this file works under, stated so the next reader does not repeat
//! the search:
//!
//!   * `SyncManager` holds `tauri::AppHandle<Wry>` and cannot be built under
//!     `MockRuntime`, so no manager-level integration test compiles.
//!   * The pure functions the handover names as the way round that —
//!     `admit_inbound`, `make_room_for_peer`, `note_peer_record`, `stop_claim` —
//!     are PRIVATE to `manager`, and `Inner` is private too. They are reachable
//!     only from the `#[cfg(test)]` modules inside `manager.rs` itself. From a
//!     sibling module such as this one, only `manager::retention_widened`
//!     (`pub(crate)`) can be called. Findings that would need those are reported
//!     as traces, with a note saying which item needs widening to `pub(crate)`.
//!
//! Every test below is hard-bounded. Every socket carries a read AND a write
//! timeout, every loop has a fixed iteration ceiling, and every wait has a wall
//! clock budget that FAILS naming the stalled side rather than parking the
//! suite. One test deliberately produces a permanently deadlocked thread — see
//! the comment on it — which is safe because the mutex it wedges is local to
//! that test and libtest exits the process without joining it.

#![cfg(test)]

use echokey_core::history::{RemoteItem, Store};
use echokey_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";

/// Nothing in this file may exceed this. A stall is a failed assertion.
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
    // READ and WRITE both: a write/write stall never reaches a read.
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

/// One full exchange, `x` dialling, both sides on their own thread, under a
/// wall clock budget. Copied in shape from `adversarial_r7_scale::sync_bounded`
/// for the reason the handover gives: a stall must FAIL naming the side that
/// never returned, not park the suite.
fn sync_bounded(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
) -> (RoundStats, RoundStats) {
    sync_bounded_kinds(x, both(), y, both())
}

/// As `sync_bounded`, with each side's sync-kind toggles given explicitly.
fn sync_bounded_kinds(
    x: (&Arc<Mutex<Store>>, &'static str),
    x_kinds: Kinds,
    y: (&Arc<Mutex<Store>>, &'static str),
    y_kinds: Kinds,
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
            let known = vec![A.to_string(), B.to_string()];
            let attr = Attribution { peer_id: y_id, local_id: x_id, known: &known };
            exchange(
                &mut s,
                &x_store,
                (x_id, "peer"),
                x_kinds,
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
                "the exchange did not finish inside {BUDGET:?}; only {:?} returned",
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

/// One row authored by `source`, stored in `store`.
fn seed_one(store: &Arc<Mutex<Store>>, source: &str, origin: &str, clock: i64) {
    store
        .lock()
        .apply_remote_item(
            source,
            &RemoteItem {
                source_machine: source.into(),
                origin_id: origin.into(),
                kind: "clipboard".into(),
                text: "the row the user will never see again".into(),
                created_at: clock,
                updated_at: clock,
                pinned: false,
            },
        )
        .unwrap();
}

// ===========================================================================
// R8-1. A BANKED RECEIPT WITHOUT ITS ROW IS PERMANENT, SILENT DATA LOSS.
//
// `drain` banks the receipt for an item in its OWN standalone transaction
// (replicate.rs:700-705) and only later stores the row (replicate.rs:775 ->
// apply_item -> Store::apply_remote_item). Those are two separate commits.
//
// `Store::mark_received_in` documents the invariant this breaks, at
// history.rs:1040-1046: "apply_remote_item and apply_remote_tombstone call this
// in the same transaction that stores the row, so 'we hold it' and 'we have
// seen it' commit together or not at all." The early call in `drain` is outside
// any such transaction, so between the two commits the process holds the
// promise without the row.
//
// The app quits by calling `libc::_exit(0)` (lib.rs:274) immediately after
// `sync.stop()`, and `stop()` does not wait for, or even know about, an
// in-flight exchange: the drain checks `abort()` once per MESSAGE
// (replicate.rs:661), not once per item. So every quit taken during a drain
// lands somewhere inside a per-item loop, and roughly half of each item's work
// sits inside this window.
//
// This test does not kill a process. It creates the state a kill leaves behind
// and shows that state is UNRECOVERABLE: a full, healthy exchange afterwards
// will never re-offer the row.
// ===========================================================================

#[test]
fn r8_a_receipt_banked_without_its_row_hides_that_row_for_ever() {
    let clock = now_ms() - 60_000;

    // -- CONTROL. No crash: B receives the row, so the harness works. --------
    {
        let a = store_for(A);
        let b = store_for(B);
        seed_one(&a, A, "row-000001", clock);
        assert_eq!(a.lock().count().unwrap(), 1);
        assert_eq!(b.lock().count().unwrap(), 0);

        sync_bounded((&a, A), (&b, B));

        assert_eq!(
            b.lock().count().unwrap(),
            1,
            "the control failed: an ordinary exchange did not move the row at all, \
             so the loss below would prove nothing"
        );
    }

    // -- THE CRASH RESIDUE. -------------------------------------------------
    // Exactly what replicate.rs:700-705 commits, and nothing else. This is the
    // on-disk state after a quit between that commit and apply_item.
    let a = store_for(A);
    let b = store_for(B);
    seed_one(&a, A, "row-000001", clock);
    b.lock().note_received(A, A, clock).unwrap();

    // The promise is durable; the row is not. Both halves asserted, so the test
    // cannot pass vacuously on a store that silently refused the receipt (it
    // would, for a clock outside the skew window).
    let marks = b.lock().watermarks(A).unwrap();
    assert!(
        marks.iter().any(|(src, c)| src == A && *c == clock),
        "the receipt did not commit, so this test is not exercising the window; marks = {marks:?}"
    );
    assert_eq!(b.lock().count().unwrap(), 0, "and the row is not here");

    // -- RECOVERY ATTEMPT: a full, healthy exchange. ------------------------
    sync_bounded((&a, A), (&b, B));

    assert_eq!(
        b.lock().count().unwrap(),
        0,
        "sanity: the row cannot appear from nowhere"
    );
    assert_eq!(
        a.lock().count().unwrap(),
        1,
        "the author still holds it, so nothing is wrong with A"
    );

    // And again, because "it will catch up next time" is the obvious defence.
    for _ in 0..3 {
        sync_bounded((&a, A), (&b, B));
    }
    assert_eq!(
        b.lock().count().unwrap(),
        0,
        "R8-1 CONFIRMED: the receipt is a promise never to ask for that clock again, \
         so a row whose receipt committed without it is gone from this machine for \
         ever. Four exchanges did not recover it. The user sees an item present on \
         one device and absent on the other, with no error anywhere."
    );

    // The proof that the receipt is the cause, not the harness: clear it and
    // the very next exchange delivers the row.
    b.lock().reset_source_marks().unwrap();
    sync_bounded((&a, A), (&b, B));
    assert_eq!(
        b.lock().count().unwrap(),
        1,
        "clearing the receipt recovers the row, so the receipt was the thing hiding it"
    );
}

/// R8-1c. The production drain really does commit a receipt without its row,
/// and the repair for it is not durable.
///
/// The two tests above create the residue by hand. This one lets
/// `replicate::exchange` create it, using the ordinary kind gate: `drain` banks
/// the receipt at replicate.rs:700-705 and only then decides, at
/// replicate.rs:772, that this machine is not storing clipboard rows. So the
/// state "receipt committed, row absent" is not exotic; it is reached every time
/// a peer offers a kind the user has switched off.
///
/// That is deliberate and `set_kinds` repairs it — by calling
/// `Store::reset_source_marks` at manager.rs:1409. But that repair is NOT
/// durable and NOT retried: it is one in-memory call whose failure is a
/// `tracing::warn!` (manager.rs:1410), while the setting that caused it was
/// already written to settings.json before `apply_settings` ran
/// (commands.rs:43 then commands.rs:48). Miss that one call — a store error, a
/// crash, a quit — and the rows stay hidden for ever.
///
/// The outbound half of exactly the same widening IS made durable, as
/// `resend_owed` in settings.json (manager.rs:1403-1405, 1480). The inbound half
/// is not. This shows what that costs.
#[test]
fn r8_the_drain_banks_a_receipt_for_a_row_it_does_not_store_and_the_repair_is_not_durable() {
    let clock = now_ms() - 60_000;
    let dictations_only = Kinds { dictations: true, clipboard: false };

    let a = store_for(A);
    let b = store_for(B);
    seed_one(&a, A, "row-000001", clock); // a clipboard row

    // B has clipboard sync switched off. Production drain, no crash, no
    // hand-written residue.
    sync_bounded_kinds((&a, A), both(), (&b, B), dictations_only);

    assert_eq!(b.lock().count().unwrap(), 0, "B refused the clipboard row, as designed");
    let marks = b.lock().watermarks(A).unwrap();
    assert!(
        marks.iter().any(|(src, c)| src == A && *c >= clock),
        "PRODUCTION did not bank a receipt for a row it did not store — the R8-1 window \
         does not exist and both tests above are moot. marks = {marks:?}"
    );

    // The user now switches clipboard sync ON. settings.json already says so.
    // `reset_source_marks` is the ONLY thing that reopens the hole, and nothing
    // records that it is owed. Model it not happening.
    for _ in 0..3 {
        sync_bounded((&a, A), (&b, B));
    }
    assert_eq!(
        b.lock().count().unwrap(),
        0,
        "R8-1c CONFIRMED: with the kind back on and settings.json saying so, three \
         exchanges did not refetch the row. The receipt is the only thing hiding it \
         and nothing durable remembers that it must be cleared."
    );

    // And that it is the receipt, not anything else, that is doing the hiding.
    b.lock().reset_source_marks().unwrap();
    sync_bounded((&a, A), (&b, B));
    assert_eq!(
        b.lock().count().unwrap(),
        1,
        "the one call `set_kinds` makes is what recovers it; losing that call loses the row"
    );
}

/// R8-1b. The same window in the TOMBSTONE arm (replicate.rs:874-877 banks the
/// receipt, replicate.rs:908-910 applies the delete), where the consequence is
/// worse: the row the user asked to forget stays on this machine for ever,
/// while the device they deleted it on shows it gone.
#[test]
fn r8_a_receipt_banked_without_its_tombstone_resurrects_a_deleted_row_for_ever() {
    use echokey_core::history::ORIGIN_CEILING;

    let clock = now_ms() - 60_000;

    // A authors a row; B already holds it. A then deletes it.
    let a = store_for(A);
    let b = store_for(B);
    seed_one(&a, A, "row-000001", clock);
    seed_one(&b, A, "row-000001", clock);
    let id = a.lock().recent(None, 10).unwrap()[0].id;
    a.lock().delete(id).unwrap();
    let tomb = a.lock().tombstones_from(A, 0, ORIGIN_CEILING, 10).unwrap();
    assert_eq!(tomb.len(), 1, "the local delete must have left a tombstone");
    let deleted_at = tomb[0].deleted_at;
    assert!(deleted_at > 0);

    // -- CONTROL: no crash, so the delete crosses. --------------------------
    {
        let a2 = store_for(A);
        let b2 = store_for(B);
        seed_one(&a2, A, "row-000001", clock);
        seed_one(&b2, A, "row-000001", clock);
        let id2 = a2.lock().recent(None, 10).unwrap()[0].id;
        a2.lock().delete(id2).unwrap();
        sync_bounded((&a2, A), (&b2, B));
        assert_eq!(
            b2.lock().count().unwrap(),
            0,
            "the control failed: an ordinary exchange did not propagate the delete, \
             so the failure below would prove nothing"
        );
    }

    // -- THE CRASH RESIDUE: the receipt for the delete committed; the
    //    tombstone did not.
    b.lock().note_received(A, A, deleted_at).unwrap();
    assert!(
        b.lock()
            .watermarks(A)
            .unwrap()
            .iter()
            .any(|(src, c)| src == A && *c >= deleted_at),
        "the receipt did not commit, so this test is not exercising the window"
    );
    assert_eq!(b.lock().count().unwrap(), 1, "and B still holds the row");

    for _ in 0..4 {
        sync_bounded((&a, A), (&b, B));
    }
    assert_eq!(
        b.lock().count().unwrap(),
        1,
        "R8-1b CONFIRMED: four exchanges did not deliver the delete. The user cleared \
         an item — a password, in the case this feature exists to handle — on one \
         device, saw it vanish there, and it is still on the other one for ever."
    );

    b.lock().reset_source_marks().unwrap();
    sync_bounded((&a, A), (&b, B));
    assert_eq!(
        b.lock().count().unwrap(),
        0,
        "clearing the receipt delivers the delete, so the receipt was what suppressed it"
    );
}

// ===========================================================================
// R8-2. A DIAL SLOT GUARD IS DROPPED WHILE `inner` IS HELD, AND ITS DROP
//       RE-LOCKS `inner`. parking_lot is NOT reentrant.
//
// Production path, every frame, on the discovery thread started by `start()`:
//
//   manager.rs:677   let mut i = me.inner.lock();          <- guard TAKEN
//   manager.rs:726   if known && due && room && i.dialing.insert(id.clone()) {
//   manager.rs:734       let guard = DialGuard::new(me3.clone(), id.clone());
//   manager.rs:735-751   let launched = std::thread::Builder::new()
//                            .name("echokey-sync-dial".into())
//                            .spawn(move || { let _slot = guard; ... });
//   manager.rs:752       if let Err(e) = launched { tracing::warn!(...) }
//   manager.rs:761   drop(i);                              <- guard released
//
// `guard` is captured BY MOVE into the closure. When `Builder::spawn` fails,
// std drops that closure on the CALLING thread (see the test below), which runs
//
//   manager.rs:118-122  impl Drop for DialGuard { self.owner.release_dial(..) }
//   manager.rs:97-101   impl ReleasesDial for SyncManager {
//                           fn release_dial(&self, id: &str) {
//                               self.inner.lock().dialing.remove(id);   <- SAME MUTEX
//                           }
//                       }
//
// `i` at manager.rs:677 is still alive. parking_lot::Mutex is not reentrant, so
// this parks the discovery thread for ever WHILE IT HOLDS `inner`.
//
// The error-handling branch at manager.rs:752, which exists precisely for this
// case and was written deliberately (`Builder`, not `thread::spawn`, "that
// PANICS if the OS refuses a thread"), is unreachable: the deadlock happens
// inside `spawn`, before it returns.
//
// The two tests below confirm the two mechanisms the trace rests on.
// ===========================================================================

/// A stack size no allocator will satisfy, used to make `Builder::spawn` fail
/// on demand. Page-aligned so `pthread_attr_setstacksize` accepts it and the
/// failure lands in the thread creation itself.
///
/// Measured on this machine (macOS 26.5, arm64): 1<<46 still succeeds, 1<<47
/// and above fail with EAGAIN, "Resource temporarily unavailable" — the same
/// errno a process that has hit its thread limit gets, which is the condition
/// manager.rs:844-848 says `Builder` is there to survive. 1<<50 leaves plenty
/// of margin on either side.
const UNSATISFIABLE_STACK: usize = 1usize << 50;

/// R8-2a. `Builder::spawn` drops the closure — and therefore everything the
/// closure captured by move — on the CALLING thread when it fails.
///
/// If this were false the trace above would be wrong, so it is asserted rather
/// than assumed.
#[test]
fn r8_a_failed_thread_spawn_drops_the_closure_on_the_calling_thread() {
    struct Tell(Arc<std::sync::atomic::AtomicUsize>, std::thread::ThreadId);
    impl Drop for Tell {
        fn drop(&mut self) {
            assert_eq!(
                std::thread::current().id(),
                self.1,
                "dropped somewhere other than the spawning thread"
            );
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let here = std::thread::current().id();
    let tell = Tell(dropped.clone(), here);

    let launched = std::thread::Builder::new()
        .name("r8-doomed".into())
        .stack_size(UNSATISFIABLE_STACK)
        .spawn(move || {
            let _moved = tell;
        });

    // A guard that can find nothing must assert that it found something: if the
    // OS satisfied the request there is no failure to observe and the test must
    // say so rather than pass.
    let err = match launched {
        Err(e) => e,
        Ok(h) => {
            let _ = h.join();
            panic!(
                "could not force a spawn failure with a {UNSATISFIABLE_STACK}-byte stack, \
                 so this test proved nothing; raise the size"
            );
        }
    };
    assert!(!err.to_string().is_empty());
    assert_eq!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "std did NOT drop the moved-in value when the spawn failed — the R8-2 trace \
         depends on it doing so"
    );
}

/// R8-2b. The production shape: a guard whose `Drop` re-locks the mutex the
/// calling thread already holds, dropped by a failing `Builder::spawn`.
///
/// **This test deliberately leaks one permanently parked thread.** That is the
/// finding. It is safe: the mutex it wedges is created here and touched by
/// nothing else, the test thread itself never blocks on it, and libtest ends the
/// process without joining detached threads. The test's own wait is bounded.
#[test]
fn r8_a_dial_guard_dropped_under_inner_deadlocks_the_discovery_thread() {
    /// `DialGuard` + `SyncManager::release_dial`, transcribed. `DialGuard`,
    /// `ReleasesDial` and `Inner` are all private to `manager`, so they cannot
    /// be driven from here; see the module header.
    struct DialGuardShape {
        dialing: Arc<Mutex<HashSet<String>>>,
        id: String,
    }
    impl Drop for DialGuardShape {
        fn drop(&mut self) {
            // manager.rs:99 — self.inner.lock().dialing.remove(id);
            self.dialing.lock().remove(&self.id);
        }
    }

    // -- CONTROL: the same guard, the same failing spawn, but the lock is NOT
    //    held. This is the outbound dial thread's own exit path, and it is
    //    fine. Without this half the test below would only be proving that
    //    parking_lot can block.
    {
        let dialing: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        dialing.lock().insert("peer".to_string());
        let guard = DialGuardShape { dialing: dialing.clone(), id: "peer".into() };
        let launched = std::thread::Builder::new()
            .stack_size(UNSATISFIABLE_STACK)
            .spawn(move || {
                let _slot = guard;
            });
        assert!(launched.is_err(), "the control needs the spawn to fail too");
        assert!(
            dialing.lock().is_empty(),
            "with the lock free the slot comes straight back, as it should"
        );
    }

    // -- THE FINDING: manager.rs:677 holds `inner` across manager.rs:735.
    let dialing: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let (tx, rx) = mpsc::channel::<&'static str>();
    let d2 = dialing.clone();

    std::thread::Builder::new()
        .name("r8-discovery".into())
        .spawn(move || {
            // manager.rs:677 — the discovery thread takes `inner` for the whole
            // of a PeerFound event.
            let mut i = d2.lock();
            // manager.rs:726 — the dial slot is claimed under that lock.
            i.insert("peer".to_string());
            // manager.rs:734 — the guard is built BEFORE the spawn, on purpose.
            let guard = DialGuardShape { dialing: d2.clone(), id: "peer".into() };
            // manager.rs:735 — and moved into a thread the OS may refuse.
            let launched = std::thread::Builder::new()
                .stack_size(UNSATISFIABLE_STACK)
                .spawn(move || {
                    let _slot = guard;
                });
            // manager.rs:752 — never reached on a failure.
            let _ = launched;
            // manager.rs:761 — never reached either.
            drop(i);
            let _ = tx.send("survived");
        })
        .expect("the stand-in discovery thread must start");

    match rx.recv_timeout(Duration::from_secs(3)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Confirmed. The thread is parked inside DialGuard::drop, holding
            // `inner`. In production that is the discovery thread, and every
            // publish(), status(), start(), stop(), sync_status command and
            // settings-panel read blocks behind it — including sync_status,
            // which is a NON-async tauri command and therefore runs on the main
            // thread. The app hard-freezes and needs a force quit.
        }
        Ok(_) => panic!(
            "R8-2 not reproduced: the guard's Drop acquired a lock the same thread \
             already held, which parking_lot does not permit. Either the spawn \
             succeeded or the shape has changed."
        ),
        Err(e) => panic!("the stand-in discovery thread died unexpectedly: {e}"),
    }
}

// ===========================================================================
// R8-3. WHAT I ATTACKED AND COULD NOT BREAK: the history UI behind the store
//       mutex during an exchange.
//
// `search_history`, `pin_item`, `delete_item`, `clear_history`, `copy_item`
// and `dict_list` (commands.rs:59-343) are all NON-async tauri commands, so
// they run on the main thread and take `store` there. `replicate::exchange`
// takes the same mutex, on a listener or dial thread, for every page it serves
// and up to four times per item it drains.
//
// This measures the worst wait the UI thread ever sees, against a real
// exchange, and self-checks that the meter can see a hold when there is one.
// ===========================================================================

#[test]
fn r8_an_exchange_never_holds_the_store_mutex_long_enough_to_freeze_the_ui() {
    /// The longest the "UI thread" ever waited to get the store lock, in ms.
    fn meter(store: Arc<Mutex<Store>>, stop: Arc<std::sync::atomic::AtomicBool>) -> u128 {
        let mut worst = 0u128;
        // Hard bound as well as the flag, so this can never spin for ever.
        for _ in 0..200_000 {
            if stop.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let t0 = Instant::now();
            let g = store.lock();
            let waited = t0.elapsed().as_millis();
            drop(g);
            worst = worst.max(waited);
            std::thread::sleep(Duration::from_millis(1));
        }
        worst
    }

    // -- SELF-CHECK: the meter must be able to find something. ---------------
    {
        let s = store_for(B);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s2, stop2) = (s.clone(), stop.clone());
        let m = std::thread::spawn(move || meter(s2, stop2));
        // Somebody holds the store for 400ms, the way a single unchunked prune
        // or an unpaged fetch would.
        let held = s.lock();
        std::thread::sleep(Duration::from_millis(400));
        drop(held);
        std::thread::sleep(Duration::from_millis(50));
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let worst = m.join().expect("meter thread");
        assert!(
            worst >= 300,
            "the meter cannot detect a 400ms hold (worst it saw was {worst}ms), \
             so a clean run below would prove nothing"
        );
    }

    // -- THE REAL RUN. ------------------------------------------------------
    let a = store_for(A);
    let b = store_for(B);
    let base = now_ms() - 5_000_000;
    for i in 0..600 {
        seed_one(&a, A, &format!("row-{i:06}"), base + i as i64);
    }

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (b2, stop2) = (b.clone(), stop.clone());
    let m = std::thread::spawn(move || meter(b2, stop2));

    let t0 = Instant::now();
    sync_bounded((&a, A), (&b, B));
    let took = t0.elapsed();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let worst = m.join().expect("meter thread");

    assert!(
        b.lock().count().unwrap() > 0,
        "the exchange must actually have done work, or the meter measured an idle mutex"
    );
    assert!(
        took < BUDGET,
        "the exchange itself overran its budget: {took:?}"
    );
    assert!(
        worst < 250,
        "a receiving exchange froze the history UI for {worst}ms in one go. \
         Every history command is a synchronous tauri command on the main thread."
    );
}

// ===========================================================================
// R8-4. WHAT I ATTACKED AND COULD NOT BREAK: `retention_widened`.
//
// The one manager helper reachable from here. Both directions and the
// "keep for ever" special case, which every naive version gets backwards.
// ===========================================================================

#[test]
fn r8_retention_widened_reads_keep_for_ever_as_the_widest_window() {
    use crate::sync::manager::retention_widened;
    assert!(!retention_widened(7, 7), "no change is not a widening");
    assert!(!retention_widened(0, 0));
    assert!(retention_widened(7, 30), "a longer finite window widens");
    assert!(!retention_widened(30, 7), "a shorter one narrows");
    assert!(retention_widened(7, 0), "to keep-for-ever is the widest there is");
    assert!(!retention_widened(0, 7), "from keep-for-ever, anything narrows");
    assert!(!retention_widened(0, u32::MAX), "still narrower than for ever");
    assert!(retention_widened(1, u32::MAX));
}
