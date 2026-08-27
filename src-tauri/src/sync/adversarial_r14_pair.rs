//! ADVERSARIAL REVIEW, ROUND 14 — pairing refusal and device identity.
//!
//! Target: commit `517bc3d` (round 13). Round 13 rewrote the pairing refusal
//! (`RefusalCode`, `refusal_frame`, `refusal_reason`, the `REFUSED_PREFIX`
//! clause in `looks_like_pairing_message`) and moved the peer's display name
//! off unsigned mDNS and onto `RoundStats::peer_name`, taken from the `Hello`
//! inside the Noise session. That is the newest code, so it is where this round
//! looks first.
//!
//! Nothing here edits production code. Two kinds of test live here, and the
//! difference is stated rather than blurred, following the convention rounds 12
//! and 13 established:
//!
//!   * RUNTIME tests, which drive real production functions.
//!   * SURFACE tests, which read a source file. `SyncManager` holds a
//!     `tauri::AppHandle<Wry>` and cannot be constructed in a unit test, and
//!     `run_session` is a private method on it, so the wiring between
//!     `RoundStats::peer_name` and `settings.json` is only reachable this way.
//!     Every surface test asserts its ANCHOR first — that the code it reasons
//!     about is present in the shape it expects — before asserting the
//!     property. A guard that can find nothing must first assert that it found
//!     something.

#![cfg(test)]

use echokey_core::history::Store;
use echokey_sync::{
    sanitise_device_name, validate_device_name, ConfirmTag, PairedKey, Pairing, PairingCode,
    PairingRole, Session, SyncMessage, DeviceId, MAX_DEVICE_NAME_BYTES, PROTOCOL_VERSION,
};
use parking_lot::Mutex;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::guard::{GuardError, PairingGuard, MAX_PER_SOURCE};
use crate::sync::pair_flow::{
    self, looks_like_pairing_message, refusal_frame, spake2_msg_len, PairFlowError, RefusalCode,
    REFUSED_PREFIX,
};
use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};
use crate::sync::wire_tcp::{read_frame, write_frame};

const ME: &str = "11111111-1111-4111-8111-111111111111";
const PEER: &str = "22222222-2222-4222-8222-222222222222";
const IMPOSTOR: &str = "deadbeef-dead-4ead-8ead-deadbeefdead";

/// Nothing in this file may outlive this, on either side of any socket.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const BUDGET: Duration = Duration::from_secs(60);

fn evil() -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 168, 1, 66))
}

fn honest() -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7))
}

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

/// Code with every run of whitespace removed, so a rustfmt line break cannot
/// break a match.
fn tight(s: &str) -> String {
    s.split_whitespace().collect()
}

fn socket_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c = TcpStream::connect(addr).unwrap();
    let (srv, _) = l.accept().unwrap();
    for sock in [&c, &srv] {
        sock.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        sock.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
    }
    (c, srv)
}

/// What the entering machine ends up with, driving the REAL `pair_flow::run`.
///
/// `hostile` is handed the showing machine's end of the socket AFTER the
/// victim's opening SPAKE2 frame has been written, and may do whatever it
/// likes. The victim runs the real Responder path, exactly as `pair_with` does.
///
/// The result is flattened to a string so the whole thing is `Send` and so the
/// VARIANT is asserted rather than only the words.
fn victim_against(hostile: impl FnOnce(&mut TcpStream) + Send + 'static) -> String {
    let (mut victim_end, mut hostile_end) = socket_pair();
    let code = PairingCode::parse("314159").unwrap();
    let t = std::thread::spawn(move || {
        match pair_flow::run(&mut victim_end, PairingRole::Responder, &code, (ME, "Victim")) {
            Ok(p) => format!("PAIRED:{}", p.device_name),
            Err(PairFlowError::Refused(why)) => format!("REFUSED:{why}"),
            Err(PairFlowError::BadTag) => "BADTAG".to_string(),
            Err(PairFlowError::BadIdentity) => "BADIDENTITY".to_string(),
            Err(PairFlowError::Session(_)) => "SESSION".to_string(),
            Err(PairFlowError::Pairing(_)) => "PAIRING".to_string(),
            Err(PairFlowError::Version { .. }) => "VERSION".to_string(),
            Err(PairFlowError::Transport(_)) => "TRANSPORT".to_string(),
        }
    });
    // The victim writes its opening message before it reads anything.
    let _ = read_frame(&mut hostile_end);
    hostile(&mut hostile_end);
    let _ = hostile_end.flush();
    drop(hostile_end);
    t.join().expect("the victim thread must not panic")
}

/// One frame from the showing machine, and nothing else.
fn victim_reads(frame: Vec<u8>) -> String {
    victim_against(move |s| {
        let _ = write_frame(s, &frame);
    })
}

// ===========================================================================
// 1. The refusal frame, as hostile unauthenticated input.
// ===========================================================================

/// FINDING A1 (runtime). The 3600-second clamp bounds nothing that matters.
///
/// `refusal_reason` clamps the peer-supplied seconds to 3600 and calls that
/// "a peer-supplied number still reaches a sentence, so it must not be able to
/// make an absurd one". 3600 IS the absurd one. The guard that produces the
/// only honest `LockedOut` uses `backoff_for`, doubling from one second, and
/// `MAX_PER_SOURCE` is 3, so the largest `retry_in` any real Parle can ever
/// send is `backoff_for(3)` = FOUR seconds. The code itself dies at `CODE_TTL`
/// = 120 seconds, so any advice past two minutes is advice about a code that
/// no longer exists.
///
/// The frame is read BEFORE `verify_peer`, so nothing has been proven about who
/// sent it, and mDNS is unsigned, so the device the user tapped is whatever
/// answered. Anyone on the LAN can therefore put "That device will accept
/// another attempt in 3600 seconds" on the pairing screen: a one-hour wait
/// instruction that no honest Parle could produce, aimed at the exact moment
/// the user is trying to pair.
#[test]
fn r14_pair_a1_the_refusal_clamp_is_900_times_the_worst_an_honest_parle_can_say() {
    // -- control: what the REAL guard can actually ask for ------------------
    let mut g = PairingGuard::new();
    let t0 = Instant::now();
    g.begin("123456".into(), t0).unwrap();

    let mut worst_honest: u64 = 0;
    let mut locked_out_seen = 0usize;
    let mut granted = 0usize;
    // Walk the whole life of one source's allowance: guess, be locked out,
    // wait, guess again, until the source is exhausted.
    for step in 0..40u64 {
        let now = t0 + Duration::from_secs(step);
        match g.reserve(now, evil()) {
            Ok(_) => granted += 1,
            Err(GuardError::LockedOut { retry_in }) => {
                locked_out_seen += 1;
                // Round UP, because `as_secs()` truncates on the way out.
                let secs = retry_in.as_secs() + u64::from(retry_in.subsec_nanos() > 0);
                worst_honest = worst_honest.max(secs);
            }
            Err(_) => {}
        }
    }
    // A guard that could find nothing must first assert that it found
    // something: without these two the ceiling below is measured over an empty
    // set and passes for the wrong reason.
    assert_eq!(
        granted, MAX_PER_SOURCE as usize,
        "control: the source must actually spend its whole allowance"
    );
    assert!(
        locked_out_seen > 0,
        "control: the guard never produced a LockedOut, so worst_honest measures nothing"
    );
    assert!(
        worst_honest <= 4,
        "control: an honest guard asked for {worst_honest}s, so the arithmetic below is wrong"
    );

    // -- the attack --------------------------------------------------------
    let told = victim_reads(refusal_frame(RefusalCode::LockedOut, u32::MAX));
    assert!(
        told.starts_with("REFUSED:"),
        "an unauthenticated frame must at least be recognised as a refusal; got {told}"
    );
    // INVERTED. The clamp is the guard's OWN ceiling now, asked for rather
    // than restated, so `u32::MAX` renders as the longest wait an honest Parle
    // can ask for and not a second more.
    let clamped = u64::from(crate::sync::guard::max_honest_retry_secs());
    // A BOUND, not an equality: `worst_honest` is what one run of the guard
    // happened to ask for, and a backoff counts down in real time, so a sample
    // sits at or just under the ceiling. What must hold is that the clamp is
    // that ceiling and not a separate number somebody wrote down.
    assert!(
        clamped >= worst_honest && clamped <= worst_honest + 1,
        "the clamp ({clamped}s) and the worst an honest guard actually asks for \
         ({worst_honest}s) have drifted apart, so one of them is a restatement rather than \
         the real rule"
    );
    assert!(
        told.contains(&format!("{clamped} seconds")),
        "an unauthenticated peer can still put a wait of its own choosing on the pairing \
         screen; it produced {told:?}"
    );
}

/// FINDING A2 (runtime). "in 0 seconds" is reachable, honestly and hostilely.
///
/// `GuardError::LockedOut` carries a `Duration`; `manager::serve_pairing`
/// converts it with `as_secs()`, which TRUNCATES. Every honest backoff is a
/// whole number of seconds counted down in real time, so the last fraction of
/// every lockout renders as zero. An attacker gets there directly by sending
/// code byte 3 with four zero bytes.
#[test]
fn r14_pair_a2_a_lockout_of_zero_seconds_is_a_sentence_the_user_can_read() {
    let told = victim_reads(refusal_frame(RefusalCode::LockedOut, 0));
    assert!(told.starts_with("REFUSED:"), "got {told}");
    assert!(
        told.contains("in 0 seconds"),
        "expected the degenerate sentence, got {told:?}"
    );

    // The honest path reaches it too: `as_secs()` on any sub-second remainder.
    let remainder = Duration::from_millis(400);
    assert_eq!(remainder.as_secs(), 0, "control: as_secs truncates, it does not round");
}

/// The truncated, absurd and empty shapes. No panic, always a sentence, and
/// the code byte genuinely selects between five different ones.
#[test]
fn r14_pair_a3_every_malformed_refusal_still_produces_exactly_one_of_parles_own_sentences() {
    let full = refusal_frame(RefusalCode::LockedOut, 30);
    assert_eq!(
        full.len(),
        REFUSED_PREFIX.len() + 5,
        "control: the frame shape is prefix + code + 4 bytes"
    );

    let mut shapes: Vec<Vec<u8>> = Vec::new();
    // Prefix only, and every truncation of the tail.
    for keep in 0..=5 {
        shapes.push(full[..REFUSED_PREFIX.len() + keep].to_vec());
    }
    // Every code byte there is, including the four defined ones.
    for b in [0u8, 1, 2, 3, 4, 5, 99, 255] {
        let mut v = REFUSED_PREFIX.to_vec();
        v.push(b);
        v.extend_from_slice(&7u32.to_be_bytes());
        shapes.push(v);
    }
    // A refusal with kilobytes of tail: the reader's only bound is the 4 KB
    // frame limit, so this is the largest one that can arrive.
    let mut fat = refusal_frame(RefusalCode::Unknown, 1);
    fat.resize(4096, b'A');
    shapes.push(fat);

    let mut sentences = std::collections::BTreeSet::new();
    for shape in &shapes {
        let told = victim_reads(shape.clone());
        assert!(
            told.starts_with("REFUSED:"),
            "{shape:02x?} was not read as a refusal: {told}"
        );
        let words = told.trim_start_matches("REFUSED:").to_string();
        assert!(!words.is_empty(), "a refusal must never be a blank sentence");
        // Parle's own words only: no byte of the frame may appear.
        assert!(
            !words.contains('A'),
            "attacker bytes reached the sentence: {words:?}"
        );
        // The number is the only variable part, so compare TEMPLATES.
        sentences.insert(words.chars().filter(|c| !c.is_ascii_digit()).collect::<String>());
    }
    assert_eq!(
        sentences.len(),
        5,
        "the five RefusalCodes must produce five distinct sentences, got {sentences:#?}"
    );
}

/// FINDING A4 (runtime). `refusal_reason` is consulted at exactly ONE point,
/// and a refusal anywhere else is reported as the peer being untrustworthy.
///
/// A hostile peer that completes the SPAKE2 phase and then sends a refusal
/// where the confirmation tag belongs gets `BadTag`, which `pair_with` renders
/// as "make sure it really is your device". That is the right direction for a
/// hostile peer. It is recorded here because it is the boundary of the feature:
/// a refusal is only ever honoured as the FIRST frame, so a future change that
/// lets the showing machine refuse later would be silently unreadable.
#[test]
fn r14_pair_a4_a_refusal_sent_where_the_confirmation_tag_belongs_is_not_honoured() {
    let code = PairingCode::parse("314159").unwrap();
    let told = victim_against(move |s| {
        // A genuine opening message, so the victim's `finish` succeeds.
        let (_state, msg) = Pairing::start(PairingRole::Initiator, &code);
        let _ = write_frame(s, &msg);
        // Drain the victim's own tag first. Closing a socket with unread data
        // in the receive queue sends RST and destroys what we just wrote, which
        // would make this test read as a transport failure whatever the code
        // under test did.
        let _ = read_frame(s);
        // Now a refusal, where a 32-byte tag is expected.
        let _ = write_frame(s, &refusal_frame(RefusalCode::LockedOut, 30));
    });
    assert_eq!(
        told, "BADTAG",
        "a refusal after the opening frame must not be read as a refusal"
    );
}

/// A refusal sent AFTER the code is proven correct cannot say anything either:
/// by then the stream belongs to Noise, and a peer that does not hold the key
/// cannot produce a handshake message at all.
#[test]
fn r14_pair_a5_a_refusal_after_a_correct_code_is_only_a_broken_session() {
    let code = PairingCode::parse("314159").unwrap();
    let told = victim_against(move |s| {
        let (state, msg) = Pairing::start(PairingRole::Initiator, &code);
        let _ = write_frame(s, &msg);
        let peer_msg = match read_frame(s) {
            Ok(m) => m,
            Err(_) => return,
        };
        let (confirm, my_tag) = match state.finish(&peer_msg) {
            Ok(v) => v,
            Err(_) => return,
        };
        let _ = write_frame(s, my_tag.as_bytes());
        let peer_tag = match read_frame(s) {
            Ok(t) => t,
            Err(_) => return,
        };
        let tag: [u8; 32] = match peer_tag.try_into() {
            Ok(t) => t,
            Err(_) => return,
        };
        // The code IS correct, so both sides now hold the same key.
        assert!(
            confirm.verify_peer(&ConfirmTag::from_bytes(tag)).is_ok(),
            "control: this hostile peer must genuinely have passed verify_peer"
        );
        // A refusal where the Noise handshake belongs.
        let _ = s.write_all(&refusal_frame(RefusalCode::CodeExhausted, 30));
    });
    assert!(
        told == "SESSION" || told == "TRANSPORT",
        "a post-verification refusal must not become a refusal message; got {told}"
    );
}

// ===========================================================================
// 2. The REFUSED_PREFIX clause in looks_like_pairing_message.
// ===========================================================================

/// The mapping `serve_pairing` applies, replicated here ONLY because it is six
/// lines inline in a private method on a struct that cannot be built in a unit
/// test. `r14_pair_b0` anchors it against the real source before any test uses
/// it, so it cannot drift silently.
fn refusal_for(e: &GuardError) -> Vec<u8> {
    let (code, secs) = match e {
        GuardError::NotPairing => (RefusalCode::NotPairing, 0),
        GuardError::Expired => (RefusalCode::Expired, 0),
        GuardError::LockedOut { retry_in } => (RefusalCode::LockedOut, retry_in.as_secs() as u32),
        GuardError::CodeExhausted => (RefusalCode::CodeExhausted, 0),
    };
    refusal_frame(code, secs)
}

/// SURFACE anchor for `refusal_for`.
#[test]
fn r14_pair_b0_the_replicated_refusal_mapping_matches_the_production_one() {
    let m = tight(&code_of("src-tauri/src/sync/manager.rs"));
    for needle in [
        "GuardError::NotPairing=>(pair_flow::RefusalCode::NotPairing,0),",
        "GuardError::Expired=>(pair_flow::RefusalCode::Expired,0),",
        "(pair_flow::RefusalCode::LockedOut,retry_in.as_secs_f64().ceil().max(1.0)asu32,)",
        "GuardError::CodeExhausted=>(pair_flow::RefusalCode::CodeExhausted,0),",
        "write_frame(&muts,&pair_flow::refusal_frame(code,secs))",
    ] {
        assert!(
            m.contains(needle),
            "anchor missing from manager.rs, so `refusal_for` no longer mirrors production: {needle}"
        );
    }
}

/// FINDING B1 (runtime). The refusal frame is a FREE, UNLIMITED, LAN-wide
/// oracle for "is a pairing code on screen right now".
///
/// `PairingGuard::reserve` charges nothing when there is no active code
/// (`NotPairing` returns before `spent` is touched) and nothing once a source
/// has spent its three (`CodeExhausted` returns before it too). Round 12 made
/// that Err arm WRITE A FRAME naming which of the four states it was in.
/// Before that commit the socket simply closed, in every case, so an attacker
/// that had spent its allowance learned nothing at all.
///
/// The guard's own module header says the per-source carve-out exists because
/// "an automated attacker always wins the race to the next open slot". This
/// frame tells the attacker the instant the slot opens, for the price of 33
/// bytes of zeroes, for ever, without ever costing the user a guess.
#[test]
fn r14_pair_b1_the_refusal_frame_tells_anyone_on_the_lan_when_a_code_is_on_screen() {
    let mut g = PairingGuard::new();
    let t0 = Instant::now();

    // -- nothing on screen: probe as often as you like ---------------------
    const PROBES: usize = 500;
    let junk = vec![0u8; spake2_msg_len()];
    let mut idle_frames = Vec::new();
    for i in 0..PROBES {
        // The whole cost of one probe, exactly as `serve_pairing` spends it.
        assert!(
            looks_like_pairing_message(&junk),
            "control: the probe must get past the shape gate"
        );
        let e = g
            .reserve(t0 + Duration::from_millis(i as u64), evil())
            .expect_err("no code is live, so every probe must be refused");
        idle_frames.push(refusal_for(&e));
    }
    let idle = idle_frames[0].clone();
    assert!(
        idle_frames.iter().all(|f| *f == idle),
        "control: an idle machine must answer every probe the same way"
    );
    assert_eq!(
        idle,
        refusal_frame(RefusalCode::NotPairing, 0),
        "control: an idle machine answers NotPairing"
    );

    // -- a code appears. Nothing above was charged against it. -------------
    let t1 = t0 + Duration::from_secs(1);
    g.begin("123456".into(), t1).unwrap();
    let mut granted = 0usize;
    for step in 0..12u64 {
        if g.reserve(t1 + Duration::from_secs(step * 8), evil()).is_ok() {
            granted += 1;
        }
    }
    assert_eq!(
        granted, MAX_PER_SOURCE as usize,
        "control: {PROBES} free probes must not have spent any of the attacker's allowance"
    );

    // -- allowance gone, and the attacker can STILL tell, for free ---------
    let spent_state = g
        .reserve(t1 + Duration::from_secs(100), evil())
        .expect_err("the attacker's allowance is gone");
    let live = refusal_for(&spent_state);
    assert_ne!(
        live, idle,
        "the refusal frame must not distinguish 'a code is on screen' from 'none is'"
    );

    // And the user's own device is untouched by all of it, which is the
    // property that makes the oracle worth reporting rather than fatal.
    assert!(
        g.reserve(t1 + Duration::from_secs(100), honest()).is_ok(),
        "control: the honest device must still have its own allowance"
    );

    // What the two states read as, end to end through the real reader.
    let idle_words = victim_reads(idle);
    let live_words = victim_reads(live);
    assert_ne!(
        idle_words, live_words,
        "the two states must not be readable apart: {idle_words} / {live_words}"
    );
}

/// FINDING B2 (runtime). The `REFUSED_PREFIX` clause creates a frame the
/// showing machine drops for free, and that is NOT a lever on the code.
///
/// Round 13 added `!buf.starts_with(REFUSED_PREFIX)` because a refusal padded
/// to `spake2_msg_len()` passed the length check. The obvious worry is that an
/// attacker can now get a frame dropped without paying, and grind from there.
/// It cannot: the free path returns before `reserve` AND before
/// `Pairing::finish`, so no guess is tested and no reply is written. The cost
/// of the clause is one wasted TCP connection per probe, which the attacker
/// already had.
#[test]
fn r14_pair_b2_a_refused_prefixed_frame_is_free_but_buys_no_guesses() {
    let mut padded = refusal_frame(RefusalCode::LockedOut, 7);
    padded.resize(spake2_msg_len(), 0);
    assert_eq!(
        padded.len(),
        spake2_msg_len(),
        "control: the padding must reach the length gate for this to mean anything"
    );
    assert!(
        !looks_like_pairing_message(&padded),
        "round 13's prefix clause is gone: a padded refusal is admitted as an opening message"
    );
    assert!(
        looks_like_pairing_message(&vec![0u8; spake2_msg_len()]),
        "control: the same length WITHOUT the prefix must still be admitted"
    );

    // The free path spends nothing, and therefore learns nothing.
    let mut g = PairingGuard::new();
    let t0 = Instant::now();
    g.begin("123456".into(), t0).unwrap();
    let mut dropped = 0usize;
    for _ in 0..1000 {
        if !looks_like_pairing_message(&padded) {
            dropped += 1;
            continue;
        }
        let _ = g.reserve(t0, evil());
    }
    assert_eq!(dropped, 1000, "control: every probe must take the free path");
    // Every allowance is intact, attacker's included, because nothing was
    // charged and nothing was tested.
    assert!(g.reserve(t0, honest()).is_ok(), "the honest device is untouched");
    assert!(g.reserve(t0, evil()).is_ok(), "the attacker still has its own allowance");

    // And `serve_pairing` returns on that branch rather than replying, so the
    // free frame is not an oracle either. SURFACE, with its anchor.
    let m = tight(&code_of("src-tauri/src/sync/manager.rs"));
    assert!(
        m.contains("if!pair_flow::looks_like_pairing_message(&peer_first){"),
        "anchor missing: serve_pairing no longer shape-checks the opening frame"
    );
    let at = m.find("if!pair_flow::looks_like_pairing_message(&peer_first){").unwrap();
    let branch = &m[at..at + 120];
    assert!(
        !branch.contains("write_frame"),
        "the free-drop branch now writes something back: {branch}"
    );
}

// ===========================================================================
// 3. The peer name: RoundStats::peer_name -> the paired list.
// ===========================================================================

fn store_for(me: &str) -> Arc<Mutex<Store>> {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(me);
    Arc::new(Mutex::new(s))
}

fn both_kinds() -> Kinds {
    Kinds { dictations: true, clipboard: true }
}

/// One real exchange, both sides on their own thread under a wall-clock
/// budget, so a stall fails with a message rather than parking the suite.
///
/// `peer_declares` is the `(id, name)` the far side puts in its `Hello`. It is
/// deliberately allowed to differ from `honest_thinks_peer_is`, which is what
/// the near side's `Attribution` carries — that is the keychain-bound id, the
/// one `run_session` uses to find the row it writes.
fn exchange_with_declared_identity(
    peer_declares: (&'static str, &'static str),
    honest_thinks_peer_is: &'static str,
) -> RoundStats {
    let (sock_a, sock_b) = socket_pair();
    let key = PairedKey::from_bytes([9u8; 32]);
    let k2 = key.clone();
    let a_store = store_for(ME);
    let b_store = store_for(peer_declares.0);

    let (tx, rx) = mpsc::channel::<(&'static str, Result<RoundStats, String>)>();
    let tx2 = tx.clone();

    let far = std::thread::spawn(move || {
        let r = (|| {
            let mut s = Session::accept(sock_b, &k2).map_err(|e| e.to_string())?;
            let known = vec![ME.to_string(), PEER.to_string(), IMPOSTOR.to_string()];
            let attr = Attribution { peer_id: ME, local_id: peer_declares.0, known: &known };
            exchange(
                &mut s,
                &b_store,
                peer_declares,
                both_kinds(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
            .map_err(|e| e.to_string())
        })();
        let _ = tx2.send(("far", r));
    });

    let near = std::thread::spawn(move || {
        let r = (|| {
            let mut s = Session::initiate(sock_a, &key).map_err(|e| e.to_string())?;
            let known = vec![ME.to_string(), PEER.to_string(), IMPOSTOR.to_string()];
            let attr =
                Attribution { peer_id: honest_thinks_peer_is, local_id: ME, known: &known };
            exchange(
                &mut s,
                &a_store,
                (ME, "Near"),
                both_kinds(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::First,
                false,
                0,
                &|| false,
            )
            .map_err(|e| e.to_string())
        })();
        let _ = tx.send(("near", r));
    });

    let mut near_stats = None;
    let deadline = Instant::now() + BUDGET;
    for _ in 0..2 {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "the exchange did not finish inside {BUDGET:?}");
        match rx.recv_timeout(left) {
            Ok((who, r)) => {
                let stats = r.unwrap_or_else(|e| panic!("{who} side failed: {e}"));
                if who == "near" {
                    near_stats = Some(stats);
                }
            }
            Err(e) => panic!("an exchange thread died without reporting: {e}"),
        }
    }
    far.join().expect("far side panicked");
    near.join().expect("near side panicked");
    near_stats.expect("the near side reported")
}

/// The binding, proven rather than assumed: the name comes from the `Hello`
/// inside the session, the session is keyed by the PAIRED key for `peer_id`,
/// and the id the `Hello` CLAIMS is never consulted.
///
/// This is the good news of round 13's change. A peer that lies about its id
/// in the `Hello` does not move the name onto another device's row, because the
/// row is chosen by the keychain-bound `peer_id` and not by anything on the
/// wire. It is recorded here because the property is one `if` away from being
/// lost, and nothing else pins it.
#[test]
fn r14_pair_c1_the_name_is_bound_to_the_keyed_peer_not_to_the_id_the_hello_claims() {
    // Control: the ordinary case. The name arrives.
    let honest_stats = exchange_with_declared_identity((PEER, "Deck B"), PEER);
    assert_eq!(
        honest_stats.peer_name.as_deref(),
        Some("Deck B"),
        "control: the name must arrive at all, or the test below proves nothing"
    );

    // The lie: the far side declares a completely different device id. The
    // near side's Attribution still says PEER, because that is the id whose
    // key opened the session.
    let lying_stats = exchange_with_declared_identity((IMPOSTOR, "Impostor Deck"), PEER);
    assert_eq!(
        lying_stats.peer_name.as_deref(),
        Some("Impostor Deck"),
        "the name is taken from the Hello whatever id it claims"
    );

    // SURFACE: and it is applied to the row whose id is `run_session`'s
    // `peer_id`, which `dial` and `serve_session` both take from the keychain
    // lookup that keyed the session.
    let m = tight(&code_of("src-tauri/src/sync/manager.rs"));
    assert!(
        m.contains("ifletSome(name)=stats.peer_name.clone(){"),
        "anchor missing: the peer-name refresh is gone from manager.rs"
    );
    assert!(
        m.contains("ifletSome(d)=i.paired.iter_mut().find(|d|d.id==peer_id){")
            && m.contains("letfresh=usable_peer_name(&name,&peer_id);"),
        "the name is no longer written to the row selected by the keyed peer_id"
    );
    assert!(
        m.contains("letkey=matchkeystore::load(&peer_id){"),
        "anchor missing: peer_id is no longer the keychain lookup key"
    );
}

/// FINDING C2 (surface, with a runtime half). The one AUTHENTICATED statement
/// of a peer's name is written to memory and never to disk.
///
/// `run_session` calls `self.persist()` in exactly one place — inside
/// `if resend_all || stats.truncated`, the re-offer debt branch. The peer-name
/// refresh sits AFTER that branch and nothing persists again, so the fresh name
/// lives only in `Inner::paired`. `SyncManager::new` rebuilds `paired` from
/// `settings.json`, so every restart puts the stale name back, and it stays
/// there until the next FULLY SUCCESSFUL exchange with that device.
///
/// That is round 12's headline defect, "a renamed peer keeps its old name",
/// reintroduced by round 13's fix for it. Round 12's version read the name out
/// of `i.peers` inside `snapshot`, which is recomputed on every status call, so
/// it was fresh after a restart as soon as mDNS saw the device. Round 13 was
/// right to take the name off unsigned mDNS. It was not right to make the
/// authenticated value the only one that cannot survive a restart.
///
/// The smallest fix is one line: `self.persist();` after the refresh block.
#[test]
fn r14_pair_c2_the_authenticated_peer_name_is_never_written_to_settings_json() {
    let src = code_of("src-tauri/src/sync/manager.rs");
    let m = tight(&src);

    // Anchors. Each one is the code this test reasons about.
    assert!(
        m.contains("persist()"),
        "anchor missing: manager.rs has no persist() at all, so the search string is wrong"
    );
    assert!(
        m.contains("s.sync.paired=paired;"),
        "anchor missing: persist() no longer writes the paired roster"
    );

    let refresh_at = m
        .find("ifletSome(name)=stats.peer_name.clone(){")
        .expect("anchor missing: the peer-name refresh is gone from manager.rs");
    let end_of_run_session = m
        .find("fnprune_after_exchange(&self)")
        .expect("anchor missing: prune_after_exchange no longer follows run_session");
    assert!(
        end_of_run_session > refresh_at,
        "anchor wrong: prune_after_exchange is not after the refresh"
    );

    let tail = &m[refresh_at..end_of_run_session];
    // Control: the tail really is the end of run_session, not an empty slice.
    assert!(
        tail.contains("letfresh=usable_peer_name(&name,&peer_id);"),
        "control: the slice does not contain the refresh it is named for"
    );
    assert!(
        tail.contains("self.publish();"),
        "control: the slice does not reach the end of run_session"
    );
    assert!(
        tail.contains("d.last_seen=Some(now_ms());"),
        "control: the slice does not reach run_session's last write"
    );

    // INVERTED: the refresh persists now, so the name survives a restart.
    assert!(
        tail.contains("persist()"),
        "the authenticated peer name is written to memory only, so every restart restores \
         the stale one and it stays until the next fully successful exchange with that \
         device, which for a device that is switched off is never"
    );
}

/// FINDING C3 (runtime). An exchange that fails ANYWHERE after the `Hello`
/// throws the authenticated name away.
///
/// `peer_name` rides on `RoundStats`, which `exchange` returns BY VALUE and
/// only on `Ok`. The `Hello` is the first message and is complete and
/// authenticated long before anything else can go wrong, yet a peer with a
/// clock outside the skew window, an oversized row, or a dropped Wi-Fi never
/// gets its name refreshed. Combined with C2, the authenticated name reaches
/// the user only on a fully successful exchange, in memory, until the next
/// restart.
#[test]
fn r14_pair_c3_a_failure_after_the_hello_discards_the_name_it_already_proved() {
    let (sock_a, sock_b) = socket_pair();
    let key = PairedKey::from_bytes([9u8; 32]);
    let k2 = key.clone();
    let store = store_for(ME);

    let far = std::thread::spawn(move || {
        let mut s = match Session::accept(sock_b, &k2) {
            Ok(s) => s,
            Err(_) => return,
        };
        // A complete, valid, authenticated Hello...
        let _ = s.send(&SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::parse(PEER).unwrap(),
            device_name: "Renamed Deck".into(),
        });
        // ...and then the network dies.
        drop(s);
    });

    let known = vec![ME.to_string(), PEER.to_string()];
    let attr = Attribution { peer_id: PEER, local_id: ME, known: &known };
    let mut s = Session::initiate(sock_a, &key).expect("the near side handshakes");
    let r = exchange(
        &mut s,
        &store,
        (ME, "Near"),
        both_kinds(),
        Retention { oldest_allowed: None },
        &attr,
        Turn::First,
        false,
        0,
        &|| false,
    );
    far.join().expect("far side panicked");

    assert!(
        r.is_err(),
        "control: this exchange must actually fail, or the point below is untested"
    );
    // There is no surviving statement of the name. The RoundStats that held it
    // was dropped with the error.
    let e = r.err().unwrap().to_string();
    assert!(
        !e.contains("Renamed Deck"),
        "control: the error must not be carrying the name either: {e}"
    );
}

/// SURFACE. The comment that justifies NOT policing the name on the wire is
/// now false.
///
/// `wire::SyncMessage::validate` deliberately only bounds the `Hello` name
/// rather than running `validate_device_name`, and the reason it gives is
/// "`exchange` reads the Hello name and discards it, so nothing here reaches a
/// screen". Round 13 made `exchange` KEEP it, on `RoundStats::peer_name`, and
/// `run_session` writes it into the paired list, which is what the device list
/// and the Unpair confirmation render.
///
/// The outcome is still safe, because `usable_peer_name` sanitises before the
/// value is stored — `r14_pair_d1` proves that sanitising is total. But the
/// safety now rests on a call site in a different crate, while the comment
/// still claims it rests on the value never being displayed. That is the
/// justification for a security decision, and it is wrong.
#[test]
fn r14_pair_c4_the_wire_comment_that_licenses_an_unpoliced_name_is_stale() {
    // The claim spans a line break in the source, so match it whitespace-free.
    let wire = tight(&read_src("crates/echokey-sync/src/wire.rs"));
    assert!(
        wire.contains("Hellonameanddiscardsit,sonothingherereachesascreen"),
        "anchor missing: the claim is no longer in wire.rs, so nothing to report"
    );
    let rep = tight(&code_of("src-tauri/src/sync/replicate.rs"));
    assert!(
        rep.contains("stats.peer_name=Some(device_name);"),
        "control: exchange no longer keeps the name, so the comment would be true again"
    );
    let m = tight(&code_of("src-tauri/src/sync/manager.rs"));
    assert!(
        m.contains("letfresh=usable_peer_name(&name,&peer_id);") && m.contains("d.name=fresh;"),
        "control: the kept name no longer reaches the paired list"
    );
}

// ===========================================================================
// 4. validate_device_name and is_invisible_or_bidi.
// ===========================================================================

/// The claim a previous reviewer left behind: no name we ever put on the wire
/// can be refused, because every one is sanitised first. Verified here rather
/// than trusted, over a corpus far larger than a hand-written list.
///
/// The invariant that makes it true is `sanitise_device_name(x) == Some(y)`
/// implies `validate_device_name(y) == Ok`. In production that is guarded ONLY
/// by a `debug_assert!`, which is compiled out of a release build, so if it
/// ever fails it fails silently in the shipped app as `Discovery::start`
/// refusing to start and the UI blaming the network.
#[test]
fn r14_pair_d1_sanitising_a_name_is_total_so_no_outbound_name_can_be_refused() {
    let mut checked = 0usize;
    let mut survived = 0usize;

    let mut probe = |s: &str| {
        checked += 1;
        if let Some(clean) = sanitise_device_name(s) {
            survived += 1;
            assert!(
                validate_device_name(&clean).is_ok(),
                "sanitise_device_name({s:?}) produced {clean:?}, which the validator refuses"
            );
            assert!(
                clean.len() <= MAX_DEVICE_NAME_BYTES,
                "sanitise_device_name({s:?}) produced {} bytes",
                clean.len()
            );
        }
    };

    // Every char below the CJK block, plus the ranges the deny-list names, in
    // four positions each: alone, leading, trailing and internal.
    let ranges: [std::ops::RangeInclusive<u32>; 5] = [
        0x0000..=0x30FF,
        0xFE00..=0xFEFF,
        0xFF00..=0xFFAF,
        0xE0000..=0xE007F,
        0x1D170..=0x1D17F,
    ];
    for r in ranges {
        for cp in r {
            let Some(c) = char::from_u32(cp) else { continue };
            probe(&c.to_string());
            probe(&format!("{c}Ben"));
            probe(&format!("Ben{c}"));
            probe(&format!("Ben{c}Mac"));
        }
    }
    // Byte-budget edges, where the character-boundary truncation lives.
    for n in 60..=70 {
        probe(&"あ".repeat(n));
        probe(&"x".repeat(n));
        probe(&format!("{}{}", "x".repeat(n), " tail"));
        probe(&format!("{} {}", "x".repeat(n), "\u{00A0}"));
    }
    // Whitespace runs, which round 11 and round 12 both moved between
    // functions.
    for s in ["  ", " a ", "a  b", "a\t\tb", "a\u{00A0}\u{00A0}b", "\u{3000}a\u{3000}"] {
        probe(s);
    }

    assert!(
        checked > 50_000,
        "control: only {checked} names were tried, which is not a corpus"
    );
    assert!(
        survived > 10_000,
        "control: only {survived} names survived sanitising, so the assertion above \
         was mostly never evaluated"
    );
}

/// The cost of refusing a name outright, measured on real hostname shapes.
///
/// `validate_device_name` is called in exactly two production places, both in
/// `discovery.rs`: on OUR OWN name in `Discovery::start`, where a refusal stops
/// discovery and kills sync, and on a PEER's name in `peer_from`, where a
/// refusal removes that device from the pairing list entirely.
///
/// The first is unreachable, because `d1` proves sanitising is total and every
/// path into `Inner::device_name` sanitises. The second is reachable and IS the
/// cost: a device whose announced name carries a double space, a leading space,
/// an `=`, or an Arabic number sign cannot be paired with at all, and nothing
/// anywhere says why.
#[test]
fn r14_pair_d2_a_real_hostname_is_never_denied_sync_but_may_be_denied_a_row() {
    // Names Parle will happily carry, exactly as typed.
    let fine = [
        "Ben's MacBook Pro",
        "MacBook-Pro-de-Benjamin",
        "DESKTOP-4G9K2L",
        "benmac.local",
        "ベンジャミンのMac",
        "Бенджамин-ПК",
        "Ben's 💻",
        "Ben's ✈️ Mac",
        "کتاب\u{200C}های بن",
    ];
    for n in fine {
        assert!(
            validate_device_name(n).is_ok(),
            "an ordinary name is refused outright: {n:?}"
        );
    }

    // Names refused outright. Each is a shape a real machine can announce, and
    // each disappears from the pairing list rather than being cleaned up.
    let refused = [
        ("an equals sign, which macOS allows in a computer name", "Ben=Work"),
        ("a double space", "Ben's  MacBook Pro"),
        ("a leading space", " Ben's MacBook Pro"),
        ("a trailing space", "Ben's MacBook Pro "),
        ("65 bytes of Japanese", "あああああああああああああああああああああ ああ"),
        ("ARABIC NUMBER SIGN, a legitimate Cf in Arabic text", "بن\u{0600}"),
        ("a non-breaking space", "Ben's\u{00A0}MacBook Pro"),
    ];
    let mut refused_count = 0usize;
    for (what, n) in refused {
        assert!(
            validate_device_name(n).is_err(),
            "{what}: no longer refused outright, so the cost described here is gone: {n:?}"
        );
        refused_count += 1;
        // And every one of them IS recoverable, so refusing rather than
        // sanitising is a choice, not a necessity.
        assert!(
            sanitise_device_name(n).is_some(),
            "{what}: not even sanitisable, so refusing is the only option: {n:?}"
        );
    }
    assert_eq!(
        refused_count,
        refused.len(),
        "control: the loop did not run over every shape"
    );

    // The two call sites, anchored, so the cost above is attributed correctly.
    let d = tight(&code_of("crates/echokey-sync/src/discovery.rs"));
    assert!(
        d.contains("validate_device_name(&config.device_name)?;"),
        "anchor missing: Discovery::start no longer validates our own name"
    );
    assert!(
        d.contains("validate_device_name(&name).ok()?;"),
        "anchor missing: peer_from no longer validates the peer's name"
    );
    // And the wire does NOT, which is why a paired peer with such a name still
    // syncs while an unpaired one cannot be seen.
    let w = tight(&code_of("crates/echokey-sync/src/wire.rs"));
    assert!(
        !w.contains("validate_device_name"),
        "wire.rs polices the name again; the round-9 sync-denial regression is back"
    );
}
