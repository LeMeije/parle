//! ADVERSARIAL REVIEW — round 6, security pass (`r6_sec`).
//!
//! Demonstrations of live findings and of properties that HOLD. Not fixes.
//! In its own file because `replicate.rs`, `manager.rs` and `guard.rs` are
//! being edited concurrently by other reviewers.
//!
//! House rules for everything in here:
//!   * every socket carries a read AND a write timeout,
//!   * every loop has a hard bound,
//! so a defect surfaces as a failed assertion, never as a hung suite.

#![cfg(test)]

use echokey_core::history::{RemoteItem, RemoteTombstone, Store};
use echokey_sync::{
    DeviceId, ItemKind, PairedKey, Session, SyncItem, SyncMessage, Tombstone, Watermark,
    PROTOCOL_VERSION,
};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111"; // us
const B: &str = "22222222-2222-4222-8222-222222222222"; // the paired peer
const C: &str = "33333333-3333-4333-8333-333333333333"; // a third device

fn socket_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c = TcpStream::connect(addr).unwrap();
    let (srv, _) = l.accept().unwrap();
    // BOTH directions: a protocol desync parks both sides in write(), so a
    // read timeout alone would hang the suite instead of failing it.
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Put a row into `store` as if `source` had written it.
fn plant(store: &Arc<Mutex<Store>>, from_peer: &str, source: &str, origin: &str, kind: &str, text: &str, clock: i64) {
    store
        .lock()
        .apply_remote_item(
            from_peer,
            &RemoteItem {
                source_machine: source.into(),
                origin_id: origin.into(),
                kind: kind.into(),
                text: text.into(),
                created_at: clock,
                updated_at: clock,
                pinned: false,
            },
        )
        .unwrap();
}

/// The stored text of one replication identity, or None if we do not hold it.
///
/// Goes through the public replication read path rather than raw SQL, so it
/// sees exactly what a peer would be offered.
fn text_of(store: &Arc<Mutex<Store>>, source: &str, origin: &str) -> Option<String> {
    let rows = store.lock().items_since(source, 0, 10_000).ok()?;
    rows.into_iter().find(|r| r.origin_id == origin).map(|r| r.text)
}

/// The scripted-peer harness.
///
/// Runs the REAL `exchange` on our side as `Turn::Second` (the acceptor, which
/// drains before it serves) while `script` drives a hand-written peer over the
/// same Noise session. The script speaks the protocol itself, so it can say
/// things our own `serve` would never say.
fn against_scripted_peer<F>(
    ours: &Arc<Mutex<Store>>,
    kinds: Kinds,
    retention: Retention,
    script: F,
) -> RoundStats
where
    F: FnOnce(&mut Session<TcpStream>) + Send + 'static,
{
    let (c, srv) = socket_pair();
    let key = PairedKey::from_bytes([0x5au8; 32]);
    let k2 = key.clone();

    let peer = std::thread::spawn(move || {
        let mut s = Session::initiate(c, &k2).unwrap();
        s.send(&SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::parse(B).unwrap(),
            device_name: "peer".into(),
        })
        .unwrap();
        let _ = s.recv().unwrap(); // our Hello
        // We are Turn::Second on the app side, so it reads watermarks first.
        s.send(&SyncMessage::Watermarks { entries: Vec::<Watermark>::new(), more: false })
            .unwrap();
        let _ = s.recv().unwrap(); // our watermarks
        script(&mut s);
        // End of the peer's serve.
        s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
        // Absorb whatever we serve back, hard bounded.
        for _ in 0..4_000 {
            match s.recv() {
                Ok(SyncMessage::Items { items, more }) if items.is_empty() && !more => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });

    let mut session = Session::accept(srv, &key).unwrap();
    let known = vec![A.to_string(), B.to_string(), C.to_string()];
    let attr = Attribution { peer_id: B, local_id: A, known: &known };
    let stats = exchange(
        &mut session,
        ours,
        (A, "Deck A"),
        kinds,
        retention,
        &attr,
        Turn::Second,
        false,
        0,
        &|| false,
    )
    .expect("the exchange must complete");
    peer.join().expect("the scripted peer must not panic");
    stats
}

fn wire_item(source: &str, origin: &str, kind: ItemKind, text: &str, clock: i64) -> SyncItem {
    SyncItem {
        source_device: DeviceId::parse(source).unwrap(),
        origin_id: origin.into(),
        kind,
        text: text.into(),
        created_at: clock,
        updated_at: clock,
        pinned: false,
        clock: clock.max(0) as u64,
    }
}

fn wire_tomb(source: &str, origin: &str, clock: i64) -> Tombstone {
    Tombstone {
        source_device: DeviceId::parse(source).unwrap(),
        origin_id: origin.into(),
        deleted_at: clock,
        clock: clock.max(0) as u64,
    }
}

// ===========================================================================
// R6S-1. CONTENT AUTHORITY. A paired peer must not be able to change the
// content of a row it did not author, by ANY route on the wire.
// ===========================================================================
#[test]
fn r6sec_a_paired_peer_cannot_rewrite_our_rows_or_a_third_devices() {
    let ours = store_for(A);
    let t = now_ms() - 100_000;
    // A row we wrote, and a row a third device wrote that we hold.
    plant(&ours, A, A, "ours-1", "transcription", "our secret", t);
    plant(&ours, C, C, "cee-1", "transcription", "cee original", t);

    let later = now_ms(); // strictly newer, so last-writer-wins would take it
    let stats = against_scripted_peer(&ours, both(), Retention { oldest_allowed: None }, move |s| {
        // Every content-bearing route a peer has.
        s.send(&SyncMessage::Items {
            items: vec![
                // 1. claim to BE us and rewrite our row
                wire_item(A, "ours-1", ItemKind::Transcription, "PWNED-us", later),
                // 2. rewrite a third device's row we hold
                wire_item(C, "cee-1", ItemKind::Transcription, "PWNED-cee", later),
                // 3. invent a brand new row attributed to us
                wire_item(A, "forged-new", ItemKind::Transcription, "PWNED-new", later),
                // 4. invent a brand new row attributed to the third device
                wire_item(C, "forged-cee", ItemKind::Transcription, "PWNED-cee-new", later),
            ],
            more: false,
        })
        .unwrap();
    });

    assert_eq!(
        text_of(&ours, A, "ours-1").as_deref(),
        Some("our secret"),
        "a paired peer rewrote a row WE authored"
    );
    assert_eq!(
        text_of(&ours, C, "cee-1").as_deref(),
        Some("cee original"),
        "a paired peer rewrote a THIRD device's row"
    );
    assert!(text_of(&ours, A, "forged-new").is_none(), "a peer forged a row as us");
    assert!(text_of(&ours, C, "forged-cee").is_none(), "a peer forged a row as a third device");
    assert_eq!(stats.applied_items, 0, "nothing in that batch should have landed");
    assert_eq!(stats.refused, 4, "all four routes must be refused, not silently ignored");
}

// ===========================================================================
// R6S-2. A paired peer must not be able to move a cursor that does not
// belong to it — including the one we keep for a third device.
// ===========================================================================
#[test]
fn r6sec_a_paired_peer_cannot_move_a_third_devices_cursor() {
    let ours = store_for(A);
    // A cursor we hold for the third device, from a real exchange with it.
    ours.lock().note_received(C, C, now_ms() - 50_000).unwrap();
    let before: Vec<(String, i64)> = ours.lock().watermarks(C).unwrap();

    let far = now_ms() + 100_000; // inside MAX_CLOCK_SKEW_MS
    against_scripted_peer(&ours, both(), Retention { oldest_allowed: None }, move |s| {
        s.send(&SyncMessage::Items {
            items: vec![wire_item(C, "cee-future", ItemKind::Transcription, "x", far)],
            more: false,
        })
        .unwrap();
    });

    let after: Vec<(String, i64)> = ours.lock().watermarks(C).unwrap();
    assert_eq!(before, after, "peer B moved the cursor we keep for device C");
}

// ===========================================================================
// R6S-3. The kind gate. A peer must not be able to touch a kind the user
// excluded — the author exemption on tombstones is the interesting edge.
// ===========================================================================
#[test]
fn r6sec_an_excluded_kind_cannot_be_touched_by_a_relayed_change() {
    let dictations_only = Kinds { dictations: true, clipboard: false };
    let ours = store_for(A);
    let t = now_ms() - 100_000;
    // Clipboard rows this machine holds while clipboard sync is OFF.
    plant(&ours, A, A, "clip-ours", "clipboard", "our password", t);
    plant(&ours, C, C, "clip-cee", "clipboard", "cee password", t);

    let later = now_ms();
    against_scripted_peer(&ours, dictations_only, Retention { oldest_allowed: None }, move |s| {
        // Relayed DELETES of an excluded kind, on rows we hold.
        s.send(&SyncMessage::Tombstones {
            entries: vec![wire_tomb(A, "clip-ours", later), wire_tomb(C, "clip-cee", later)],
            more: false,
        })
        .unwrap();
        // And a relabelling attempt: call the clipboard row a transcription.
        s.send(&SyncMessage::Items {
            items: vec![
                wire_item(A, "clip-ours", ItemKind::Transcription, "PWNED", later),
                wire_item(C, "clip-cee", ItemKind::Transcription, "PWNED", later),
            ],
            more: false,
        })
        .unwrap();
    });

    assert_eq!(
        text_of(&ours, A, "clip-ours").as_deref(),
        Some("our password"),
        "a relayed change reached a clipboard row while clipboard sync was off"
    );
    assert_eq!(
        text_of(&ours, C, "clip-cee").as_deref(),
        Some("cee password"),
        "a relayed change reached a third device's clipboard row while clipboard sync was off"
    );
}

// ===========================================================================
// R6S-4. THE SELF-WATERMARK.
//
// TRIAGE: the finding is real, the remedy it named was not.
//
// The reviewer's claim was structural, `docs/SYNC_DESIGN.md` says "A device
// does not advertise a mark for itself at all", and a peer's ordinary delete of
// one of our rows makes us advertise exactly that. Both halves are true. But
// that sentence describes a design in which `serve` offered ITEMS for every
// source it held, and it does not any more: since content became
// author-only, a peer never sends us items attributed to our own device.
// A `(peer, our own id)` mark therefore gates ONE stream, the deletes that
// peer relays back to us about our own rows, which is precisely the stream a
// receipt should gate.
//
// Removing the mark, as the reviewer proposed, would make that stream ungated:
// every peer would re-offer every tombstone it holds for our source on every
// exchange for the life of the pairing, with nothing able to say "stop". That
// is a criterion C failure traded for a doc sentence.
//
// What was genuinely broken was the ORDERING the mark relies on. A cursor is a
// promise never to ask below a clock again, which is only sound if the stream
// is created in clock order. Tombstones were stamped `max(now, row.updated_at)`,
// so deleting a row from a peer inside the accepted skew produced a tombstone
// up to two minutes in the FUTURE; the mark went there; and the next delete,
// stamped normally, fell below it and was never offered again. A delete lost by
// nothing more exotic than deleting two synced rows in a row.
//
// `Store::delete_clock` fixes that at the source: a local delete is stamped
// strictly above every tombstone we already hold for that source, bounded so it
// can never exceed what a peer will accept. The tests below assert the
// behaviour that actually matters, no delete of our own rows can be hidden by
// the mark, rather than the absence of a table row.
//
// `docs/SYNC_DESIGN.md` has been corrected to match.
// ===========================================================================
#[test]
fn r6sec_a_relayed_delete_of_our_own_row_is_never_hidden_by_the_mark_it_sets() {
    let ours = store_for(A);
    // A row of ours that came back from a peer whose clock runs fast, well
    // inside the accepted skew. This is what used to poison the mark.
    let fast = now_ms() + 90_000;
    plant(&ours, A, A, "ours-fast", "transcription", "recorded here", fast);
    plant(&ours, A, A, "ours-normal", "transcription", "also recorded here", now_ms() - 1_000);

    // The peer deletes BOTH, in the order that used to lose the second one:
    // the future-stamped row first.
    let peer = store_for(B);
    for (origin, clock) in [("ours-fast", fast), ("ours-normal", now_ms() - 1_000)] {
        plant(&peer, A, A, origin, "transcription", "copy", clock);
    }
    let first = {
        let g = peer.lock();
        let id = g
            .conn_for_test()
            .query_row(
                "SELECT id FROM items WHERE origin_id='ours-fast'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        g.delete_item_local(id).unwrap();
        g.conn_for_test()
            .query_row(
                "SELECT deleted_at FROM tombstones WHERE origin_id='ours-fast'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    };
    // Deliver that one, which is what sets our mark for our own source.
    let t1 = first;
    against_scripted_peer(&ours, both(), Retention { oldest_allowed: None }, move |s| {
        s.send(&SyncMessage::Tombstones { entries: vec![wire_tomb(A, "ours-fast", t1)], more: false })
            .unwrap();
    });
    assert!(text_of(&ours, A, "ours-fast").is_none(), "precondition: the first delete landed");

    // The SECOND delete, made afterwards on the peer, must carry a clock the
    // mark cannot hide. That is `delete_clock`'s whole job.
    let second = {
        let g = peer.lock();
        let id = g
            .conn_for_test()
            .query_row("SELECT id FROM items WHERE origin_id='ours-normal'", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        g.delete_item_local(id).unwrap();
        g.conn_for_test()
            .query_row(
                "SELECT deleted_at FROM tombstones WHERE origin_id='ours-normal'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    };
    let mark = ours
        .lock()
        .watermarks(B)
        .unwrap()
        .into_iter()
        .find(|(src, _)| src == A)
        .map(|(_, c)| c)
        .unwrap_or(0);
    assert!(
        second > mark,
        "the second delete is stamped {second}, at or below the mark {mark} the first one set, \
         so the peer will never offer it: a delete lost by deleting two synced rows in a row"
    );

    // And it does in fact land.
    let t2 = second;
    against_scripted_peer(&ours, both(), Retention { oldest_allowed: None }, move |s| {
        s.send(&SyncMessage::Tombstones {
            entries: vec![wire_tomb(A, "ours-normal", t2)],
            more: false,
        })
        .unwrap();
    });
    assert!(
        text_of(&ours, A, "ours-normal").is_none(),
        "the second delete of one of our own rows never landed"
    );
}

/// The mark a relayed delete sets is a RECEIPT, bounded by what actually
/// arrived, and not an invitation to park it in the future.
#[test]
fn r6sec_a_peer_cannot_park_the_mark_it_sets_for_our_own_source() {
    let ours = store_for(A);
    plant(&ours, A, A, "ours-1", "transcription", "our row", now_ms() - 100_000);

    // Far beyond the skew window: refused outright, and nothing banked.
    let absurd = i64::MAX;
    against_scripted_peer(&ours, both(), Retention { oldest_allowed: None }, move |s| {
        s.send(&SyncMessage::Tombstones { entries: vec![wire_tomb(A, "ours-1", absurd)], more: false })
            .unwrap();
    });
    let mark = ours
        .lock()
        .watermarks(B)
        .unwrap()
        .into_iter()
        .find(|(src, _)| src == A)
        .map(|(_, c)| c);
    assert!(
        mark.map(|c| c < now_ms() + 200_000).unwrap_or(true),
        "a peer parked the mark we keep for our own source at {mark:?}; \
         everything it later relays about our rows falls below it"
    );
    assert!(
        text_of(&ours, A, "ours-1").is_some(),
        "an out-of-range tombstone must be refused, not applied"
    );
}

// ===========================================================================
// R6S-5. THE REFUSED TOMBSTONE. Round 5 banks no receipt for a tombstone
// naming an identity we do not hold, on purpose. Verify that cannot become an
// endless re-offer.
//
// Both sides run the REAL `exchange`, twice, over real sockets. A converged
// pair sends nothing the second time.
// ===========================================================================
fn run_real_pair(a_store: &Arc<Mutex<Store>>, b_store: &Arc<Mutex<Store>>) -> (RoundStats, RoundStats) {
    let (c, srv) = socket_pair();
    let key = PairedKey::from_bytes([0x11u8; 32]);
    let k2 = key.clone();
    let b_store2 = b_store.clone();

    let b = std::thread::spawn(move || {
        let mut session = Session::accept(srv, &k2).unwrap();
        // B has paired with A only. It has never met C.
        let known = vec![A.to_string(), B.to_string()];
        let attr = Attribution { peer_id: A, local_id: B, known: &known };
        exchange(
            &mut session,
            &b_store2,
            (B, "Deck B"),
            both(),
            Retention { oldest_allowed: None },
            &attr,
            Turn::Second,
            false,
            0,
            &|| false,
        )
    });

    let mut session = Session::initiate(c, &key).unwrap();
    let known = vec![A.to_string(), B.to_string(), C.to_string()];
    let attr = Attribution { peer_id: B, local_id: A, known: &known };
    let a_stats = exchange(
        &mut session,
        a_store,
        (A, "Deck A"),
        both(),
        Retention { oldest_allowed: None },
        &attr,
        Turn::First,
        false,
        0,
        &|| false,
    );
    let b_stats = b.join().expect("the accepting side must not panic");
    (a_stats.expect("dialling side"), b_stats.expect("accepting side"))
}

#[test]
fn r6sec_tombstones_a_peer_can_never_accept_are_not_re_offered_forever() {
    let a_store = store_for(A);
    let b_store = store_for(B);

    // The ordinary hub topology: A (a desktop) has paired with B (a laptop)
    // and C (a second machine); B and C have never paired with each other.
    // C's rows sync to A, the user deletes some of them ON A, and A therefore
    // holds tombstones whose source is C. B can never hold those identities —
    // nothing will ever hand it C's rows, because a device serves only rows it
    // authored — so it refuses every one of them, and a refusal banks no
    // receipt.
    let base = now_ms() - 5_000_000;
    {
        let g = a_store.lock();
        for i in 0..300 {
            g.apply_remote_item(
                C,
                &RemoteItem {
                    source_machine: C.into(),
                    origin_id: format!("cee-{i:05}"),
                    kind: "transcription".into(),
                    text: "from the other machine".into(),
                    created_at: base + i as i64,
                    updated_at: base + i as i64,
                    pinned: false,
                },
            )
            .unwrap();
        }
        // The user clears history on A. Every C row becomes a C tombstone.
        let n = g.clear(None).unwrap();
        assert_eq!(n, 300, "precondition: the clear removed C's rows");
        assert_eq!(g.tombstone_count(C).unwrap(), 300);
    }

    let (first, _) = run_real_pair(&a_store, &b_store);
    assert!(first.sent_tombstones >= 300, "precondition: A offers them once");

    // Converged? Nothing should move on a second pass.
    let (second, _) = run_real_pair(&a_store, &b_store);
    assert_eq!(
        second.sent_tombstones, 0,
        "A re-offered {} tombstones the peer refused and can never accept; \
         a refusal banks no receipt, so this repeats on EVERY exchange for the \
         life of the pairing and consumes the shared message budget",
        second.sent_tombstones
    );
}

// ===========================================================================
// R6S-6. Hostile numeric input must not panic or overflow. Debug builds trap
// on overflow, so this is a real check rather than a formality.
// ===========================================================================
#[test]
fn r6sec_extreme_clocks_and_payloads_do_not_panic() {
    let ours = store_for(A);
    plant(&ours, B, B, "bee-1", "transcription", "bee row", now_ms() - 10_000);

    let stats = against_scripted_peer(&ours, both(), Retention { oldest_allowed: Some(0) }, |s| {
        // Every extreme an i64/u64 on the wire can carry.
        let hostile: Vec<SyncItem> = vec![
            wire_item(B, "x-max", ItemKind::Transcription, "x", i64::MAX),
            wire_item(B, "x-min", ItemKind::Clipboard, "x", i64::MIN),
            wire_item(B, "x-zero", ItemKind::Transcription, "", 0),
            wire_item(B, "x-neg", ItemKind::Transcription, "\u{0}\u{FFFF}", -1),
        ];
        s.send(&SyncMessage::Items { items: hostile, more: true }).unwrap();
        // A clock field that is nonsense next to updated_at.
        let mut skewed = wire_item(B, "x-skew", ItemKind::Transcription, "x", now_ms() - 1_000);
        skewed.clock = u64::MAX;
        s.send(&SyncMessage::Items { items: vec![skewed], more: true }).unwrap();
        s.send(&SyncMessage::Tombstones {
            entries: vec![
                wire_tomb(B, "bee-1", i64::MAX),
                wire_tomb(B, "t-min", i64::MIN),
                wire_tomb(B, "t-zero", 0),
            ],
            more: false,
        })
        .unwrap();
    });

    // The far-future row must be refused, not stored, and must not have parked
    // the cursor at the ceiling.
    assert!(text_of(&ours, B, "x-max").is_none(), "a row stamped i64::MAX was stored");
    assert_eq!(
        text_of(&ours, B, "bee-1").as_deref(),
        Some("bee row"),
        "a tombstone stamped i64::MAX deleted a live row"
    );
    let marks = ours.lock().watermarks(B).unwrap();
    assert!(
        marks.iter().all(|(_, c)| *c <= now_ms() + 5 * 60 * 1000),
        "a hostile clock parked the cursor in the future: {marks:?}"
    );
    let _ = stats;
}

// ===========================================================================
// R6S-7. A watermark clock is a u64 on the wire and an i64 in the store. The
// cast must not hand a peer a negative floor or a way to make us serve from a
// nonsense cursor.
// ===========================================================================
#[test]
fn r6sec_a_hostile_watermark_clock_cannot_produce_a_nonsense_floor() {
    let ours = store_for(A);
    let base = now_ms() - 200_000;
    for i in 0..5 {
        ours.lock()
            .insert_clipboard(&format!("row {i}"), None, None)
            .unwrap();
    }
    let _ = base;

    let (c, srv) = socket_pair();
    let key = PairedKey::from_bytes([0x33u8; 32]);
    let k2 = key.clone();

    let peer = std::thread::spawn(move || {
        let mut s = Session::initiate(c, &k2).unwrap();
        s.send(&SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::parse(B).unwrap(),
            device_name: "peer".into(),
        })
        .unwrap();
        let _ = s.recv().unwrap();
        // Clocks that are not representable as a positive i64.
        s.send(&SyncMessage::Watermarks {
            entries: vec![
                Watermark { source_device: DeviceId::parse(A).unwrap(), clock: u64::MAX },
                Watermark { source_device: DeviceId::parse(B).unwrap(), clock: 1u64 << 63 },
                Watermark { source_device: DeviceId::parse(C).unwrap(), clock: i64::MAX as u64 },
            ],
            more: false,
        })
        .unwrap();
        let _ = s.recv().unwrap();
        s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
        let mut served = 0usize;
        for _ in 0..4_000 {
            match s.recv() {
                Ok(SyncMessage::Items { items, more }) => {
                    served += items.len();
                    if items.is_empty() && !more {
                        break;
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        served
    });

    let mut session = Session::accept(srv, &key).unwrap();
    let known = vec![A.to_string(), B.to_string(), C.to_string()];
    let attr = Attribution { peer_id: B, local_id: A, known: &known };
    let _ = exchange(
        &mut session,
        &ours,
        (A, "Deck A"),
        both(),
        Retention { oldest_allowed: None },
        &attr,
        Turn::Second,
        false,
        0,
        &|| false,
    )
    .expect("the exchange must survive a hostile watermark");
    let served = peer.join().expect("the scripted peer must not panic");
    // u64::MAX folds to -1 and is floored at 0, so we simply re-offer. The
    // property that matters is that nothing panics and the exchange survives.
    assert!(served <= 5, "served {served} rows against a five-row store");
}

// ===========================================================================
// R6S-8. Secrets. Nothing that reaches settings.json, a log line or a Debug
// rendering may carry key material or a live pairing code.
// ===========================================================================
#[test]
fn r6sec_a_real_pairing_leaves_no_key_material_anywhere_persisted() {
    use crate::sync::pair_flow;
    use echokey_core::settings::{PairedDevice, Settings};
    use echokey_sync::{PairingCode, PairingRole};

    // Run a genuine pairing over a real socket pair, both roles.
    let (mut c, mut s) = socket_pair();
    let code = PairingCode::parse("314159").unwrap();
    let c2 = code.clone();
    let t = std::thread::spawn(move || {
        pair_flow::run(&mut s, PairingRole::Initiator, &c2, (A, "Deck A"))
    });
    let responder = pair_flow::run(&mut c, PairingRole::Responder, &code, (B, "Deck B"))
        .expect("responder pairs");
    let initiator = t.join().expect("no panic").expect("initiator pairs");
    assert_eq!(initiator.key.as_bytes(), responder.key.as_bytes());
    let hex: String = initiator.key.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex.len(), 64);

    // Exactly what the manager persists after a successful pairing.
    let mut settings = Settings::default();
    settings.sync.enabled = true;
    settings.sync.device_id = A.into();
    settings.sync.device_name = "Deck A".into();
    settings.sync.paired = vec![PairedDevice {
        id: initiator.device_id.clone(),
        name: initiator.device_name.clone(),
        last_seen: Some(now_ms()),
    }];
    let json = serde_json::to_string(&settings).unwrap();
    assert!(!json.contains(&hex), "the paired key was written into settings.json");
    assert!(
        !json.to_ascii_uppercase().contains(&hex.to_ascii_uppercase()),
        "the paired key was written into settings.json in another case"
    );
    assert!(!json.contains("314159"), "the pairing code was written into settings.json");

    // Nothing that gets Debug-printed or logged may carry it either.
    for rendered in [
        format!("{:?}", initiator.key),
        format!("{:?}", code),
        echokey_sync::PairingError::CodeMismatch.to_string(),
        format!("{:?}", DeviceId::parse(B).unwrap()),
    ] {
        assert!(!rendered.contains(&hex), "key material in {rendered:?}");
        assert!(!rendered.contains("314159"), "pairing code in {rendered:?}");
    }

    // The hex form the keystore writes must round-trip only through the
    // keystore, never through the roster type.
    let roster = format!("{:?}", settings.sync.paired);
    assert!(!roster.contains(&hex));
}

// ===========================================================================
// R6S-9. A device name the settings layer happily accepts and persists can
// disable sync completely — discovery AND every exchange — with a diagnosis
// that points at the network.
//
// `sync_set_device_name` (commands.rs) trims, refuses empty, and truncates to
// 64 CHARACTERS. `validate_device_name` (identity.rs) requires 64 BYTES, no
// '=', and no control characters, and is never called on the way in. It is
// called on the way out, in three places that all fail closed:
//   * `Discovery::start`  -> DiscoveryError::Identity, reported to the user as
//     "no usable network on this machine right now" (manager.rs)
//   * `SyncMessage::validate` on the Hello every `exchange` sends first
//   * the same Hello inside `pair_flow::run_with`
// ===========================================================================
#[test]
fn r6sec_a_device_name_the_ui_accepts_can_disable_sync_entirely() {
    use echokey_sync::{sanitise_device_name, validate_device_name, Discovery, DiscoveryConfig};

    // The PRODUCTION path, not a copy of it.
    //
    // This test used to inline the command's old body, trim, then take 64
    // CHARACTERS, and assert that the result was unsendable. It was, and that
    // was the finding: `validate_device_name` counts BYTES and refuses `=`,
    // because the name rides in an mDNS TXT `key=value` pair, so the settings
    // layer happily stored names the wire would not carry. Every `Hello` then
    // failed to encode and discovery refused to start, which the UI showed as
    // "no usable network".
    //
    // `sync_set_device_name` now runs `sanitise_device_name`, so the test calls
    // that instead of restating it. Pointing at the real function is the whole
    // point: an inlined copy would keep passing after the next change to the
    // rule, whichever way that change went.
    for raw in [
        "Ben=Work",                                     // '=' is legal in the UI
        "デスクトップ・パソコン・書斎の机の上のやつです",   // 64 chars, >64 bytes
        "  Ben's G14  ",                                // ordinary, just untrimmed
        "line\nbreak",                                  // a control character
    ] {
        let name = sanitise_device_name(raw)
            .unwrap_or_else(|| panic!("{raw:?} has usable characters and must survive"));

        validate_device_name(&name).unwrap_or_else(|e| {
            panic!("sanitise_device_name returned {name:?}, which the wire refuses: {e}")
        });

        // 1. Every exchange starts with this message. It must encode.
        let hello = SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::parse(A).unwrap(),
            device_name: name.clone(),
        };
        hello.encode().unwrap_or_else(|e| {
            panic!("a stored name ({name:?}) makes every Hello unsendable: {e}")
        });

        // 2. And discovery must not refuse it, or the UI reports a bad name as
        //    a network problem.
        let cfg = DiscoveryConfig {
            device_id: DeviceId::parse(A).unwrap(),
            device_name: name.clone(),
            port: 0,
        };
        match Discovery::start(&cfg) {
            Ok((d, _rx)) => drop(d),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    !msg.contains("device name"),
                    "discovery refused the persisted name ({name:?}): {msg}. \
                     The user is told there is no usable network."
                );
            }
        }
    }

    // A name with nothing usable in it is the one case the caller must report,
    // and `sync_set_device_name` does, it returns the "give this device a
    // name" error rather than storing something the wire will refuse.
    assert!(sanitise_device_name("===").is_none());
    assert!(sanitise_device_name("   ").is_none());
    assert!(sanitise_device_name("").is_none());
}
