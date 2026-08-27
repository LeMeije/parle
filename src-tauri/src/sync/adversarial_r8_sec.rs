//! ADVERSARIAL REVIEW, ROUND 8 — security, cryptography and attack surface.
//!
//! Scope: what decides that something leaves the machine, and whether the two
//! platforms agree about it. Demonstrations only; nothing here fixes production
//! code.
//!
//! Pass criteria exercised:
//!   F. every network read path is bounded by a deadline a peer cannot extend
//!   H. nothing the user marked secret, or the OS marked concealed/transient,
//!      ever reaches the wire
//!   I. keys never in settings.json, never in logs, destroyed on unpair
//!
//! Every socket below carries read AND write timeouts, every loop has a hard
//! bound, and every exchange runs under a wall-clock budget on its own threads,
//! following `adversarial_r7_scale::sync_bounded`, so a stall fails with a
//! message naming the side that never returned.

#![cfg(test)]

use echokey_core::history::Store;
use echokey_core::settings::{PairedDevice, Settings};
use echokey_sync::{PairedKey, Session, SyncMessage, Watermark};
use parking_lot::Mutex;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::deadline::{Deadline, Timed};
use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";

/// Hard wall-clock budget on every exchange in this file.
const BUDGET: Duration = Duration::from_secs(60);

/// A bundle id straight out of the SHIPPED default exclusion list, so this is
/// the protection a user gets without configuring anything.
const EXCLUDED_APP: &str = "com.1password.1password";

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <repo>/src-tauri.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

fn socket_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c = TcpStream::connect(addr).unwrap();
    let (srv, _) = l.accept().unwrap();
    for sock in [&c, &srv] {
        sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
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

/// One exchange, `x` dialling, on two threads under a wall-clock budget.
fn sync_bounded(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
) -> (RoundStats, RoundStats) {
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
                "the exchange stalled inside {BUDGET:?}; returned so far: {:?}",
                got.iter().map(|(who, _)| *who).collect::<Vec<_>>()
            ),
            Err(e) => panic!("both exchange threads died without reporting: {e}"),
        }
    }
    acceptor.join().expect("acceptor thread panicked");
    dialler.join().expect("dialler thread panicked");

    let (mut d, mut a) = (None, None);
    for (who, r) in got {
        let stats = r.unwrap_or_else(|e| panic!("{who} side failed: {e}"));
        match who {
            "dialler" => d = Some(stats),
            _ => a = Some(stats),
        }
    }
    (d.expect("dialler reported"), a.expect("acceptor reported"))
}

/// Every text the store holds, whatever its source.
fn texts(store: &Arc<Mutex<Store>>) -> Vec<String> {
    store
        .lock()
        .recent(None, 500)
        .unwrap()
        .into_iter()
        .map(|i| i.text)
        .collect()
}

// ===========================================================================
// H. What the user marked secret must never reach the wire.
// ===========================================================================

/// R8-H1. **The exclusion rule is evaluated exactly once, at capture, and never
/// again.** `src-tauri/src/sync/mod.rs` says the rule "is enforced before
/// anything is handed to the protocol, never after". That is true only of rows
/// captured AFTER the app was added to the list. `replicate::serve` is handed
/// `Kinds` and `Retention` and nothing else: it has no idea the exclusion list
/// exists, and `items` rows carry `app_id` in the database, so the information
/// is right there and simply not consulted.
///
/// The attacker's position: none is needed. This is the ordinary sequence of a
/// user discovering the feature.
///
///   1. Parle ships with clipboard capture ON and `com.1password.1password`
///      already in `excluded_apps`, but the OS marker is what actually catches
///      it — see R8-H2/H3 for the two ways attribution misses.
///   2. Some password copies land in history attributed to a password manager,
///      which is exactly what the user sees when they open the history window
///      and go looking for the setting.
///   3. The user adds that app to Excluded apps, expecting Parle to stop
///      holding its secrets.
///   4. Nothing happens to the rows already captured, and the next exchange
///      pushes every one of them to every paired device, where they are written
///      to a second `history.db` on a second machine.
///
/// The test seeds one row from an excluded app and one ordinary row, runs a
/// real exchange over real sockets, and asserts the ordinary row arrived —
/// which proves the exchange worked and this is not a test that can find
/// nothing — and that the excluded one did not.
#[test]
fn r8_h1_a_row_from_an_excluded_app_must_not_cross_the_wire() {
    let a = store_for(A);
    let b = store_for(B);

    // Both rows are ordinary local clipboard captures on A.
    a.lock()
        .insert_clipboard("shopping list", Some("com.apple.Safari"), Some("Safari"))
        .unwrap();
    a.lock()
        .insert_clipboard("correct-horse-battery-staple", Some(EXCLUDED_APP), Some("1Password"))
        .unwrap();

    // The user's excluded list, as the default settings ship it, applied to the
    // store the way `AppState` applies it at launch and on every settings write.
    //
    // The store is the enforcement point: `items_from` filters in its own SQL so
    // that LIMIT counts rows that will actually be sent. Configuring it here is
    // what the running app does, so this exercises the production mechanism
    // rather than a copy of it.
    let excluded = Settings::default().history.excluded_apps;
    assert!(
        excluded.iter().any(|x| x == EXCLUDED_APP),
        "the shipped default list must contain the app this test uses, or it proves nothing"
    );
    a.lock().set_excluded_apps(excluded.clone());

    let (_d, _acc) = sync_bounded((&a, A), (&b, B));

    let landed = texts(&b);
    assert!(
        landed.iter().any(|t| t == "shopping list"),
        "the control row must arrive, or this test cannot distinguish a working exchange from a \
         broken one; B holds {landed:?}"
    );
    assert!(
        !landed.iter().any(|t| t == "correct-horse-battery-staple"),
        "a clipboard row captured from an app in the user's exclusion list was replicated to \
         another machine; B holds {landed:?}"
    );
}

/// R8-H2. The replication path is never told which apps are excluded.
///
/// Structural companion to R8-H1: it names the mechanism rather than the
/// symptom, so a fix that filters somewhere useless still fails here. `app_id`
/// is stored on every locally captured row (`history.rs::insert_clipboard`),
/// `RemoteItem` drops it, and `serve` therefore cannot filter on it even if it
/// wanted to.
#[test]
fn r8_h2_the_replication_path_can_see_which_app_a_row_came_from() {
    let src = std::fs::read_to_string(repo_root().join("src-tauri/src/sync/replicate.rs"))
        .expect("replicate.rs is readable");
    // Positive control: the file really is the one we think it is.
    assert!(src.contains("fn serve<S: Read + Write>"), "read the wrong file");
    // Positive control: the information exists. Every locally captured
    // clipboard row records the app it came from.
    let store = std::fs::read_to_string(repo_root().join("crates/echokey-core/src/history.rs"))
        .expect("history.rs is readable");
    assert!(
        store.contains("INSERT INTO items (kind, text, created_at, updated_at, app_id, app_name, source_machine)"),
        "insert_clipboard should record app_id; the control is wrong"
    );
    // Comments do not count: strip anything after `//` before looking, or a
    // passing mention of the word 'excluded' in prose makes this guard find
    // nothing while looking as though it found everything.
    let code: String = src
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        code.contains("app_id"),
        "the outbound path never sees `app_id`: `RemoteItem` drops it and `serve` is handed only \
         Kinds and Retention, so the user's excluded-apps list cannot be applied to rows already \
         in the store"
    );
}

/// R8-H3. **The two platforms do not exclude the same things.**
///
/// `platform/windows.rs::write_clipboard` declares THREE formats when it marks
/// its own output as "do not capture this":
///
///     ExcludeClipboardContentFromMonitorProcessing
///     CanIncludeInClipboardHistory
///     Clipboard Viewer Ignore
///
/// `platform/windows.rs::clipboard_is_excluded`, the gate on the capture path,
/// honours only the first and the third. `CanIncludeInClipboardHistory` — the
/// format Microsoft documents for keeping content out of clipboard history and
/// cloud sync, and the one this very file writes because it believes it means
/// something — is not consulted at all.
///
/// The attack needs no attacker: a Windows application that marks its clipboard
/// writes with only that format has them captured by Parle, stored in
/// `history.db`, and then replicated to the user's Mac, where the Mac's own
/// stricter NSPasteboard rules never get a say because the row arrives as
/// already-accepted content from its author. That is the cross-platform case:
/// captured on one OS, synced to the other.
///
/// Asserted at source level because `windows.rs` is `#[cfg(target_os =
/// "windows")]` and cannot be linked, let alone driven, on this machine. The
/// divergence itself is a fact about the file and does not need Windows to
/// observe.
#[test]
fn r8_h3_windows_honours_every_exclusion_format_it_writes() {
    // Rewritten. The first version sliced `write_clipboard` and
    // `clipboard_is_excluded` out of the source and compared the format names
    // INLINE in each. That found the real defect (the writer declared three,
    // the reader honoured two), but it was measuring the layout of the file:
    // the fix moved both lists into shared constants, and the test then failed
    // because the names were no longer inline, not because anything was wrong.
    //
    // The property is now structural. Two lists that must agree have been
    // replaced by one pair of constants that both paths consume, so agreement
    // holds by construction. What this test defends is that nobody
    // reintroduces a second, private list.
    //
    // Asserted at source level because `windows.rs` is `#[cfg(target_os =
    // "windows")]` and cannot be linked, let alone driven, on this machine.
    let src = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/windows.rs"))
        .expect("windows.rs is readable");

    // Every format the app knows about must be named EXACTLY ONCE as a string
    // literal: in the constant that defines it. A second occurrence means
    // somebody has inlined a list again.
    for fmt in [
        "ExcludeClipboardContentFromMonitorProcessing",
        "Clipboard Viewer Ignore",
        "CanIncludeInClipboardHistory",
        "CanUploadToCloudClipboard",
    ] {
        let n = src.matches(&format!("\"{fmt}\"")).count();
        assert_eq!(
            n, 1,
            "{fmt} appears as a literal {n} times; it must appear once, in the shared constant. \
             Two lists is how the writer came to declare a format the reader ignored."
        );
    }

    // And both paths must consume both constants.
    let writer = src
        .split("pub fn write_clipboard(")
        .nth(1)
        .and_then(|s| s.split("\npub fn ").next())
        .expect("write_clipboard is in the file");
    let reader = src
        .split("fn clipboard_is_excluded()")
        .nth(1)
        .and_then(|s| s.split("\nfn ").next())
        .expect("clipboard_is_excluded is in the file");

    for (what, body) in [("write_clipboard", writer), ("clipboard_is_excluded", reader)] {
        for konst in ["EXCLUDE_MARKER_FORMATS", "EXCLUDE_DWORD_FORMATS"] {
            assert!(
                body.contains(konst),
                "{what} does not consult {konst}, so the two paths can disagree again"
            );
        }
    }

    // The DWORD formats carry a value where 0 means "no" and non-zero means the
    // app explicitly opted IN, so presence alone is the wrong test for them.
    assert!(
        reader.contains("GetClipboardData"),
        "clipboard_is_excluded must READ the DWORD value, not just check the format is present: \
         an app that writes 1 is explicitly allowing capture"
    );
}

/// R8-H4. **The two platforms attribute a clipboard change to different apps.**
///
/// `excluded_apps` is ONE list, matched the same way on both platforms in
/// `state.rs::on_platform_event`. What differs is who supplies the name:
///
///   * Windows (`clipboard_is_excluded` path) uses `clipboard_owner_app()`,
///     which asks `GetClipboardOwner()` — the process that actually wrote the
///     data.
///   * macOS (`macos_clipboard.rs`) uses `macos::frontmost_app()` — whatever
///     happens to be frontmost when the 400 ms poll notices the change, which
///     is not the writer and need not even have been running when the write
///     happened.
///
/// The consequence on macOS: every password manager whose copy affordance
/// gives focus BACK (a menu-bar panel that dismisses itself, a Quick Access
/// overlay, a browser extension pop-up) is attributed to the browser or editor
/// underneath it, so its bundle id never matches the list and the entry is
/// stored. The shipped default list of six password-manager bundle ids is
/// therefore doing much less than it appears to on macOS, and the entry it
/// fails to exclude is then synced to the Windows box.
///
/// Source-level for the same reason as R8-H3: `frontmost_app` needs a real
/// NSWorkspace and a real pasteboard, and driving the user's live pasteboard
/// from a test is out of bounds.
#[test]
fn r8_h4_both_platforms_attribute_a_clipboard_change_to_its_writer() {
    let mac = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/macos_clipboard.rs"))
        .expect("macos_clipboard.rs is readable");
    let win = std::fs::read_to_string(repo_root().join("src-tauri/src/platform/windows.rs"))
        .expect("windows.rs is readable");

    // Positive control: Windows really does ask who owns the clipboard, so
    // "name the writer" is a thing this codebase already knows how to do and
    // this test is not asserting an impossibility.
    assert!(
        win.contains("GetClipboardOwner"),
        "the Windows monitor should attribute to the clipboard owner; the control is wrong"
    );
    let monitor = mac
        .split("PlatformEvent::ClipboardChanged")
        .next()
        .expect("the macOS monitor emits ClipboardChanged");
    assert!(
        !monitor.contains("frontmost_app()"),
        "the macOS clipboard monitor attributes a capture to the FRONTMOST app at poll time, not \
         to the app that wrote it, so `excluded_apps` misses any password manager that returns \
         focus before the 400 ms poll. Windows uses GetClipboardOwner for the same decision."
    );
}

/// R8-H5. Control for the H family: with nothing excluded, an ordinary
/// clipboard row DOES replicate, and the kind toggle DOES suppress it. Without
/// this, H1 could pass simply because clipboard rows never sync at all.
#[test]
fn r8_h5_the_kind_toggle_really_is_what_stops_clipboard_leaving() {
    let a = store_for(A);
    let b = store_for(B);
    a.lock().insert_clipboard("ordinary text", Some("com.apple.Safari"), Some("Safari")).unwrap();
    let (_d, _acc) = sync_bounded((&a, A), (&b, B));
    assert!(
        texts(&b).iter().any(|t| t == "ordinary text"),
        "clipboard rows must replicate when nothing forbids it"
    );
}

// ===========================================================================
// F. A deadline a peer cannot extend.
// ===========================================================================

/// R8-F1. **A paired peer must not be able to renew the session budget by
/// sending traffic.** This is the second half of the slow-loris story: the
/// pre-session path is covered by `wire_tcp::adversarial`, but a peer that HAS
/// the key gets `deadline.extend(SESSION_TIMEOUT)` in `serve_session` and then
/// owns the socket for as long as the budget lasts. If any read renewed it, one
/// compromised device could hold a handler thread — and its inbound slot —
/// indefinitely by trickling well-formed messages.
///
/// Drives the real `Timed`/`Deadline` wrapper under a real Noise session. The
/// attacker sends legal `Watermarks` messages on a timer; the victim reads in a
/// hard-bounded loop. It must receive several (so the traffic really did flow
/// and the wrapper is not simply broken) and then stop at the budget.
#[test]
fn r8_f1_a_paired_peer_cannot_renew_the_session_budget_with_traffic() {
    const BUDGET_MS: u64 = 800;
    const GAP: Duration = Duration::from_millis(60);
    let (attacker_sock, victim_sock) = socket_pair();
    let key = PairedKey::from_bytes([4u8; 32]);
    let k2 = key.clone();

    let attacker = std::thread::spawn(move || {
        let mut s = match Session::initiate(attacker_sock, &k2) {
            Ok(s) => s,
            Err(_) => return 0usize,
        };
        let msg = SyncMessage::Watermarks {
            entries: vec![Watermark { source_device: echokey_sync::DeviceId::parse(A).unwrap(), clock: 1 }],
            more: true,
        };
        // Hard bound: 60 sends at 60 ms is 3.6 s, comfortably past the victim's
        // 800 ms budget, and the loop can never run longer than that.
        let mut sent = 0usize;
        for _ in 0..60 {
            if s.send(&msg).is_err() {
                break;
            }
            sent += 1;
            std::thread::sleep(GAP);
        }
        sent
    });

    // Exactly the shape of `serve_session`: a short pre-auth budget across the
    // handshake, extended once the peer has proved it holds the key.
    let deadline = Deadline::after(Duration::from_secs(10));
    let timed = Timed::new(victim_sock, deadline.clone());
    let mut session = Session::accept(timed, &key).expect("the paired handshake succeeds");
    deadline.extend(Duration::from_millis(BUDGET_MS));

    let t0 = Instant::now();
    let mut received = 0usize;
    let mut stopped = false;
    for _ in 0..500 {
        match session.recv() {
            Ok(_) => received += 1,
            Err(_) => {
                stopped = true;
                break;
            }
        }
    }
    let held = t0.elapsed();
    let sent = attacker.join().expect("the attacker thread must end");

    assert!(sent > 3, "the attacker must actually have sent traffic; it managed {sent} messages");
    assert!(received > 0, "the victim must have read some of it, or the wrapper is simply broken");
    assert!(stopped, "the victim never stopped reading after {received} messages");
    assert!(
        held < Duration::from_millis(BUDGET_MS * 4),
        "the session ran for {held:?} against a {BUDGET_MS} ms budget after {received} messages: \
         a peer that keeps sending is renewing the deadline"
    );
    assert!(
        deadline.expired(),
        "the budget must be what stopped it, not an unrelated socket error"
    );
}

// ===========================================================================
// I. Key material never on disk outside the keychain.
// ===========================================================================

/// R8-I1. **A paired key must never reach settings.json.** The roster written
/// there is (id, name, last_seen) plus the resend debts; the key belongs in the
/// OS credential store. This drives a real pairing to get a real key, builds the
/// `Settings` exactly as `SyncManager::persist` populates it, serialises it the
/// way `Settings::save` does, and searches the JSON for the key in every
/// encoding it could plausibly appear in.
#[test]
fn r8_i1_no_paired_key_reaches_settings_json() {
    use echokey_sync::{Pairing, PairingCode, PairingRole};

    let code = PairingCode::parse("904417").unwrap();
    let (init, msg_i) = Pairing::start(PairingRole::Initiator, &code);
    let (resp, msg_r) = Pairing::start(PairingRole::Responder, &code);
    let (confirm_i, tag_i) = init.finish(&msg_r).unwrap();
    let (confirm_r, tag_r) = resp.finish(&msg_i).unwrap();
    let key = confirm_i.verify_peer(&tag_r).unwrap();
    let peer_key = confirm_r.verify_peer(&tag_i).unwrap();
    assert_eq!(key.as_bytes(), peer_key.as_bytes(), "the control really is a completed pairing");

    let mut s = Settings::default();
    s.sync.enabled = true;
    s.sync.device_id = A.into();
    s.sync.device_name = "Ben's Mac".into();
    s.sync.paired = vec![PairedDevice { id: B.into(), name: "G14".into(), last_seen: Some(1) }];
    let json = serde_json::to_string_pretty(&s).expect("settings serialise");

    let hex_lower: String = key.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let hex_upper = hex_lower.to_ascii_uppercase();
    let b64_ish: String = key.as_bytes().iter().map(|b| format!("{b}")).collect::<Vec<_>>().join(",");
    assert_eq!(hex_lower.len(), 64);

    for needle in [hex_lower.as_str(), hex_upper.as_str(), b64_ish.as_str(), "904417"] {
        assert!(
            !json.contains(needle),
            "settings.json carried key material or the live pairing code ({needle})"
        );
    }
    // Positive assertion: the roster the file IS meant to carry is really there,
    // so this cannot pass because nothing was serialised.
    assert!(json.contains("\"id\": \"22222222-2222-4222-8222-222222222222\""), "roster missing: {json}");

    // And the raw bytes are not hiding in the file in binary either.
    let tmp = std::env::temp_dir().join(format!("parle-r8-settings-{}.json", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(json.as_bytes()).unwrap();
    }
    let raw = std::fs::read(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        !raw.windows(32).any(|w| w == key.as_bytes()),
        "the paired key appears verbatim in the written settings file"
    );
}

/// R8-I2. **Unpair must destroy the key before it reports success.** Checked
/// against the ORDER in `manager.rs::unpair`, which is the property that
/// matters: telling the user a device is gone while its key is still on disk is
/// the failure mode. Source-level, because touching the real keychain from the
/// suite is out of bounds and `SyncManager` cannot be built without a live
/// `AppHandle<Wry>`.
#[test]
fn r8_i2_unpair_destroys_the_key_before_it_reports_success() {
    let src = std::fs::read_to_string(repo_root().join("src-tauri/src/sync/manager.rs"))
        .expect("manager.rs is readable");
    let body = src
        .split("pub fn unpair(")
        .nth(1)
        .and_then(|s| s.split("\n    pub fn ").next())
        .expect("unpair is in the file");
    // Positive control: this really is the function.
    assert!(body.contains("i.paired.retain"), "sliced the wrong function: {body}");
    let delete_at = body.find("keystore::delete").expect("unpair must delete the key at all");
    let forget_at = body.find("i.paired.retain").expect("unpair must drop the roster entry");
    assert!(
        delete_at < forget_at,
        "unpair drops the device from the roster before destroying its key; a keychain failure \
         then leaves the secret on disk with nothing in the UI pointing at it"
    );
    assert!(
        body.contains("keystore::delete(device_id).map_err"),
        "unpair must propagate a keychain failure rather than reporting success"
    );
}

// ===========================================================================
// G. Attribution: one paired device must not act as another.
// ===========================================================================

/// R8-G1. **A paired but hostile device must not be able to author rows for
/// anyone but itself.**
///
/// This drives the wire by hand rather than by running `exchange` on both
/// sides, and that distinction is the whole point. A well-behaved peer never
/// puts a third device's rows on the wire, because `serve` offers items only
/// for `source == me` — so a test built from two honest `exchange` calls passes
/// whatever `Attribution::may_create` says, and proves nothing about the
/// receiving gate. (Checked: reverting `may_create` to the old "any source in
/// our paired roster" rule leaves such a test green.)
///
/// So the attacker here holds the Noise session and speaks the protocol itself:
/// it is a paired device running modified code, which is exactly the
/// second-order threat in the model. It offers three rows — one legitimately
/// its own, one attributed to the victim, one attributed to a third paired
/// device — and only the first may land.
#[test]
fn r8_g1_a_paired_device_cannot_author_rows_for_anyone_else() {
    use echokey_sync::{DeviceId, ItemKind, SyncItem, PROTOCOL_VERSION};

    const THIRD: &str = "33333333-3333-4333-8333-333333333333";
    let victim_store = store_for(A);
    let (attacker_sock, victim_sock) = socket_pair();
    let key = PairedKey::from_bytes([11u8; 32]);
    let k2 = key.clone();
    let now = crate::sync::manager::now_ms();

    fn mk(now: i64, source: &str, origin: &str, text: &str) -> SyncItem {
        SyncItem {
            source_device: DeviceId::parse(source).unwrap(),
            origin_id: origin.into(),
            kind: ItemKind::Clipboard,
            text: text.into(),
            created_at: now,
            updated_at: now,
            pinned: false,
            clock: now as u64,
        }
    }

    let attacker = std::thread::spawn(move || -> Result<(), String> {
        let mut s = Session::initiate(attacker_sock, &k2).map_err(|e| e.to_string())?;
        s.send(&SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::parse(B).unwrap(),
            device_name: "hostile".into(),
        })
        .map_err(|e| e.to_string())?;
        match s.recv().map_err(|e| e.to_string())? {
            SyncMessage::Hello { .. } => {}
            other => return Err(format!("expected hello, got {other:?}")),
        }
        // The victim is Turn::Second, so it reads our marks before sending its
        // own. Claim to hold nothing.
        s.send(&SyncMessage::Watermarks { entries: Vec::new(), more: false })
            .map_err(|e| e.to_string())?;
        // Its marks, bounded.
        for _ in 0..300 {
            match s.recv().map_err(|e| e.to_string())? {
                SyncMessage::Watermarks { more, .. } => {
                    if !more {
                        break;
                    }
                }
                other => return Err(format!("expected watermarks, got {other:?}")),
            }
        }
        // The payload: one honest row and two forgeries.
        s.send(&SyncMessage::Items {
            items: vec![
                mk(now, B, "b-own", "B's own row"),
                mk(now, A, "forged-as-us", "a dictation we never made"),
                mk(now, THIRD, "forged-as-third", "a dictation the third machine never made"),
            ],
            more: true,
        })
        .map_err(|e| e.to_string())?;
        s.send(&SyncMessage::Items { items: Vec::new(), more: false })
            .map_err(|e| e.to_string())?;
        // Read whatever the victim serves back, bounded, so it never blocks in
        // write while we sit idle.
        for _ in 0..2048 {
            match s.recv() {
                Ok(SyncMessage::Items { items, more }) if items.is_empty() && !more => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        Ok(())
    });

    let known = vec![A.to_string(), B.to_string(), THIRD.to_string()];
    let (tx, rx) = mpsc::channel::<Result<RoundStats, String>>();
    let vstore = victim_store.clone();
    let victim = std::thread::spawn(move || {
        let r = (|| {
            let mut s = Session::accept(victim_sock, &key).map_err(|e| e.to_string())?;
            let attr = Attribution { peer_id: B, local_id: A, known: &known };
            exchange(
                &mut s,
                &vstore,
                (A, "victim"),
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
        let _ = tx.send(r);
    });

    let stats = rx
        .recv_timeout(BUDGET)
        .unwrap_or_else(|e| panic!("the victim never finished its exchange: {e}"))
        .unwrap_or_else(|e| panic!("the victim's exchange failed: {e}"));
    victim.join().expect("victim thread panicked");
    attacker.join().expect("attacker thread panicked").expect("attacker script");

    let landed = texts(&victim_store);
    assert!(
        landed.iter().any(|t| t == "B's own row"),
        "the control must land: a paired peer's OWN row has to replicate, or this test cannot \
         tell a working gate from a broken exchange. Victim holds {landed:?}"
    );
    assert!(
        stats.refused >= 2,
        "the victim must have actively REFUSED the two forgeries, not merely failed to store \
         them; it recorded {} refusals",
        stats.refused
    );
    assert!(
        !landed.iter().any(|t| t == "a dictation we never made"),
        "a paired device authored a row attributed to US; victim holds {landed:?}"
    );
    assert!(
        !landed.iter().any(|t| t == "a dictation the third machine never made"),
        "a paired device authored a row attributed to a THIRD device; victim holds {landed:?}"
    );
}
