//! ADVERSARIAL REVIEW, ROUND 12 — the wire and identity surface.
//!
//! Round 11's fixes are attacked first, per the handover's own instruction.
//! Round 11 is commit `67ab14c`.
//!
//! Every test here asserts the contract the code OUGHT to hold. A failure is
//! the finding. Where a thing was attacked and held, the test passes and says
//! which line it is pinning, so a later round cannot quietly undo it.
//!
//! Nothing here opens a socket, spawns a thread, sleeps, or touches the real
//! keychain.

use std::path::{Path, PathBuf};

use echokey_sync::identity::{
    sanitise_device_name, validate_device_name, DeviceId, MAX_DEVICE_NAME_BYTES,
};
use echokey_sync::wire::{
    ItemKind, SyncItem, SyncMessage, Tombstone, Watermark, WireError, MAX_BATCH_LEN,
    MAX_ITEM_TEXT_BYTES, MAX_MESSAGE_BYTES, MAX_ORIGIN_ID_BYTES, PROTOCOL_VERSION,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/echokey-sync has two ancestors")
        .to_path_buf()
}

/// Source with `//` comments stripped, so prose cannot satisfy a guard that is
/// looking for code.
fn code_of(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn dev(n: u8) -> DeviceId {
    DeviceId::parse(&format!("3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c{n:02x}")).unwrap()
}

// ---------------------------------------------------------------------------
// R12-1. Round 11's whitespace collapse is on a function the pairing list
//        never calls.
//
// Round 11 taught `sanitise_device_name` to collapse internal whitespace runs,
// and the comment explaining why names the target precisely: "the pairing list
// is HTML, so the browser collapses the run, and the UI's duplicate check is
// exact string equality, so the two rows are pixel-identical AND no device id
// is shown to tell them apart."
//
// The pairing list is `unpaired` in `src/views/SettingsView.tsx:929`, which is
// `status.peers`. Those rows are `UiPeer`s built in `manager.rs:657` straight
// from `PeerInfo.name`, and a `PeerInfo` is built in
// `discovery.rs:243 peer_from`, whose only gate is `validate_device_name`.
// `sanitise_device_name` is reached from `usable_peer_name`, which
// `manager.rs:1366` applies to the PAIRED list only.
//
// So the fix landed on a function that is never called on the name it was
// written to defend against, and `r11_ascii_double_space_survives_every_gate_
// the_nbsp_ban_added` passes by asserting a property of the wrong function.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_the_gate_the_pairing_list_uses_still_admits_a_double_space() {
    let honest = "Ben's MacBook Pro";
    let twospace = "Ben's  MacBook Pro";

    // Positive controls. Without these the test could pass by finding nothing:
    // an honest name must be admitted, and the character the round-10 ban was
    // written for must be refused, or `validate_device_name` is not the gate
    // this test thinks it is.
    assert!(validate_device_name(honest).is_ok(), "an honest name must be usable");
    assert!(
        validate_device_name("Ben's\u{00A0}MacBook Pro").is_err(),
        "the NBSP ban is missing from the gate; the rest of this test proves nothing"
    );

    // `sanitise_device_name` does hold the property. That is exactly why the
    // round-11 test passes and the defect is still live.
    assert_eq!(
        sanitise_device_name(twospace).as_deref(),
        Some(honest),
        "precondition: round 11's collapse is present in the sanitiser"
    );

    // The gate the discovery path actually uses. `PeerInfo.name` is stored
    // verbatim after this returns Ok, so this is the last chance to refuse a
    // name that renders identically to another.
    assert!(
        validate_device_name(twospace).is_err(),
        "`validate_device_name` admits {twospace:?}, which renders in the pairing list \
         exactly like {honest:?}. `peer_from` stores the raw string, `UiPeer` carries it \
         unchanged, and the collision disclosure is `o.name === p.name`, so the user is \
         shown two identical labels with no device id and picks one to type the pairing \
         code into"
    );
}

/// The other half: prove the discovery path really is the un-collapsed one, so
/// the finding above is about the code and not about my reading of it.
#[test]
fn r12_sec_discovery_stores_the_raw_name_it_only_validated() {
    let discovery = code_of("crates/echokey-sync/src/discovery.rs");
    let fun = discovery
        .split("fn peer_from(")
        .nth(1)
        .and_then(|s| s.split("\nfn ").next())
        .expect("peer_from is in discovery.rs");
    // Positive control: the function really does build the PeerInfo here.
    assert!(fun.contains("PeerInfo {"), "peer_from no longer builds a PeerInfo");
    // The fix taken was the OTHER of the two available: the collapse went onto
    // `validate_device_name`, the gate this function already calls, rather than
    // swapping the call to the sanitiser. Either closes the hole, so this
    // asserts the composition instead of naming one of them: whatever gate
    // `peer_from` uses, a name needing collapse must not get through it.
    let gate_is_validate = fun.contains("validate_device_name(");
    let gate_is_sanitise = fun.contains("sanitise_device_name");
    assert!(
        gate_is_validate || gate_is_sanitise,
        "`peer_from` gates the peer's name with nothing at all, so the unsigned mDNS name \
         reaches the pairing list exactly as it arrived"
    );
    assert!(
        !gate_is_validate || validate_device_name("Ben's  MacBook Pro").is_err(),
        "`peer_from` gates on `validate_device_name`, and that function admits a name with \
         a doubled space. It renders in the pairing list exactly like the honest one, the \
         duplicate check is exact string equality so neither row is flagged, and no device \
         id is shown to tell them apart"
    );
}

// ---------------------------------------------------------------------------
// R12-2. The invisible set is still being extended by example.
//
// Round 11 added U+115F/U+1160 (the jamo counterparts of the already-banned
// U+3164) with the note that "listing one without the others was banning a
// construct by example". It then did the same thing again: U+2060 WORD JOINER
// is banned and U+2061..U+2064 — FUNCTION APPLICATION, INVISIBLE TIMES,
// INVISIBLE SEPARATOR, INVISIBLE PLUS — are its immediate neighbours in the
// same Cf block, render as nothing, and are not.
//
// U+FFF9..U+FFFB (the interlinear annotation controls) are the same story.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_the_invisible_operator_block_is_still_admitted() {
    // Positive control: the neighbour that IS banned. If this ever passes,
    // the loop below is measuring nothing.
    assert!(
        validate_device_name("Ben\u{2060}s Mac").is_err(),
        "U+2060 WORD JOINER is no longer refused; this test can no longer discriminate"
    );

    let mut admitted = Vec::new();
    for c in [
        '\u{2061}', '\u{2062}', '\u{2063}', '\u{2064}', // invisible operators
        '\u{FFF9}', '\u{FFFA}', '\u{FFFB}', // interlinear annotation
    ] {
        if validate_device_name(&format!("Ben{c}s Mac")).is_ok() {
            admitted.push(format!("U+{:04X}", c as u32));
        }
    }
    assert!(
        admitted.is_empty(),
        "characters that render as nothing are still accepted in a device name: {admitted:?}. \
         Each one produces a label pixel-identical to an honest peer's in the pairing list, \
         which is where the only authentication in the system is aimed"
    );
}

// ---------------------------------------------------------------------------
// R12-3. What a hostile but correctly-paired peer can do to the wire.
//
// These PASS. They are here so a later round cannot quietly remove a bound,
// and so the report can say which attacks were tried.
// ---------------------------------------------------------------------------

#[test]
fn r12_sec_a_hostile_hello_cannot_exhaust_downgrade_or_reach_a_screen() {
    // 1. Oversized name: bounded, refused, and the bound is on BYTES.
    let over = "x".repeat(MAX_DEVICE_NAME_BYTES + 1);
    let bytes = serde_json::to_vec(&serde_json::json!({
        "hello": { "protocol_version": PROTOCOL_VERSION, "device_id": dev(1).as_str(),
                   "device_name": over }
    }))
    .unwrap();
    assert!(matches!(SyncMessage::decode(&bytes), Err(WireError::DeviceName(_))));

    // 2. A name the DISPLAY policy hates must still decode, because a display
    //    string must never deny sync. This is the deliberate design choice the
    //    brief asks about, and it is safe as implemented ONLY because every
    //    consumer of the Hello name sanitises or discards it:
    //    `replicate::exchange` ignores it entirely (`SyncMessage::Hello { .. }`)
    //    and `pair_flow` hands it to `usable_peer_name`.
    let hostile = "Ben\u{202E}koobcaM sneB";
    assert!(validate_device_name(hostile).is_err(), "the display policy still refuses it");
    let bytes = serde_json::to_vec(&serde_json::json!({
        "hello": { "protocol_version": PROTOCOL_VERSION, "device_id": dev(2).as_str(),
                   "device_name": hostile }
    }))
    .unwrap();
    assert!(SyncMessage::decode(&bytes).is_ok(), "a hostile display string must not deny sync");
    // And the sanitiser the paired list uses actually removes the override,
    // rather than merely refusing and leaving the caller to guess.
    assert_eq!(sanitise_device_name(hostile).as_deref(), Some("BenkoobcaM sneB"));

    // 3. No version negotiation to downgrade.
    for v in [0u16, 1, 3, PROTOCOL_VERSION + 1, u16::MAX] {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "hello": { "protocol_version": v, "device_id": dev(3).as_str(),
                       "device_name": "G14" }
        }))
        .unwrap();
        assert!(
            matches!(SyncMessage::decode(&bytes), Err(WireError::ProtocolVersionMismatch { .. })),
            "protocol version {v} was not refused"
        );
    }

    // 4. An absent `device_name` is a decode error, not an empty label.
    let bytes = br#"{"hello":{"protocol_version":4,"device_id":"3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c01"}}"#;
    assert!(matches!(SyncMessage::decode(bytes), Err(WireError::Malformed(_))));
}

#[test]
fn r12_sec_an_absent_watermark_origin_means_the_conservative_thing() {
    // `#[serde(default)]` on `Watermark.origin`. Absent decodes to empty, and
    // empty means "re-offer the whole millisecond" — the safe direction, and
    // the v6 meaning. A peer omitting the field cannot narrow what we send.
    let bytes = br#"{"watermarks":{"entries":[{"source_device":"3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c01","clock":42}],"more":false}}"#;
    let SyncMessage::Watermarks { entries, .. } = SyncMessage::decode(bytes).unwrap() else {
        panic!("expected watermarks");
    };
    assert_eq!(entries[0].origin, "", "an absent origin must decode to the widest meaning");

    // A present but oversized origin is refused rather than truncated.
    let mut w = Watermark { source_device: dev(1), clock: 1, origin: "z".repeat(MAX_ORIGIN_ID_BYTES + 1) };
    assert!(matches!(w.validate(), Err(WireError::InvalidOriginId { .. })));
    // And an empty one is legal, which is what makes the default safe.
    w.origin = String::new();
    assert!(w.validate().is_ok());
}

#[test]
fn r12_sec_no_adversarial_field_value_panics_a_decoding_thread() {
    // Clocks and timestamps are NOT range-checked on the wire (deliberately —
    // the store applies the skew ceiling). Pin that the extremes at least
    // decode without arithmetic panicking a session thread.
    for (created, updated, clock) in [
        (i64::MIN, i64::MIN, 0u64),
        (i64::MAX, i64::MAX, u64::MAX),
        (-1, i64::MAX, u64::MAX),
    ] {
        let msg = SyncMessage::Items {
            items: vec![SyncItem {
                source_device: dev(1),
                origin_id: "row-1".into(),
                kind: ItemKind::Transcription,
                text: "x".into(),
                created_at: created,
                updated_at: updated,
                pinned: true,
                clock,
            }],
            more: false,
        };
        let bytes = msg.encode().expect("extremes encode");
        assert_eq!(SyncMessage::decode(&bytes).unwrap(), msg);
    }

    // A tombstone at the extremes, same question.
    let t = SyncMessage::Tombstones {
        entries: vec![Tombstone {
            source_device: dev(1),
            origin_id: "row-1".into(),
            deleted_at: i64::MIN,
            clock: u64::MAX,
        }],
        more: false,
    };
    assert!(t.encode().is_ok());

    // `DeviceId::short()` slices the first eight bytes. Every construction path
    // goes through `parse`, so the slice is always on a char boundary; a
    // non-hex or short id cannot be built at all.
    assert!(DeviceId::parse("3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5").is_err());
    assert_eq!(dev(1).short().len(), 8);
}

#[test]
fn r12_sec_a_batch_is_still_a_bound_on_allocation_not_a_verdict() {
    // Round 2's finding, re-pinned because `bounded_batch` is easy to lose in a
    // refactor and the failure is silent.
    let mut json = String::from(r#"{"tombstones":{"more":false,"entries":["#);
    for i in 0..40_000 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(
            r#"{"source_device":"3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c01","origin_id":"r","deleted_at":1,"clock":1}"#,
        );
    }
    json.push_str("]}}");
    assert!(json.len() < MAX_MESSAGE_BYTES, "the attack must fit inside the byte cap");
    match SyncMessage::decode(json.as_bytes()) {
        Err(WireError::BatchTooLong { len, .. }) => {
            panic!("the whole {len}-entry vector was built before the cap was consulted")
        }
        Err(_) => {}
        Ok(_) => panic!("an oversized batch must not decode"),
    }
    // Found-something control: a batch at the limit still decodes.
    let ok = SyncMessage::Items {
        items: (0..MAX_BATCH_LEN)
            .map(|i| SyncItem {
                source_device: dev(1),
                origin_id: format!("r{i}"),
                kind: ItemKind::Clipboard,
                text: "x".into(),
                created_at: 1,
                updated_at: 1,
                pinned: false,
                clock: 1,
            })
            .collect(),
        more: false,
    };
    assert!(ok.encode().is_ok(), "exactly at the limit must still work");
    assert!(MAX_ITEM_TEXT_BYTES * 2 < MAX_MESSAGE_BYTES);
}
