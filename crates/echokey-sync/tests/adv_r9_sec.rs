//! ADVERSARIAL REVIEW, ROUND 9 — secrets, security, cross-platform.
//!
//! Scope here: the device NAME, which is the label a user reads when deciding
//! which machine to type a 6-digit pairing code into, and which arrives from an
//! UNSIGNED mDNS record. Round 8 added `is_invisible_or_bidi` to stop a hostile
//! machine presenting a name that reads like the user's own laptop. These are
//! attacks on that fix.
//!
//! Everything in this file is pure logic: no sockets, no threads, no clocks, so
//! nothing here can hang.

use echokey_sync::identity::{sanitise_device_name, validate_device_name};

/// CONTROL for the whole file. The filter really does strip the characters it
/// names, so a failure below is a gap in the SET, not a broken harness. Without
/// this a bug that disabled the filter entirely would make every "the filter
/// misses X" test pass for the wrong reason.
#[test]
fn r9_control_the_filter_strips_what_it_claims_to_strip() {
    let hostile = "Ben\u{202E}s MacBook\u{200B}";
    let out = sanitise_device_name(hostile).expect("something survives");
    assert!(
        !out.contains('\u{202E}') && !out.contains('\u{200B}'),
        "the control character the filter names survived: {out:?}; the harness is wrong, \
         not the code"
    );
}

/// R9-N0. **The invisible-character filter is on the wrong side of the wire.**
///
/// `is_invisible_or_bidi` exists only inside `sanitise_device_name`, and every
/// caller of that function passes OUR OWN name: `commands::sync_set_device_name`
/// (what the user typed into their own settings), `SyncManager::set_device_name`
/// and `usable_device_name` (the stored or fallback local name).
///
/// A PEER's name never goes through it. The mDNS path
/// (`discovery::peer_from_service`) and the `Hello` message
/// (`wire::SyncMessage::validate`) both gate on `validate_device_name`, which
/// checks emptiness, byte length, `=` and `char::is_control` — and U+202E is
/// category Cf, not Cc, so it is not a control character.
///
/// So the fix protects the one name the user chose and can see, and does
/// nothing about the one that arrives from an unsigned mDNS record on a hostile
/// LAN, which is the threat its own doc comment describes: the label the user
/// reads when deciding which machine to type a 6-digit pairing code into.
#[test]
fn r9_n0_a_peers_name_never_meets_the_invisible_character_filter() {
    // The exact character the fix's doc comment calls out by name.
    let hostile = "Ben\u{202E}koobcaM sneB";

    // Control: our OWN name really is cleaned, so the filter works and this is
    // about which side it is applied to.
    let ours = sanitise_device_name(hostile).expect("something survives");
    assert!(
        !ours.contains('\u{202E}'),
        "the control is wrong: sanitise_device_name no longer strips U+202E"
    );

    // Control: the inbound gate is doing SOMETHING, so a failure below is a
    // gap and not a dead code path.
    assert!(
        validate_device_name("Ben=Work").is_err(),
        "the control is wrong: validate_device_name no longer refuses '='"
    );

    // The gate both inbound paths actually use.
    assert!(
        validate_device_name(hostile).is_err(),
        "a peer can announce {hostile:?} over unsigned mDNS and it reaches the pairing list \
         intact; only our own name is ever sanitised"
    );
}

/// R9-N1. The set is not the Unicode set it is trying to be.
///
/// `is_invisible_or_bidi` lists `200E..200F`, `202A..202E` and `2066..2069`.
/// Unicode's own `Bidi_Control` property is those PLUS **U+061C ARABIC LETTER
/// MARK**, and the invisible-format family it is trying to cover also contains
/// U+2060 WORD JOINER, the Hangul fillers (which render as blank in essentially
/// every font, the classic "invisible username" trick) and the TAG block
/// U+E0000..U+E007F, which is the ASCII-smuggling character set.
///
/// Each of these survives `sanitise_device_name` AND passes
/// `validate_device_name`, so each one lets a hostile LAN device put a name in
/// the pairing list that is pixel-identical to the user's own machine.
#[test]
fn r9_n1_invisible_characters_the_filter_does_not_know_about() {
    let missed: Vec<(&str, char)> = [
        ("U+061C ARABIC LETTER MARK (Bidi_Control)", '\u{061C}'),
        ("U+2060 WORD JOINER", '\u{2060}'),
        ("U+180E MONGOLIAN VOWEL SEPARATOR", '\u{180E}'),
        ("U+3164 HANGUL FILLER", '\u{3164}'),
        ("U+FFA0 HALFWIDTH HANGUL FILLER", '\u{FFA0}'),
        ("U+E0041 TAG LATIN CAPITAL A", '\u{E0041}'),
        ("U+FE0E VARIATION SELECTOR-15", '\u{FE0E}'),
    ]
    .into_iter()
    .filter(|(_, c)| {
        let name = format!("Mac{c}Book");
        sanitise_device_name(&name).is_some_and(|s| s.contains(*c))
    })
    .collect();

    assert!(
        missed.is_empty(),
        "{} invisible characters reach the pairing list: {}",
        missed.len(),
        missed.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
    );
}

/// R9-N2. `char::is_control` is Unicode category Cc only, so the LINE and
/// PARAGRAPH separators are not control characters by that test — and the
/// invisible-character filter does not list them either.
///
/// The result is a device name that renders as two lines in the pairing list.
/// `validate_device_name` accepts it, so it also goes straight into the mDNS
/// TXT record.
#[test]
fn r9_n2_line_and_paragraph_separators_are_neither_control_nor_filtered() {
    for (what, c) in [("U+2028 LINE SEPARATOR", '\u{2028}'), ("U+2029 PARAGRAPH SEPARATOR", '\u{2029}')] {
        assert!(!c.is_control(), "{what}: the premise of this test is gone, it is now a control char");
        let name = format!("Ben{c}Admin");
        let out = sanitise_device_name(&name);
        assert!(
            !out.as_deref().is_some_and(|s| s.contains(c)),
            "{what} survives sanitising as {out:?}"
        );
        assert!(
            validate_device_name(&name).is_err(),
            "{what} also passes validate_device_name, so it reaches the TXT record"
        );
    }
}

/// R9-N3. The other direction: the filter mangles names that are CORRECT.
///
/// U+200C and U+200D are not decoration in several scripts. In Persian the
/// zero-width non-joiner is orthographically required, and in emoji the
/// zero-width joiner is what makes a sequence one glyph. Stripping them turns
/// a legitimate name into a different, wrong one, silently, at the settings
/// boundary — the user types their machine's name and gets something else.
#[test]
fn r9_n3_the_filter_mangles_legitimate_non_latin_and_emoji_names() {
    // Persian "کتاب‌های بن": the ZWNJ is part of the spelling of کتاب‌ها.
    let persian = "کتاب\u{200C}های بن";
    let got = sanitise_device_name(persian).expect("something survives");
    assert_eq!(
        got, persian,
        "a required Persian zero-width non-joiner was stripped: {got:?}"
    );

    // "Ben 👨‍💻" is ONE glyph; without the ZWJ it is a man and a laptop.
    let emoji = "Ben \u{1F468}\u{200D}\u{1F4BB}";
    let got = sanitise_device_name(emoji).expect("something survives");
    assert_eq!(
        got, emoji,
        "an emoji ZWJ sequence was split into separate glyphs: {got:?}"
    );
}
