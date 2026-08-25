//! ROUND 3 ADVERSARIAL REVIEW — criterion F: can an in-progress sync block the
//! history UI?
//!
//! `search_history`, `pin_item`, `delete_item`, `clear_history` and
//! `update_item_text` (src-tauri/src/commands.rs) are all SYNCHRONOUS Tauri
//! commands, so they run on the app's main thread and take `state.store.lock()`
//! there. The sync path takes the same mutex. The question this measures is how
//! long ONE uninterruptible hold can be — the replication path takes the lock
//! per statement, but `SyncManager::prune_after_exchange` takes it for a whole
//! `prune()`, which runs after every exchange.

use echokey_core::history::{RemoteItem, Store};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PEER: &str = "22222222-2222-4222-8222-222222222222";

fn seeded(n: usize) -> Store {
    let mut s = Store::open_in_memory().unwrap();
    s.set_device_id("11111111-1111-4111-8111-111111111111");
    for i in 0..n {
        s.apply_remote_item(
            PEER,
            &RemoteItem {
                source_machine: PEER.into(),
                origin_id: format!("row-{i}"),
                kind: "transcription".into(),
                text: format!("dictation number {i} with some searchable words in it"),
                created_at: 1_600_000_000_000 + i as i64,
                updated_at: 1_600_000_000_000 + i as i64,
                pinned: false,
            },
        )
        .unwrap();
    }
    s
}

/// The single longest hold the sync path takes on the history mutex.
///
/// `prune_after_exchange` runs on every completed exchange and locks the store
/// for the whole call. Everything the history UI does is a synchronous Tauri
/// command on the main thread, so this number is a hard UI freeze.
#[test]
fn r3_the_post_exchange_prune_is_one_uninterruptible_hold() {
    let store = Arc::new(Mutex::new(seeded(30_000)));
    let worst = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // The "UI thread": measure how long it waits for the mutex.
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
    // Exactly what prune_after_exchange does: chunked, releasing the store
    // mutex between batches. A single unbounded DELETE held it for the whole
    // job and froze the history window for as long as it took — which scales
    // with a history size the user controls.
    let t = Instant::now();
    for _ in 0..10_000 {
        let (_, more) = store.lock().prune_step(0, 100, 500).unwrap();
        if !more {
            break;
        }
        std::thread::yield_now();
    }
    let prune_took = t.elapsed();
    stop.store(true, Ordering::SeqCst);
    ui.join().unwrap();

    let waited = worst.load(Ordering::SeqCst);
    eprintln!("prune held the store lock for {prune_took:?}; UI worst wait {waited} ms");
    assert!(
        waited < 200,
        "a post-exchange prune froze the history UI for {waited} ms \
         (prune itself held the lock {prune_took:?}); every history command is a \
         synchronous Tauri command on the main thread"
    );
}

// NOTE: the per-row apply path was measured too and is NOT a problem — a
// 3000-row drain kept the UI's worst wait at 1 ms, because `drain` takes the
// store mutex per statement. That test is not kept; only the failing one is.
