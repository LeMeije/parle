//! End-to-end test over real TCP on 127.0.0.1: pair two devices with a shared
//! 6-digit code, upgrade the same socket to a Noise session, and replicate
//! items in both directions.
//!
//! This doubles as the reference for how the app layer is meant to drive the
//! crate: the pairing state machine is transport-agnostic, so the framing of
//! the four pairing messages is the caller's choice (here, a `u32` length
//! prefix on the same TCP connection that later carries the session).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use echokey_sync::identity::DeviceId;
use echokey_sync::pairing::{
    ConfirmTag, PairedKey, Pairing, PairingCode, PairingError, PairingRole,
};
use echokey_sync::session::Session;
use echokey_sync::wire::{ItemKind, SyncItem, SyncMessage, Tombstone, Watermark};

const DEVICE_A: &str = "3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d";
const DEVICE_B: &str = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f2a3b4c5e";

fn write_blob(stream: &mut TcpStream, blob: &[u8]) {
    stream
        .write_all(&(blob.len() as u32).to_be_bytes())
        .expect("write length");
    stream.write_all(blob).expect("write blob");
    stream.flush().expect("flush");
}

fn read_blob(stream: &mut TcpStream) -> Vec<u8> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).expect("read length");
    let mut blob = vec![0u8; u32::from_be_bytes(len) as usize];
    stream.read_exact(&mut blob).expect("read blob");
    blob
}

/// Run the four-message pairing exchange over an already-connected socket.
fn pair_over(
    stream: &mut TcpStream,
    role: PairingRole,
    code: &PairingCode,
) -> Result<PairedKey, PairingError> {
    let (state, our_msg) = Pairing::start(role, code);
    let peer_msg = match role {
        PairingRole::Initiator => {
            write_blob(stream, &our_msg);
            read_blob(stream)
        }
        PairingRole::Responder => {
            let peer = read_blob(stream);
            write_blob(stream, &our_msg);
            peer
        }
    };

    let (confirm, our_tag) = state.finish(&peer_msg)?;

    let peer_tag = match role {
        PairingRole::Initiator => {
            write_blob(stream, our_tag.as_bytes());
            read_blob(stream)
        }
        PairingRole::Responder => {
            let peer = read_blob(stream);
            write_blob(stream, our_tag.as_bytes());
            peer
        }
    };
    let peer_tag: [u8; 32] = peer_tag.try_into().expect("32-byte tag");

    confirm.verify_peer(&ConfirmTag::from_bytes(peer_tag))
}

fn item(device: &DeviceId, n: u64, text: &str) -> SyncItem {
    SyncItem {
        source_device: device.clone(),
        origin_id: format!("row-{n}"),
        kind: if n.is_multiple_of(2) {
            ItemKind::Transcription
        } else {
            ItemKind::Clipboard
        },
        text: text.to_string(),
        created_at: 1_700_000_000_000 + n as i64,
        updated_at: 1_700_000_000_000 + n as i64,
        pinned: n == 1,
        clock: n,
    }
}

/// Spawn the two devices against a loopback listener and run `a`/`b` on them.
fn run_pair<A, B, RA, RB>(a: A, b: B) -> (RA, RB)
where
    A: FnOnce(TcpStream) -> RA + Send + 'static,
    B: FnOnce(TcpStream) -> RB + Send + 'static,
    RA: Send + 'static,
    RB: Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();

    let dialer = std::thread::spawn(move || {
        let stream = TcpStream::connect(addr).expect("connect");
        // Never let a protocol bug hang the suite.
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        a(stream)
    });

    let (stream, _) = listener.accept().expect("accept");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let b_out = b(stream);
    (dialer.join().expect("device A thread"), b_out)
}

#[test]
fn pair_then_replicate_in_both_directions() {
    let code = PairingCode::generate().unwrap();
    let (code_a, code_b) = (code.clone(), code);

    let (a_result, b_result) = run_pair(
        move |mut stream| {
            let id = DeviceId::parse(DEVICE_A).unwrap();
            let key = pair_over(&mut stream, PairingRole::Initiator, &code_a).unwrap();
            let mut session = Session::initiate(stream, &key).unwrap();

            session
                .send(&SyncMessage::hello(id.clone(), "G14"))
                .unwrap();
            let peer_hello = session.recv().unwrap();

            // A tells B what it already holds, then ships its own rows.
            session
                .send(&SyncMessage::Watermarks {
                    entries: vec![Watermark {
                        source_device: DeviceId::parse(DEVICE_B).unwrap(),
                        clock: 4,
                    }],
                    more: false,
                })
                .unwrap();
            session
                .send(&SyncMessage::Items {
                    items: vec![
                        item(&id, 1, "dictated on the G14"),
                        item(&id, 2, "clipboard from the G14"),
                    ],
                    more: false,
                })
                .unwrap();

            let peer_items = session.recv().unwrap();
            let peer_tombstones = session.recv().unwrap();
            (peer_hello, peer_items, peer_tombstones)
        },
        move |mut stream| {
            let id = DeviceId::parse(DEVICE_B).unwrap();
            let key = pair_over(&mut stream, PairingRole::Responder, &code_b).unwrap();
            let mut session = Session::accept(stream, &key).unwrap();

            let peer_hello = session.recv().unwrap();
            session
                .send(&SyncMessage::hello(id.clone(), "MacBook"))
                .unwrap();

            let peer_watermarks = session.recv().unwrap();
            let peer_items = session.recv().unwrap();

            session
                .send(&SyncMessage::Items {
                    items: vec![item(&id, 5, "dictated on the MacBook")],
                    more: false,
                })
                .unwrap();
            session
                .send(&SyncMessage::Tombstones {
                    entries: vec![Tombstone {
                        source_device: id,
                        origin_id: "row-3".into(),
                        deleted_at: 1_700_000_100_000,
                        clock: 6,
                    }],
                    more: false,
                })
                .unwrap();

            (peer_hello, peer_watermarks, peer_items)
        },
    );

    let (a_saw_hello, a_saw_items, a_saw_tombstones) = a_result;
    let (b_saw_hello, b_saw_watermarks, b_saw_items) = b_result;

    // Both sides learned the other's identity.
    match (a_saw_hello, b_saw_hello) {
        (
            SyncMessage::Hello {
                device_id: from_b,
                device_name: name_b,
                ..
            },
            SyncMessage::Hello {
                device_id: from_a,
                device_name: name_a,
                ..
            },
        ) => {
            assert_eq!(from_b.as_str(), DEVICE_B);
            assert_eq!(name_b, "MacBook");
            assert_eq!(from_a.as_str(), DEVICE_A);
            assert_eq!(name_a, "G14");
        }
        other => panic!("expected two hellos, got {other:?}"),
    }

    // B received A's watermark and both of A's rows, tagged with A's device id.
    match b_saw_watermarks {
        SyncMessage::Watermarks { entries, .. } => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].clock, 4);
        }
        other => panic!("expected watermarks, got {other:?}"),
    }
    match b_saw_items {
        SyncMessage::Items { items, more } => {
            assert!(!more);
            assert_eq!(items.len(), 2);
            assert!(items.iter().all(|i| i.source_device.as_str() == DEVICE_A));
            assert_eq!(items[0].text, "dictated on the G14");
            assert!(items[0].pinned, "pins must propagate");
            assert_eq!(items[1].kind, ItemKind::Transcription);
        }
        other => panic!("expected items, got {other:?}"),
    }

    // A received B's row and B's delete.
    match a_saw_items {
        SyncMessage::Items { items, .. } => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].source_device.as_str(), DEVICE_B);
            assert_eq!(items[0].text, "dictated on the MacBook");
        }
        other => panic!("expected items, got {other:?}"),
    }
    match a_saw_tombstones {
        SyncMessage::Tombstones { entries, .. } => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].origin_id, "row-3");
        }
        other => panic!("expected tombstones, got {other:?}"),
    }
}

#[test]
fn a_one_megabyte_item_survives_the_round_trip() {
    let code = PairingCode::parse("314159").unwrap();
    let (code_a, code_b) = (code.clone(), code);
    let text = "ż".repeat(echokey_sync::wire::MAX_ITEM_TEXT_BYTES / 2); // exactly 1 MiB

    let expected = text.clone();
    let (_, received) = run_pair(
        move |mut stream| {
            let id = DeviceId::parse(DEVICE_A).unwrap();
            let key = pair_over(&mut stream, PairingRole::Initiator, &code_a).unwrap();
            let mut session = Session::initiate(stream, &key).unwrap();
            session
                .send(&SyncMessage::Items {
                    items: vec![item(&id, 1, &text)],
                    more: false,
                })
                .unwrap();
        },
        move |mut stream| {
            let key = pair_over(&mut stream, PairingRole::Responder, &code_b).unwrap();
            let mut session = Session::accept(stream, &key).unwrap();
            session.recv().unwrap()
        },
    );

    match received {
        SyncMessage::Items { items, .. } => assert_eq!(items[0].text, expected),
        other => panic!("expected items, got {other:?}"),
    }
}

#[test]
fn a_mistyped_code_fails_pairing_and_no_session_is_possible() {
    let (a_result, b_result) = run_pair(
        move |mut stream| {
            let code = PairingCode::parse("428193").unwrap();
            pair_over(&mut stream, PairingRole::Initiator, &code)
        },
        move |mut stream| {
            // One digit off: the human misread the screen.
            let code = PairingCode::parse("428198").unwrap();
            pair_over(&mut stream, PairingRole::Responder, &code)
        },
    );

    assert_eq!(a_result.unwrap_err(), PairingError::CodeMismatch);
    assert_eq!(b_result.unwrap_err(), PairingError::CodeMismatch);
}
