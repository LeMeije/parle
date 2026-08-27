//! ADVERSARIAL REVIEW, ROUND 12 — workflow and cross-OS experience.
//!
//! Not the algebra. The question here is the one a person asks: I installed
//! Parle on a Mac and on a PC and I want my history on both. Does it work, and
//! when it does not, do I find out why?
//!
//! Round 11 is the target, per the handover's own instruction, and round 11's
//! headline UX change is the one attacked hardest: `store_transcription` now
//! returns a message and the pipeline emits `PipelineEvent::Empty { reason }`
//! so a withheld dictation is visible. Section R12-A asks whether that message
//! ever reaches a screen.
//!
//! Nothing here changes production code. Nothing opens a socket that outlives
//! a test, sleeps on a wall clock, writes the real clipboard or touches the
//! real keychain. Every exchange runs on its own threads under a budget, both
//! sockets carry read AND write timeouts, and every loop is hard bounded.
//!
//! Two kinds of test live here, and the difference is stated rather than
//! blurred:
//!
//!   * RUNTIME tests, which build a real `Store` or run a real `exchange`.
//!   * SURFACE tests, which read a source file. The user-facing half of this
//!     product is React, there is no JS test runner in `package.json`, and the
//!     Windows half cannot execute on this machine. Every one of them asserts
//!     its ANCHOR first — that the code it is reasoning about is present in the
//!     shape it expects — before asserting the property. A guard that can find
//!     nothing must first assert that it found something.

#![cfg(test)]

use echokey_core::history::Store;
use echokey_core::types::TranscriptionResult;
use echokey_sync::{PairedKey, Session};
use parking_lot::Mutex;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sync::replicate::{exchange, Attribution, Kinds, Retention, RoundStats, Turn};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";

const BUDGET: Duration = Duration::from_secs(60);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

/// The file verbatim.
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}

fn store_for(me: &str) -> Arc<Mutex<Store>> {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(me);
    Arc::new(Mutex::new(s))
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

/// One exchange, `x` dialling, under a wall-clock budget. Same structure as
/// `adversarial_r11_data::sync_bounded`, for the reason handover section 4.4
/// gives: a stall must fail naming the side that never returned, not park the
/// suite.
fn sync_bounded(
    x: (&Arc<Mutex<Store>>, &'static str),
    y: (&Arc<Mutex<Store>>, &'static str),
) -> (RoundStats, RoundStats) {
    let (sock_x, sock_y) = socket_pair();
    let key = PairedKey::from_bytes([7u8; 32]);
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
                Kinds { dictations: true, clipboard: true },
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
                Kinds { dictations: true, clipboard: true },
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
                "the exchange did not finish inside {BUDGET:?}; {} of 2 sides returned: {:?}",
                got.len(),
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
        let stats = r.unwrap_or_else(|e| panic!("{who} side failed: {e}"));
        match who {
            "dialler" => d = Some(stats),
            _ => a = Some(stats),
        }
    }
    (d.expect("dialler reported"), a.expect("acceptor reported"))
}

// ===========================================================================
// R12-A. ROUND 11's OWN UX FIX, ATTACKED FIRST.
//
// In ordinary terms. You dictate into a password field. Round 11 added a
// message so you are told the dictation was copied but not saved to History,
// because the round before that discarded every dictation on a machine with a
// password manager running and said nothing at all.
//
// The message is emitted. It is then overwritten before you can read it, by
// the very next event, and what replaces it tells a Windows user to press a
// key their keyboard does not have.
// ===========================================================================

/// The withholding notice is emitted, and then superseded, in the same burst.
///
/// `Pipeline::stop_and_process` emits `PipelineEvent::Empty { reason }` and
/// then, unconditionally, `PipelineEvent::Completed { injection, .. }`.
/// `Hud.tsx` is a last-writer-wins reducer over that stream:
///
///     if (e.kind === 'empty') setOutcome({ text: e.reason, ... });
///     if (e.kind === 'completed' && e.injection?.manual_paste_required)
///       setOutcome({ text: 'Copied. Press ⌘V to paste (secure field)', ... });
///
/// On the SECURE path — the one the notice was written for —
/// `macos::inject_text` returns `manual_paste_required: true`, because
/// `focused_field_is_secure() == Some(true)` is its first branch. So the
/// completed handler always fires and always wins, and the string round 11
/// added is never on screen for a single frame.
///
/// The replacement is not equivalent. It says nothing about History, which is
/// the entire content of the notice: "copied, NOT saved". A user who reads it
/// concludes the dictation is in History and merely needs pasting.
#[test]
fn r12_flow_the_withheld_dictation_notice_is_overwritten_by_the_next_event() {
    let pipe = code_of("src-tauri/src/pipeline.rs");
    let hud = code_of("src/Hud.tsx");
    let mac = code_of("src-tauri/src/platform/macos.rs");

    // --- anchors: the code this test reasons about exists, in this shape ---
    assert!(
        pipe.contains("PipelineEvent::Empty { reason }"),
        "anchor lost: pipeline.rs no longer emits Empty with a reason; \
         this test is reasoning about code that has moved"
    );
    assert!(
        // Round 13 split this: the empty-after-cleanup branch never copies, so
        // it no longer says it did.
        pipe.contains("Password field: not saved to History"),
        "anchor lost: round 11's withholding notice is not in pipeline.rs"
    );
    assert!(
        hud.contains("e.kind === 'empty'") && hud.contains("e.kind === 'completed'"),
        "anchor lost: Hud.tsx no longer handles both 'empty' and 'completed'"
    );
    assert!(
        mac.contains("field == Some(true)"),
        "anchor lost: macos::inject_text no longer branches on a known-secure field"
    );

    // --- 1. Empty is emitted BEFORE Completed, on both dictation paths. ---
    // Every `Empty { reason }` that comes from `store_transcription` is
    // followed by a `Completed` with no state change in between.
    let notice_sites: Vec<usize> = pipe
        .match_indices("(self.sink)(PipelineEvent::Empty { reason });")
        .map(|(i, _)| i)
        .collect();
    // THREE. Round 12 found a third store path (the empty-after-cleanup
    // branch, which stored the raw transcript before the secrecy sample had
    // even been taken) and routed it through the same gate, so it emits the
    // same notice.
    assert_eq!(
        notice_sites.len(),
        3,
        "expected the withholding notice on every dictation path; found {} \
         sites. If the paths were merged, re-point this test.",
        notice_sites.len()
    );
    for at in &notice_sites {
        let after = &pipe[*at..];
        let completed = after
            .find("(self.sink)(PipelineEvent::Completed {")
            .expect("a withholding notice with no Completed after it");
        let state_change = after.find("PipelineEvent::StateChanged").unwrap_or(usize::MAX);
        assert!(
            completed < state_change,
            "Completed no longer follows the notice directly; re-check the ordering"
        );
    }

    // --- 2. Hud.tsx handles 'completed' AFTER 'empty', with no guard. ---
    let empty_at = hud.find("e.kind === 'empty'").unwrap();
    let completed_at = hud.find("e.kind === 'completed' && e.injection?.manual_paste_required").unwrap_or_else(
        || panic!("anchor lost: the completed/manual-paste branch has changed shape"),
    );
    assert!(
        completed_at > empty_at,
        "the reducer order changed; this finding may no longer hold"
    );

    // Between the two there is nothing that could preserve the notice: no
    // early return, no check of an existing outcome.
    let between = &hud[empty_at..completed_at];
    // INVERTED. Something MUST guard it now. The notice used to be overwritten
    // by the very next event, on the exact path it was written for.
    assert!(
        between.contains("return") || between.contains("e.withheld"),
        "FINDING: nothing between the two handlers preserves the notice, so the withholding \
         message never reaches the screen for a single frame and is replaced by one that \
         says nothing about History"
    );

    // --- 3. THE FINDING. ---
    // The secure path guarantees manual_paste_required, so the completed
    // branch always fires; the replacement string never mentions History.
    // The replacement string is now platform-aware and no longer asserts a
    // cause it cannot know, so the anchor is the shape rather than the literal.
    let replacement = "Copied. Press ${PASTE_KEYS} to paste";
    assert!(
        hud.contains(replacement),
        "anchor lost: the replacement string changed"
    );
    let inject = mac
        .split("pub fn inject_text")
        .nth(1)
        .expect("anchor lost: macos::inject_text not found");
    let secure_branch = inject
        .split("field == Some(true)")
        .nth(1)
        .expect("anchor lost")
        .split("}\n\n")
        .next()
        .unwrap();
    assert!(
        secure_branch.contains("manual_paste_required: true"),
        "the secure-field branch no longer forces manual_paste_required; \
         if so, this finding is closed"
    );
    // INVERTED. The replacement still says nothing about History, and that is
    // now fine, because it no longer WINS: the handler returns early when the
    // event is marked withheld, so round 11's notice is what stays on screen.
    assert!(
        hud.contains("e.withheld) return;"),
        "FINDING: on the secure-field path the last event wins and its message \
         says nothing about the dictation being withheld from History. Round \
         11's notice is emitted and immediately overwritten."
    );
}

/// The message that wins on Windows tells the user to press Command-V.
///
/// `Hud.tsx` has no platform branch anywhere in it — `IS_MAC` exists in
/// `SettingsView.tsx` and is never imported here — so the literal
/// `'Copied. Press ⌘V to paste (secure field)'` is what a Windows user is
/// shown after every dictation that could not be injected. The correct key is
/// Ctrl-V, and this is the ONLY instruction the HUD ever gives.
#[test]
fn r12_flow_the_hud_tells_a_windows_user_to_press_command_v() {
    let hud = read_src("src/Hud.tsx");

    // INVERTED IN ROUND 12. The HUD now knows what platform it is on, so the
    // assertions run the other way: the branch must exist and both keys must
    // be reachable.
    assert!(
        hud.contains("IS_MAC") || hud.contains("navigator.userAgent"),
        "Hud.tsx branches on no platform at all, and it carries the ONLY paste \
         instruction the product gives"
    );
    assert!(
        hud.contains("Ctrl+V"),
        "FINDING: Hud.tsx offers only the macOS key. A Windows user is told to \
         press a key that does not exist on their keyboard."
    );
    assert!(
        hud.contains("PASTE_KEYS"),
        "the paste chord is not read from one place, so the two platforms can drift again"
    );

    // And the same file uses ⌘ once more, in the search hint it renders, so
    // this is a pattern rather than one slip.
    let history = read_src("src/views/History.tsx");
    assert!(
        history.contains("⌘Enter"),
        "anchor lost: History.tsx no longer carries the ⌘Enter hint"
    );
    assert!(
        !history.contains("IS_MAC") && !history.contains("navigator."),
        "FINDING: History.tsx labels its keyboard shortcuts ⌘Enter on every \
         platform, with no branch"
    );
}

// ===========================================================================
// R12-B. THE LOCAL-ONLY ROW IS INVISIBLE.
//
// Round 11 added schema v8 `items.local_only`: a dictation we cannot classify
// is KEPT on this device and never offered to a peer. That is the right call.
//
// The consequence nobody wired up: the History list cannot tell the user which
// rows those are. `HistoryItem` — the struct that crosses the IPC boundary and
// the one `src/types.ts` mirrors — has no such field. So a row that will never
// reach the other machine is pixel-identical to one that will, on a feature
// whose entire promise is "it is on both machines".
//
// And this is not rare. On macOS `focused_field_is_secure()` returns None for
// any application that does not answer the accessibility tree, which round 11's
// own comment names as Chromium and Electron: Chrome, Slack, VS Code, Discord,
// Teams. Combine that with `secure_input_active()`, which round 9 MEASURED as
// reading true continuously with a password manager merely running, and every
// dictation into an Electron app on a Mac with 1Password open is local-only.
// ===========================================================================

/// A local-only row and an ordinary row are indistinguishable to the UI.
#[test]
fn r12_flow_a_local_only_row_is_indistinguishable_from_a_syncing_one() {
    let s = Store::open_in_memory().unwrap();

    let ordinary = s.insert_transcription(&tr("this one syncs"), Some("com.apple.Notes"), Some("Notes")).unwrap();
    let withheld = s
        .insert_transcription_local_only(&tr("this one never leaves"), Some("com.apple.Notes"), Some("Notes"))
        .unwrap();

    // Vacuity guard: the premise must be constructible. The two rows really do
    // differ in the store, or this test proves nothing.
    let flags: Vec<i64> = {
        let mut st = s
            .conn_for_test()
            .prepare("SELECT local_only FROM items WHERE id IN (?1, ?2) ORDER BY id")
            .unwrap();
        st.query_map([ordinary, withheld], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    assert_eq!(flags, vec![0, 1], "the local_only flag is not being written; premise gone");

    // What the UI actually receives.
    let rows = s.search("", None, 50).unwrap();
    assert_eq!(rows.len(), 2, "both rows must be listed; only the WIRE hides one");
    let a = rows.iter().find(|r| r.id == ordinary).unwrap();
    let b = rows.iter().find(|r| r.id == withheld).unwrap();

    let ja = serde_json::to_value(a).unwrap();
    let jb = serde_json::to_value(b).unwrap();
    let ja = ja.as_object().unwrap();
    let jb = jb.as_object().unwrap();

    // Anchor: the payload is non-trivial, so "no difference" means something.
    assert!(ja.len() >= 10, "HistoryItem shrank to {} fields; re-check", ja.len());

    let differing: Vec<&String> = ja
        .keys()
        // created_at is a wall-clock stamp and differs by a millisecond or two
        // between two inserts; it is not a marker anyone could read.
        .filter(|k| !matches!(k.as_str(), "id" | "text" | "raw_text" | "created_at"))
        .filter(|k| ja.get(*k) != jb.get(*k))
        .collect();
    // INVERTED. The payload now carries the marker, so the two rows MUST
    // differ, and they must differ in exactly that field.
    assert!(
        ja.contains_key("local_only"),
        "FINDING: HistoryItem carries no local-only marker, so a row that will never reach \
         the other machine is indistinguishable from one that will, on a feature whose \
         whole promise is that it does"
    );
    assert_eq!(
        differing,
        vec![&"local_only".to_string()],
        "the withheld row and the ordinary row differ in {differing:?}, which is not the \
         one field that should tell them apart"
    );

    // And the mirror on the TypeScript side agrees, so the UI could not render
    // one even if it wanted to.
    let types = read_src("src/types.ts");
    assert!(
        types.contains("export interface HistoryItem"),
        "anchor lost: HistoryItem is no longer declared in src/types.ts"
    );
    let iface = types.split("export interface HistoryItem").nth(1).unwrap().split('}').next().unwrap();
    assert!(
        iface.contains("app_name"),
        "anchor lost: the HistoryItem interface changed shape"
    );
    assert!(
        iface.contains("local_only"),
        "FINDING: src/types.ts cannot mirror a local-only marker because the Rust payload \
         does not carry one, so the UI cannot mark a row that will never sync"
    );
}

// ===========================================================================
// R12-C. A ROW WRITTEN ON WINDOWS LOSES ITS APP LABEL ON BOTH MACHINES.
//
// `macos::frontmost_app` returns (bundle id, localized name).
// `windows::frontmost_app` returns (exe name, None) — the second slot is
// hard-coded None, there is no Windows equivalent call.
//
// `History.tsx` renders `item.app_name` and nothing else. So every row a
// Windows machine writes shows no application at all, on the PC and on the Mac,
// while the Mac's rows show "Slack", "Notes", "Mail". The information is not
// missing: it is sitting in `app_id` as "Slack.exe", which is more readable
// than the bundle id the Mac has. The UI simply never looks at it.
//
// This is exactly the shape the brief asks about: a row created on one OS
// displays differently on the other.
// ===========================================================================

#[test]
fn r12_flow_a_windows_authored_row_shows_no_application_anywhere() {
    // --- runtime half: the row really does carry app_id and no app_name ---
    let s = Store::open_in_memory().unwrap();
    // What `state.rs::on_platform_event` stores on Windows, given what
    // `windows::frontmost_app` returns.
    let win = s.insert_clipboard("copied on the PC", Some("Slack.exe"), None).unwrap();
    // What it stores on macOS.
    let mac = s
        .insert_clipboard("copied on the Mac", Some("com.tinyspeck.slackmacgap"), Some("Slack"))
        .unwrap();

    let rows = s.search("", None, 50).unwrap();
    let w = rows.iter().find(|r| r.id == win).unwrap();
    let m = rows.iter().find(|r| r.id == mac).unwrap();

    // Vacuity guard: the Mac row must actually have a name, or "the Windows
    // row has none" is not a comparison.
    assert_eq!(m.app_name.as_deref(), Some("Slack"), "the macOS row lost its name; premise gone");
    assert_eq!(w.app_id.as_deref(), Some("Slack.exe"), "the Windows row lost its id; premise gone");
    assert_eq!(w.app_name, None, "the Windows row unexpectedly has a name");

    // --- surface half: windows.rs really does hard-code None ---
    let winsrc = code_of("src-tauri/src/platform/windows.rs");
    assert!(
        winsrc.contains("pub fn frontmost_app()"),
        "anchor lost: windows::frontmost_app not found"
    );
    let body = winsrc.split("pub fn frontmost_app()").nth(1).unwrap().split("\nfn ").next().unwrap();
    assert!(
        body.contains("(process_image_name(pid), None)"),
        "FINDING closed? windows::frontmost_app now returns a display name"
    );

    let macsrc = code_of("src-tauri/src/platform/macos.rs");
    let macbody = macsrc.split("pub fn frontmost_app()").nth(1).unwrap().split("\n\n").next().unwrap();
    assert!(
        macbody.contains("localizedName"),
        "anchor lost: macos::frontmost_app no longer reports a localized name"
    );

    // --- the finding: the UI renders only the half Windows never fills ---
    let hist = code_of("src/views/History.tsx");
    assert!(
        hist.contains("item.app_name"),
        "anchor lost: History.tsx no longer renders app_name"
    );
    assert!(
        hist.contains("item.app_name ?? item.app_id"),
        "FINDING: History.tsx renders only `app_name`, and the Windows probe hard-codes that \
         slot to null, so a Windows-authored row shows no application at all, on both \
         machines, in the same list as Mac rows that show one"
    );
}

// ===========================================================================
// R12-D. A PEER WITH A WRONG CLOCK IS SHOWN AS HEALTHY.
//
// Field test step 10 says, in `docs/SYNC_FIELD_TEST.md`:
//
//   "Set one machine's clock 5 minutes fast. Rows must be REFUSED with a
//    warning naming the device, not silently accepted"
//
// The refusal happens. The warning does not exist. `report_device_problem` —
// the one function that puts a per-device message in front of the user, and the
// one round 10 added for oversized rows — is called from exactly three places,
// none of which is the skew path. A skewed row increments `stats.ignored`,
// which is a `tracing::info!` and nothing more.
//
// Worse, the exchange returns Ok, so `d.last_sync_ok = Some(now_ms())` runs,
// and `SettingsView.tsx` renders the green dot and "Synced just now" for a
// device from which not one row has been accepted. The comment above that dot
// says it was moved off `online` precisely so it would stop being "green and
// confident while not a single row moved". It still is, for this cause.
// ===========================================================================

#[test]
fn r12_flow_a_five_minute_clock_skew_is_refused_in_total_silence() {
    let fast = store_for(A);
    let ok = store_for(B);

    // A machine five minutes fast. MAX_CLOCK_SKEW_MS is two.
    let ahead = now_ms() + 5 * 60 * 1000;
    for i in 0..4 {
        let id = fast.lock().insert_clipboard(&format!("row {i}"), None, None).unwrap();
        let t = ahead + i;
        fast.lock()
            .conn_for_test()
            .execute_batch(&format!(
                "UPDATE items SET created_at={t}, updated_at={t} WHERE id={id};"
            ))
            .unwrap();
    }

    // Vacuity guard: the rows exist and really are in the future.
    let floor = now_ms() + 120_000;
    let n: i64 = fast
        .lock()
        .conn_for_test()
        .query_row(&format!("SELECT COUNT(*) FROM items WHERE updated_at > {floor}"), [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4, "the skewed rows were not written; premise gone");

    let (d, a) = sync_bounded((&fast, A), (&ok, B));

    // The healthy machine offered them and refused every one.
    assert!(d.sent_items >= 4, "the fast machine did not offer its rows: {d:?}");
    assert_eq!(a.applied_items, 0, "a five-minute-skewed row was ACCEPTED: {a:?}");
    assert!(a.ignored >= 4, "the refusals were not counted: {a:?}");

    // Nothing about this reaches a user-facing channel. `oversized` is the ONE
    // per-row problem that does, and it is zero here.
    assert_eq!(a.oversized, 0, "premise gone: these were refused for size, not clock");
    assert_eq!(a.refused, 0, "premise gone: these were refused as unentitled, not for clock");

    let mgr = code_of("src-tauri/src/sync/manager.rs");
    assert!(
        mgr.contains("fn report_device_problem"),
        "anchor lost: report_device_problem is gone"
    );
    // Anchor: it IS wired up for something, so "not wired up for skew" is a gap
    // and not simply an unused function.
    assert!(
        mgr.contains("if stats.oversized > 0 {"),
        "anchor lost: the oversized report is no longer conditional on stats"
    );
    // FOUR now: round 12 added the refusal report this test was written to
    // demand.
    let callers = mgr.matches("self.report_device_problem(").count();
    assert_eq!(callers, 4, "the caller set changed ({callers}); re-verify this finding");

    // The finding: no caller mentions ignored, and last_sync_ok is set on the
    // Ok arm with no reference to what was applied.
    let ok_arm = mgr
        .split("if stats.applied_items + stats.applied_tombstones > 0 {")
        .nth(1)
        .expect("anchor lost: the history-changed emit moved");
    let ok_arm = ok_arm.split("Err(e) =>").next().unwrap();
    assert!(
        ok_arm.contains("d.last_sync_ok = Some(now_ms())"),
        "anchor lost: last_sync_ok is no longer set here"
    );
    assert!(
        ok_arm.contains("stats.ignored"),
        "FINDING: `last_sync_ok` is stamped on the Ok arm regardless of what was applied, so \
         the UI shows the green dot and 'Synced just now' for a device from which not one \
         row has been accepted"
    );
    assert!(
        !mgr.contains("clock") || !mgr.contains("report_device_problem(\n                    &peer_id,\n                    \"clock"),
        "FINDING closed? a clock warning now names the device"
    );
}

/// The field test asks for something the code cannot do.
///
/// Step 10 is not a step the tester can carry out: there is no surface where
/// the warning it describes could appear.
#[test]
fn r12_flow_field_test_step_ten_asks_for_a_warning_that_does_not_exist() {
    let doc = read_src("docs/SYNC_FIELD_TEST.md");
    assert!(
        doc.contains("Rows must be REFUSED with a")
            && doc.contains("warning naming the device, not silently accepted"),
        "anchor lost: step 10 has been reworded; re-read it"
    );
    let mgr = code_of("src-tauri/src/sync/manager.rs");
    let rep = code_of("src-tauri/src/sync/replicate.rs");

    // `report_device_problem` is the ONLY function that puts a per-device
    // message in front of the user. Anchor: it exists and is used.
    assert!(
        mgr.contains("fn report_device_problem"),
        "anchor lost: report_device_problem is gone"
    );
    let sites: Vec<&str> = mgr.split("self.report_device_problem(").skip(1).collect();
    assert_eq!(sites.len(), 4, "the caller set changed ({}); re-verify", sites.len());
    // INVERTED. Field test step 10 asks for "a warning naming the device", and
    // one of these sites must now be it.
    let mentions_clock = sites.iter().any(|site| {
        let arg = site.split(");").next().unwrap_or(site).to_lowercase();
        arg.contains("clock") || arg.contains("skew")
    });
    assert!(
        mentions_clock,
        "FINDING: no report site mentions the clock, so a peer whose clock is wrong has \
         every row refused in total silence and field test step 10 cannot be performed"
    );
    // And the exchange itself has no channel of its own to the user.
    assert!(
        !rep.contains("report_device_problem"),
        "FINDING closed? replicate.rs now reports a per-device problem"
    );
    // The counter that a skewed row moves is never read by anything that
    // reaches a screen.
    assert!(
        mgr.contains("stats.ignored"),
        "anchor lost: manager.rs no longer looks at stats.ignored at all"
    );
    for line in mgr.lines().filter(|l| l.contains("stats.ignored")) {
        assert!(
            !line.contains("report_device_problem") && !line.contains("i.error"),
            "FINDING closed? stats.ignored now feeds a user-visible surface: {line}"
        );
    }
}

// ===========================================================================
// R12-E. THREE WRONG CODES, AND THE USER IS TOLD THE OTHER MACHINE IS ASLEEP.
//
// `PairingGuard` writes four genuinely useful failures — NotPairing, Expired,
// LockedOut { retry_in }, CodeExhausted — with `Display` text that tells the
// user exactly what to do ("try again in N seconds", "show a new code").
//
// Not one of them can reach a user. `serve_pairing` logs the error and
// `return`s, which closes the socket. On the ENTERING machine that surfaces
// through `pair_flow::PairFlowError::Transport`, which `pair_with` maps to
// "Lost the connection to that device before pairing finished. Check it is
// still awake and on this network, then try again."
//
// So the fourth wrong code sends the user to check their Wi-Fi. The comment
// above that mapping says six failures used to collapse into one wrong answer
// and that "telling the user their digits were wrong sends them to retype
// something that was never the problem". The same defect survives, pointed the
// other way: telling them the network dropped sends them to fix a network that
// is fine.
// ===========================================================================

#[test]
fn r12_flow_a_locked_out_pairing_code_reports_itself_as_a_network_drop() {
    use crate::sync::guard::{GuardError, PairingGuard, MAX_PER_SOURCE};
    use std::net::{IpAddr, Ipv4Addr};

    // --- runtime half: the guard really does lock out, and really does have
    //     something worth saying. ---
    let mut g = PairingGuard::new();
    let t0 = Instant::now();
    g.begin("123456".into(), t0).unwrap();
    let from = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));

    // Guess once, immediately guess again. The backoff is one second.
    assert!(g.reserve(t0, from).is_ok(), "the first guess was refused; premise gone");
    let too_soon = g
        .reserve(t0 + Duration::from_millis(100), from)
        .expect_err("a second guess 100ms later was admitted; premise gone");
    let retry_advice = too_soon.to_string();
    assert!(
        matches!(too_soon, GuardError::LockedOut { .. }),
        "expected a backoff, got {too_soon:?}"
    );
    assert!(
        retry_advice.contains("try again in"),
        "the guard's backoff advice changed: {retry_advice}"
    );

    // Wait each backoff out, no wall clock involved, and spend the budget.
    let mut spent = 1;
    let mut at = t0 + Duration::from_secs(1);
    let mut exhausted: Option<GuardError> = None;
    for _ in 0..8 {
        match g.reserve(at, from) {
            Ok(_) => spent += 1,
            Err(e) => {
                exhausted = Some(e);
                break;
            }
        }
        at += Duration::from_secs(30);
    }
    // Vacuity guard: guesses were actually spent before the refusal. A guard
    // that refused the SECOND attempt outright would pass a naive version of
    // this test while proving nothing.
    assert_eq!(
        spent, MAX_PER_SOURCE,
        "{spent} guesses were admitted from one source, budget is {MAX_PER_SOURCE}"
    );
    let exhausted = exhausted.expect("the source was never refused inside 8 attempts");
    let advice = exhausted.to_string();
    assert!(
        advice.contains("too many incorrect codes"),
        "the guard's advice changed: {advice}"
    );

    // --- surface half: that advice has no path to a screen. ---
    // `code_of`, not `read_src`. The refusal arm is inspected through a fixed
    // 400-character window, and a comment explaining the arm is prose that
    // pushes the code out of it: the same reason `code_of` exists to stop prose
    // SATISFYING a guard applies to prose DEFEATING one.
    // Comments stripped AND the blank lines they leave behind dropped: the
    // refusal arm below is inspected through a fixed-size window, and a
    // stripped comment would push the code out of it while looking like
    // nothing at all.
    let mgr: String = code_of("src-tauri/src/sync/manager.rs")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        mgr.contains("fn serve_pairing"),
        "anchor lost: serve_pairing is gone"
    );
    let serve = mgr.split("fn serve_pairing").nth(1).unwrap().split("fn serve_session").next().unwrap();
    assert!(
        serve.contains("i.guard.reserve(Instant::now(), from)"),
        "anchor lost: serve_pairing no longer reserves against the guard"
    );
    // The refusal arm: log, return. No frame written to the peer, no publish.
    // From the reserve call onwards, so the FIRST `Err` arm we see is the
    // budget refusal and not the earlier read_frame one.
    let after_reserve = serve
        .split("i.guard.reserve(Instant::now(), from)")
        .nth(1)
        .expect("anchor lost: the reserve call moved");
    let refusal_at = after_reserve
        .find("Err(e) => {")
        .expect("anchor lost: the reserve refusal arm changed shape");
    // The whole arm and then some: enough to catch anything it might do.
    let refusal = &after_reserve[refusal_at..(refusal_at + 700).min(after_reserve.len())];
    assert!(
        refusal.contains("tracing::info!") || refusal.contains("refusal_frame"),
        "anchor lost: the refusal neither logs nor answers"
    );
    assert!(
        refusal.contains("write_frame"),
        "FINDING: the refusal closes the socket without a word, so the entering machine sees \
         a transport drop and tells the user to check their network. The guard's four \
         messages are the most actionable in the product and every one is thrown away"
    );

    // --- and the entering machine's diagnosis. ---
    let map = mgr
        .split("PairFlowError::Transport(_) => {")
        .nth(1)
        .expect("anchor lost: the Transport mapping changed")
        .split("PairFlowError::BadTag")
        .next()
        .unwrap();
    assert!(
        map.contains("still awake and on this network"),
        "anchor lost: the Transport message changed"
    );
    assert!(
        !map.contains("code") && !map.contains("attempt") && !map.contains("wait"),
        "FINDING closed? the Transport message now allows for a spent budget"
    );

    // --- and the SHOWING machine says nothing either: SyncStatus has no
    //     field that could carry a lockout or an attempt count. ---
    let types = read_src("src/types.ts");
    let pairing = types
        .split("export interface SyncPairingState")
        .nth(1)
        .expect("anchor lost: SyncPairingState is gone")
        .split('}')
        .next()
        .unwrap();
    assert!(pairing.contains("expires_at"), "anchor lost: SyncPairingState changed shape");
    for field in ["attempts", "locked", "retry", "exhausted", "failures"] {
        assert!(
            !pairing.contains(field),
            "FINDING closed? SyncPairingState now carries {field}"
        );
    }
}

// ===========================================================================
// R12-F. A PEER THAT RENAMES ITSELF KEEPS ITS OLD NAME FOR EVER.
//
// `UiPaired.name` is written in exactly one place: `complete_pairing`. The
// discovery thread stores the fresh mDNS record in `i.peers` and never touches
// `i.paired`. `snapshot` then copies `..p.clone()` from `i.paired`, overriding
// only `online`.
//
// So: rename the PC from "Windows PC" to "Office PC" and the Mac's paired list
// says "Windows PC" until it is unpaired and paired again. The Unpair
// confirmation — the one destructive action in that panel — asks
// "Unpair Windows PC?" about a machine that has not been called that for weeks.
//
// The same staleness breaks the same-name disambiguation. Two machines that
// both call themselves "MacBook Pro" are disambiguated by an id suffix, but
// only against OTHER ROWS IN THE SAME LIST: a paired device and a NEW INSTALL
// of that same device (reinstall, new device id, same name) sit in two
// different lists and neither one triggers the other's suffix.
// ===========================================================================

#[test]
fn r12_flow_a_renamed_peer_keeps_its_old_name_in_the_paired_list() {
    let mgr = code_of("src-tauri/src/sync/manager.rs");

    // Anchors: both halves of the mechanism are present in the shape assumed.
    assert!(
        mgr.contains("existing.name = usable_peer_name(&p.device_name, &existing.id);"),
        "anchor lost: complete_pairing no longer sets the stored name"
    );
    assert!(
        mgr.contains("fn decide_dial"),
        "anchor lost: decide_dial is gone"
    );
    assert!(
        mgr.contains("fn snapshot"),
        "anchor lost: snapshot is gone"
    );

    // 1. The discovery path never writes a paired name.
    let dial = mgr.split("fn decide_dial").nth(1).unwrap().split("\npub(crate) fn stop_claim").next().unwrap();
    assert!(
        dial.contains("note_peer_record(peers, last_dial, last_move, &id, p, known);"),
        "anchor lost: decide_dial no longer records the peer"
    );
    assert!(
        !dial.contains("paired.iter_mut") && !dial.contains(".name ="),
        "FINDING closed? decide_dial now refreshes the stored name"
    );

    // 2. note_peer_record only touches the peers map.
    let note = mgr.split("pub(crate) fn note_peer_record").nth(1).unwrap().split("\npub(crate) fn admit_inbound").next().unwrap();
    assert!(
        note.contains("peers.insert(id.to_string(), record);"),
        "anchor lost: note_peer_record changed"
    );
    assert!(
        !note.contains("name"),
        "FINDING closed? note_peer_record now propagates the name"
    );

    // 3. snapshot copies the stored entry wholesale, overriding only `online`.
    let snap = mgr.split("fn snapshot").nth(1).unwrap().split("fn publish").next().unwrap();
    assert!(
        snap.contains("online: i.peers.contains_key(&p.id),"),
        "anchor lost: snapshot no longer derives online from the peers map"
    );
    // Round 13 moved this OFF `snapshot`. Reading the name from `i.peers`
    // fixed the staleness by taking it from unsigned mDNS, which let anyone on
    // the LAN relabel an authenticated device. The refresh now happens after an
    // exchange, from the peer's Hello inside the Noise session.
    let mgr_all = code_of("src-tauri/src/sync/manager.rs");
    assert!(
        mgr_all.contains("if let Some(name) = stats.peer_name.clone()"),
        "FINDING: nothing refreshes a paired device's name, so a peer that has been renamed \
         keeps its old label for ever, including in the Unpair confirmation, which is the \
         one destructive action in that panel"
    );
    assert!(
        !snap.contains("i.peers.get(&p.id).map(|q| usable_peer_name"),
        "the paired name is read from the unsigned mDNS map, which is worse than stale"
    );
    // The freshly-discovered name is right there, one field away, and unused
    // for paired devices.
    let paired_block = snap.split("paired: i").nth(1).unwrap().split(".collect(),").next().unwrap();
    // Round 13: the refresh is deliberately NOT in `snapshot`. See above.
    assert!(
        !paired_block.contains("i.peers.get(&p.id)"),
        "the paired snapshot derives the displayed name from the unsigned mDNS map"
    );
}

/// A reinstall and its own corpse are shown with the same label.
///
/// `SettingsView.tsx` disambiguates same-named devices, but each list is
/// compared only against itself: `status.paired.filter(o => o.name === d.name)`
/// for paired, `unpaired.filter(o => o.name === p.name)` for discovered. A
/// reinstalled machine appears as a NEW id in `peers` and its dead pairing
/// stays in `paired`, so the user sees "Ben's PC" twice, in two panels, with
/// nothing to tell them which is which or that one is a ghost.
#[test]
fn r12_flow_a_reinstalled_peer_is_not_disambiguated_from_its_own_dead_pairing() {
    let view = read_src("src/views/SettingsView.tsx");

    assert!(
        view.contains("status.paired.filter((o) => o.name === d.name).length > 1"),
        "anchor lost: the paired-list disambiguation changed shape"
    );
    assert!(
        view.contains("unpaired.filter((o) => o.name === p.name).length > 1"),
        "anchor lost: the peer-list disambiguation changed shape"
    );

    // The finding: neither predicate looks at the other list.
    let paired_pred = "status.paired.filter((o) => o.name === d.name)";
    let peer_pred = "unpaired.filter((o) => o.name === p.name)";
    assert!(
        !view.contains("unpaired.some((o) => o.name === d.name)")
            && !view.contains("status.paired.some((o) => o.name === p.name)"),
        "FINDING closed? the two lists now disambiguate against each other"
    );
    assert!(
        view.contains(paired_pred) && view.contains(peer_pred),
        "both predicates must still exist for this finding to mean anything"
    );

    // And nothing anywhere flags a paired device that has never synced AND is
    // not currently visible as possibly gone for good.
    assert!(
        view.contains("Never connected"),
        "anchor lost: the never-connected label is gone"
    );
    assert!(
        !view.contains("reinstall") && !view.contains("Reinstall"),
        "FINDING closed? the UI now explains the reinstall case"
    );
}

// ===========================================================================
// R12-G. THE DELETE STORY.
//
// Clearing history has a confirmation that names every paired device by name
// and says it cannot be undone. Deleting ONE row — the action the whole
// authority design was built around, the one field test step 6 calls the
// riskiest thing in the product — is a single unguarded click on a trash icon.
//
// It travels. It is absorbing. It cannot be undone. The user is told none of
// that, and the row does not even say which machine it came from.
// ===========================================================================

#[test]
fn r12_flow_deleting_one_row_is_unguarded_while_clearing_all_is_not() {
    // Comments stripped: a comment that mentions a paired device is not
    // something the user reads.
    let hist = code_of("src/views/History.tsx");
    let settings = read_src("src/views/SettingsView.tsx");

    // Anchor: the app DOES know how to warn about a cross-device destructive
    // action, so the absence below is a decision, not a missing capability.
    assert!(
        settings.contains("This deletes every unpinned item on this device and on"),
        "anchor lost: the Clear confirmation changed"
    );
    assert!(
        settings.contains("It cannot be undone."),
        "anchor lost: the Clear confirmation no longer says it is permanent"
    );
    assert!(
        settings.contains("st.paired.map((d) => d.name)"),
        "anchor lost: Clear no longer names the paired devices"
    );

    // Anchor: the per-row delete exists and is reachable.
    assert!(
        hist.contains("api.deleteItem(item.id)"),
        "anchor lost: the per-row delete is gone"
    );

    // The finding: the delete button has no confirmation of any kind.
    let btn = hist
        .split("className=\"danger\" title=\"Delete\"")
        .nth(1)
        .expect("anchor lost: the delete button changed shape")
        .split("</button>")
        .next()
        .unwrap();
    assert!(
        btn.contains("confirmDelete(item)"),
        "FINDING: the per-row delete has no confirmation of any kind, while clearing all \
         history is guarded by a callout naming every paired device. The delete travels, is \
         absorbing on the peer and cannot be undone"
    );
    // INVERTED. The confirmation has to SAY what a delete does, or it is a
    // speed bump rather than an explanation.
    for phrase in ["cannot be undone", "paired"] {
        assert!(
            hist.contains(phrase),
            "FINDING: nothing in the History view says a delete travels to the other machine \
             or that it cannot be undone; it is a single unguarded click ('{phrase}' absent)"
        );
    }

    // The one message it DOES carry is the receiving end of the same action,
    // which proves the author knew deletes travel.
    assert!(
        hist.contains("It was deleted on another device"),
        "anchor lost: the vanished-row callout is gone"
    );
}

/// A synced row gives the user no way to know it is synced.
///
/// `HistoryItem` carries no source device, so the list cannot mark a row as
/// "from your PC". `Store::source_machine_of` exists and is never exposed
/// through a command. The delete confirmation could not name the other machine
/// even if someone wrote one.
#[test]
fn r12_flow_the_history_row_cannot_say_which_machine_it_came_from() {
    // Runtime: the store knows.
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id(A);
    let mine = s.insert_clipboard("mine", None, None).unwrap();
    let src = s.source_machine_of(mine).unwrap();
    assert_eq!(src.as_deref(), Some(A), "the store no longer records a source; premise gone");

    // And the payload does not carry it.
    let rows = s.search("", None, 10).unwrap();
    let j = serde_json::to_value(&rows[0]).unwrap();
    let j = j.as_object().unwrap();
    assert!(j.len() >= 10, "HistoryItem shrank to {} fields; re-check", j.len());
    for k in ["source_machine", "source", "device", "origin_id"] {
        assert!(!j.contains_key(k), "FINDING closed? HistoryItem now carries {k}");
    }

    // And no command exposes it.
    let cmds = code_of("src-tauri/src/commands.rs");
    assert!(
        cmds.contains("fn search_history") || cmds.contains("search_history"),
        "anchor lost: the history search command is gone"
    );
    assert!(
        !cmds.contains("source_machine_of"),
        "FINDING closed? a command now exposes the source machine"
    );
}

// ===========================================================================
// R12-H. AN EXCLUSION ADDED TODAY DOES NOT RECALL WHAT ALREADY LEFT.
//
// Round 10 closed the outbound half: `items_from` filters on `excluded_apps`,
// so adding a password manager stops its rows leaving from now on. Rows that
// left BEFORE that moment are on the other machine and stay there for ever.
// No tombstone is written, nothing is re-examined.
//
// The setting's own hint claims otherwise: "Parle never sends a row from an
// excluded app to your other devices."
// ===========================================================================

#[test]
fn r12_flow_excluding_an_app_leaves_everything_already_replicated_in_place() {
    let author = store_for(A);
    let peer = store_for(B);

    author.lock().insert_clipboard("hunter2", Some("Vault.exe"), None).unwrap();
    author.lock().insert_clipboard("ordinary", Some("Notepad.exe"), None).unwrap();

    let (_d, a) = sync_bounded((&author, A), (&peer, B));
    assert_eq!(a.applied_items, 2, "the premise needs both rows to arrive first: {a:?}");

    // The user now adds the password manager to the exclusion list.
    author.lock().set_excluded_apps(vec!["Vault.exe".into()]);

    // Vacuity guard: the filter really is in force. Without this the test would
    // pass against a build where set_excluded_apps did nothing at all.
    let offered = author.lock().items_from(A, 0, "", 100).unwrap();
    assert_eq!(offered.len(), 1, "the outbound filter is not working; premise gone");
    assert_eq!(offered[0].text, "ordinary");

    // Another exchange, and another, changes nothing on the peer.
    for _ in 0..2 {
        let _ = sync_bounded((&author, A), (&peer, B));
    }
    let still: i64 = peer
        .lock()
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM items WHERE text = 'hunter2'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        still, 1,
        "FINDING closed? the excluded row was recalled from the peer"
    );
    let tombs: i64 = peer
        .lock()
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM tombstones", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tombs, 0, "FINDING closed? excluding an app now writes a tombstone");

    // The claim the user reads.
    let view = read_src("src/views/SettingsView.tsx");
    // INVERTED. The rows already replicated genuinely are not recalled, which
    // the two assertions above prove; round 12 fixed the CLAIM rather than the
    // behaviour, because recall is a feature and the sentence was simply false.
    assert!(
        !view.contains("Parle never sends a row from an excluded app to your other devices"),
        "FINDING: the hint tells the user Parle never sends a row from an excluded app, and \
         everything sent before the entry was added stays on the peer for ever"
    );
    assert!(
        view.contains("Anything already synced stays on them"),
        "the hint no longer says what happens to rows that were already synced"
    );
}

// ===========================================================================
// R12-I. DOCUMENTATION TRUTH.
// ===========================================================================

/// `SYNC_HANDOVER.md` still reports round 10's test counts as the state.
///
/// Round 11 added 78 tests and its own commit message gives the new totals.
/// Section 1 of the handover was not updated, so the file a new reviewer is
/// told to read first opens with three numbers that are all wrong. That is the
/// same class of defect as a stale doc claim: it reads as a verified state.
#[test]
fn r12_flow_the_handover_still_reports_round_tens_test_counts() {
    let doc = read_src("docs/SYNC_HANDOVER.md");
    assert!(
        doc.contains("| Package | Result |"),
        "anchor lost: the state table is gone"
    );
    // INVERTED. The table now carries counts verified against a round-12 run of
    // all three packages. There is no way for a test to know the CURRENT
    // numbers without running them, so this asserts the stale ones are gone:
    // that is the failure mode, a file a new reviewer is told to read first
    // opening with three numbers presented as verified state.
    assert!(
        !doc.contains("153 pass") && !doc.contains("205 pass"),
        "FINDING: the handover still reports the test counts from an earlier round"
    );
}

/// Field test step 3 states the pairing direction backwards.
///
/// "The machine SHOWING the code is the one that only receives." It is true
/// that it only receives the CONNECTION — `serve_pairing` is inbound — and
/// `admit_inbound`'s own comment says as much. But a reader following step 3
/// with a two-machine setup in front of them reads "only receives" as "only
/// receives history", which is exactly what step 4 then asks them to check. It
/// is not: pairing is mutual and content flows both ways from the first
/// exchange.
#[test]
fn r12_flow_field_test_step_three_says_the_showing_machine_only_receives() {
    let doc = read_src("docs/SYNC_FIELD_TEST.md");
    // INVERTED. Step 3 no longer says the showing machine "only receives",
    // which is true of the connection and false of the data, and steps 4 and 5
    // then ask the tester to check exactly the data flowing the other way.
    assert!(
        !doc.contains("The machine SHOWING the code is the one that only receives."),
        "FINDING: step 3 states the direction in a way a tester will read as 'history only \
         flows one way', and the next two steps contradict it"
    );
    assert!(
        doc.contains("history flows BOTH ways"),
        "step 3 dropped the wrong claim without replacing it with the right one"
    );
    assert!(
        doc.contains("Pair.") && doc.contains("A dictation on one appears on the other."),
        "anchor lost: the steps around it changed"
    );
    // The code is unambiguous that pairing is mutual and symmetric.
    let view = read_src("src/views/SettingsView.tsx");
    assert!(
        view.contains("Pairing is mutual."),
        "anchor lost: the UI no longer says pairing is mutual"
    );
    assert!(
        view.contains("Either machine can start."),
        "anchor lost: the UI no longer says either machine can start"
    );
    // INVERTED. The doc now reconciles itself with what the UI says two lines
    // apart, instead of leaving the tester to notice the contradiction.
    assert!(
        doc.contains("mutual"),
        "FINDING: the field test never says pairing is mutual, while the UI the tester is \
         looking at says it twice"
    );
}

/// `SYNC_DESIGN.md`'s account of the secure-field gate is a round behind.
#[test]
fn r12_flow_the_design_doc_predates_the_third_answer() {
    let doc = read_src("docs/SYNC_DESIGN.md");
    let pipe = read_src("src-tauri/src/pipeline.rs");
    // Anchor: round 11 really did introduce a third answer.
    assert!(
        pipe.contains("enum FieldSecrecy") && pipe.contains("Unknown"),
        "anchor lost: FieldSecrecy is gone"
    );
    assert!(
        pipe.contains("fn keep_local_only"),
        "anchor lost: the local-only path is gone"
    );
    // INVERTED. The design doc has to describe the path, in a document that
    // already discusses the secure-field gate and stopped one answer short.
    for phrase in ["local_only", "FieldSecrecy", "third answer"] {
        assert!(
            doc.contains(phrase),
            "FINDING: SYNC_DESIGN.md predates the local-only path and never mentions \
             '{phrase}', while discussing the secure-field gate it belongs to"
        );
    }
    // And it is not a doc that ignores this area: it does discuss the gate.
    assert!(
        doc.to_lowercase().contains("secure") || doc.to_lowercase().contains("password"),
        "premise gone: SYNC_DESIGN.md does not discuss the secure-field gate at all"
    );
}

// ===========================================================================
// R12-J. THE EXCLUSION MIGRATION CAN ONLY EVER FIRE ONCE.
//
// Round 10's fix exists because `#[serde(default)]` reaches absent fields and
// not stale ones, so additions to the shipped password-manager list arrived at
// new installs only. `migrate` unions the defaults in — gated on the LITERAL
// `self.version < 2`, and then sets `self.version = SETTINGS_VERSION`.
//
// So the next entry added to `default_excluded_apps` has exactly the defect
// this function was written to fix, on every machine that has already run this
// build, unless someone remembers to bump SETTINGS_VERSION and widen the gate.
// Nothing in the code says so, and the gate's own doc comment describes the
// problem in the past tense.
// ===========================================================================

#[test]
fn r12_flow_the_exclusion_union_cannot_fire_a_second_time() {
    use echokey_core::settings::{Settings, SETTINGS_VERSION};

    let dir = std::env::temp_dir().join(format!("parle-r12-flow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("settings.json");

    // A machine that has already migrated: version 2, and a list that is
    // missing everything a later release might add.
    std::fs::write(
        &p,
        r#"{ "version": 2, "history": { "excluded_apps": ["com.1password.1password"] } }"#,
    )
    .unwrap();
    let loaded = Settings::load(&p).unwrap();

    // Vacuity guard: the file really did load and really did keep its list, so
    // "nothing was added" is a statement about the migration and not about a
    // parse failure.
    assert_eq!(loaded.version, SETTINGS_VERSION);
    // INVERTED. The union no longer asks what VERSION the file is, it asks
    // which defaults this install has already been OFFERED, so a version-2 file
    // with no record is repaired like any other. That is the whole finding: a
    // gate on a literal 2 could only ever fire once, and reproduced for the
    // next addition exactly the defect the union exists to fix.
    assert!(
        loaded.history.excluded_apps.len() > 1,
        "FINDING: a version-2 file is not repaired, so the next addition to the shipped \
         exclusions reaches new installs and nobody else"
    );

    // And the same file at version 1 IS repaired, which is what proves the
    // union works at all and that the gate is the only thing stopping it.
    std::fs::write(
        &p,
        r#"{ "version": 1, "history": { "excluded_apps": ["com.1password.1password"] } }"#,
    )
    .unwrap();
    let v1 = Settings::load(&p).unwrap();
    assert!(
        v1.history.excluded_apps.len() > 5,
        "premise gone: the union does not work even from version 1 ({} entries)",
        v1.history.excluded_apps.len()
    );
    assert!(
        v1.history.excluded_apps.iter().any(|a| a == "com.apple.Passwords"),
        "premise gone: the union no longer adds the macOS system password manager"
    );

    // The gate is a literal, not a comparison against the shipped list.
    let src = code_of("crates/echokey-core/src/settings.rs");
    assert!(
        src.contains("fn migrate"),
        "anchor lost: migrate is gone"
    );
    assert!(
        !src.contains("if self.version < 2 {"),
        "FINDING: the migration is gated on a hard-coded 2, so it can only ever fire once and \
         the next addition to the shipped exclusions reaches new installs and nobody else"
    );
    assert!(
        src.contains("excluded_defaults_seen"),
        "the gate is gone but nothing records which defaults were already offered, so a \
         deliberate removal is undone on every launch instead"
    );

    let _ = std::fs::remove_file(&p);
}
