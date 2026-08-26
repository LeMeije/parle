//! ROUND 4 ADVERSARIAL REVIEW — criterion F, second front.
//!
//! Round 3 chunked `prune_after_exchange` so the post-exchange prune could no
//! longer freeze the history window. The SERVE path was not chunked, and it
//! takes the same mutex in one statement:
//!
//!     src-tauri/src/sync/replicate.rs:747  fetch_page()
//!         store.lock().items_since(source, after, PAGE)       // PAGE = 256
//!         ... if the whole page shares one millisecond ...
//!         store.lock().items_since(source, after, WIDE_PAGE)  // WIDE_PAGE = 20_000
//!
//! The widening trigger is `page.first().updated_at == page.last().updated_at`,
//! and `updated_at` is a value a PAIRED PEER chooses: `apply_remote_item`
//! stores the peer's clock verbatim (history.rs:1113). So a peer that stamps
//! its rows with one identical `updated_at` makes every later `serve()` on this
//! machine materialise up to 20 000 rows — text included, up to
//! `MAX_ITEM_TEXT_BYTES` (1 MiB) each — inside ONE uninterruptible hold of the
//! store mutex.
//!
//! Every history command (`search_history`, `pin_item`, `delete_item`,
//! `clear_history`, `update_item_text`) is a synchronous Tauri command on the
//! main thread taking that same mutex, so the hold is a hard UI freeze.

use echokey_core::history::{RemoteItem, Store};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PEER: &str = "22222222-2222-4222-8222-222222222222";
/// replicate.rs:38
const WIDE_PAGE: usize = 20_000;
const MAX_BATCHES: usize = 64;
/// replicate.rs:31 — echokey_sync::MAX_BATCH_LEN
const PAGE: usize = 256;

/// `n` rows from PEER, every one stamped with the SAME `updated_at`.
/// Exactly what `apply_remote_item` stores when the peer sends them that way.
fn saturated_millisecond(n: usize, text_bytes: usize) -> Store {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id("11111111-1111-4111-8111-111111111111");
    let clock = 1_600_000_000_000i64;
    let text = "x".repeat(text_bytes);
    for i in 0..n {
        s.apply_remote_item(
            PEER,
            &RemoteItem {
                source_machine: PEER.into(),
                origin_id: format!("row-{i}"),
                kind: "transcription".into(),
                text: text.clone(),
                created_at: clock,
                // The whole point: one millisecond, chosen by the peer.
                updated_at: clock,
                pinned: false,
            },
        )
        .unwrap();
    }
    s
}

/// Drive the real `fetch_page` shape and measure the worst wait a UI command
/// suffers for the store mutex while it runs.
fn worst_ui_wait_during_serve(store: Arc<Mutex<Store>>) -> (u64, Duration) {
    let worst = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let s2 = store.clone();
    let w2 = worst.clone();
    let st2 = stop.clone();
    let ui = std::thread::spawn(move || {
        while !st2.load(Ordering::SeqCst) {
            let t = Instant::now();
            let g = s2.lock();
            let waited = t.elapsed();
            let _ = g.search("dictation", None, 60);
            drop(g);
            w2.fetch_max(waited.as_millis() as u64, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    std::thread::sleep(Duration::from_millis(50));

    // === serve()'s paging, verbatim ===
    //
    // The cursor is a (clock, origin_id) pair, so a millisecond no longer has
    // to be swallowed whole: every page is PAGE rows and the store mutex is
    // released between them. The old code detected a fully-saturated page and
    // re-fetched up to 20,000 rows in ONE statement — which both missed the
    // common case (a page that merely ENDS inside a millisecond) and let a
    // paired peer freeze the history window by choosing one `updated_at`.
    let t = Instant::now();
    let mut after = 0i64;
    let mut after_origin = String::new();
    let mut fetched = 0usize;
    for _ in 0..MAX_BATCHES {
        let page = store
            .lock()
            .items_from(PEER, after, &after_origin, PAGE)
            .unwrap();
        if page.is_empty() {
            break;
        }
        fetched += page.len();
        let last = page.last().unwrap();
        after = last.updated_at;
        after_origin = last.origin_id.clone();
        if page.len() < PAGE {
            break;
        }
        std::thread::yield_now();
    }
    let took = t.elapsed();
    assert!(fetched > PAGE, "precondition: more than one page of rows");

    stop.store(true, Ordering::SeqCst);
    ui.join().unwrap();
    (worst.load(Ordering::SeqCst), took)
}

/// A paired peer choosing one `updated_at` for 20,000 rows must not freeze the
/// history window: the pages are bounded and the mutex is released between
/// them.
#[test]
fn r4_the_widened_serve_page_is_one_uninterruptible_hold() {
    // 4 KiB a row: well under the 1 MiB the wire allows, i.e. a conservative
    // version of the attack.
    let store = Arc::new(Mutex::new(saturated_millisecond(WIDE_PAGE, 4096)));
    let (waited, took) = worst_ui_wait_during_serve(store);
    eprintln!("widened serve page held the store lock {took:?}; UI worst wait {waited} ms");
    assert!(
        waited < 200,
        "a single serve() page froze the history UI for {waited} ms (the fetch itself \
         held the store mutex {took:?}). `prune_after_exchange` was chunked in round 3; \
         `fetch_page`'s WIDE_PAGE re-fetch was not, and its trigger — every row in a \
         page sharing one `updated_at` — is a value the PAIRED PEER chooses."
    );
}
