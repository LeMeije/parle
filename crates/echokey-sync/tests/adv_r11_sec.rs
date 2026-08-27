//! ADVERSARIAL REVIEW, ROUND 11 — secrets, security, cross-platform.
//!
//! Scope here: the half of round 10's fixes that lives in `echokey-sync`, which
//! is the device-name character policy and the wire's decode bounds. The rest
//! of round 11 (the macOS secure-field gate, the pasteboard race, the settings
//! migration, the Windows style-bit read) is in
//! `src-tauri/src/sync/adversarial_r11_sec.rs` or is READ-ONLY.
//!
//! Every test here is designed to FAIL if the round-10 line it attacks is
//! reverted, or to state plainly which invariant it is pinning when the defect
//! it was written for turns out to be unreachable.

use echokey_sync::identity::{
    sanitise_device_name, validate_device_name, DeviceId, MAX_DEVICE_NAME_BYTES,
};
use echokey_sync::wire::{
    ItemKind, SyncItem, SyncMessage, Watermark, MAX_BATCH_LEN, MAX_ITEM_TEXT_BYTES,
    MAX_MESSAGE_BYTES,
};

fn dev(n: u8) -> DeviceId {
    DeviceId::parse(&format!("3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c{n:02x}")).unwrap()
}

// ---------------------------------------------------------------------------
// R11-1. The NBSP ban is defeated by an ordinary ASCII double space.
//
// Round 10 added U+00A0 and the exotic spaces because "Ben's\u{00A0}MacBook
// Pro" renders exactly like "Ben's MacBook Pro" and is a different string on
// the wire. The pairing list is HTML (`src/views/SettingsView.tsx`, spans
// `.sync-peer-name` / `.sync-device-name`, and `src/App.css` sets no
// `white-space` on either), so the browser COLLAPSES a run of ordinary spaces
// to one. A second plain space achieves the identical render with a character
// that is not exotic at all.
//
// The collision disclosure that is supposed to save the user is exact string
// equality — `unpaired.filter((o) => o.name === p.name).length > 1` — so the
// two rows do not collide, no device id is shown, and the user picks between
// two visually identical labels when deciding where to type the pairing code.
// ---------------------------------------------------------------------------

#[test]
fn r11_ascii_double_space_survives_every_gate_the_nbsp_ban_added() {
    let honest = "Ben's MacBook Pro";
    let nbsp = "Ben's\u{00A0}MacBook Pro";
    let twospace = "Ben's  MacBook Pro";

    // The fix under attack is present: this is what makes the test able to
    // discriminate at all, rather than passing against unfixed code.
    assert!(validate_device_name(honest).is_ok(), "honest name must be usable");
    assert!(
        validate_device_name(nbsp).is_err(),
        "round 10's NBSP ban is missing; the rest of this test proves nothing"
    );

    // The attack. A peer name reaching the list has been through
    // `usable_peer_name` -> `sanitise_device_name`, so that is what is compared.
    let shown = match sanitise_device_name(twospace) {
        Some(s) => s,
        None => return, // PREMISE GONE: the name no longer reaches the list.
    };
    let collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");

    // The property the NBSP ban exists to provide: two devices cannot show
    // labels that are indistinguishable on screen and different underneath.
    // `shown != honest` is what makes the UI's `o.name === p.name` collision
    // check miss, so the device id is never shown for either row.
    assert!(
        shown == honest || collapse(&shown) != collapse(honest),
        "two peer names render identically in HTML ({:?} vs {:?}) but differ as \
         strings, so the pairing list shows no device id for either",
        shown,
        honest
    );
}

// ---------------------------------------------------------------------------
// R11-2. The invisible-filler family was banned incompletely.
//
// `is_invisible_or_bidi` refuses U+3164 HANGUL FILLER and U+FFA0 HALFWIDTH
// HANGUL FILLER by name ("invisible letters used to make a label look blank or
// padded"). U+115F HANGUL CHOSEONG FILLER and U+1160 HANGUL JUNGSEONG FILLER
// are the same construct and are not refused. U+1680 OGHAM SPACE MARK is a Zs
// that the "non-breaking and exotic spaces" block does not list, and U+2800
// BRAILLE PATTERN BLANK renders as blank width in the common fonts.
//
// This is the same class of hole as the NBSP one, not a new one, so it is
// reported at the severity that fits: the general fix is to normalise, not to
// keep extending a list of characters by hand.
// ---------------------------------------------------------------------------

#[test]
fn r11_the_filler_family_is_banned_by_example_not_by_class() {
    // Banned, so the test discriminates.
    for banned in ['\u{3164}', '\u{FFA0}', '\u{00A0}', '\u{2000}'] {
        assert!(
            validate_device_name(&format!("Ben{banned}s Mac")).is_err(),
            "expected {banned:?} to be refused"
        );
    }
    // Not banned. Same family, same effect on a rendered label.
    let mut allowed = Vec::new();
    for c in ['\u{115F}', '\u{1160}', '\u{1680}', '\u{2800}', '\u{034F}'] {
        if validate_device_name(&format!("Ben{c}s Mac")).is_ok() {
            allowed.push(format!("U+{:04X}", c as u32));
        }
    }
    assert!(
        allowed.is_empty(),
        "invisible/blank characters still accepted in a device name: {allowed:?}"
    );
}

// ---------------------------------------------------------------------------
// R11-3. The wire bounds the name's LENGTH only, and that is correct.
//
// Round 10's own note says the character policy must not live in `validate()`
// because a Hello that fails to decode kills the whole exchange. Pin both
// halves so a later round does not "tidy" one into the other: a hostile name
// must reach the byte cap and no further, and a name that the display policy
// refuses must still decode.
// ---------------------------------------------------------------------------

#[test]
fn r11_hello_bounds_the_name_without_policing_it() {
    let hostile = SyncMessage::Hello {
        protocol_version: echokey_sync::wire::PROTOCOL_VERSION,
        device_id: dev(1),
        device_name: "Ben\u{202E}koobcaM".into(),
    };
    assert!(
        hostile.validate().is_ok(),
        "a hostile display name must not be able to deny sync at the Hello"
    );
    assert!(
        validate_device_name("Ben\u{202E}koobcaM").is_err(),
        "…but it must never be shown"
    );

    let oversized = SyncMessage::Hello {
        protocol_version: echokey_sync::wire::PROTOCOL_VERSION,
        device_id: dev(1),
        device_name: "x".repeat(MAX_DEVICE_NAME_BYTES + 1),
    };
    assert!(oversized.validate().is_err(), "the byte cap must still bite");

    let empty = SyncMessage::Hello {
        protocol_version: echokey_sync::wire::PROTOCOL_VERSION,
        device_id: dev(1),
        device_name: String::new(),
    };
    assert!(empty.validate().is_err());
}

// ---------------------------------------------------------------------------
// R11-4. Criterion E, verified independently: the batch limit bounds
// ALLOCATION on decode, not merely the verdict afterwards.
//
// The assertion that matters is that a 60,000-entry message is refused without
// the vector ever reaching 60,000, which is what `bounded_batch` claims. A peak
// RSS assertion would be flaky, so this asserts the observable consequence: the
// error comes from the visitor (a decode error naming the limit), not from
// `check_len` after the fact.
// ---------------------------------------------------------------------------

#[test]
fn r11_oversized_batch_is_refused_inside_the_visitor() {
    let mut json = String::from(r#"{"watermarks":{"entries":["#);
    for i in 0..(MAX_BATCH_LEN * 4) {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"source_device":"{}","clock":{i},"origin":"r"}}"#,
            dev(2)
        ));
    }
    json.push_str(r#"],"more":false}}"#);
    assert!(json.len() < MAX_MESSAGE_BYTES, "test input must fit the byte cap");

    let err = SyncMessage::decode(json.as_bytes()).expect_err("must be refused");
    let text = err.to_string();
    assert!(
        text.contains("entry limit"),
        "expected the visitor's own message, got: {}",
        &text[..text.len().min(120)]
    );
}

#[test]
fn r11_declared_length_above_the_cap_is_refused_before_any_reserve() {
    // The framing cap is enforced on the declared length, so a peer claiming a
    // huge message costs a comparison. Pin the decode half of the same rule.
    let too_big = vec![b'x'; MAX_MESSAGE_BYTES + 1];
    let err = SyncMessage::decode(&too_big).expect_err("must be refused");
    assert!(err.to_string().contains("limit"), "{err}");
}

#[test]
fn r11_one_item_cannot_exceed_the_item_cap_on_decode_either() {
    let item = SyncItem {
        source_device: dev(3),
        origin_id: "row-1".into(),
        kind: ItemKind::Transcription,
        text: "x".repeat(MAX_ITEM_TEXT_BYTES + 1),
        created_at: 1,
        updated_at: 1,
        pinned: false,
        clock: 1,
    };
    let msg = SyncMessage::Items { items: vec![item], more: false };
    assert!(msg.encode().is_err(), "encode must refuse it");
    // And the same message arriving from a peer is refused on the way in.
    let json = serde_json::to_vec(&msg).unwrap();
    assert!(SyncMessage::decode(&json).is_err(), "decode must refuse it too");
}

// ---------------------------------------------------------------------------
// R11-5. A watermark's origin is peer-controlled and must be bounded.
// ---------------------------------------------------------------------------

#[test]
fn r11_watermark_origin_is_bounded_but_may_be_empty() {
    let ok = Watermark { source_device: dev(4), clock: 1, origin: String::new() };
    assert!(ok.validate().is_ok(), "empty means re-offer the millisecond");

    let hostile = Watermark {
        source_device: dev(4),
        clock: 1,
        origin: "x".repeat(4096),
    };
    assert!(hostile.validate().is_err(), "an unbounded origin must be refused");
}
