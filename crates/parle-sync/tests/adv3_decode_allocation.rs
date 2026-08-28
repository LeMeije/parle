//! ADVERSARIAL REVIEW (round 3) — demonstration of a live finding. NOT a fix.
//!
//! Claim under test (wire.rs `bounded_batch`): "Stopping inside the visitor
//! makes the limit a bound on allocation rather than a verdict on it", and the
//! round-2 regression test's assertion that "the vector never grows past the
//! limit in the first place".
//!
//! `SyncMessage` is an INTERNALLY TAGGED enum (`#[serde(tag = "type")]`). To
//! find the tag, serde must buffer the entire JSON object into its private
//! `Content` tree BEFORE it knows which variant to build — and only then does
//! it replay that tree through the variant's fields, which is where
//! `bounded_batch` finally runs. So the peer still picks the allocation, and
//! `MAX_MESSAGE_BYTES` (4 MiB) is the only thing bounding it.
//!
//! This is a standalone test binary so the counting allocator cannot perturb
//! the crate's own unit tests, and it holds exactly one test so no other thread
//! is allocating while the measurement runs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use parle_sync::{SyncMessage, MAX_BATCH_LEN, MAX_MESSAGE_BYTES};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn grew(n: usize) {
    let now = LIVE.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(l);
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        System.dealloc(p, l);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            if new >= l.size() {
                grew(new - l.size());
            } else {
                LIVE.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// A `watermarks` message with `n` entries. Every entry is 62 bytes of JSON.
fn watermarks_json(n: usize) -> String {
    const ENTRY: &str = r#"{"source_device":"3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d","clock":1}"#;
    let mut s = String::with_capacity(64 + n * (ENTRY.len() + 1));
    s.push_str(r#"{"watermarks":{"more":false,"entries":["#);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push_str(ENTRY);
    }
    s.push_str("]}}");
    s
}

/// Peak *extra* bytes live at any moment inside `decode`.
fn peak_extra_during_decode(bytes: &[u8]) -> usize {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let _ = SyncMessage::decode(bytes);
    PEAK.load(Ordering::Relaxed).saturating_sub(base)
}

#[test]
fn adv3_decode_allocates_no_more_than_the_batch_limit_permits() {
    // Warm up: first-touch allocations in serde_json / the formatter must not
    // land inside the measurement.
    let warm = watermarks_json(MAX_BATCH_LEN);
    assert!(SyncMessage::decode(warm.as_bytes()).is_ok());
    let _ = peak_extra_during_decode(warm.as_bytes());

    // Control: a legal message, exactly at the batch limit.
    let legal = watermarks_json(MAX_BATCH_LEN);
    let legal_peak = peak_extra_during_decode(legal.as_bytes());

    // The attack: one message inside MAX_MESSAGE_BYTES carrying far more
    // entries than the cap. `bounded_batch` refuses it — the question is how
    // much memory the peer made us touch on the way to that refusal.
    let hostile = watermarks_json(60_000);
    assert!(
        hostile.len() < MAX_MESSAGE_BYTES,
        "the attack must fit inside the byte cap to be interesting ({} bytes)",
        hostile.len()
    );
    assert!(
        SyncMessage::decode(hostile.as_bytes()).is_err(),
        "an oversized batch must not decode"
    );
    let hostile_peak = peak_extra_during_decode(hostile.as_bytes());

    // The former worst case: an unknown field used to be buffered into
    // serde's Content tree and then thrown away. The enum is externally tagged
    // and `deny_unknown_fields` now, so this is refused rather than parsed.
    let mut junk = String::from(r#"{"watermarks":{"entries":[],"more":false,"x":["#);
    while junk.len() < MAX_MESSAGE_BYTES - 16 {
        junk.push_str("1,");
    }
    junk.push_str("1]}}");
    assert!(junk.len() <= MAX_MESSAGE_BYTES);
    let junk_peak = peak_extra_during_decode(junk.as_bytes());
    assert!(
        junk_peak <= junk.len(),
        "a {} byte message whose payload is an IGNORED unknown field made decode          peak at {junk_peak} live bytes ({}x the wire size). MAX_MESSAGE_BYTES is          the only bound on decode, and it is not a bound on memory.",
        junk.len(),
        junk_peak / junk.len().max(1)
    );

    assert!(
        hostile_peak <= legal_peak * 8,
        "decoding a refused {}-entry batch peaked at {hostile_peak} bytes against \
         {legal_peak} bytes for a legal {MAX_BATCH_LEN}-entry one ({}x). \
         The {MAX_BATCH_LEN}-entry cap is still a verdict, not a bound: the \
         internally-tagged enum buffers the whole message into serde's Content \
         tree before bounded_batch ever runs, so a peer picks the allocation.",
        60_000,
        hostile_peak / legal_peak.max(1)
    );
    let _ = junk_peak;
}
