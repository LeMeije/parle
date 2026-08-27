//! ADVERSARIAL REVIEW — ROUND 6. Demonstrations of live findings. NOT fixes.
//!
//! In its own file because `replicate.rs` is edited concurrently by other
//! reviewers and a whole-file rewrite has destroyed a fix once already.
//!
//! Every socket carries a read AND a write timeout, and every loop is hard
//! bounded, so a defect surfaces as a failed assertion or a timeout — never as
//! a hung suite.

#![cfg(test)]

use echokey_core::history::{RemoteItem, Store};
use echokey_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";
/// A device A once held rows from, and which never talks to B.
const C: &str = "33333333-3333-4333-8333-333333333333";

fn socket_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c = TcpStream::connect(addr).unwrap();
    let (srv, _) = l.accept().unwrap();
    // BOTH directions: a write/write stall parks both sides in `write` and a
    // read timeout alone would let the suite hang rather than fail.
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

/// The paired roster each side carries into an exchange.
///
/// This used to be `[A, B, C]` for BOTH sides in every test in this file, which
/// silently contradicted the scenarios written above them: R6-1 says in prose
/// that "B has never met C and never will", and then handed B a roster claiming
/// it had paired with C. Nothing noticed, because `Attribution::known` was dead
/// code at the time, no rule read it.
///
/// It is read now: whether a relayed delete for an identity we do not hold is a
/// TEMPORARY refusal (the row may still arrive from its author) or a PERMANENT
/// one (nothing will ever hand it to us) turns on exactly this. A roster that
/// claims a pairing the scenario denies tests the wrong branch.
///
/// A and C are paired with everyone; B has paired with A only.
fn roster_for(me: &str) -> Vec<String> {
    if me == B {
        vec![A.to_string(), B.to_string()]
    } else {
        vec![A.to_string(), B.to_string(), C.to_string()]
    }
}

/// One full exchange. `x` dials (`Turn::First`), `y` accepts. Returns
/// (x's stats, y's stats).
fn sync(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
) -> (RoundStats, RoundStats) {
    sync_with(x, y, Retention { oldest_allowed: None }, Retention { oldest_allowed: None })
}

/// The same, with a per-side retention window — the user's "keep history for N
/// days" setting, which each machine applies to what it will accept.
fn sync_with(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
    x_retention: Retention,
    y_retention: Retention,
) -> (RoundStats, RoundStats) {
    let (sock_x, sock_y) = socket_pair();
    let key = PairedKey::from_bytes([7u8; 32]);
    let k2 = key.clone();
    let (y_store, y_id, x_id) = (y.0.clone(), y.1, x.1);
    let yt = std::thread::spawn(move || {
        let mut s = Session::accept(sock_y, &k2).unwrap();
        let known = roster_for(y_id);
        let attr = Attribution { peer_id: x_id, local_id: y_id, known: &known };
        exchange(
            &mut s,
            &y_store,
            (y_id, "peer"),
            both(),
            y_retention,
            &attr,
            Turn::Second,
            false,
            0,
            &|| false,
        )
        .expect("accepting side")
    });
    let mut s = Session::initiate(sock_x, &key).unwrap();
    let known = roster_for(x.1);
    let attr = Attribution { peer_id: y.1, local_id: x.1, known: &known };
    let xs = exchange(
        &mut s,
        x.0,
        (x.1, "peer"),
        both(),
        x_retention,
        &attr,
        Turn::First,
        false,
        0,
        &|| false,
    )
    .expect("dialling side");
    let ys = yt.join().expect("accepting side must not panic");
    (xs, ys)
}

// ---------------------------------------------------------------------------
// R6-1. A tombstone for a source the peer can never learn about is re-sent on
// EVERY exchange, for ever, and refused every time.
//
// `serve` offers tombstones for every source it holds. `drain` accepts one
// only from its author, or from anyone when we already hold the identity — and
// a refusal for "we do not hold the identity" is TEMPORARY, so it banks no
// receipt. When the identity is one the receiving device can never acquire
// (because `serve` offers ITEMS only for `source == me`, so nobody but the
// author will ever hand it that row), the two rules close a loop with no exit:
// the sender never stops offering, the receiver never stops refusing, and
// nothing in the protocol can say "stop".
//
// The sequence is ordinary, not hostile: A syncs with C, the user clears
// history on A, C is retired or simply never pairs with B, and A then pairs
// with B. From that moment every A<->B exchange carries the same dead
// tombstones and books the same refusals.
// ---------------------------------------------------------------------------
#[test]
fn r6_a_tombstone_for_an_unreachable_source_never_stops_being_re_sent() {
    let a = store_for(A);
    let b = store_for(B);

    // A once held a row written by C, and the user deleted it on A.
    a.lock()
        .apply_remote_item(
            C,
            &RemoteItem {
                source_machine: C.into(),
                origin_id: "c-row-1".into(),
                kind: "clipboard".into(),
                text: "hunter2".into(),
                created_at: now_ms() - 5_000,
                updated_at: now_ms() - 5_000,
                pinned: false,
            },
        )
        .unwrap();
    assert_eq!(a.lock().count().unwrap(), 1, "precondition: A holds C's row");
    let id: i64 = a.lock().recent(None, 10).unwrap()[0].id;
    a.lock().delete_item_local(id).unwrap();
    assert_eq!(a.lock().tombstone_count(C).unwrap(), 1, "precondition: A holds C's tombstone");

    // B has never met C and never will. Ten exchanges, hard bounded.
    let mut per_round = Vec::new();
    for _ in 0..10 {
        let (a_stats, b_stats) = sync((&a, A), (&b, B));
        per_round.push((a_stats.sent_tombstones, b_stats.refused));
    }

    // CONVERGENCE, not silence.
    //
    // This asserted `(0, 0)` summed over all ten rounds, which the comment
    // directly above it contradicted ("quiet would be zero of each AFTER the
    // first round or two"). Offering the tombstone once is not the defect and
    // cannot be removed: A cannot know what B holds without telling it, so the
    // first offer is how B finds out. Demanding zero would have been satisfied
    // only by never relaying a delete at all, which loses deletes, the failure
    // this feature exists to avoid.
    //
    // What criterion C actually requires is that the exchange goes quiet. So:
    // the first round may carry the tombstone, and every round after it must
    // carry nothing and refuse nothing.
    assert!(per_round[0].0 >= 1, "precondition: A offers the tombstone at least once");
    let tail: Vec<_> = per_round[1..].to_vec();
    assert!(
        tail.iter().all(|&(carried, refused)| carried == 0 && refused == 0),
        "the same dead tombstone is re-sent and refused on every exchange for ever; \
         (carried, refused) per round was {per_round:?}"
    );
}

// ---------------------------------------------------------------------------
// R6-1b. The same loop reached through Clear History rather than a single
// delete, and with more than one tombstone, so the cost scales with how much
// the user cleared. `MAX_TOMBSTONES_PER_SOURCE` is 10,000, so a retired device
// whose rows were cleared can put 10,000 dead tombstones on every exchange for
// the life of the pairing.
// ---------------------------------------------------------------------------
#[test]
fn r6_the_dead_tombstone_loop_is_not_an_artefact_of_clear_history() {
    let a = store_for(A);
    let b = store_for(B);

    // Two of C's rows on A; clear history rather than a single delete.
    for i in 0..2 {
        a.lock()
            .apply_remote_item(
                C,
                &RemoteItem {
                    source_machine: C.into(),
                    origin_id: format!("c-row-{i}"),
                    kind: "transcription".into(),
                    text: format!("secret {i}"),
                    created_at: now_ms() - 5_000,
                    updated_at: now_ms() - 5_000,
                    pinned: false,
                },
            )
            .unwrap();
    }
    a.lock().clear(None).unwrap();
    assert_eq!(a.lock().tombstone_count(C).unwrap(), 2);

    let mut last = 0usize;
    for _ in 0..6 {
        let (a_stats, _) = sync((&a, A), (&b, B));
        last = a_stats.sent_tombstones;
    }
    assert_eq!(last, 0, "round 6 still carries {last} tombstones B can never accept");
}

// ---------------------------------------------------------------------------
// R6-2. A local pin on a replicated row silently swallows the AUTHOR's own
// later-delivered edit, permanently.
//
// `set_pinned` on a peer's row is documented as a LOCAL change that does not
// travel. What is not documented, and is not intended by anything in the file,
// is that it also raises that row's `updated_at` — the single last-writer-wins
// clock the AUTHOR's content changes are judged against. So a pin made after
// the author's edit but before the next exchange makes the receiving device
// permanently deaf to that edit: the row loses on LWW, the receipt is banked
// anyway, and `serve` never offers it again.
//
// Ordinary sequence, no hostility, no clock skew, no third device:
//   A dictates -> syncs to B -> A corrects the text -> B pins the row it has
//   -> next sync. B keeps the uncorrected text for ever.
// ---------------------------------------------------------------------------
#[test]
fn r6_a_local_pin_on_the_peer_permanently_swallows_the_authors_correction() {
    let a = store_for(A);
    let b = store_for(B);

    a.lock().insert_clipboard("teh wrong text", None, None).unwrap();
    sync((&a, A), (&b, B));
    assert_eq!(b.lock().count().unwrap(), 1, "precondition: B has the row");

    // The author corrects its own row. Nothing has synced yet.
    let a_id: i64 = a.lock().recent(None, 10).unwrap()[0].id;
    a.lock().update_text(a_id, "the right text").unwrap();

    // Meanwhile the user pins the copy on B. Local-only by design — but it
    // moves the same clock the author's correction will be judged against.
    std::thread::sleep(Duration::from_millis(5));
    let b_id: i64 = b.lock().recent(None, 10).unwrap()[0].id;
    b.lock().set_pinned(b_id, true).unwrap();

    // Sync as often as you like: bounded at 5 rounds.
    for _ in 0..5 {
        sync((&a, A), (&b, B));
    }

    let a_text = a.lock().recent(None, 10).unwrap()[0].text.clone();
    let b_text = b.lock().recent(None, 10).unwrap()[0].text.clone();
    assert_eq!(
        (a_text.as_str(), b_text.as_str()),
        ("the right text", "the right text"),
        "the author's correction never reaches B; the two machines disagree for ever"
    );
}

// ---------------------------------------------------------------------------
// R6-3. Three devices, a bounded random walk, checking for divergence.
//
// A, B and C are fully paired. Each step applies one ordinary operation on one
// device, then syncs one pair. After the walk every pair is synced to a fixed
// point (hard bounded) and all three stores must agree on the set of live
// (source, origin) identities.
// ---------------------------------------------------------------------------
#[test]
fn r6_three_devices_converge_under_a_bounded_random_walk() {
    // A tiny deterministic PRNG so a failure is reproducible.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn pick(&mut self, n: usize) -> usize {
            (self.next() as usize) % n
        }
    }

    for seed in 0..8u64 {
        let a = store_for(A);
        let b = store_for(B);
        let c = store_for(C);
        let stores: [(&Arc<Mutex<Store>>, &'static str); 3] = [(&a, A), (&b, B), (&c, C)];
        let mut rng = Rng(0x9E3779B97F4A7C15 ^ seed);

        for step in 0..40 {
            let who = rng.pick(3);
            let (store, _) = stores[who];
            match rng.pick(4) {
                0 => {
                    store
                        .lock()
                        .insert_clipboard(&format!("s{seed}-step{step}"), None, None)
                        .unwrap();
                }
                1 => {
                    // Delete a row (any row, ours or replicated).
                    let rows = store.lock().recent(None, 50).unwrap();
                    if !rows.is_empty() {
                        let id = rows[rng.pick(rows.len())].id;
                        store.lock().delete_item_local(id).unwrap();
                    }
                }
                2 => {
                    // Edit a row we authored, which is the only edit that
                    // travels.
                    let rows = store.lock().recent(None, 50).unwrap();
                    if !rows.is_empty() {
                        let id = rows[rng.pick(rows.len())].id;
                        let _ = store.lock().update_text(id, &format!("edit-{seed}-{step}"));
                    }
                }
                _ => {}
            }
            // Sync one pair.
            let i = rng.pick(3);
            let j = (i + 1 + rng.pick(2)) % 3;
            sync(stores[i], stores[j]);
        }

        // Drive to a fixed point. Hard bounded.
        for _ in 0..12 {
            sync((&a, A), (&b, B));
            sync((&b, B), (&c, C));
            sync((&a, A), (&c, C));
        }

        let ids = |s: &Arc<Mutex<Store>>| -> Vec<(String, String)> {
            let g = s.lock();
            let mut out = Vec::new();
            for src in g.known_sources().unwrap() {
                for r in g.items_from(&src, 0, "", 10_000).unwrap() {
                    out.push((r.source_machine.clone(), r.origin_id.clone()));
                }
            }
            out.sort();
            out
        };
        let (ia, ib, ic) = (ids(&a), ids(&b), ids(&c));
        assert_eq!(ia, ib, "seed {seed}: A and B hold different rows");
        assert_eq!(ib, ic, "seed {seed}: B and C hold different rows");
    }
}

// ---------------------------------------------------------------------------
// R6-4. Widening the retention window leaves a permanent, silent hole.
//
// `drain` banks a receipt BEFORE the retention check, and the comment there
// justifies it with an invariant: "retention only ever gets truer". It does
// not. `retention_days` is a user setting (`SyncManager::set_retention_days`,
// manager.rs:1196) and the user may enlarge it, or set it to 0 = keep for
// ever. Nothing calls `reset_source_marks` when they do — that repair exists
// only for the kind toggles (`set_kinds`, manager.rs:1223).
//
// So: while "keep 7 days" was set, the peer offered rows from last month; we
// refused them and banked a cursor past them. The user then asks to keep
// history for ever. Those rows are now strictly below our cursor, the peer
// will never offer them again, and the two machines disagree permanently.
// ---------------------------------------------------------------------------
#[test]
fn r6_widening_retention_never_refetches_what_the_narrow_window_refused() {
    let a = store_for(A);
    let b = store_for(B);

    // B authored a row a month ago.
    let month_ago = now_ms() - 30 * 86_400_000;
    b.lock()
        .apply_remote_item(
            B,
            &RemoteItem {
                source_machine: B.into(),
                origin_id: "old-row".into(),
                kind: "clipboard".into(),
                text: "last month's note".into(),
                created_at: month_ago,
                updated_at: month_ago,
                pinned: false,
            },
        )
        .unwrap();
    // And something recent, so the exchange is not vacuous.
    b.lock().insert_clipboard("today's note", None, None).unwrap();

    // A keeps 7 days. The old row is refused — correctly — and a receipt for
    // it is banked all the same.
    let week = Retention { oldest_allowed: Some(now_ms() - 7 * 86_400_000) };
    sync_with((&a, A), (&b, B), week, Retention { oldest_allowed: None });
    assert_eq!(a.lock().count().unwrap(), 1, "precondition: only the recent row landed");

    // Without the repair, the hole is permanent: the receipt banked for the
    // refused row sits above it, so the peer never offers it again.
    for _ in 0..3 {
        sync((&a, A), (&b, B));
    }
    assert_eq!(
        a.lock().count().unwrap(),
        1,
        "precondition: nothing brings the row back on its own, that IS the defect"
    );

    // The user now sets retention to "keep for ever". Two things have to be
    // true, and they are asserted separately because they failed separately:

    // 1. The manager must RECOGNISE that as a widening. `0` means keep for
    //    ever, so it is the widest window and not the narrowest, the
    //    comparison every naive version gets backwards. This is the production
    //    rule, not a copy of it; `SyncManager` itself cannot be built under
    //    MockRuntime, which is why the decision is a free function.
    assert!(
        crate::sync::manager::retention_widened(7, 0),
        "'keep for ever' must read as a widening of a 7-day window"
    );
    assert!(crate::sync::manager::retention_widened(7, 30));
    assert!(!crate::sync::manager::retention_widened(30, 7));
    assert!(!crate::sync::manager::retention_widened(0, 7));
    assert!(!crate::sync::manager::retention_widened(7, 7));

    // 2. And the repair it performs must actually refill the hole. Only the
    //    INBOUND half is needed: `serve` never filtered on retention, so
    //    nothing was suppressed outbound and no re-offer debt is owed.
    a.lock().reset_source_marks().unwrap();
    for _ in 0..5 {
        sync((&a, A), (&b, B));
    }
    assert_eq!(
        a.lock().count().unwrap(),
        2,
        "the row refused under the old, narrower window is unreachable for ever"
    );
}

// ---------------------------------------------------------------------------
// R6-5. Clear History over a large replicated history loses part of the delete
// on the wire, and the peer keeps the cleared rows for ever.
//
// `cap_tombstones` (history.rs:1029) runs ONLY from `apply_remote_tombstone`,
// so a local Clear writes one tombstone per row with no ceiling. The first
// tombstone that then arrives from a peer trims the table back to
// MAX_TOMBSTONES_PER_SOURCE by dropping the OLDEST — and right after a Clear
// the oldest are precisely the deletes we have not delivered yet.
//
// `Turn::Second` drains before it serves, so on any exchange where this device
// ACCEPTED the connection the eviction happens before the serve. Those deletes
// are never offered, and never will be: nothing re-creates a tombstone.
//
// Bounded: one exchange, and the store work is O(rows).
// ---------------------------------------------------------------------------
// UN-QUARANTINED. It passed, five runs, ~4.4s each, alone and in the full
// suite.
//
// Stated plainly because it matters for anyone reading this later: I did NOT
// find the cause of the original hang, and I am not claiming one. What I know
// is that it blocked at 0% CPU on the pre-round-6 build, blocked on I/O, not
// slow, and that it now completes deterministically. Three things changed
// underneath it in the meantime, any of which could be responsible: the
// tombstone cap no longer evicts local deletes, `clear` no longer stamps every
// tombstone from the row's own clock, and relayed tombstones for an unreachable
// source now bank a receipt instead of being re-offered on every exchange.
//
// The durable protection is not this test, which is bounded only by its socket
// timeouts. It is `adversarial_r7_scale`, which re-derives the same scenario
// with a wall-clock budget on the exchange and both sides on their own thread,
// so a stall fails with a message naming the side that never returned instead
// of parking the suite. If this one ever hangs again, that is where to look.
#[test]
fn r6_clear_history_loses_deletes_when_a_peer_tombstone_arrives_first() {
    let over = echokey_core::history::MAX_TOMBSTONES_PER_SOURCE as usize + 50;
    let a = store_for(A);
    let b = store_for(B);
    let base = now_ms() - 10_000_000;

    // B authored a large history; A holds all of it.
    for i in 0..over {
        let row = RemoteItem {
            source_machine: B.into(),
            origin_id: format!("row-{i:06}"),
            kind: "clipboard".into(),
            text: format!("secret {i}"),
            created_at: base + i as i64,
            updated_at: base + i as i64,
            pinned: false,
        };
        b.lock().apply_remote_item(B, &row).unwrap();
        a.lock().apply_remote_item(B, &row).unwrap();
    }
    assert_eq!(a.lock().count().unwrap(), over as i64);
    assert_eq!(b.lock().count().unwrap(), over as i64);

    // The user clears history on A.
    a.lock().clear(None).unwrap();
    assert_eq!(a.lock().count().unwrap(), 0);
    assert_eq!(a.lock().tombstone_count(B).unwrap(), over as i64);

    // Meanwhile B deletes one of its own rows — an entirely ordinary thing.
    let b_id: i64 = b.lock().recent(None, 1).unwrap()[0].id;
    b.lock().delete_item_local(b_id).unwrap();

    // One exchange, with B dialling: A accepts, so A drains (and evicts)
    // before it serves.
    sync((&b, B), (&a, A));

    assert_eq!(
        b.lock().count().unwrap(),
        0,
        "the user cleared history and B kept {} of the cleared rows; \
         nothing will ever offer those deletes again",
        b.lock().count().unwrap()
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}
