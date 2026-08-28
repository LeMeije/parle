//! ADVERSARIAL REVIEW — round 5, concurrency / lifecycle pass.
//!
//! Demonstrations of live findings. NOT fixes. In its own file because
//! `replicate.rs` was being edited concurrently while this pass ran.
//!
//! Every socket carries a read AND a write timeout, and every loop is hard
//! bounded, so a defect shows up as a failed assertion or a timeout — never as
//! a hung suite.

#![cfg(test)]

use parle_core::history::Store;
use parle_sync::{
    DeviceId, ItemKind, PairedKey, Session, SyncItem, SyncMessage, Watermark, PROTOCOL_VERSION,
};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111"; // us
const B: &str = "22222222-2222-4222-8222-222222222222"; // the peer

/// Mirrors `replicate::PAGE`, which is private. Pinned to the wire's own cap,
/// which is what `PAGE` is defined as.
const PAGE: usize = parle_sync::MAX_BATCH_LEN;
/// Mirrors `replicate::MAX_BATCHES` (replicate.rs:40).
const MAX_BATCHES: usize = 64;
/// What `drain` will read: `MAX_BATCHES * 4` messages (replicate.rs:574).
const DRAIN_BUDGET: usize = MAX_BATCHES * 4;

fn socket_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c = TcpStream::connect(addr).unwrap();
    let (srv, _) = l.accept().unwrap();
    // BOTH directions. A protocol desync parks both sides in write(), so a read
    // timeout alone would let the suite hang instead of failing.
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

fn item(source: &str, i: usize, clock: i64) -> SyncItem {
    SyncItem {
        source_device: DeviceId::parse(source).unwrap(),
        origin_id: format!("o-{i:07}"),
        kind: ItemKind::Transcription,
        text: "x".into(),
        created_at: clock,
        updated_at: clock,
        pinned: false,
        clock: clock as u64,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// R5B-1. `drain` reads at most MAX_BATCHES * 4 messages and then returns Ok,
// silently discarding everything the peer had still to send — and leaving the
// stream desynchronised, with the peer still writing while we start writing
// back.
//
// The budget is smaller than what our OWN `serve` can legitimately emit: per
// source it sends up to MAX_BATCHES item messages (replicate.rs:415) plus up
// to MAX_BATCHES tombstone messages (replicate.rs:502), i.e. up to 128, and it
// iterates every source in `known_sources()`. Three sources is enough to pass
// 256. The turn-taking that the whole design rests on ("at every point exactly
// one side is writing and the other is reading", replicate.rs:246-252) then
// stops holding: the reader gives up, starts serving, and both sides block in
// write until the 120 s session deadline kills the exchange.
// ---------------------------------------------------------------------------
#[test]
fn r5b_drain_silently_drops_everything_past_its_message_budget() {
    let ours = store_for(A);
    // Comfortably past the budget, and small enough that the scripted peer can
    // write it all without blocking on a socket buffer.
    let messages = DRAIN_BUDGET + 8;

    let (c, srv) = socket_pair();
    let key = PairedKey::from_bytes([7u8; 32]);
    let k2 = key.clone();

    // The scripted peer. Speaks the real protocol, one row per message, then
    // the end-of-serve sentinel.
    let peer = std::thread::spawn(move || {
        let mut s = Session::initiate(c, &k2).unwrap();
        s.send(&SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::parse(B).unwrap(),
            device_name: "peer".into(),
        })
        .unwrap();
        let _ = s.recv().unwrap(); // our Hello
                                   // We are Turn::Second, so we read watermarks first: send ours now.
        s.send(&SyncMessage::Watermarks { entries: Vec::<Watermark>::new(), more: false })
            .unwrap();
        let _ = s.recv().unwrap(); // our (empty) watermarks
        let base = now_ms() - 5_000_000;
        let mut sent = 0usize;
        for i in 0..messages {
            s.send(&SyncMessage::Items {
                items: vec![item(B, i, base + i as i64)],
                more: true,
            })
            .unwrap();
            sent += 1;
        }
        s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
        // Drain whatever we serve back, bounded.
        for _ in 0..16 {
            match s.recv() {
                Ok(SyncMessage::Items { items, more }) if items.is_empty() && !more => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        sent
    });

    let mut session = Session::accept(srv, &key).unwrap();
    let known = vec![A.to_string(), B.to_string()];
    let attr = Attribution { peer_id: B, local_id: A, known: &known };
    let stats = exchange(
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
    .expect("the exchange reports success");
    let sent = peer.join().expect("the scripted peer must not panic");

    assert_eq!(
        stats.applied_items, sent,
        "drain stopped after {DRAIN_BUDGET} messages and reported success: \
         the peer sent {sent} rows, {} landed, {} were silently lost",
        stats.applied_items,
        sent - stats.applied_items
    );
}

// ---------------------------------------------------------------------------
// R5B-3. The round-4 fix for "receipts minted from invented source ids" caps
// `source_marks` per peer and evicts the LOWEST `received_clock`
// (history.rs:895-907). But the clock in a receipt is `it.updated_at`, a number
// the peer chooses (replicate.rs:606-608), and `mark_received_in` accepts
// anything up to `now + MAX_CLOCK_SKEW_MS` (history.rs:859). A genuine cursor
// carries the clock of a row that has actually been written, which is in the
// past; an invented one can carry `now + 2 minutes`.
//
// So the eviction order is under the attacker's control, and it evicts exactly
// the receipts that matter. Losing our cursor for a source means that source's
// whole history is re-offered on every exchange from then on — and the peer can
// re-do it in every exchange, so it never recovers.
// ---------------------------------------------------------------------------
#[test]
fn r5b_a_peer_chooses_which_receipts_the_cap_evicts() {
    let ours = store_for(A);
    let invented = 128usize; // comfortably past MAX_SOURCES_PER_PEER (64)

    let (c, srv) = socket_pair();
    let key = PairedKey::from_bytes([5u8; 32]);
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
        s.send(&SyncMessage::Watermarks { entries: Vec::<Watermark>::new(), more: false })
            .unwrap();
        let _ = s.recv().unwrap();

        // One honest row, with an honest clock: a few seconds old.
        let honest = now_ms() - 5_000;
        let mut batch = vec![item(B, 0, honest)];
        // ...and a pile of rows for devices that do not exist, every one of
        // them stamped as far ahead as the store will accept.
        let ahead = now_ms() + 100_000;
        for i in 0..invented {
            let src = format!("44444444-4444-4444-8444-{i:012x}");
            batch.push(item(&src, i + 1, ahead));
        }
        s.send(&SyncMessage::Items { items: batch, more: false }).unwrap();
        s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
        for _ in 0..16 {
            match s.recv() {
                Ok(SyncMessage::Items { items, more }) if items.is_empty() && !more => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });

    let mut session = Session::accept(srv, &key).unwrap();
    let known = vec![A.to_string(), B.to_string()];
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
    .expect("the exchange completes");
    peer.join().expect("the scripted peer must not panic");

    let marks = ours.lock().watermarks(B).unwrap();
    assert!(
        marks.iter().any(|(src, _)| src == B),
        "the receipt for the peer's OWN rows — the only thing that stops it \
         re-offering its whole history on every exchange — was evicted by {invented} \
         invented sources the peer stamped further ahead. {} cursors remain, none \
         of them the real one",
        marks.len()
    );
}

// ---------------------------------------------------------------------------
// R5B-2. The other half of the same defect, measured on the SENDING side:
// `serve` emits far more messages for ONE source than a share of the reader's
// whole-exchange budget. Nothing anywhere reconciles the two numbers.
//
// This seeds a single source at the paging ceiling and counts the messages a
// real `exchange` puts on the wire. A three-device mesh — which the design
// explicitly supports — multiplies this by three.
// ---------------------------------------------------------------------------
#[test]
fn r5b_one_source_alone_emits_most_of_the_readers_whole_budget() {
    let ours = store_for(A);
    // 63 full pages plus one row: 64 item messages, which is the MAX_BATCHES
    // ceiling, without tripping the truncation trim.
    let items = (MAX_BATCHES - 1) * PAGE + 1;
    // 39 full pages plus one row, just under the store's own per-source
    // tombstone cap. One "Clear History" on a full store produces this many.
    let tombs = 39 * PAGE + 1;
    let base = now_ms() - 50_000_000;
    {
        let g = ours.lock();
        for i in 0..items {
            g.apply_remote_item(
                B,
                &parle_core::history::RemoteItem {
                    source_machine: B.into(),
                    origin_id: format!("o-{i:07}"),
                    kind: "transcription".into(),
                    text: "x".into(),
                    created_at: base + i as i64,
                    updated_at: base + i as i64,
                    pinned: false,
                },
            )
            .unwrap();
        }
        // Deletes of rows that are long gone — distinct identities, so the
        // live rows above survive.
        for i in 0..tombs {
            g.apply_remote_tombstone(
                B,
                &parle_core::history::RemoteTombstone {
                    source_machine: B.into(),
                    origin_id: format!("d-{i:07}"),
                    deleted_at: base + i as i64,
                },
            )
            .unwrap();
        }
    }
    assert_eq!(ours.lock().count().unwrap(), items as i64, "the deletes hit nothing live");

    let (c, srv) = socket_pair();
    let key = PairedKey::from_bytes([9u8; 32]);
    let k2 = key.clone();

    // A peer that reads everything and applies nothing, so the count is exact
    // and nothing can block.
    let peer = std::thread::spawn(move || {
        let mut s = Session::initiate(c, &k2).unwrap();
        s.send(&SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::parse(B).unwrap(),
            device_name: "peer".into(),
        })
        .unwrap();
        let _ = s.recv().unwrap(); // our Hello
        s.send(&SyncMessage::Watermarks { entries: Vec::<Watermark>::new(), more: false })
            .unwrap();
        let _ = s.recv().unwrap(); // our watermarks
        let mut serve_messages = 0usize;
        // Hard bound, so a protocol change cannot make this spin.
        for _ in 0..4_000 {
            match s.recv() {
                Ok(SyncMessage::Items { items, more }) => {
                    serve_messages += 1;
                    if items.is_empty() && !more {
                        break;
                    }
                }
                Ok(_) => serve_messages += 1,
                Err(_) => break,
            }
        }
        // We are Turn::First on the app's side, so it drains after serving.
        s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
        serve_messages
    });

    let mut session = Session::accept(srv, &key).unwrap();
    let known = vec![A.to_string(), B.to_string()];
    let attr = Attribution { peer_id: B, local_id: A, known: &known };
    let _ = exchange(
        &mut session,
        &ours,
        (A, "Deck A"),
        both(),
        Retention { oldest_allowed: None },
        &attr,
        Turn::First,
        false,
        0,
        &|| false,
    );
    let serve_messages = peer.join().expect("the counting peer must not panic");

    // One end-of-serve sentinel is sent per exchange, not per source.
    let per_source = serve_messages - 1;
    let three_sources = per_source * 3 + 1;
    assert!(
        three_sources <= DRAIN_BUDGET,
        "one source alone put {per_source} messages on the wire; the reader's \
         budget for the WHOLE exchange is {DRAIN_BUDGET} messages, so a \
         three-device mesh ({three_sources}) overruns it — the reader gives up \
         mid-stream and starts writing while the sender is still writing"
    );
}
