//! ADVERSARIAL REVIEW, ROUND 8. Cross-platform and clipboard semantics.
//!
//! The question this file exists to answer: a user copies something on a
//! Windows PC and it lands in their history on a Mac, and back the other way.
//! Is that correct, safe and pleasant, given the two operating systems disagree
//! about almost everything to do with clipboards?
//!
//! Most of that scope is `#[cfg(windows)]` code that cannot be executed from a
//! Mac; those findings are recorded in the report as READ-ONLY with both sides
//! quoted. What CAN be executed from here is everything that happens once a
//! clipboard row exists: what the store keeps, what the wire carries, and what
//! the peer ends up holding. That is what this file pins down.
//!
//! Rules followed, from `SYNC_HANDOVER.md` section 4:
//! - every socket has read AND write timeouts, every loop a hard bound, and the
//!   whole exchange sits under a wall-clock budget on its own threads, so a
//!   stall FAILS naming the side that never returned (`sync_try`, below);
//! - every test that reports a defect carries a CONTROL that passes, so the
//!   assertion cannot be green for an unrelated reason. A guard that can find
//!   nothing must assert that it found something.

#![cfg(test)]

use echokey_core::history::{RemoteItem, Store};
use echokey_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";

/// Every exchange in this file must finish inside this, or the test fails and
/// says which side was still running.
const BUDGET: Duration = Duration::from_secs(60);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}

fn socket_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c = TcpStream::connect(addr).unwrap();
    let (srv, _) = l.accept().unwrap();
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

/// One exchange, `x` dialling, under a wall-clock budget.
///
/// Unlike `adversarial_r7_scale::sync_bounded` this one RETURNS the two
/// results instead of unwrapping them, because half of this file is about what
/// happens when an exchange fails. A side that never returns is still a failed
/// assertion naming the stall, never a parked suite.
fn sync_try(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
) -> (Result<RoundStats, String>, Result<RoundStats, String>) {
    let (sock_x, sock_y) = socket_pair();
    let key = PairedKey::from_bytes([9u8; 32]);
    let k2 = key.clone();
    let (x_store, y_store) = (x.0.clone(), y.0.clone());
    let (x_id, y_id) = (x.1, y.1);

    let (tx, rx) = mpsc::channel::<(&'static str, Result<RoundStats, String>)>();
    let tx2 = tx.clone();

    let acceptor = std::thread::spawn(move || {
        let r = (|| {
            let mut s = Session::accept(sock_y, &k2).map_err(|e| e.to_string())?;
            let known = vec![A.to_string(), B.to_string()];
            let attr = Attribution { peer_id: x_id, local_id: y_id, known: &known };
            exchange(
                &mut s,
                &y_store,
                (y_id, "peer"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
            .map_err(|e| e.to_string())
        })();
        let _ = tx2.send(("acceptor", r));
    });

    let dialler = std::thread::spawn(move || {
        let r = (|| {
            let mut s = Session::initiate(sock_x, &key).map_err(|e| e.to_string())?;
            let known = vec![A.to_string(), B.to_string()];
            let attr = Attribution { peer_id: y_id, local_id: x_id, known: &known };
            exchange(
                &mut s,
                &x_store,
                (x_id, "peer"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::First,
                false,
                0,
                &|| false,
            )
            .map_err(|e| e.to_string())
        })();
        let _ = tx.send(("dialler", r));
    });

    let mut got: Vec<(&'static str, Result<RoundStats, String>)> = Vec::new();
    let deadline = Instant::now() + BUDGET;
    while got.len() < 2 {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "the exchange did not finish inside {BUDGET:?}; {} of 2 sides returned",
            got.len()
        );
        match rx.recv_timeout(left) {
            Ok(r) => got.push(r),
            Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                "the exchange did not finish inside {BUDGET:?}; only {:?} returned",
                got.iter().map(|(who, _)| *who).collect::<Vec<_>>()
            ),
            Err(e) => panic!("both exchange threads died without reporting: {e}"),
        }
    }
    acceptor.join().expect("acceptor thread panicked");
    dialler.join().expect("dialler thread panicked");

    let mut d = None;
    let mut a = None;
    for (who, r) in got {
        match who {
            "dialler" => d = Some(r),
            _ => a = Some(r),
        }
    }
    (d.expect("dialler reported"), a.expect("acceptor reported"))
}

/// Put one row authored by `source` into `store` with an exact clock.
fn seed_one(store: &Arc<Mutex<Store>>, source: &str, origin: &str, text: &str, clock: i64) {
    store
        .lock()
        .apply_remote_item(
            source,
            &RemoteItem {
                source_machine: source.into(),
                origin_id: origin.into(),
                kind: "clipboard".into(),
                text: text.into(),
                created_at: clock,
                updated_at: clock,
                pinned: false,
            },
        )
        .unwrap();
}

/// Texts a store holds, sorted, whatever their source.
fn texts(store: &Arc<Mutex<Store>>) -> Vec<String> {
    let mut v: Vec<String> =
        store.lock().recent(None, 500).unwrap().into_iter().map(|i| i.text).collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------
// R8-1. A SINGLE OVERSIZED ROW WEDGES THE WHOLE PAIRING, FOR EVER, IN SILENCE.
//
// `wire::MAX_ITEM_TEXT_BYTES` is 1 MiB and the doc comment says oversized items
// are "REJECTED, never truncated". Nothing on the SERVING side ever consults
// that limit: `replicate::to_wire` copies `r.text` unconditionally and `serve`
// filters only on kind, so the refusal happens inside `session.send`, which
// fails the whole exchange rather than the row.
//
// Nothing capped the row on the way in either: `Store::insert_clipboard` stores
// whatever the platform monitor read, and neither `macos::read_clipboard` nor
// `windows::read_clipboard` has a size limit.
//
// The dialler serves BEFORE it drains (`Turn::First`), so the failure is
// bidirectional: the peer's rows never arrive either.
// ---------------------------------------------------------------------------
#[test]
fn r8_one_oversized_row_stops_the_pairing_permanently_in_both_directions() {
    let cap = echokey_sync::wire::MAX_ITEM_TEXT_BYTES;
    let base = now_ms() - 5_000_000;

    // CONTROL FIRST, so a green defect assertion cannot be green by accident.
    // The same shape of history, with the big row one byte UNDER the cap,
    // syncs completely. If this control ever fails, the test below is measuring
    // something other than the size limit and must not be believed.
    {
        let (a, b) = (store_for(A), store_for(B));
        seed_one(&a, A, "row-1", "before", base);
        seed_one(&a, A, "row-2", &"x".repeat(cap - 1), base + 1);
        seed_one(&a, A, "row-3", "after", base + 2);
        seed_one(&b, B, "row-b", "from the pc", base + 3);

        let (d, ac) = sync_try((&a, A), (&b, B));
        d.expect("control: the dialling side must complete");
        ac.expect("control: the accepting side must complete");

        assert!(
            texts(&b).contains(&"after".to_string()),
            "control: a row just under the cap must not block the rows behind it"
        );
        assert!(
            texts(&a).contains(&"from the pc".to_string()),
            "control: the peer's own row must reach the dialler"
        );
    }

    // ONE BYTE MORE. This used to be the defect; it is now the contract.
    //
    // Originally this asserted the exchange FAILED, three times running, and
    // that neither the row in front of the oversized one nor the peer's own
    // rows ever arrived. That was true and it was severe: `to_wire` copied the
    // text unconditionally, nothing on the serving side consulted
    // `MAX_ITEM_TEXT_BYTES`, so the refusal happened inside `session.send` and
    // killed the whole exchange. One 5 MB clipboard row stopped a pairing dead
    // and permanently, in both directions, deletes included.
    //
    // `serve` now skips an oversized row and lets the cursor advance past it,
    // so the assertions are inverted to pin the fixed behaviour: everything
    // else must get through, in both directions, and the skip must be
    // REPORTED rather than silent.
    let (a, b) = (store_for(A), store_for(B));
    seed_one(&a, A, "row-1", "before", base);
    seed_one(&a, A, "row-2", &"x".repeat(cap + 1), base + 1);
    seed_one(&a, A, "row-3", "after", base + 2);
    seed_one(&b, B, "row-b", "from the pc", base + 3);

    let mut reported = 0usize;
    // Hard bound, and three rounds because a fix that works once and then
    // stalls on the next exchange would be no fix at all.
    for round in 1..=3 {
        let (d, ac) = sync_try((&a, A), (&b, B));
        let stats = d.unwrap_or_else(|e| panic!("round {round}: the exchange still fails: {e}"));
        ac.unwrap_or_else(|e| panic!("round {round}: the accepting side still fails: {e}"));
        reported += stats.oversized;
    }
    assert!(
        reported >= 1,
        "the oversized row must be COUNTED, not silently dropped: the user is owed an \
         explanation for the one row that is missing on the other machine"
    );

    let on_b = texts(&b);
    assert!(
        on_b.iter().any(|t| t == "before"),
        "the row in front of the oversized one must still arrive: {on_b:?}"
    );
    assert!(
        on_b.iter().any(|t| t == "after"),
        "the row BEHIND the oversized one must still arrive: {on_b:?}"
    );
    assert!(
        !on_b.iter().any(|t| t.len() > cap),
        "the oversized row itself must NOT arrive: it cannot fit on the wire"
    );
    assert!(
        texts(&a).iter().any(|t| t == "from the pc"),
        "and the other direction must work too: the dialler serves before it drains, so a \
         failure here used to take the peer's rows down with it"
    );
}

// ---------------------------------------------------------------------------
// R8-2. THE WIRE STRIPS PROVENANCE, SO THE RECEIVING MACHINE'S OWN EXCLUSION
//       LIST CAN NEVER APPLY TO A ROW THAT ARRIVES FROM THE OTHER OS.
//
// `state.rs::on_platform_event` is the ONLY place `history.excluded_apps` is
// consulted, and it is consulted against the app reported at CAPTURE time on
// the capturing machine. `SyncItem` (wire.rs) and `RemoteItem` (history.rs)
// carry no app_id and no app_name, so the receiver is handed text with no idea
// where it came from.
//
// Combined with the fact that the identifier differs per OS — `macos.rs`
// returns a bundle id plus a localised name, `windows.rs` returns an exe name
// and `None` — a user's exclusion list is not portable and cannot be applied on
// arrival even if it were.
// ---------------------------------------------------------------------------
#[test]
fn r8_a_replicated_row_arrives_with_no_source_app_so_exclusions_cannot_be_applied() {
    let (a, b) = (store_for(A), store_for(B));

    // Captured on A from a password manager. This is exactly what the monitor
    // hands `insert_clipboard`.
    a.lock()
        .insert_clipboard("correct horse battery staple", Some("1Password.exe"), None)
        .unwrap();

    // CONTROL: the provenance really is stored locally, so if it is missing on
    // the peer that is the wire's doing and not a store that never kept it.
    let local = a.lock().recent(None, 10).unwrap();
    assert_eq!(local.len(), 1);
    assert_eq!(
        local[0].app_id.as_deref(),
        Some("1Password.exe"),
        "control: the capturing machine DOES record which app the text came from"
    );

    let (d, ac) = sync_try((&a, A), (&b, B));
    d.expect("dialler completed");
    ac.expect("acceptor completed");

    let arrived = b.lock().recent(None, 10).unwrap();
    assert_eq!(arrived.len(), 1, "the row crossed");
    assert_eq!(arrived[0].text, "correct horse battery staple");
    assert_eq!(
        arrived[0].app_id, None,
        "the receiving machine is told the text but not where it came from, so its own \
         excluded_apps list has nothing to match on"
    );
    assert_eq!(arrived[0].app_name, None);
}

// ---------------------------------------------------------------------------
// R8-3. COPYING A SYNCED ROW RE-AUTHORS IT AND SENDS IT BACK AS A DUPLICATE.
//
// `commands::copy_item` calls `platform::imp::write_clipboard`. On macOS that
// is the UNMARKED write (`write_clipboard_impl(text, false, false)`) — no
// TransientType — so the app's own monitor captures it 400 ms later and calls
// `insert_clipboard`, whose dedupe only ever compares against OUR OWN most
// recent clipboard row. A row authored by the other machine is invisible to
// that comparison by design, so a second copy of the same text is created,
// stamped with OUR device id and a fresh origin id, and replicates back.
//
// On Windows `write_clipboard` always sets
// `ExcludeClipboardContentFromMonitorProcessing`, which `clipboard_is_excluded`
// checks, so the same user action produces no duplicate there. The two
// platforms therefore behave differently for the single most common action in
// the product.
// ---------------------------------------------------------------------------
#[test]
fn r8_copying_a_peers_row_creates_a_second_copy_that_syncs_back() {
    let base = now_ms() - 5_000_000;
    let (a, b) = (store_for(A), store_for(B));

    // A authored it; it has already synced to B.
    seed_one(&a, A, "row-1", "the shared snippet", base);
    let (d, ac) = sync_try((&a, A), (&b, B));
    d.unwrap();
    ac.unwrap();
    assert_eq!(b.lock().count().unwrap(), 1, "the row reached B");

    // On B the user copies something of their own in between.
    b.lock().insert_clipboard("something else", None, None).unwrap();

    // CONTROL: with "the shared snippet" as B's own most recent clipboard row,
    // a re-copy dedupes into it and creates nothing. This is the branch that
    // makes the defect below look impossible if you only read the happy path.
    {
        let (c, _) = (store_for(A), ());
        c.lock().insert_clipboard("same text", None, None).unwrap();
        let before = c.lock().count().unwrap();
        c.lock().insert_clipboard("same text", None, None).unwrap();
        assert_eq!(
            c.lock().count().unwrap(),
            before,
            "control: insert_clipboard DOES dedupe when the previous row is our own"
        );
    }

    // Now the user opens the palette on B and presses Enter on the row that
    // came from A. macOS: unmarked clipboard write -> our own monitor sees it.
    b.lock().insert_clipboard("the shared snippet", None, None).unwrap();

    // Ids first, then sources: `parking_lot::Mutex` is not reentrant, and a
    // second lock taken inside a statement that still holds the first is a
    // deadlock, not an error.
    let ids: Vec<i64> = b
        .lock()
        .recent(None, 50)
        .unwrap()
        .into_iter()
        .filter(|i| i.text == "the shared snippet")
        .map(|i| i.id)
        .collect();
    let copies: Vec<String> = ids
        .iter()
        .map(|id| b.lock().source_machine_of(*id).unwrap().unwrap_or_default())
        .collect();
    assert_eq!(
        copies.len(),
        2,
        "copying a peer's row created a SECOND row holding the same text: {copies:?}"
    );
    assert!(copies.contains(&A.to_string()) && copies.contains(&B.to_string()));

    // And it goes back across the wire as a new row.
    let (d, ac) = sync_try((&a, A), (&b, B));
    d.unwrap();
    ac.unwrap();
    let on_a = texts(&a);
    assert_eq!(
        on_a.iter().filter(|t| *t == "the shared snippet").count(),
        2,
        "the authoring machine now holds the row twice: {on_a:?}"
    );
}

// ---------------------------------------------------------------------------
// R8-4. TEXT FIDELITY. What survives the store and the wire unchanged.
//
// This one is a positive guard, so it asserts that it actually exercised the
// hard cases rather than only that the easy ones passed.
// ---------------------------------------------------------------------------
#[test]
fn r8_awkward_text_survives_the_store_and_the_wire_byte_for_byte() {
    let base = now_ms() - 5_000_000;
    let (a, b) = (store_for(A), store_for(B));

    let payloads: Vec<(&str, String)> = vec![
        ("crlf", "line one\r\nline two\r\n".to_string()),
        ("lf", "line one\nline two\n".to_string()),
        ("lone cr", "old\rmac".to_string()),
        ("nul", "before\u{0}after".to_string()),
        ("rtl", "مرحبا بالعالم".to_string()),
        ("bidi override", "safe\u{202E}txt.exe".to_string()),
        ("emoji zwj", "👩‍👩‍👧‍👦 family".to_string()),
        ("combining", "e\u{0301}gal".to_string()),
        ("astral", "𝕳𝖊𝖑𝖑𝖔".to_string()),
        ("long single line", "z".repeat(200_000)),
    ];

    // Assert the fixtures really are awkward, or this test proves nothing.
    assert!(payloads.iter().any(|(_, t)| t.contains('\r')));
    assert!(payloads.iter().any(|(_, t)| t.contains('\u{0}')));
    assert!(payloads.iter().any(|(_, t)| t.chars().any(|c| c as u32 > 0xFFFF)));

    for (i, (_, text)) in payloads.iter().enumerate() {
        seed_one(&a, A, &format!("row-{i}"), text, base + i as i64);
    }

    let (d, ac) = sync_try((&a, A), (&b, B));
    d.unwrap();
    ac.unwrap();

    let got: Vec<String> = b.lock().recent(None, 50).unwrap().into_iter().map(|i| i.text).collect();
    assert_eq!(got.len(), payloads.len(), "every payload crossed");
    for (label, text) in &payloads {
        assert!(
            got.iter().any(|g| g == text),
            "{label}: the text did not survive the round trip unchanged"
        );
    }

    // The load-bearing half: nothing normalises line endings anywhere. That is
    // correct for a byte-exact store and it is exactly why a Windows-authored
    // paragraph shows up on the Mac still carrying its CR characters, and why
    // the same paragraph copied on both machines is two rows that never merge.
    let crlf = got.iter().find(|g| g.contains("\r\n")).expect("a CRLF row survived as CRLF");
    let lf = got.iter().find(|g| g.contains('\n') && !g.contains('\r')).unwrap();
    assert_ne!(
        crlf.replace("\r\n", "\n"),
        **crlf,
        "the CRLF row is still CRLF after the trip, so the two OSes store different bytes \
         for the same visible text"
    );
    assert_ne!(crlf.as_str(), lf.as_str(), "and the LF twin remains a separate row");
}

// ---------------------------------------------------------------------------
// R8-6. THE SHIPPED DEFAULT EXCLUSION LIST IS NOT SYMMETRIC ACROSS THE TWO
//       OPERATING SYSTEMS, AND SYNC MAKES THAT GAP TRAVEL.
//
// `HistorySettings::default().excluded_apps` carries macOS bundle ids and
// Windows exe names in one list, which is the right shape. But the two halves
// do not name the same products. Every manager present on only ONE side is a
// secret that this app will decline to store on one machine and then accept
// from the other over LAN sync, because nothing on the receiving side re-checks
// anything (see R8-2: the wire does not even carry the app).
// ---------------------------------------------------------------------------
#[test]
fn r8_the_default_exclusion_list_protects_a_password_manager_on_only_one_of_the_two_platforms() {
    // INVERTED: this now pins SYMMETRY, having demonstrated the lack of it.
    //
    // The list was two hand-kept halves and they had drifted. LastPass carried
    // a macOS bundle id and no Windows exe name, and KeePass 2.x (a different
    // product from KeePassXC) was in neither. That is not a smaller hole on one
    // platform: the exclusion rule runs once, at capture, on the capturing
    // machine, so a manager missing from the Windows half means the PC captures
    // the password and replicates it to the Mac, which would have refused it.
    //
    // The list is generated from one table of products now, so every entry has
    // both identifiers by construction. This test is what stops the halves
    // drifting apart again.
    let list = echokey_core::settings::Settings::default().history.excluded_apps;

    let mac: Vec<String> = list
        .iter()
        .filter(|e| e.starts_with("com.") || e.starts_with("org.") || e.starts_with("in."))
        .map(|e| e.to_ascii_lowercase())
        .collect();
    let win: Vec<String> = list
        .iter()
        .filter(|e| e.to_ascii_lowercase().ends_with(".exe"))
        .map(|e| e.to_ascii_lowercase())
        .collect();

    // CONTROL: both halves are populated, so a "no gaps" result below cannot
    // come from an empty list on one side.
    assert!(!mac.is_empty() && !win.is_empty(), "both halves are populated: {list:?}");
    assert!(mac.len() >= 5 && win.len() >= 5, "both halves are real: {list:?}");

    // Every product must appear on BOTH sides.
    for product in ["1password", "bitwarden", "lastpass", "keepassxc", "dashlane", "enpass"] {
        assert!(
            mac.iter().any(|e| e.contains(product)),
            "{product} is missing from the macOS half: {mac:?}"
        );
        assert!(
            win.iter().any(|e| e.contains(product)),
            "{product} is missing from the Windows half, so a password copied from it on the \
             PC is captured there and then synced to the Mac, which would have refused it: {win:?}"
        );
    }

    // KeePass 2.x is a different product and a different executable from
    // KeePassXC, and was absent from both halves.
    let keepass_classic = |v: &[String]| {
        v.iter().any(|e| e.contains("keepass") && !e.contains("keepassxc"))
    };
    assert!(
        keepass_classic(&mac) && keepass_classic(&win),
        "KeePass 2.x is covered on neither platform: mac {mac:?}, win {win:?}"
    );
}

// ---------------------------------------------------------------------------
// R8-5. DEVICE NAMES: what `sanitise_device_name` lets through.
//
// The gate rejects control characters and `=` and trims to 64 BYTES. It does
// not touch bidi controls, zero-width characters or soft hyphens, none of which
// are `char::is_control`. The name arrives from an UNSIGNED mDNS TXT record —
// `identity::PeerInfo` says so itself — and is what the user reads in the
// pairing list when deciding which machine to type a 6-digit code into.
// ---------------------------------------------------------------------------
#[test]
fn r8_device_names_pass_invisible_and_direction_changing_characters() {
    use echokey_sync::{sanitise_device_name, validate_device_name};

    // CONTROL: the gate does reject what it claims to reject, so a pass below
    // is a gap in the rule and not a broken import.
    assert_eq!(sanitise_device_name("Ben=Work").as_deref(), Some("BenWork"));
    assert!(validate_device_name("has=equals").is_err());
    assert!(validate_device_name("has\nnewline").is_err());
    assert_eq!(sanitise_device_name("   "), None);

    // Realistic hostnames from both operating systems all survive intact.
    for host in ["DESKTOP-4K2J9A1", "Bens-MacBook-Pro.local", "ASUS-G14", "MacBook-Pro-2"] {
        assert_eq!(
            sanitise_device_name(host).as_deref(),
            Some(host),
            "an ordinary hostname must be left alone"
        );
    }

    // The gap. Each of these is accepted verbatim and shown to the user.
    let sneaky = [
        ("bidi override", "Ben's Mac\u{202E}kcaM s'reggoL"),
        ("zero width space", "Ben's\u{200B} Mac"),
        ("soft hyphen", "Ben's\u{00AD}Mac"),
        ("rtl mark", "G14\u{200F}"),
    ];
    for (label, raw) in sneaky {
        let out = sanitise_device_name(raw)
            .unwrap_or_else(|| panic!("{label}: expected it to be accepted"));
        assert_eq!(out, raw, "{label}: passed through untouched");
        assert!(validate_device_name(&out).is_ok(), "{label}: and the wire accepts it");
    }

    // Two different machines can also collapse to the SAME label, because the
    // 64-byte trim cuts a non-Latin hostname at about 21 characters.
    let one = sanitise_device_name(&format!("{}-laptop", "会議室".repeat(7))).unwrap();
    let two = sanitise_device_name(&format!("{}-desktop", "会議室".repeat(7))).unwrap();
    assert_eq!(
        one, two,
        "two distinct hostnames sanitise to one indistinguishable name in the pairing list"
    );
}
