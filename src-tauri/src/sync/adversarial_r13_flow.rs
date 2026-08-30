//! ADVERSARIAL REVIEW, ROUND 13 — workflow, UI and identity.
//!
//! Target: commit `a1ceaf7` (round 12). The brief's rule is that the newest
//! code is the most dangerous code, and round 12 changed two identity gates,
//! one settings migration, one wire frame and five pieces of UI.
//!
//! Nothing here edits production code. Two kinds of test live here and the
//! difference is stated rather than blurred, following the convention round 12
//! established in `adversarial_r12_flow.rs`:
//!
//!   * RUNTIME tests, which drive real production functions.
//!   * SURFACE tests, which read a source file. The user-facing half of this
//!     product is React, there is no JS test runner in `package.json`, and
//!     `SyncManager::snapshot` is a private method on a struct that cannot be
//!     constructed without a real `tauri::AppHandle<Wry>`. Every surface test
//!     asserts its ANCHOR first — that the code it reasons about is present in
//!     the shape it expects — before asserting the property. A guard that can
//!     find nothing must first assert that it found something.

#![cfg(test)]

use parle_core::history::Store;
use parle_core::settings::Settings;
use parle_core::types::TranscriptionResult;
use parle_sync::PeerInfo;
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::sync::manager::{make_room_for_peer, note_peer_record, UiPaired};
use crate::sync::pair_flow::{
    looks_like_pairing_message, refusal_frame, spake2_msg_len, PairFlowError, RefusalCode,
    REFUSED_PREFIX,
};

const PAIRED_ID: &str = "22222222-2222-4222-8222-222222222222";
const ME: &str = "11111111-1111-4111-8111-111111111111";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

fn read_src(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The file with `//` line comments stripped, so prose cannot satisfy a guard
/// that is looking for code.
fn code_of(rel: &str) -> String {
    read_src(rel)
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Code with every run of whitespace removed, so a rustfmt line break inside an
/// expression cannot make an anchor miss.
fn squashed(rel: &str) -> String {
    code_of(rel).chars().filter(|c| !c.is_whitespace()).collect()
}

/// JSX/TS with `//` comments stripped and whitespace squashed.
fn squashed_tsx(rel: &str) -> String {
    read_src(rel)
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn tr(text: &str) -> TranscriptionResult {
    TranscriptionResult {
        raw_text: text.to_string(),
        text: text.to_string(),
        language: Some("en".into()),
        model_id: "whisper-small".into(),
        duration_ms: 1200,
        transcribe_ms: 300,
        segments: Vec::new(),
        trimmed: Vec::new(),
        low_confidence: Vec::new(),
        cleanup_tier: 1,
    }
}

fn paired(id: &str, name: &str) -> UiPaired {
    UiPaired {
        id: id.into(),
        name: name.into(),
        last_seen: None,
        online: false,
        last_sync_ok: None,
    }
}

fn record(id: &str, name: &str, last_octet: u8) -> PeerInfo {
    PeerInfo {
        id: parle_sync::DeviceId::parse(id).unwrap(),
        name: name.into(),
        addr: IpAddr::V4(Ipv4Addr::new(192, 168, 1, last_octet)),
        port: 51234,
    }
}

// ===========================================================================
// R13-A. The paired device list now reads its label off an UNSIGNED record.
// ===========================================================================
//
// `snapshot` used to take `UiPaired.name` from `complete_pairing`, which learns
// the name inside a Noise session keyed by the SPAKE2 secret. Round 12 replaced
// it with the name from `i.peers`, the mDNS map. mDNS is unsigned — the module
// doc on `PeerInfo` says so in capitals — so the label the user reads for an
// ALREADY PAIRED device is now whatever anyone on the LAN last announced under
// that device id.

/// The anchor for the whole of R13-A: `snapshot` really does read the paired
/// name out of the peer map.
#[test]
fn r13_flow_a0_snapshot_reads_the_paired_name_out_of_the_unsigned_peer_map() {
    let m = squashed("src-tauri/src/sync/manager.rs");
    // INVERTED. `snapshot` no longer reads the name out of the unsigned peer
    // map at all; the stored name is refreshed from the peer's Hello, which
    // arrives inside the Noise session and is the only statement of the name
    // nobody on the LAN can forge.
    assert!(
        !m.contains("name:i.peers.get(&p.id).map(|q|usable_peer_name(&q.name,&p.id))"),
        "snapshot takes the displayed name of a PAIRED device from `i.peers`, which is filled \
         from unsigned mDNS, so anyone on the LAN announcing that device's id can relabel an \
         already-authenticated peer in the Unpair confirmation"
    );
    assert!(
        m.contains("ifletSome(name)=stats.peer_name.clone(){"),
        "nothing refreshes the stored name from the authenticated Hello, so a renamed peer \
         keeps its old label for ever"
    );
    // And the map it reads is the one discovery fills.
    assert!(
        m.contains("note_peer_record(peers,last_dial,last_move,&id,p,known);"),
        "ANCHOR MISSING: decide_dial no longer feeds i.peers from mDNS"
    );
}

/// FINDING R13-A1 (HIGH). Anyone on the LAN can relabel a paired device.
///
/// This drives the REAL admission path — `make_room_for_peer` then
/// `note_peer_record`, exactly as `decide_dial` calls them — with an mDNS
/// record that claims a paired device's id and carries a name of the
/// attacker's choosing. The record lands in `i.peers`, replacing the genuine
/// one, and `usable_peer_name` (which is `sanitise_device_name` plus a
/// fallback) passes the attacker's string through untouched.
#[test]
fn r13_flow_a1_an_unsigned_mdns_record_relabels_a_paired_device() {
    let paired_list = vec![paired(PAIRED_ID, "Ben's MacBook Pro")];
    let mut peers: std::collections::HashMap<String, PeerInfo> = std::collections::HashMap::new();
    let mut last_dial = std::collections::HashMap::new();
    let mut last_move = std::collections::HashMap::new();

    // The genuine device announces itself first.
    assert!(make_room_for_peer(&mut peers, &paired_list, PAIRED_ID, true));
    note_peer_record(
        &mut peers,
        &mut last_dial,
        &mut last_move,
        PAIRED_ID,
        record(PAIRED_ID, "Ben's MacBook Pro", 7),
        true,
    );
    assert_eq!(
        peers.get(PAIRED_ID).map(|p| p.name.as_str()),
        Some("Ben's MacBook Pro"),
        "control: the genuine record must be in the map before the attack"
    );

    // Now anyone else on the LAN announces the same id from their own address.
    // Nothing authenticates this; the id travels in the clear in the TXT record.
    const LIE: &str = "Ben's Windows PC";
    assert!(make_room_for_peer(&mut peers, &paired_list, PAIRED_ID, true));
    note_peer_record(
        &mut peers,
        &mut last_dial,
        &mut last_move,
        PAIRED_ID,
        record(PAIRED_ID, LIE, 66),
        true,
    );

    assert_eq!(
        peers.get(PAIRED_ID).map(|p| p.name.as_str()),
        Some(LIE),
        "the unsigned record must have replaced the genuine one for the attack to work"
    );
    // And the character policy does not stand in the way: an ordinary name is
    // returned verbatim by the sanitiser snapshot applies.
    assert_eq!(
        parle_sync::sanitise_device_name(LIE).as_deref(),
        Some(LIE),
        "usable_peer_name passes an ordinary attacker-chosen name through unchanged"
    );
    // Which is the whole finding: the label beside the Unpair button, and the
    // name in the Unpair confirmation, is now attacker-controlled for a device
    // the user has already authenticated.
}

/// FINDING R13-A2 (MEDIUM). The displayed name FLAPS with mDNS presence.
///
/// The stored, authenticated name is never updated, so the paired row shows one
/// string while a record is being announced and a different one the moment it
/// stops. Both are "the name of this device" as far as the user is concerned.
#[test]
fn r13_flow_a2_the_paired_name_flaps_between_two_values_with_presence() {
    let stored = paired(PAIRED_ID, "Ben's MacBook Pro");
    let mut peers: std::collections::HashMap<String, PeerInfo> = std::collections::HashMap::new();
    let mut last_dial = std::collections::HashMap::new();
    let mut last_move = std::collections::HashMap::new();

    // snapshot's rule, transcribed from the anchor asserted in r13_flow_a0.
    let displayed = |peers: &std::collections::HashMap<String, PeerInfo>| -> String {
        peers
            .get(&stored.id)
            .map(|q| {
                parle_sync::sanitise_device_name(&q.name)
                    .unwrap_or_else(|| format!("unnamed device {}", &stored.id[..8]))
            })
            .unwrap_or_else(|| stored.name.clone())
    };

    let offline_before = displayed(&peers);
    note_peer_record(
        &mut peers,
        &mut last_dial,
        &mut last_move,
        PAIRED_ID,
        record(PAIRED_ID, "Ben-MBP-2", 7),
        true,
    );
    let online = displayed(&peers);
    peers.remove(PAIRED_ID); // PeerLost, or simply a goodbye an attacker sent
    let offline_after = displayed(&peers);

    assert_eq!(offline_before, "Ben's MacBook Pro");
    assert_eq!(online, "Ben-MBP-2", "online, the live mDNS name wins");
    assert_eq!(
        offline_after, "Ben's MacBook Pro",
        "offline, the stale stored name comes back: one device, two labels"
    );
    assert_ne!(
        online, offline_after,
        "the label a user reads for one paired device changes with mDNS presence alone"
    );
}

// ===========================================================================
// R13-B. The new pairing refusal frame.
// ===========================================================================

/// FINDING R13-B1 (LOW, but it is a false statement on a security gate).
///
/// `REFUSED_PREFIX`'s doc says the refusal "is a frame that
/// `looks_like_pairing_message` rejects by length, so it can never be mistaken
/// for an opening message". `looks_like_pairing_message` is `len ==
/// spake2_msg_len()`, and `spake2_msg_len()` is comfortably larger than the
/// 19-byte prefix, so a refusal frame of exactly that length sails through.
#[test]
fn r13_flow_b1_a_refusal_frame_is_not_rejected_by_length() {
    let n = spake2_msg_len();
    // The claim is only meaningful if a refusal CAN be that long.
    assert!(
        n > REFUSED_PREFIX.len(),
        "vacuous: spake2_msg_len() {n} is not longer than the {} byte prefix",
        REFUSED_PREFIX.len()
    );
    // A refusal frame is a FIXED length now (prefix + code byte + 4 bytes of
    // seconds), so the attacker pads it by hand rather than through the
    // builder, which no longer takes free text at all.
    let mut frame = refusal_frame(RefusalCode::LockedOut, 30);
    frame.truncate(REFUSED_PREFIX.len());
    frame.resize(n, b'x');
    assert_eq!(frame.len(), n, "constructed a refusal of exactly opening-message length");
    // INVERTED. The gate refuses it explicitly now rather than relying on a
    // length coincidence that was never true.
    assert!(
        !looks_like_pairing_message(&frame),
        "a refusal padded to opening-message length still passes the gate that protects the \
         rate limiter, so the two classifications overlap"
    );
    // Belt and braces: the code that reads it agrees this is a refusal, so the
    // two classifications genuinely overlap.
    assert!(frame.starts_with(REFUSED_PREFIX));
}

/// A peer that answers a dial with nothing but a refusal frame.
fn hostile_refuser(l: TcpListener, body: Vec<u8>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Ok((mut s, _)) = l.accept() else { return };
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
        // Swallow the mode byte and the victim's SPAKE2 opening message.
        let _ = crate::sync::wire_tcp::read_byte(&mut s);
        let _ = crate::sync::wire_tcp::read_frame(&mut s);
        let _ = crate::sync::wire_tcp::write_frame(&mut s, &body);
        // Bounded: close, so the victim can never hang this test.
    })
}

/// FINDING R13-B2 (MEDIUM). An unauthenticated peer chooses ~4 KB of the text
/// the user reads in the pairing dialogue, verbatim, with no character policy.
///
/// The refusal is read where a SPAKE2 reply is expected — before `verify_peer`,
/// so before anything is proven — and `pair_with` maps it to `why.clone()`,
/// which is what the pairing sheet shows. `refusal_frame` caps the reason at
/// 256 bytes, but that cap is on the SENDER. The reader's only bound is
/// `MAX_FRAME`, 4096.
///
/// In the same commit, `validate_device_name` grew a Unicode `Cf` ban and a
/// whitespace-collapse refusal so that a peer's 12-word LABEL cannot mislead
/// the user at this exact moment. This string is a hundred times longer, on the
/// same screen, and is filtered by nothing.
#[test]
fn r13_flow_b2_a_hostile_peer_writes_the_users_pairing_error_message() {
    use parle_sync::{PairingCode, PairingRole};

    // Everything `validate_device_name` refuses, plus enough length to fill the
    // dialogue. U+202E is the right-to-left override the identity module bans
    // by name; the newlines and the ASCII are ordinary phishing furniture.
    let mut evil = String::from(
        "\u{202E}Parle needs your Mac password to finish pairing.\n\n\
         Type it into the field on the other device.\n",
    );
    while evil.len() < 3_000 {
        evil.push_str("Support code 8842. ");
    }
    let body = {
        let mut v = REFUSED_PREFIX.to_vec();
        v.extend_from_slice(evil.as_bytes());
        v
    };
    assert!(body.len() > 256, "the point is that this is past refusal_frame's own cap");
    assert!(body.len() <= 4096, "and still inside MAX_FRAME, so read_frame accepts it");

    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let peer = hostile_refuser(l, body);

    // Exactly the shape `pair_with` sets up.
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut s = crate::sync::deadline::Timed::new(
        s,
        crate::sync::deadline::Deadline::after(Duration::from_secs(10)),
    );
    crate::sync::wire_tcp::write_byte(&mut s, crate::sync::wire_tcp::MODE_PAIR).unwrap();

    let code = PairingCode::parse("314159").unwrap();
    let r = crate::sync::pair_flow::run(&mut s, PairingRole::Responder, &code, (ME, "Victim"));
    let _ = peer.join();

    let why = match r {
        Err(PairFlowError::Refused(w)) => w,
        other => panic!("expected Refused, got {other:?}", other = other.err()),
    };
    // INVERTED. The frame carries a code byte and a number now, so the
    // attacker's 3 KB of copy cannot reach the sentence at all. The three
    // assertions below are the attack, run against the current reader.
    assert!(
        !why.contains("your Mac password"),
        "the attacker's own copy reaches the pairing screen, on a frame read before anything \
         about the peer has been authenticated"
    );
    assert!(
        !why.contains('\u{202E}'),
        "the bidi override `validate_device_name` exists to refuse arrives here untouched"
    );
    assert!(
        why.len() <= 256,
        "the reader has no bound of its own; got {} bytes",
        why.len()
    );
}

/// The anchor for R13-B2: `pair_with` really does surface it verbatim.
#[test]
fn r13_flow_b3_pair_with_surfaces_the_refusal_string_verbatim() {
    let m = squashed("src-tauri/src/sync/manager.rs");
    // The arm still surfaces the string verbatim, and that is now correct:
    // INVERTED at the source instead. `PairFlowError::Refused` carries a
    // sentence Parle chose from a one-byte code, so there is no peer-supplied
    // text for it to surface.
    let pf = squashed("src-tauri/src/sync/pair_flow.rs");
    assert!(
        pf.contains("enumRefusalCode{") && pf.contains("fnadvice(self,retry_secs:u32)->String{"),
        "the refusal still carries the peer's own words rather than a code Parle renders"
    );
    assert!(
        !pf.contains("v.extend_from_slice(reason.as_bytes()"),
        "the refusal frame still copies an arbitrary reason string onto the wire"
    );
    // Every other arm substitutes Parle's own words. This one does not.
    assert!(
        m.contains("\"Thatcodedidnotmatch.Checkthedigitsandtryagain.\".to_string()"),
        "ANCHOR MISSING: the surrounding arms have changed shape"
    );
}

/// The reverse direction, checked as the brief asks. A refusal DOES reach the
/// entering user, and `PairFlowError` is matched in exactly one place, so there
/// is no arm that silently drops the new variant.
#[test]
fn r13_flow_b4_refused_is_handled_everywhere_pairflowerror_is_matched() {
    let m = code_of("src-tauri/src/sync/manager.rs");
    let arms = m.matches("PairFlowError::Transport").count();
    assert!(arms >= 1, "ANCHOR MISSING: manager.rs no longer matches PairFlowError");
    assert_eq!(
        arms, 1,
        "PairFlowError is matched in more than one place in manager.rs; check each for a Refused arm"
    );
    // And nothing else in the crate matches it.
    for f in ["src-tauri/src/commands.rs", "src-tauri/src/lib.rs"] {
        assert!(
            !code_of(f).contains("PairFlowError::"),
            "{f} matches PairFlowError and was not checked for a Refused arm"
        );
    }
}

// ===========================================================================
// R13-C. `withheld` conflates "we deliberately withheld it" with "the insert
// failed", and both UIs return early on it.
// ===========================================================================

/// FINDING R13-C1 (MEDIUM). A failed history insert now produces NO user
/// feedback at all, in either window.
///
/// SURFACE. Every link is an asserted source fact:
///   1. `withheld: item_id < 0 || secrecy.keep_local_only()`.
///   2. the ordinary store branch is `(insert_transcription(..).unwrap_or(-1), None)`,
///      so an insert error yields item_id `-1` AND no notice, so no `Empty`.
///   3. `App.tsx` returns out of the whole handler on `e.withheld`, before the
///      only toast a `completed` event ever produces.
///   4. `Hud.tsx` returns before the only outcome a `completed` event ever sets.
/// Therefore: insert fails -> withheld true -> no Empty, no toast, no HUD line.
/// At 67ab14c the same event still produced "Inserted ..." / "Copied ...".
#[test]
fn r13_flow_c1_a_failed_insert_is_reported_to_the_user_as_nothing_at_all() {
    // Tolerant of the exact spelling, because `withheld` is being edited in the
    // working tree by another round-13 reviewer while this runs. What matters
    // is that `item_id < 0` still feeds it, which is true in every spelling so
    // far, including the committed one at a1ceaf7
    // (`withheld: item_id < 0 || secrecy.keep_local_only()`).
    let p = squashed("src-tauri/src/pipeline.rs");
    // INVERTED. `withheld` means withheld. A failed insert is an error and is
    // reported as one, instead of being folded into the flag that both
    // handlers use to return early.
    assert!(
        !p.contains("withheld:item_id<0||"),
        "`withheld` still folds a FAILED WRITE together with a deliberate withholding, and \
         both handlers return early on it, so a store that failed is reported as nothing"
    );
    assert!(
        p.contains("ifitem_id<0&&!secrecy.drop_entirely(){")
            && p.contains("PipelineEvent::Error{"),
        "nothing reports a failed history write, so the text is injected and no surface says \
         it was not saved"
    );
    assert!(
        p.contains("g.insert_transcription(tr,app_id.as_deref(),app_name.as_deref()).unwrap_or(-1),None,"),
        "ANCHOR MISSING: the ordinary store branch no longer maps an error to (-1, None)"
    );

    let app = squashed_tsx("src/App.tsx");
    assert!(
        app.contains("if(e.kind==='completed'){"),
        "ANCHOR MISSING: App.tsx no longer has a completed branch"
    );
    assert!(
        app.contains("if(e.withheld&&!e.injection?.manual_paste_required)return;"),
        "ANCHOR MISSING: App.tsx's early return is not immediately before the preview"
    );
    // There is no other toast for a completed event, so the return is total.
    let app_raw = read_src("src/App.tsx");
    let completed_toasts = app_raw.matches("showToast(").count();
    assert!(completed_toasts >= 3, "ANCHOR MISSING: App.tsx toasts have changed shape");

    let hud = squashed_tsx("src/Hud.tsx");
    assert!(
        hud.contains("if(e.kind==='completed'&&e.withheld&&!e.injection?.manual_paste_required)return;"),
        "ANCHOR MISSING: the HUD no longer returns early on withheld"
    );
    // Round 14: the instruction is NOT gated on `!withheld` any more, because
    // a withheld dictation still sits on the clipboard waiting to be pasted.
    // What stays gated is the transcript.
    assert!(
        hud.contains("e.kind==='completed'&&e.injection?.manual_paste_required"),
        "ANCHOR MISSING: the HUD's only completed outcome has changed shape"
    );

    // The precondition, proven rather than assumed: insert_transcription is a
    // fallible call whose error the pipeline swallows into -1. Show that the
    // happy path returns a POSITIVE id, so -1 really is the error sentinel and
    // not something an ordinary insert can produce.
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(ME);
    let id = s.insert_transcription(&tr("an ordinary dictation"), None, None).unwrap();
    assert!(id > 0, "an ordinary insert returns a real row id, so -1 can only be the error arm");
}

// ===========================================================================
// R13-D. `confirmDelete` fails OPEN.
// ===========================================================================

/// FINDING R13-D1 (MEDIUM). With sync switched off but devices still paired,
/// the confirmation promises nothing about travel, and the delete travels
/// anyway the moment sync is switched back on.
///
/// RUNTIME half: `delete_item_local` banks a durable tombstone with no
/// reference to whether sync is enabled. SURFACE half: the confirmation drops
/// every paired name when `st.enabled` is false.
#[test]
fn r13_flow_d1_a_delete_still_travels_when_sync_is_switched_off() {
    let h = squashed_tsx("src/views/History.tsx");
    // INVERTED. A delete banks a durable tombstone whatever the sync toggle
    // says, and it absorbs on the peer the moment sync comes back on, so the
    // warning must not depend on `enabled`.
    assert!(
        !h.contains("st.enabled?"),
        "History.tsx blanks the paired names when sync is switched off, but the delete still \
         travels: the tombstone is durable and absorbs on the peer when sync returns"
    );
    // And the disclosure it was copied FROM does not have that condition. The
    // round-12 comment says the capability existed and had simply not been
    // applied to the riskier action; the copy added a gate the original never
    // had, and `clearHistory` is the more destructive of the two.
    let sv = squashed_tsx("src/views/SettingsView.tsx");
    assert!(
        sv.contains(".then((st)=>setConfirmClear(st.paired.map((d)=>d.name)))"),
        "ANCHOR MISSING: the Clear-all disclosure has changed shape"
    );
    assert!(
        !sv.contains("setConfirmClear(st.enabled?"),
        "the Clear-all disclosure has grown the same enabled gate; this finding then covers both"
    );

    // Now the runtime half. Nothing about the store knows or cares whether the
    // sync toggle is on.
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(ME);
    let id = s.insert_transcription(&tr("a dictation that has already synced"), None, None).unwrap();
    // Control: no tombstone yet, so the assertion below cannot pass vacuously.
    assert!(
        s.tombstones_from(ME, 0, "", 100).unwrap().is_empty(),
        "control: there must be no tombstone before the delete"
    );

    s.delete_item_local(id).unwrap();

    let tombs = s.tombstones_from(ME, 0, "", 100).unwrap();
    assert_eq!(
        tombs.len(),
        1,
        "the delete banked a replicable tombstone while the user was told the delete was local"
    );
    assert_eq!(tombs[0].source_machine, ME);
}

/// FINDING R13-D2 (MEDIUM). An IPC failure is indistinguishable from "no
/// paired devices", and the destructive action proceeds either way.
///
/// `.catch(() => setPairedNames([]))` collapses "the backend did not answer"
/// into the same state as "there is nobody to warn about". The delete button is
/// not disabled while the fetch is in flight either, so the first click after
/// the History tab mounts races an IPC round trip.
#[test]
fn r13_flow_d2_a_failed_syncstatus_silently_downgrades_the_delete_warning() {
    let h = squashed_tsx("src/views/History.tsx");
    // INVERTED. `null` is "we do not know yet or the call failed" and `[]` is
    // "nobody to warn about". Collapsing them let a failed status call silently
    // downgrade the warning on an irreversible, travelling delete.
    // Repointed, not weakened. The roster is no longer its own state: History
    // keeps the whole `SyncStatus`, because the per-device row markers need the
    // local device id as well as the peer names, and DERIVES the roster from
    // it. The property under test is unchanged, and is now carried by the
    // derivation: a null status must yield a null roster, never an empty one.
    assert!(
        h.contains("useState<SyncStatus|null>(null)") && h.contains("pairedNames===null"),
        "the initial state and the failure state are the same value, so nothing downstream \
         can tell them apart and the delete proceeds either way"
    );
    assert!(
        h.contains("constpairedNames=syncStatus?syncStatus.paired.map((d)=>d.name):null;"),
        "the roster is no longer derived null-for-null, so 'we do not know' has collapsed \
         back into 'nobody to warn about' on an irreversible, travelling delete"
    );
    assert!(
        !h.contains(".catch(()=>setSyncStatus([]));")
            && h.contains(".catch(()=>setSyncStatus(null));"),
        "a failed syncStatus is still swallowed into the same empty list that means \
         'no paired devices'"
    );
    // And the button still fires on a false return only.
    assert!(
        h.contains("if(!confirmDelete(item))return;api.deleteItem(item.id)"),
        "ANCHOR MISSING: the delete no longer runs immediately after confirmDelete"
    );
}

// ===========================================================================
// R13-E. `Settings::migrate` and `excluded_defaults_seen`.
// ===========================================================================

fn tmp_dir(tag: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("r13-tmp").join(tag);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// DISPROVES A PREMISE. `default_excluded_apps()` is NOT platform-specific: it
/// flattens the macOS bundle ids and the Windows executable names into one
/// list, unconditionally. So a settings.json copied between a Mac and a PC
/// carries an identical `excluded_defaults_seen` and an identical
/// `excluded_apps`, and the union has nothing to disagree about.
#[test]
fn r13_flow_e1_the_shipped_exclusion_list_is_the_same_on_every_platform() {
    let d = Settings::default();
    for entry in [
        "com.1password.1password",
        "1Password.exe",
        "com.apple.Passwords",
        "KeePass.exe",
        "Authy Desktop.exe",
    ] {
        assert!(
            d.history.excluded_apps.iter().any(|a| a == entry),
            "{entry} is missing from the shipped list on this platform"
        );
        assert!(
            d.excluded_defaults_seen.iter().any(|a| a == entry),
            "{entry} is missing from excluded_defaults_seen on this platform"
        );
    }
    assert_eq!(
        d.history.excluded_apps, d.excluded_defaults_seen,
        "a fresh install has been offered exactly what it has"
    );
}

/// CONTROL for R13-E3. On an ordinary relaunch a deliberate removal stands.
#[test]
fn r13_flow_e2_a_removal_stands_across_an_ordinary_relaunch() {
    let p = tmp_dir("e2").join("settings.json");
    let mut s = Settings::default();
    s.history.excluded_apps.retain(|a| a != "com.apple.Passwords");
    s.save(&p).unwrap();

    let back = Settings::load(&p).unwrap();
    assert!(
        !back.history.excluded_apps.iter().any(|a| a == "com.apple.Passwords"),
        "the union re-added an entry the user removed, on an ordinary relaunch"
    );
}

/// FINDING R13-E3 (LOW, and it fails SAFE). A downgrade to a build that does
/// not know the field, followed by an upgrade, resurrects a deliberate removal.
///
/// The old build round-trips settings.json without `excluded_defaults_seen`
/// (there is no `deny_unknown_fields`, and it writes the struct it knows). On
/// the next upgrade the field is ABSENT, the field-level `#[serde(default)]`
/// fills it with an EMPTY vec rather than from `Settings::default()`,
/// `first_run_of_this_scheme` is true again, and every default is re-offered.
#[test]
fn r13_flow_e3_a_downgrade_then_upgrade_resurrects_a_deliberate_removal() {
    let p = tmp_dir("e3").join("settings.json");
    let mut s = Settings::default();
    s.history.excluded_apps.retain(|a| a != "com.apple.Passwords");
    s.save(&p).unwrap();

    // The downgrade: an older build parses the file, ignores the field it does
    // not know, and writes back what it knows. Modelled by deleting the key.
    let raw = std::fs::read_to_string(&p).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(
        v.get("excluded_defaults_seen").is_some(),
        "control: the field must be present before the downgrade removes it"
    );
    v.as_object_mut().unwrap().remove("excluded_defaults_seen");
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    // The upgrade.
    let back = Settings::load(&p).unwrap();
    assert!(
        back.history.excluded_apps.iter().any(|a| a == "com.apple.Passwords"),
        "if this fails the downgrade hole is closed and this finding is void"
    );
    // Stated plainly: the field-level default is empty, not the shipped list.
    let empty: Settings = serde_json::from_str("{}").unwrap();
    assert!(
        empty.excluded_defaults_seen.is_empty(),
        "a missing excluded_defaults_seen reads as EMPTY, not as Settings::default()'s list"
    );
    assert!(
        !empty.history.excluded_apps.is_empty(),
        "control: the container-level serde(default) still fills other missing fields"
    );
}

// ===========================================================================
// R13-F. Do the round-12 identity refusals deny sync to an honest machine?
// ===========================================================================
//
// The brief asks whether `name == name.split_whitespace().join(" ")` and the
// blanket `Cf` ban can refuse a real device. Answer: not through any path this
// app has, and these tests prove the invariant that makes that true.

/// Every name this app can put on the wire has been through
/// `sanitise_device_name`, and the sanitiser's output always satisfies the new
/// validator. So the whitespace refusal cannot deny sync to an honest machine.
#[test]
fn r13_flow_f1_no_sanitised_name_is_refused_by_the_new_validator() {
    let corpus = [
        "Ben's  MacBook Pro",          // doubled ASCII space
        "Ben's\tMacBook",              // tab
        "  Leading and trailing  ",    // both ends
        "Ben's\u{00A0}MacBook",        // NBSP
        "Ben=Work",                    // TXT separator
        "ベンジャミンのマックブックプロ本体", // non-Latin, multi-byte
        "پژمان\u{200C}نژاد",           // Persian with a load-bearing ZWNJ
        "Ben ✈️ travel Mac",            // emoji with U+FE0F
        "Family 👨‍👩‍👧 Mac",              // ZWJ sequence
        "\u{0600}Ben",                 // ARABIC NUMBER SIGN, a Cf that is not invisible
        &"x".repeat(200),              // over the byte budget
    ];
    let mut rescued = 0usize;
    for raw in corpus {
        let refused_raw = parle_sync::validate_device_name(raw).is_err();
        match parle_sync::sanitise_device_name(raw) {
            Some(clean) => {
                assert!(
                    parle_sync::validate_device_name(&clean).is_ok(),
                    "sanitise_device_name produced {clean:?} from {raw:?}, which the validator refuses"
                );
                if refused_raw {
                    rescued += 1;
                }
            }
            None => assert!(refused_raw, "the sanitiser gave up on a name the validator accepts"),
        }
    }
    // A guard that could find nothing must first assert that it found
    // something: the corpus really does contain names the new validator
    // refuses outright, and the sanitiser really did rescue them.
    assert!(
        rescued >= 6,
        "only {rescued} names in the corpus were refused raw and rescued by the sanitiser; \
         the invariant is not being exercised"
    );
}

/// The whitespace refusal is a REAL behaviour change on the inbound door, and
/// it is worth stating what it costs: a peer advertising a doubled space is now
/// invisible in the pairing list, with no diagnostic anywhere. That is only
/// reachable from a build older than 67ab14c (round 11), because from that
/// commit onwards every Parle install collapses its own name before it
/// advertises. Recorded, not claimed as a live defect.
#[test]
fn r13_flow_f2_a_doubled_space_is_now_refused_outright_not_collapsed() {
    assert!(
        parle_sync::validate_device_name("Ben's  MacBook Pro").is_err(),
        "the round-12 collapse check is not in force"
    );
    assert!(
        parle_sync::validate_device_name("Ben's MacBook Pro").is_ok(),
        "control: the collapsed form is accepted"
    );
    // And the same string is accepted by the sanitiser, which is what every
    // outbound path uses. The two doors disagree by design.
    assert_eq!(
        parle_sync::sanitise_device_name("Ben's  MacBook Pro").as_deref(),
        Some("Ben's MacBook Pro")
    );
}

/// The blanket `Cf` ban is wider than "invisible". These are format characters
/// that RENDER, and they are now refused on the inbound door and silently
/// stripped on the outbound one. No path in this app can turn that into a
/// denial of sync, because the outbound name is always sanitised first, but the
/// stripping is the same class of silent corruption the module's own comment
/// warns about for the variation selectors.
#[test]
fn r13_flow_f3_the_cf_ban_reaches_format_characters_that_are_not_invisible() {
    // U+0600 ARABIC NUMBER SIGN, U+06DD ARABIC END OF AYAH, U+070F SYRIAC
    // ABBREVIATION MARK. All Cf, all with visible effect in real text.
    for c in ['\u{0600}', '\u{06DD}', '\u{070F}'] {
        let name = format!("Ben {c}Mac");
        assert!(
            parle_sync::validate_device_name(&name).is_err(),
            "{c:?} is no longer refused; the Cf ban has been narrowed"
        );
        let cleaned = parle_sync::sanitise_device_name(&name).expect("something survives");
        assert!(!cleaned.contains(c), "{c:?} survived the sanitiser");
        assert_ne!(cleaned, name, "the sanitiser silently changed the user's name");
    }
    // ZWJ and ZWNJ are the deliberate exemptions and must stay exempt.
    for c in ['\u{200C}', '\u{200D}'] {
        assert!(
            parle_sync::validate_device_name(&format!("a{c}b")).is_ok(),
            "{c:?} must stay allowed; it is orthographically load-bearing"
        );
    }
}
