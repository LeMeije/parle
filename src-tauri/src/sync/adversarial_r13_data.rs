//! ADVERSARIAL REVIEW, ROUND 13. Data integrity, exchange side.
//!
//! Round 12 changed four things this file can reach:
//!
//!   * `Store::edit_stamp` now takes `max(clamped, current + 1)`, which is the
//!     only stamp in the store allowed above `now + MAX_CLOCK_SKEW_MS`.
//!   * `manager.rs` banks a resume debt on `resend_all || stats.truncated`,
//!     not on `resend_all` alone.
//!   * `recv_watermarks` takes `i64::try_from(w.clock).unwrap_or(i64::MAX)`,
//!     with `i64::MAX` deliberately chosen so `unreachable_cursor` fires.
//!
//! Harness shape is the one rounds 11 and 12 settled on: every exchange runs on
//! its own pair of threads under a wall-clock budget, both sockets carry read
//! and write timeouts, and every convergence loop is hard-bounded, so a stall
//! fails with the name of the side that never returned instead of parking the
//! suite.

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

/// One exchange, `x` dialling, under a wall-clock budget. `debt` is what
/// `manager.rs` passes as `(resend_all, resend_from)`.
fn sync_bounded_with(
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

fn sync_bounded(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
) -> (RoundStats, RoundStats) {
    sync_bounded_with((x.0, x.1, both()), (y.0, y.1, both()), None)
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

/// The durable state one backwards clock step past the skew window leaves.
///
/// `now_ms()` cannot be moved from inside the process and both stores share one
/// wall clock, so the step is applied to the two durable records it changes:
/// the author's own row clocks, which were the wall clock when they were
/// written, and the peer's copy plus its receipt, which `mark_received_in` set
/// to the row's `(updated_at, origin_id)` when it was applied.
///
/// Same modelling as `adversarial_r11_data::step_the_authors_clock_back` and
/// `adversarial_r12_data`'s copy of it. Neither is touched.
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
// R13-X1. ROUND 12 MOVED THE REFUSAL, IT DID NOT REMOVE IT.
//
// In ordinary terms. The Mac's clock has been six minutes fast, an NTP
// correction puts it right, and you then correct a transcription. Round 11 sent
// a clock LOWER than the one the PC holds and the PC refused it for ever.
// Round 12 sends `row + 1` instead, which is above the row but also above every
// correctly-clocked machine's ceiling, so `apply_remote_item` throws it away on
// arrival and keeps doing so on every exchange until the wall clock has climbed
// past the original error.
//
// That is a genuine improvement — the refusal banks no receipt, so it is
// self-repairing where round 11's was not — and it is not what round 12's
// commit message claims, which is that the correction now reaches the peer.
// It reaches nothing for as long as the clock was wrong.
// ===========================================================================

#[test]
fn r13_data_a_correction_past_the_window_is_refused_on_every_exchange() {
    let a = store_for(A);
    let b = store_for(B);

    let id = a.lock().insert_clipboard("the original", None, None).unwrap();
    let (origin, _) = a.lock().origin_and_text_for_test(id).unwrap();
    sync_bounded((&a, A), (&b, B));
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("the original"),
        "premise: the peer holds the row before the clock moves"
    );

    // Six minutes of clock error, corrected.
    step_the_authors_clock_back(&a, A, &b, 3 * MAX_CLOCK_SKEW_MS);
    a.lock().update_text(id, "the correction").unwrap();

    // THE DISCRIMINATOR. Round 11's `edit_stamp` is clamped and cannot produce
    // this; round 12's `.max(current + 1)` does.
    let stamped = clock_of(&a, A, &origin).expect("the author still holds the row");
    let ceiling = now_ms() + MAX_CLOCK_SKEW_MS;
    assert!(
        stamped > ceiling,
        "round 12 stamps the edit above the ceiling; stamped {stamped}, ceiling {ceiling}"
    );

    // Three exchanges. The row is re-offered every time, because a refusal
    // banks no receipt, and refused every time, because the clock is still out.
    for round in 1..=3 {
        let (_, acceptor) = sync_bounded((&a, A), (&b, B));
        assert!(
            acceptor.ignored >= 1,
            "round {round}: the peer must be REFUSING the correction, not quietly \
             not being offered it: {acceptor:?}"
        );
        assert_eq!(
            text_of(&b, A, &origin).as_deref(),
            Some("the original"),
            "round {round}: the peer still holds the old text"
        );
    }
}

/// The other side of R13-X1: a backwards step INSIDE the skew window is
/// delivered. This one passes under round 11's rule as well, deliberately, and
/// it is here to pin the boundary rather than to discriminate: the clamp only
/// bites once the error is bigger than the window, so that is the only place
/// the two rules can differ.
#[test]
fn r13_data_a_correction_inside_the_window_still_reaches_the_peer() {
    let a = store_for(A);
    let b = store_for(B);

    let id = a.lock().insert_clipboard("the original", None, None).unwrap();
    let (origin, _) = a.lock().origin_and_text_for_test(id).unwrap();
    sync_bounded((&a, A), (&b, B));

    step_the_authors_clock_back(&a, A, &b, MAX_CLOCK_SKEW_MS / 2);
    a.lock().update_text(id, "the correction").unwrap();
    let stamped = clock_of(&a, A, &origin).unwrap();
    assert!(
        stamped <= now_ms() + MAX_CLOCK_SKEW_MS,
        "guard integrity: inside the window the stamp must stay under the ceiling, or \
         this test has become a copy of the one above; stamped {stamped}"
    );

    let (_, acceptor) = sync_bounded((&a, A), (&b, B));
    assert_eq!(
        acceptor.ignored, 0,
        "nothing should be refused inside the window: {acceptor:?}"
    );
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("the correction"),
        "the correction must arrive"
    );
}

// ===========================================================================
// R13-X2. AN ABSURD WATERMARK IS SURVIVED, AND `i64::MAX` REACHES NO
//         ARITHMETIC.
//
// Round 12 replaced `w.clock as i64` with `i64::try_from(w.clock)
// .unwrap_or(i64::MAX)` so a `u64` above `i64::MAX` no longer wraps NEGATIVE.
// `i64::MAX` is then handed straight to `unreachable_cursor` and, on that
// branch, to nothing else: the floor is replaced by `(0, "")` before any
// comparison or addition sees it. This drives the whole path with the largest
// value the wire can carry.
// ===========================================================================

/// Advertise `clock` as the peer's mark, which is what arrives on the wire.
/// Written into the peer's own receipt table because that is what
/// `send_watermarks` reads; `mark_received_in`'s range check guards the write
/// path only.
fn peer_advertises(peer: &Arc<Mutex<Store>>, source: &str, clock: i64) {
    let g = peer.lock();
    g.conn_for_test()
        .execute_batch(&format!(
            "UPDATE source_marks SET received_clock={clock} WHERE source_machine='{source}';"
        ))
        .unwrap();
}

#[test]
fn r13_data_the_largest_watermark_the_wire_can_carry_delivers_rather_than_starves() {
    let a = store_for(A);
    let b = store_for(B);

    let first = a.lock().insert_clipboard("before", None, None).unwrap();
    let (first_origin, _) = a.lock().origin_and_text_for_test(first).unwrap();
    sync_bounded((&a, A), (&b, B));
    assert!(
        text_of(&b, A, &first_origin).is_some(),
        "premise: an ordinary exchange works before the mark is corrupted"
    );

    // `u64::MAX` on the wire. `send_watermarks` casts our stored mark with
    // `.max(0) as u64`, so the largest thing a peer can advertise is what
    // `i64::MAX` casts to, and `recv_watermarks` maps anything at or above
    // `i64::MAX` back to `i64::MAX`.
    peer_advertises(&b, A, i64::MAX);

    let fresh = a.lock().insert_clipboard("written afterwards", None, None).unwrap();
    let (fresh_origin, _) = a.lock().origin_and_text_for_test(fresh).unwrap();

    // No panic, no overflow, and the restart-at-zero actually delivers.
    let (dialler, _) = sync_bounded((&a, A), (&b, B));
    assert_eq!(
        text_of(&b, A, &fresh_origin).as_deref(),
        Some("written afterwards"),
        "an unreachable mark must make us offer in full, not starve the peer: {dialler:?}"
    );
    assert!(
        !dialler.truncated,
        "guard integrity: this history is far below the batch cap, so nothing here \
         should be reporting truncation: {dialler:?}"
    );
}

// ===========================================================================
// R13-X3. THE RESUME DEBT IS A SINGLE CLOCK APPLIED TO EVERY SOURCE, AND
//         `resend_all` IGNORES THE PEER'S CURSOR. THE VALUE IS LOAD-BEARING.
//
// Round 12 made an ORDINARY truncated pass bank a debt. That debt is written
// into the same `resend_owed` map `set_kinds` primes with 0 when the user turns
// a sync kind back on, and it is written AFTER the exchange finishes, from a
// value read BEFORE it started.
//
// What this pair establishes is the consequence, which is the part a test can
// pin: the debt's VALUE decides whether the rows a disabled kind hid below the
// peer's cursor are ever delivered. A debt of 0 reaches them. A debt of
// anything above them does not, on any later exchange, because nothing else
// will ever offer a row below the peer's mark.
// ===========================================================================

/// Build the state a disabled kind leaves behind: the clipboard row is below
/// the peer's cursor for us, and the peer does not have it. Returns the row's
/// origin and a clock strictly above it.
fn a_row_hidden_below_the_peers_cursor(
    a: &Arc<Mutex<Store>>,
    b: &Arc<Mutex<Store>>,
) -> (String, i64) {
    // Clipboard sync is OFF on our side, so `serve`'s kind filter drops the row
    // from the batch and the peer never sees it. The peer's mark is lifted OVER
    // it below, by a row stamped later that a re-offer does deliver, which is
    // exactly how a disabled kind strands a row: it ends up beneath a cursor
    // that nothing lowers.
    let hidden = a.lock().insert_clipboard("captured while the switch was off", None, None).unwrap();
    let (origin, _) = a.lock().origin_and_text_for_test(hidden).unwrap();
    let dictations_only = Kinds { dictations: true, clipboard: false };
    sync_bounded_with((a, A, dictations_only), (b, B, both()), None);
    assert!(
        text_of(b, A, &origin).is_none(),
        "premise: the row must NOT have reached the peer while the kind was off"
    );

    // Something above it, which is what a later truncated pass would report as
    // its resume point.
    let later = a.lock().insert_clipboard("a later row", None, None).unwrap();
    let later_origin = {
        let g = a.lock();
        g.origin_and_text_for_test(later).unwrap().0
    };
    let above = clock_of(a, A, &later_origin).unwrap();
    (origin, above)
}

#[test]
fn r13_data_a_reoffer_from_zero_reaches_a_row_a_disabled_kind_hid() {
    let a = store_for(A);
    let b = store_for(B);
    let (origin, _above) = a_row_hidden_below_the_peers_cursor(&a, &b);

    // The kind is back on and the debt is the 0 `set_kinds` banks.
    sync_bounded_with((&a, A, both()), (&b, B, both()), Some(0));
    assert_eq!(
        text_of(&b, A, &origin).as_deref(),
        Some("captured while the switch was off"),
        "a re-offer from zero is the only thing that can reach a row below the peer's mark"
    );
}

#[test]
fn r13_data_a_reoffer_from_a_higher_debt_never_reaches_that_row_again() {
    let a = store_for(A);
    let b = store_for(B);
    let (origin, above) = a_row_hidden_below_the_peers_cursor(&a, &b);

    // The same re-offer, resuming from a clock a truncated pass reported.
    // Three exchanges, then ordinary ones, because nothing else will offer it.
    for _ in 0..3 {
        sync_bounded_with((&a, A, both()), (&b, B, both()), Some(above));
    }
    sync_bounded((&a, A), (&b, B));
    sync_bounded((&a, A), (&b, B));
    assert!(
        text_of(&b, A, &origin).is_none(),
        "a debt above the hidden row loses it permanently; nothing offers a row below \
         the peer's own mark"
    );
    // And the row is still here, so the assertion above is about delivery and
    // not about the row having been pruned.
    assert!(
        clock_of(&a, A, &origin).is_some(),
        "guard integrity: the author still holds the row it never managed to send"
    );
}

// ===========================================================================
// R13-X4. WHAT A REFUSED ROW NOW COSTS, PINNED TO PRODUCTION.
//
// R13-X1 proves the correction is refused on every exchange while the clock
// error lasts. Round 12 also made a refusal drive two user-visible things:
// a per-device problem report, and the suppression of `last_sync_ok` when
// nothing else was applied. Both live on `SyncManager`, which needs a Tauri
// `AppHandle` to build, so this asserts the RULE against the source the way
// `adversarial_r12_data::r12_data_the_manager_banks_a_debt_on_any_truncated_pass`
// does, and R13-X1 supplies the input that makes it fire.
// ===========================================================================

#[test]
fn r13_data_a_refused_row_still_drives_the_report_and_the_stamp() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let code: String = std::fs::read_to_string(root.join("src/sync/manager.rs"))
        .expect("manager.rs is readable")
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("if stats.ignored > 0 {"),
        "premise: a refused row still raises a per-device problem"
    );
    // INVERTED. The wording no longer names the sending device. A refusal
    // means a timestamp is outside the accepted window, and after the sequence
    // in R13-X1 the sender is the machine whose clock has just been corrected,
    // so naming it sends the user to check the one that is now right. The
    // machine that mints an out-of-range stamp warns about itself instead, in
    // `edit_stamp`.
    assert!(
        !code.contains("Check that device's clock is set correctly."),
        "the refusal names the SENDING device's clock. An edit made after a backwards clock \
         step carries a stamp beyond the ceiling even once that clock is corrected, so this \
         sends the user to check the machine that is now right"
    );
    assert!(
        code.contains("Check the clocks on both devices."),
        "the refusal no longer tells the user where to look at all"
    );
    let core: String =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/echokey-core/src/history.rs"))
            .expect("history.rs is readable");
    assert!(
        core.contains("Check THIS machine's clock."),
        "the machine that mints a stamp beyond what any peer will accept says nothing about \
         it, so the only report comes from the peer and blames the wrong device"
    );
    assert!(
        code.contains(
            "if stats.applied_items > 0 || stats.applied_tombstones > 0 || stats.ignored == 0 {"
        ),
        "premise: a pass whose only outcome is a refusal records no successful sync"
    );
}

// ===========================================================================
// R13-X5. THE DEBT IS BANKED ON EVERY TRUNCATED PASS, WHICH IS A WRITE INTO
//         THE MAP `set_kinds` PRIMES.
//
// Pinning the two production rules that now share `resend_owed`, so the pair
// above is measuring the real thing. Round 11 wrote to this map only when the
// pass was already a re-offer; round 12 writes on any truncation, from a value
// read before the exchange started.
// ===========================================================================

#[test]
fn r13_data_an_ordinary_truncated_pass_writes_into_the_kind_widening_map() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let code: String = std::fs::read_to_string(root.join("src/sync/manager.rs"))
        .expect("manager.rs is readable")
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("if resend_all || stats.truncated {"),
        "premise: round 12's condition is still the one in production"
    );
    assert!(
        code.contains("i.resend_owed.insert(peer_id.clone(), from);"),
        "premise: and it writes the resume point into resend_owed"
    );
    assert!(
        code.contains("i.resend_owed.extend(owed);"),
        "premise: `set_kinds` primes the SAME map with 0 for every paired device, so the \
         two writes race over one key and the ordinary pass's value is the later one"
    );
    assert!(
        code.contains("i.resend_owed.get(&peer_id).copied(), i.resend_epoch.get(&peer_id)"),
        "premise: the value the exchange serves from is read BEFORE the exchange and \
         written back AFTER it, which is what makes the window a window"
    );
}
