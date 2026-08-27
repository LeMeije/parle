//! ADVERSARIAL REVIEW, ROUND 8 — security, cryptography, attack surface.
//! Protocol-crate half. Demonstrations only; no production code is changed here.
//!
//! Pass criteria under test in this file:
//!   E. every size limit bounds ALLOCATION, on decode as well as encode
//!   F. every network read is bounded by a deadline a peer cannot extend
//!   I. key material never reaches a serialised structure, a log line, or Debug
//!
//! Everything is bounded: no loop here runs without a hard iteration count and
//! every socket carries read AND write timeouts.

use echokey_sync::{
    ConfirmTag, PairedKey, Pairing, PairingCode, PairingRole, Session, SyncMessage, WireError,
    MAX_BATCH_LEN, MAX_ITEM_TEXT_BYTES, MAX_MESSAGE_BYTES,
};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const UUID_A: &str = "11111111-1111-4111-8111-111111111111";

fn sock_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c = TcpStream::connect(addr).unwrap();
    let (s, _) = l.accept().unwrap();
    for k in [&c, &s] {
        k.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        k.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    }
    (c, s)
}

// ---------------------------------------------------------------------------
// E. Allocation bounds on decode.
// ---------------------------------------------------------------------------

/// R8-E1. The byte cap has to be the ONLY lever a peer holds over how much we
/// allocate while decoding. Two ways it could fail:
///
///   * a length field the decoder honours before it is checked, and
///   * a JSON construct that expands, so N bytes on the wire become >N bytes of
///     heap.
///
/// This pins both directions. It asserts something positive first — that a
/// message right at the caps really does decode — so it cannot pass by refusing
/// everything.
#[test]
fn r8_decode_allocation_is_bounded_by_the_byte_cap_alone() {
    // Positive control: a full batch at the entry limit decodes.
    let mut ok = String::from(r#"{"watermarks":{"more":false,"entries":["#);
    for i in 0..MAX_BATCH_LEN {
        if i > 0 {
            ok.push(',');
        }
        ok.push_str(r#"{"source_device":"11111111-1111-4111-8111-111111111111","clock":1}"#);
    }
    ok.push_str("]}}");
    match SyncMessage::decode(ok.as_bytes()) {
        Ok(SyncMessage::Watermarks { entries, .. }) => {
            assert_eq!(entries.len(), MAX_BATCH_LEN, "the control message must decode in full")
        }
        other => panic!("a message at the limits must decode: {other:?}"),
    }

    // A batch over the limit must be refused INSIDE the deserializer, so the
    // vector never grows past the cap. `BatchTooLong` can only be produced by
    // `validate()`, which runs once the whole vector exists — seeing it here
    // would mean the cap is a verdict, not a bound.
    let mut over = String::from(r#"{"watermarks":{"more":false,"entries":["#);
    for i in 0..(MAX_BATCH_LEN * 40) {
        if i > 0 {
            over.push(',');
        }
        over.push_str(r#"{"source_device":"11111111-1111-4111-8111-111111111111","clock":1}"#);
    }
    over.push_str("]}}");
    assert!(over.len() < MAX_MESSAGE_BYTES, "the attack must fit inside the byte cap");
    match SyncMessage::decode(over.as_bytes()) {
        Err(WireError::BatchTooLong { len, .. }) => {
            panic!("the decoder materialised {len} entries before refusing them")
        }
        Err(WireError::Malformed(_)) => {}
        other => panic!("an oversized batch must be refused: {other:?}"),
    }

    // No JSON escape expands. A string made entirely of six-byte \u escapes
    // must not decode to more bytes than it occupied on the wire; if it could,
    // the 4 MiB cap would not be a cap on heap.
    const ESCAPES: usize = 40;
    let payload = "\\u0041".repeat(ESCAPES);
    let on_the_wire = payload.len();
    assert_eq!(on_the_wire, ESCAPES * 6, "the control really is six bytes per character");
    let escaped = format!(
        // PROTOCOL_VERSION, not a literal 3. The literal went stale the moment
        // the cursor became a pair and the version bumped, and a decode test
        // that fails for the wrong reason teaches you to edit the test.
        r#"{{"hello":{{"protocol_version":{v},"device_id":"{UUID_A}","device_name":"{payload}"}}}}"#,
        v = echokey_sync::PROTOCOL_VERSION
    );
    match SyncMessage::decode(escaped.as_bytes()) {
        Ok(SyncMessage::Hello { device_name, .. }) => {
            assert_eq!(device_name.len(), ESCAPES, "the escapes really did decode");
            assert!(
                device_name.len() <= on_the_wire,
                "escaped text expanded on decode: {} bytes of heap from {on_the_wire} on the wire",
                device_name.len()
            );
        }
        other => panic!("a legal hello must decode: {other:?}"),
    }
}

/// R8-E2. A single item's text cap is checked by `validate()`, i.e. after serde
/// has built the `String`. That is only safe while the message byte cap sits
/// above it, so pin the relationship rather than the numbers: the largest
/// oversize a peer can smuggle past the text cap is still bounded by the
/// message cap, and a text over the cap is refused on the way IN, not just out.
#[test]
fn r8_an_oversized_item_text_is_refused_on_the_way_in() {
    assert!(
        MAX_ITEM_TEXT_BYTES < MAX_MESSAGE_BYTES,
        "the text cap only bounds allocation because the byte cap is larger"
    );
    let text = "a".repeat(MAX_ITEM_TEXT_BYTES + 1);
    let json = format!(
        r#"{{"items":{{"more":false,"items":[{{"source_device":"{UUID_A}","origin_id":"r","kind":"clipboard","text":"{text}","created_at":1,"updated_at":1,"pinned":false,"clock":1}}]}}}}"#
    );
    assert!(json.len() < MAX_MESSAGE_BYTES, "the smuggled message fits inside the byte cap");
    assert!(
        matches!(SyncMessage::decode(json.as_bytes()), Err(WireError::ItemTextTooLarge { .. })),
        "a peer that skips our encoder must still be refused on decode"
    );
}

/// R8-E3. Deeply nested JSON must not recurse the stack to death. A stack
/// overflow aborts the process, which on a desktop app means a crash report
/// carrying whatever was in memory.
#[test]
fn r8_deeply_nested_json_does_not_overflow_the_stack() {
    const DEPTH: usize = 200_000;
    let mut json = String::from(r#"{"items":"#);
    json.push_str(&"[".repeat(DEPTH));
    json.push_str(&"]".repeat(DEPTH));
    json.push('}');
    assert!(json.len() < MAX_MESSAGE_BYTES, "must fit inside the byte cap to be interesting");
    // Runs on a thread with a deliberately SMALL stack: if the decoder recurses
    // with input depth this blows up here rather than looking fine on the
    // generous main-thread stack and crashing on a real one.
    let h = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || SyncMessage::decode(json.as_bytes()).is_err())
        .unwrap();
    assert!(h.join().expect("the decoder must not abort the process"));
}

// ---------------------------------------------------------------------------
// G. Pairing identity.
// ---------------------------------------------------------------------------

/// R8-G1. The whole trust wedge in one assertion: a machine on the LAN that
/// does not hold the paired key cannot open a session, whatever it knows.
/// Device ids are public in the mDNS TXT records, so "knows who you are" is the
/// attacker's starting position, not an achievement.
#[test]
fn r8_knowing_the_device_id_buys_an_unpaired_machine_nothing() {
    let (a, b) = sock_pair();
    let real = PairedKey::from_bytes([7u8; 32]);
    let attacker = PairedKey::from_bytes([8u8; 32]);
    let h = std::thread::spawn(move || Session::accept(b, &real).is_ok());
    let dialled = Session::initiate(a, &attacker).is_ok();
    let accepted = h.join().unwrap();
    assert!(!(dialled && accepted), "an unpaired key must never yield a usable session on both ends");
    assert!(!accepted, "the listening side must refuse a peer that does not hold the key");
}

/// R8-G2. SPAKE2 gives exactly one guess per run and the confirmation round is
/// what makes a wrong guess FAIL rather than silently derive a different key.
/// Ten wrong codes in a row, each a full protocol run, must yield nothing.
#[test]
fn r8_a_wrong_code_never_yields_a_key() {
    let right = PairingCode::parse("428193").unwrap();
    for guess in ["428192", "428194", "000000", "999999", "428183", "128193", "428093", "421893", "482193", "824193"] {
        let wrong = PairingCode::parse(guess).unwrap();
        let (init, msg_i) = Pairing::start(PairingRole::Initiator, &right);
        let (resp, msg_r) = Pairing::start(PairingRole::Responder, &wrong);
        let (confirm_i, tag_i) = init.finish(&msg_r).expect("well-formed peer message");
        let (confirm_r, tag_r) = resp.finish(&msg_i).expect("well-formed peer message");
        assert!(confirm_i.verify_peer(&tag_r).is_err(), "{guess} produced a key on the shower side");
        assert!(confirm_r.verify_peer(&tag_i).is_err(), "{guess} produced a key on the typer side");
    }
    // Positive control: the right code still pairs, so the loop above is not
    // passing because pairing is broken outright.
    let (init, msg_i) = Pairing::start(PairingRole::Initiator, &right);
    let (resp, msg_r) = Pairing::start(PairingRole::Responder, &right);
    let (confirm_i, tag_i) = init.finish(&msg_r).unwrap();
    let (confirm_r, tag_r) = resp.finish(&msg_i).unwrap();
    let ka = confirm_i.verify_peer(&tag_r).expect("the right code pairs");
    let kb = confirm_r.verify_peer(&tag_i).expect("the right code pairs");
    assert_eq!(ka.as_bytes(), kb.as_bytes());
}

// ---------------------------------------------------------------------------
// I. Key material never leaves the keychain.
// ---------------------------------------------------------------------------

/// R8-I1. Nothing that holds key material may render it. Checked on the real
/// values a pairing produces, not on placeholders, because a `Debug` that
/// redacts a constant and prints a computed field is exactly the shape of leak
/// a placeholder test misses.
#[test]
fn r8_no_pairing_secret_can_be_rendered_by_debug_or_display() {
    let code = PairingCode::parse("735192").unwrap();
    let (init, msg_i) = Pairing::start(PairingRole::Initiator, &code);
    let (resp, msg_r) = Pairing::start(PairingRole::Responder, &code);
    let (confirm_i, tag_i) = init.finish(&msg_r).unwrap();
    let (confirm_r, tag_r) = resp.finish(&msg_i).unwrap();
    let key = confirm_i.verify_peer(&tag_r).unwrap();
    let _ = confirm_r.verify_peer(&tag_i).unwrap();

    let hex: String = key.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let tag_hex: String = tag_i.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex.len(), 64, "the control value is a real 32-byte key");

    for rendered in [format!("{key:?}"), format!("{code:?}"), format!("{tag_i:?}")] {
        assert!(!rendered.contains(&hex), "a Debug rendering carried the paired key: {rendered}");
        assert!(!rendered.contains(&tag_hex), "a Debug rendering carried a confirmation tag");
        assert!(!rendered.contains("735192"), "a Debug rendering carried the live pairing code");
        // A raw byte array printed as decimal is the other way this leaks.
        assert!(
            !rendered.contains(&format!("{}", key.as_bytes()[0])) || rendered.len() < 40,
            "a Debug rendering looks like it printed the bytes: {rendered}"
        );
    }
    assert_eq!(format!("{key:?}"), "PairedKey(<redacted>)");
    assert_eq!(format!("{code:?}"), "PairingCode(******)");
}

/// R8-I2. The paired key is 32 bytes and the ONLY way out of the type is
/// `as_bytes`. If that stops being true — a `Serialize` derive, a `Display`, a
/// public field — a key can be swept into `settings.json` by any struct that
/// holds one. The substantive guard is in the app crate, which serialises real
/// `Settings` and searches the JSON for the key; this pins the shape it relies
/// on so that guard cannot quietly stop testing anything.
#[test]
fn r8_a_paired_key_exposes_nothing_but_its_bytes() {
    let k = PairedKey::from_bytes([0xAB; 32]);
    assert_eq!(std::mem::size_of::<PairedKey>(), 32);
    assert_eq!(k.as_bytes().len(), 32);
    assert_eq!(PairedKey::from_bytes(*k.as_bytes()), k);
    let t = ConfirmTag::from_bytes([0x5Au8; 32]);
    assert_eq!(t.as_bytes().len(), 32);
    assert!(!format!("{t:?}").contains("5a"));
}
