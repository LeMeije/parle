//! ADVERSARIAL REVIEW, ROUND 10 — secrets, security and cross-platform.
//!
//! Round 9 moved the invisible/bidi filter into `validate_device_name` so a
//! PEER's name meets it at both inbound gates. This file attacks that fix from
//! both sides: what the new set still lets through, and what it now wrongly
//! refuses.
//!
//! No sockets, no threads, no sleeps: nothing here can hang.

use echokey_sync::{
    sanitise_device_name, validate_device_name, DeviceId, ItemKind, SyncItem, SyncMessage,
    PROTOCOL_VERSION,
};

// ---------------------------------------------------------------------------
// R10-1. The filter's stated invariant is false: ordinary Unicode whitespace
// is not covered, and NBSP renders exactly like SPACE.
// ---------------------------------------------------------------------------

/// `is_invisible_or_bidi` promises, in its own doc comment, that two devices
/// cannot "show labels that are indistinguishable on screen and different on
/// the wire". Unicode whitespace defeats that with one substitution, and none
/// of it is `char::is_control` or in the round-9 set.
#[test]
fn r10_unicode_whitespace_walks_straight_through_the_invisible_filter() {
    let honest = "Ben's MacBook Pro";
    // Every one of these renders as a space (or as nothing at all) in a
    // proportional UI label.
    let confusables: [(&str, char); 6] = [
        ("NO-BREAK SPACE", '\u{00A0}'),
        ("EN QUAD", '\u{2000}'),
        ("EM SPACE", '\u{2003}'),
        ("HAIR SPACE", '\u{200A}'),
        ("NARROW NO-BREAK SPACE", '\u{202F}'),
        ("IDEOGRAPHIC SPACE", '\u{3000}'),
    ];

    let mut accepted = Vec::new();
    for (label, c) in confusables {
        let spoof = honest.replace(' ', &c.to_string());
        if validate_device_name(&spoof).is_ok() {
            assert_ne!(
                spoof.as_bytes(),
                honest.as_bytes(),
                "{label}: must differ on the wire or it is not a confusable"
            );
            accepted.push(label);
        }
    }

    // A guard that can find nothing must assert that it found something.
    assert!(
        accepted.is_empty(),
        "R10-1: a hostile LAN device can announce a name that renders like the \
         user's own machine, using: {accepted:?}"
    );
}

/// A name made only of whitespace passes, so the pairing list can be made to
/// show a BLANK row next to the honest one. `sanitise_device_name` refuses
/// this for our OWN name (it trims, then rejects the empty result); the peer
/// gate does not, because it only validates.
#[test]
fn r10_an_all_whitespace_peer_name_is_accepted_and_renders_blank() {
    let blank = "\u{00A0}\u{2003}\u{3000}";
    assert!(
        blank.trim().is_empty(),
        "premise: this is whitespace as far as any UI trim is concerned"
    );
    assert!(
        validate_device_name(blank).is_err(),
        "R10-1b: a peer may announce a device name that displays as nothing at all"
    );
    // Our own name cannot reach this state, which is the asymmetry:
    assert_eq!(
        sanitise_device_name(blank),
        None,
        "premise: the settings boundary already refuses it for us"
    );
}

// ---------------------------------------------------------------------------
// R10-2. The set over-corrected: U+FE00..U+FE0F bans ordinary emoji names.
// ---------------------------------------------------------------------------

/// Round 9 removed U+200C/U+200D because "both are orthographically required"
/// and stripping them "turned a family emoji into two people". The SAME
/// argument applies verbatim to VARIATION SELECTOR-16 (U+FE0F), which was
/// ADDED in the same edit: it is what makes ✈️ ❤️ ⚙️ render as emoji at all.
#[test]
fn r10_the_variation_selector_ban_refuses_ordinary_emoji_device_names() {
    let names = [
        "Ben's ✈\u{FE0F} Mac",   // aeroplane, emoji presentation
        "Ben ❤\u{FE0F} G14",     // heart
        "Studio ⚙\u{FE0F}",      // gear
    ];
    let mut refused = Vec::new();
    for n in names {
        if validate_device_name(n).is_err() {
            refused.push(n);
        }
    }
    assert!(
        refused.is_empty(),
        "R10-2: these are names a user types from the macOS emoji picker, and \
         the peer gate refuses them: {refused:?}"
    );
}

/// The half-handled emoji: round 9 kept U+200D so 👨‍💻 survives, but banned
/// U+FE0F, so any ZWJ sequence that also carries a presentation selector is
/// silently rewritten into a DIFFERENT sequence by the sanitiser. That is the
/// exact corruption the U+200C/U+200D removal was justified by.
#[test]
fn r10_the_sanitiser_still_silently_rewrites_a_zwj_emoji_sequence() {
    // couple with heart: 👩 ZWJ ❤ VS16 ZWJ 💋 ZWJ 👨
    let name = "Ben \u{1F469}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F48B}\u{200D}\u{1F468}";
    let out = sanitise_device_name(name).expect("something survives");
    assert!(
        out.contains('\u{200D}'),
        "premise: the ZWJ is deliberately preserved"
    );
    assert_eq!(
        out, name,
        "R10-2b: the sanitiser preserved the ZWJ and dropped the presentation \
         selector, so the stored name is a different sequence from the one typed"
    );
}

// ---------------------------------------------------------------------------
// R10-3. What a refused name costs: it is a hard failure of the whole feature.
// ---------------------------------------------------------------------------

/// `SyncMessage::Hello` validation is the FIRST message of every exchange, and
/// a bad name there is a decode error, not a display problem. Combined with
/// R10-2 that is an upgrade hazard: a Mac on round 10 and a Windows box still
/// on round 8 whose name carries a variation selector cannot sync at all, and
/// the user is shown a network failure.
#[test]
fn r10_a_refused_peer_name_kills_the_whole_hello_not_just_the_label() {
    // INVERTED: a display string can no longer deny sync.
    //
    // The finding was right and the blast radius was the point. `Hello` is the
    // FIRST message of every exchange, so putting the character policy there
    // made a name a decode error: a Mac and a Windows box could not sync AT
    // ALL, and the user was shown a network failure with no way to connect it
    // to a name. That is the `Ben=Work` failure mode returning by another door.
    //
    // The wire now BOUNDS the name (a cap on allocation, which is what the
    // message needs from it) and polices nothing. The character policy moved to
    // where the name is shown: discovery refuses to list such a peer, and
    // pairing sanitises before storing it.
    let hostile = "Ben\u{202E}koobcaM sneB".to_string();

    let hello = SyncMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: DeviceId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        device_name: hostile.clone(),
    };
    hello.validate().expect(
        "a hostile display name must not fail the first message of the exchange: the two \
         machines then cannot sync at all and the user sees a network error",
    );

    // Control: the wire still bounds what it must. An unbounded name is a
    // memory concern, not a cosmetic one, and it is still refused.
    let huge = SyncMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: DeviceId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        device_name: "x".repeat(10_000),
    };
    huge.validate()
        .expect_err("the wire must still refuse an unbounded device name");

    // And the policy still exists where it belongs: this name is not something
    // the user will ever be shown.
    assert!(
        echokey_sync::validate_device_name(&hostile).is_err(),
        "the display gate must still refuse a right-to-left override, or a hostile device can \
         present itself as the user's own machine in the pairing list"
    );
    assert!(
        echokey_sync::sanitise_device_name(&hostile)
            .map(|n| !n.contains('\u{202E}'))
            .unwrap_or(true),
        "and sanitising must strip it for anything that does get stored"
    );
}

// ---------------------------------------------------------------------------
// R10-4. Things that held. Recorded so the next round does not re-attack them.
// ---------------------------------------------------------------------------

/// The round-9 additions that ARE right, pinned so a later edit cannot quietly
/// drop them.
#[test]
fn r10_the_genuine_invisible_vectors_are_still_refused() {
    for (label, name) in [
        ("RLO override", "Ben\u{202E}koobcaM sneB"),
        ("zero-width space", "Ben\u{200B}s Mac"),
        ("soft hyphen", "Ben\u{00AD}s Mac"),
        ("Arabic letter mark", "Ben\u{061C}s Mac"),
        ("line separator", "Ben\u{2028}Mac"),
        ("paragraph separator", "Ben\u{2029}Mac"),
        ("Hangul filler", "Ben\u{3164}Mac"),
        ("TAG smuggling", "Ben\u{E0041}\u{E0042}"),
        ("word joiner", "Ben\u{2060}Mac"),
        ("BOM", "Ben\u{FEFF}Mac"),
    ] {
        assert!(
            validate_device_name(name).is_err(),
            "{label} must stay refused"
        );
    }
    // And the deliberate exemptions stay exempt.
    assert!(validate_device_name("کتاب\u{200C}های بن").is_ok(), "ZWNJ is required");
    assert!(
        validate_device_name("Ben \u{1F468}\u{200D}\u{1F4BB}").is_ok(),
        "ZWJ is required"
    );
}

/// Wire allocation is bounded on DECODE, not only on encode (criterion E).
/// Attacked and held: an oversized item is refused before the String is built
/// into a message we would act on, and the message cap is checked before the
/// JSON parse.
#[test]
fn r10_decode_refuses_an_oversized_item_rather_than_keeping_it() {
    let item = SyncItem {
        source_device: DeviceId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        origin_id: "o1".into(),
        kind: ItemKind::Transcription,
        text: "x".repeat(echokey_sync::MAX_ITEM_TEXT_BYTES + 1),
        created_at: 1,
        updated_at: 1,
        pinned: false,
        clock: 1,
    };
    let msg = SyncMessage::Items { items: vec![item], more: false };
    assert!(msg.encode().is_err(), "encode refuses it");
    // And the same rule runs on the way in.
    assert!(msg.validate().is_err(), "decode-side validate refuses it too");
}
