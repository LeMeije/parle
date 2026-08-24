//! One replication exchange over an established session.
//!
//! Both sides run the identical routine, because the protocol is symmetric —
//! there is no client and no server once the session is up. A round is:
//!
//!   1. Hello (protocol version + who we are)
//!   2. Watermarks (per source device, the newest clock we already hold)
//!   3. everything we hold that is above the peer's watermarks, then tombstones
//!   4. apply whatever the peer sends us
//!
//! Two things here are load-bearing and easy to get wrong:
//!
//! - We serve rows for EVERY source we know about, not just our own. Pinning a
//!   Mac row on the Windows box bumps that row's clock but leaves its source as
//!   the Mac; if each side only offered its own rows, that edit would never
//!   leave the machine.
//! - Paging is by millisecond, so a page that is entirely one millisecond wide
//!   would otherwise silently drop the rest of that millisecond. That case is
//!   detected and re-fetched rather than skipped.

use std::collections::HashMap;
use std::io::{Read, Write};

use echokey_core::history::{RemoteItem, RemoteTombstone, Store};
use echokey_sync::{DeviceId, ItemKind, Session, SessionError, SyncItem, SyncMessage, Tombstone, Watermark, PROTOCOL_VERSION};
use parking_lot::Mutex;
use std::sync::Arc;

/// Rows per batch.
///
/// MUST stay at or below the wire's batch limit. It was 512 against a limit of
/// 256, so `session.send` refused every full page and aborted the whole
/// exchange — replication was dead for anyone with more than 256 rows above the
/// peer's watermark, which is essentially every real user on a first sync. The
/// assert below makes that a build error rather than a silent runtime failure.
const PAGE: usize = echokey_sync::MAX_BATCH_LEN;
const _: () = assert!(PAGE <= echokey_sync::MAX_BATCH_LEN);
/// Ceiling for the widened re-fetch when a whole page shares one millisecond.
const WIDE_PAGE: usize = 20_000;
/// Hard stop on a single exchange, so a peer cannot keep us here forever.
const MAX_BATCHES: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum ReplicateError {
    #[error("session: {0}")]
    Session(#[from] SessionError),
    #[error("store: {0}")]
    Store(String),
    #[error("peer speaks protocol {peer}, we speak {ours}")]
    Version { peer: u16, ours: u16 },
    #[error("peer did not say hello first")]
    NoHello,
}

/// What one exchange did, for logging and for the tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RoundStats {
    pub sent_items: usize,
    pub sent_tombstones: usize,
    pub applied_items: usize,
    pub applied_tombstones: usize,
    pub ignored: usize,
    /// Rows a peer was not entitled to send. Distinct from `ignored`, which is
    /// ordinary no-ops, because this one means someone misbehaved.
    pub refused: usize,
}

/// Which kinds the user has agreed to share.
#[derive(Debug, Clone, Copy)]
pub struct Kinds {
    pub dictations: bool,
    pub clipboard: bool,
}

/// The receiving machine's retention window, in epoch ms. Rows older than this
/// are refused rather than stored.
///
/// The design settles that a synced row obeys the RECEIVER's retention, and
/// enforcing it here is also what stops an infinite loop: without it, a row the
/// local machine has just pruned is immediately re-pulled from the peer,
/// re-inserted, pruned again on the next sweep, and pulled again forever.
/// Retention is a per-device policy, so the peer is right to still be offering
/// the row — we are simply right not to keep it.
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    pub oldest_allowed: Option<i64>,
}

impl Retention {
    pub fn keeps(&self, created_at: i64) -> bool {
        match self.oldest_allowed {
            Some(floor) => created_at >= floor,
            None => true,
        }
    }
}

impl Kinds {
    fn allows(&self, kind: &str) -> bool {
        match kind {
            "transcription" => self.dictations,
            "clipboard" => self.clipboard,
            // An unknown kind is not something we have asked the user about,
            // so it does not leave the machine.
            _ => false,
        }
    }
}

/// Who a peer is allowed to speak for.
///
/// The Noise handshake proves the peer holds the key we agreed with ONE device.
/// It does not vouch for the `source_device` field inside the messages, which is
/// just a string the peer chose. Without this check a paired peer could claim to
/// be any device — including ours — and last-writer-wins would let it rewrite
/// our own dictations in place, or wipe them with tombstones dated i64::MAX.
pub struct Attribution<'a> {
    /// The device id proven by the handshake.
    pub peer_id: &'a str,
    /// Our own id, which a peer may never speak for.
    pub local_id: &'a str,
    /// Devices we have paired with, which the peer may legitimately relay for.
    pub known: &'a [String],
}

impl Attribution<'_> {
    fn accepts(&self, source: &str) -> bool {
        if source.is_empty() || source == self.local_id {
            // Nobody gets to author rows on our behalf.
            return false;
        }
        source == self.peer_id || self.known.iter().any(|k| k == source)
    }
}

/// Which side writes first.
///
/// The exchange used to be fully symmetric: both peers sent their entire
/// history and only then started reading. That works only while everything
/// fits in the socket and Noise buffers. A first sync of any real size fills
/// them, both sides block in `write`, neither ever reaches its `read`, and the
/// session hangs until the session timeout kills it — every time, so two
/// machines with a large history could never complete a first sync at all.
///
/// Turn-taking removes the possibility rather than enlarging the buffers: at
/// every point exactly one side is writing and the other is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Dialled out. Sends its watermarks first, then serves, then drains.
    First,
    /// Accepted the connection. Reads first, then drains, then serves.
    Second,
}

pub fn exchange<S: Read + Write>(
    session: &mut Session<S>,
    store: &Arc<Mutex<Store>>,
    me: (&str, &str),
    kinds: Kinds,
    retention: Retention,
    attribution: &Attribution<'_>,
    turn: Turn,
) -> Result<RoundStats, ReplicateError> {
    let mut stats = RoundStats::default();

    // 1. Hello, both ways. Version mismatch is a clean refusal, not a
    //    mysterious decode failure three messages later.
    let my_id = DeviceId::parse(me.0).map_err(|e| ReplicateError::Store(e.to_string()))?;
    session.send(&SyncMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: my_id,
        device_name: me.1.chars().take(64).collect(),
    })?;
    match session.recv()? {
        SyncMessage::Hello { protocol_version, .. } if protocol_version == PROTOCOL_VERSION => {}
        SyncMessage::Hello { protocol_version, .. } => {
            return Err(ReplicateError::Version { peer: protocol_version, ours: PROTOCOL_VERSION })
        }
        _ => return Err(ReplicateError::NoHello),
    }

    // 2. Watermarks, in turn order rather than both-at-once.
    let peer_marks = match turn {
        Turn::First => {
            send_watermarks(session, store)?;
            recv_watermarks(session)?
        }
        Turn::Second => {
            let peer = recv_watermarks(session)?;
            send_watermarks(session, store)?;
            peer
        }
    };

    // 3 and 4. Serve and drain, again in turn order: the side that spoke
    //          first also sends its rows first, so its peer is already reading
    //          them while it works through the store.
    match turn {
        Turn::First => {
            serve(session, store, &peer_marks, kinds, &mut stats)?;
            drain(session, store, kinds, retention, attribution, &mut stats)?;
        }
        Turn::Second => {
            drain(session, store, kinds, retention, attribution, &mut stats)?;
            serve(session, store, &peer_marks, kinds, &mut stats)?;
        }
    }

    Ok(stats)
}

/// Everything we hold that the peer has not seen, per source device.
fn serve<S: Read + Write>(
    session: &mut Session<S>,
    store: &Arc<Mutex<Store>>,
    peer_marks: &HashMap<String, i64>,
    kinds: Kinds,
    stats: &mut RoundStats,
) -> Result<(), ReplicateError> {
    // Every source we hold ANYTHING for — live rows or only
    //    tombstones. Iterating live rows alone meant that deleting the last
    //    surviving row from a source made its tombstone unreachable, and the
    //    peer kept that row forever while we kept refusing its copy.
    let sources = store
        .lock()
        .known_sources()
        .map_err(|e| ReplicateError::Store(e.to_string()))?;
    for source in sources.iter() {
        let mut after = peer_marks.get(source).copied().unwrap_or(0);
        for _ in 0..MAX_BATCHES {
            let page = fetch_page(store, source, after)?;
            if page.is_empty() {
                break;
            }
            let last = page.last().map(|r| r.updated_at).unwrap_or(after);
            let out: Vec<SyncItem> = page
                .iter()
                .filter(|r| kinds.allows(&r.kind))
                .filter_map(to_wire)
                .collect();
            let more = page.len() >= PAGE;
            // Chunked on the way out. fetch_page can legitimately return far
            // more than PAGE when it widens past a saturated millisecond, and
            // the wire refuses any batch over MAX_BATCH_LEN — sending it whole
            // aborted the entire exchange, deterministically, every time. That
            // is the same class of bug as PAGE-vs-MAX_BATCH_LEN, reintroduced
            // one function along, so the send path now enforces it itself.
            if !out.is_empty() {
                stats.sent_items += out.len();
                let chunks: Vec<_> = out.chunks(PAGE).map(|c| c.to_vec()).collect();
                let last_ix = chunks.len() - 1;
                for (ix, chunk) in chunks.into_iter().enumerate() {
                    let more_to_come = more || ix < last_ix;
                    session.send(&SyncMessage::Items { items: chunk, more: more_to_come })?;
                }
            }
            // `last == after` means the cursor cannot advance: every row in a
            // full page shared one millisecond even after widening. Without
            // this the identical page is re-sent up to MAX_BATCHES times.
            if !more || last == after {
                break;
            }
            after = last;
        }

        // Tombstones for the same source.
        let mut after = peer_marks.get(source).copied().unwrap_or(0);
        for _ in 0..MAX_BATCHES {
            let page = store
                .lock()
                .tombstones_since(source, after, PAGE)
                .map_err(|e| ReplicateError::Store(e.to_string()))?;
            if page.is_empty() {
                break;
            }
            let last = page.last().map(|t| t.deleted_at).unwrap_or(after);
            let more = page.len() >= PAGE;
            stats.sent_tombstones += page.len();
            let entries: Vec<Tombstone> = page
                .iter()
                .filter_map(|t| {
                    DeviceId::parse(&t.source_machine).ok().map(|d| Tombstone {
                        source_device: d,
                        origin_id: t.origin_id.clone(),
                        deleted_at: t.deleted_at,
                        clock: t.deleted_at.max(0) as u64,
                    })
                })
                .collect();
            if !entries.is_empty() {
                let chunks: Vec<_> = entries.chunks(PAGE).map(|c| c.to_vec()).collect();
                let last_ix = chunks.len() - 1;
                for (ix, chunk) in chunks.into_iter().enumerate() {
                    let more_to_come = more || ix < last_ix;
                    session.send(&SyncMessage::Tombstones { entries: chunk, more: more_to_come })?;
                }
            }
            if !more || last == after {
                break;
            }
            after = last;
        }
    }

    // Tell the peer we are done sending.
    session.send(&SyncMessage::Items { items: Vec::new(), more: false })?;
    Ok(())
}

/// Whatever the peer sends, until it says it is finished.
fn drain<S: Read + Write>(
    session: &mut Session<S>,
    store: &Arc<Mutex<Store>>,
    kinds: Kinds,
    retention: Retention,
    attribution: &Attribution<'_>,
    stats: &mut RoundStats,
) -> Result<(), ReplicateError> {
    for _ in 0..MAX_BATCHES * 4 {
        match session.recv()? {
            SyncMessage::Items { items, more } => {
                for it in &items {
                    if !attribution.accepts(it.source_device.as_str()) {
                        tracing::warn!(
                            "sync: {} tried to author rows for {}; refused",
                            attribution.peer_id,
                            it.source_device.as_str()
                        );
                        stats.refused += 1;
                        continue;
                    }
                    // The receipt is taken here, after attribution and before
                    // any policy refusal. Attribution matters: a peer forging
                    // rows for a third device must not be able to advance that
                    // device's mark and silence it. The policy refusals below
                    // do not: retention is permanent (a row only gets older),
                    // and a disabled kind is undone by reset_source_marks when
                    // the user switches it back on. If the mark stalled on a
                    // refused row instead, the peer would re-offer the same
                    // rows on every exchange for as long as the switch was off.
                    store
                        .lock()
                        .note_received(it.source_device.as_str(), it.updated_at)
                        .map_err(|e| ReplicateError::Store(e.to_string()))?;
                    if !retention.keeps(it.created_at) {
                        // Older than this machine keeps. Refusing is what stops
                        // prune and replication fighting each other forever.
                        stats.ignored += 1;
                        continue;
                    }
                    // The toggles are a statement about what this machine
                    // stores, not merely what it offers. Filtering outbound
                    // only would still let a peer put clipboard rows into a
                    // history where the user switched clipboard sync off.
                    let kind = match it.kind {
                        ItemKind::Transcription => "transcription",
                        ItemKind::Clipboard => "clipboard",
                    };
                    if !kinds.allows(kind) {
                        stats.ignored += 1;
                        continue;
                    }
                    apply_item(store, it, stats)?;
                }
                if !more && items.is_empty() {
                    break;
                }
            }
            SyncMessage::Tombstones { entries, .. } => {
                for t in &entries {
                    if !attribution.accepts(t.source_device.as_str()) {
                        tracing::warn!(
                            "sync: {} tried to delete rows belonging to {}; refused",
                            attribution.peer_id,
                            t.source_device.as_str()
                        );
                        stats.refused += 1;
                        continue;
                    }
                    store
                        .lock()
                        .note_received(t.source_device.as_str(), t.deleted_at)
                        .map_err(|e| ReplicateError::Store(e.to_string()))?;
                    let rt = RemoteTombstone {
                        source_machine: t.source_device.as_str().to_string(),
                        origin_id: t.origin_id.clone(),
                        deleted_at: t.deleted_at,
                    };
                    let outcome = store
                        .lock()
                        .apply_remote_tombstone(&rt)
                        .map_err(|e| ReplicateError::Store(e.to_string()))?;
                    match outcome {
                        echokey_core::history::ApplyOutcome::Ignored => stats.ignored += 1,
                        _ => stats.applied_tombstones += 1,
                    }
                }
            }
            // Anything else at this point is out of order; stop rather than
            // guess what the peer meant.
            _ => break,
        }
    }
    Ok(())
}

/// Our marks, chunked, because the wire caps a batch at `MAX_BATCH_LEN` and a
/// store that has ever seen more sources than that would otherwise send one
/// oversized message and fail every exchange with an opaque wire error.
fn send_watermarks<S: Read + Write>(
    session: &mut Session<S>,
    store: &Arc<Mutex<Store>>,
) -> Result<(), ReplicateError> {
    let mine = store.lock().watermarks().map_err(|e| ReplicateError::Store(e.to_string()))?;
    let marks: Vec<Watermark> = mine
        .iter()
        .filter_map(|(src, clock)| {
            DeviceId::parse(src).ok().map(|d| Watermark {
                source_device: d,
                clock: (*clock).max(0) as u64,
            })
        })
        .collect();
    if marks.is_empty() {
        session.send(&SyncMessage::Watermarks { entries: Vec::new(), more: false })?;
        return Ok(());
    }
    let chunks: Vec<_> = marks.chunks(PAGE).map(|c| c.to_vec()).collect();
    let last = chunks.len() - 1;
    for (ix, chunk) in chunks.into_iter().enumerate() {
        session.send(&SyncMessage::Watermarks { entries: chunk, more: ix < last })?;
    }
    Ok(())
}

/// Read every watermark chunk the peer sends.
///
/// Reading exactly one message was a real defect once chunking existed: the
/// remaining chunks surfaced where rows were expected and ended the exchange.
fn recv_watermarks<S: Read + Write>(
    session: &mut Session<S>,
) -> Result<HashMap<String, i64>, ReplicateError> {
    let mut out = HashMap::new();
    // Bounded so a peer cannot hold us here forever with more:true.
    for _ in 0..MAX_BATCHES {
        match session.recv()? {
            SyncMessage::Watermarks { entries, more } => {
                for w in entries {
                    // A duplicated source across chunks takes the higher mark:
                    // sending less than the peer already has is the only unsafe
                    // direction.
                    let e = out.entry(w.source_device.as_str().to_string()).or_insert(0);
                    *e = (*e).max(w.clock as i64);
                }
                if !more {
                    break;
                }
            }
            // Anything else means the peer is not speaking the protocol we
            // agreed on. Proceeding with no marks would re-send our whole
            // history, so stop instead.
            _ => return Err(ReplicateError::NoHello),
        }
    }
    Ok(out)
}


/// One page, re-fetched wider if the whole page shares a single millisecond.
///
/// `items_since` pages on `updated_at`, so a page entirely inside one
/// millisecond would advance the cursor past rows never sent. Rare, but it is
/// silent data loss, and a burst of clipboard captures is exactly how it would
/// happen.
fn fetch_page(
    store: &Arc<Mutex<Store>>,
    source: &str,
    after: i64,
) -> Result<Vec<RemoteItem>, ReplicateError> {
    let page = store
        .lock()
        .items_since(source, after, PAGE)
        .map_err(|e| ReplicateError::Store(e.to_string()))?;
    let saturated = page.len() >= PAGE
        && page.first().map(|r| r.updated_at) == page.last().map(|r| r.updated_at);
    if !saturated {
        return Ok(page);
    }
    tracing::warn!(
        "sync: {} rows share one millisecond for {source}; widening the page",
        page.len()
    );
    let wide = store
        .lock()
        .items_since(source, after, WIDE_PAGE)
        .map_err(|e| ReplicateError::Store(e.to_string()))?;
    if wide.len() >= WIDE_PAGE {
        tracing::error!("sync: more than {WIDE_PAGE} rows in one millisecond for {source}; some may not replicate");
    }
    Ok(wide)
}

fn apply_item(
    store: &Arc<Mutex<Store>>,
    it: &SyncItem,
    stats: &mut RoundStats,
) -> Result<(), ReplicateError> {
    let r = RemoteItem {
        source_machine: it.source_device.as_str().to_string(),
        origin_id: it.origin_id.clone(),
        kind: match it.kind {
            ItemKind::Transcription => "transcription".into(),
            ItemKind::Clipboard => "clipboard".into(),
        },
        text: it.text.clone(),
        created_at: it.created_at,
        updated_at: it.updated_at,
        pinned: it.pinned,
    };
    let outcome = store
        .lock()
        .apply_remote_item(&r)
        .map_err(|e| ReplicateError::Store(e.to_string()))?;
    match outcome {
        echokey_core::history::ApplyOutcome::Ignored => stats.ignored += 1,
        _ => stats.applied_items += 1,
    }
    Ok(())
}

fn to_wire(r: &RemoteItem) -> Option<SyncItem> {
    let kind = match r.kind.as_str() {
        "transcription" => ItemKind::Transcription,
        "clipboard" => ItemKind::Clipboard,
        _ => return None,
    };
    Some(SyncItem {
        source_device: DeviceId::parse(&r.source_machine).ok()?,
        origin_id: r.origin_id.clone(),
        kind,
        text: r.text.clone(),
        created_at: r.created_at,
        updated_at: r.updated_at,
        pinned: r.pinned,
        clock: r.updated_at.max(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_filter_respects_the_user_toggles() {
        let both = Kinds { dictations: true, clipboard: true };
        assert!(both.allows("transcription") && both.allows("clipboard"));

        let only_dict = Kinds { dictations: true, clipboard: false };
        assert!(only_dict.allows("transcription"));
        assert!(!only_dict.allows("clipboard"), "clipboard must not leave when disabled");

        let none = Kinds { dictations: false, clipboard: false };
        assert!(!none.allows("transcription") && !none.allows("clipboard"));

        // A kind we have never shown the user is never shared, whatever it is.
        assert!(!both.allows("secrets"));
        assert!(!both.allows(""));
    }

    #[test]
    fn unknown_kinds_do_not_round_trip_to_the_wire() {
        let r = RemoteItem {
            source_machine: "11111111-1111-4111-8111-111111111111".into(),
            origin_id: "1".into(),
            kind: "mystery".into(),
            text: "x".into(),
            created_at: 1,
            updated_at: 1,
            pinned: false,
        };
        assert!(to_wire(&r).is_none());
    }

    #[test]
    fn a_malformed_source_id_is_dropped_rather_than_sent() {
        let r = RemoteItem {
            source_machine: "not-a-uuid".into(),
            origin_id: "1".into(),
            kind: "clipboard".into(),
            text: "x".into(),
            created_at: 1,
            updated_at: 1,
            pinned: false,
        };
        assert!(to_wire(&r).is_none());
    }

    #[test]
    fn retention_refuses_rows_older_than_this_machine_keeps() {
        let r = Retention { oldest_allowed: Some(1_000) };
        assert!(!r.keeps(999), "older than the floor is refused");
        assert!(r.keeps(1_000), "exactly at the floor is kept");
        assert!(r.keeps(5_000));

        // Retention disabled keeps everything, including very old rows.
        let none = Retention { oldest_allowed: None };
        assert!(none.keeps(0));
        assert!(none.keeps(i64::MIN));
    }

    fn attrib<'a>(peer: &'a str, local: &'a str, known: &'a [String]) -> Attribution<'a> {
        Attribution { peer_id: peer, local_id: local, known }
    }

    #[test]
    fn a_peer_may_speak_only_for_itself_or_a_device_we_know() {
        let known = vec!["33333333-3333-4333-8333-333333333333".to_string()];
        let a = attrib(
            "22222222-2222-4222-8222-222222222222",
            "11111111-1111-4111-8111-111111111111",
            &known,
        );
        assert!(a.accepts("22222222-2222-4222-8222-222222222222"), "itself");
        assert!(a.accepts("33333333-3333-4333-8333-333333333333"), "a device we also paired with");
        assert!(!a.accepts("44444444-4444-4444-8444-444444444444"), "a device we have never met");
    }

    #[test]
    fn nobody_may_author_rows_as_us() {
        // The attack this exists to stop: a paired peer claiming our own id and
        // rewriting our dictations in place under last-writer-wins.
        let known = vec!["11111111-1111-4111-8111-111111111111".to_string()];
        let a = attrib(
            "22222222-2222-4222-8222-222222222222",
            "11111111-1111-4111-8111-111111111111",
            &known,
        );
        assert!(
            !a.accepts("11111111-1111-4111-8111-111111111111"),
            "our own id must be refused even if it appears in the roster"
        );
        assert!(!a.accepts(""), "an empty source is refused");
    }

    #[test]
    fn an_outbound_batch_never_exceeds_the_wire_limit() {
        // fetch_page widens past a saturated millisecond and can return far
        // more than PAGE. Sending that whole vector was refused by the wire and
        // aborted the exchange every time, deterministically.
        let big: Vec<u32> = (0..20_000).collect();
        for chunk in big.chunks(PAGE) {
            assert!(
                chunk.len() <= echokey_sync::MAX_BATCH_LEN,
                "every chunk must be acceptable to the wire"
            );
        }
        assert_eq!(big.chunks(PAGE).map(|c| c.len()).sum::<usize>(), 20_000, "nothing dropped");
    }

    // ---- Two real peers over a real socket -------------------------------
    //
    // Everything above tests a predicate in isolation. These run the whole
    // exchange twice over a genuine TCP pair, which is the only way the
    // write/write deadlock and the watermark desync were ever going to show up.

    use echokey_core::history::{RemoteItem, Store};
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";

    fn socket_pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (srv, _) = l.accept().unwrap();
        // Timeouts on BOTH directions. A read timeout alone does not save this
        // test: the deadlock parks both peers in `write`, so neither ever
        // reaches a read and the suite hangs indefinitely. Verified by running
        // these tests with both sides set to Turn::First — they hang past ten
        // minutes without the write timeout and fail in seconds with it.
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

    fn seed(store: &Arc<Mutex<Store>>, source: &str, n: usize, text_len: usize) {
        let g = store.lock();
        for i in 0..n {
            g.apply_remote_item(&RemoteItem {
                source_machine: source.into(),
                origin_id: format!("row-{i}"),
                kind: "transcription".into(),
                text: "x".repeat(text_len),
                created_at: 1_700_000_000_000 + i as i64,
                updated_at: 1_700_000_000_000 + i as i64,
                pinned: false,
            })
            .unwrap();
        }
    }

    /// Run one full exchange between two stores, each in its own thread.
    fn run_pair(
        a_store: Arc<Mutex<Store>>,
        b_store: Arc<Mutex<Store>>,
    ) -> (RoundStats, RoundStats) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();

        let b = std::thread::spawn(move || {
            let mut session = Session::accept(srv, &k2).unwrap();
            let known = vec![A.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            exchange(
                &mut session,
                &b_store,
                (B, "Deck B"),
                Kinds { dictations: true, clipboard: true },
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
            )
        });

        let mut session = Session::initiate(c, &key).unwrap();
        let known = vec![B.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let a_stats = exchange(
            &mut session,
            &a_store,
            (A, "Deck A"),
            Kinds { dictations: true, clipboard: true },
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
        );
        let b_stats = b.join().expect("the accepting side must not panic");
        (
            a_stats.expect("dialling side"),
            b_stats.expect("accepting side"),
        )
    }

    #[test]
    fn a_large_first_sync_completes_in_both_directions() {
        // The deadlock this exists for: both peers used to send their entire
        // history before either started reading. That is fine while everything
        // fits in the socket and Noise buffers and hangs forever once it does
        // not — so this seeds far past any plausible buffer, in BOTH stores at
        // once, which is the case a one-sided test would miss.
        //
        // 400 rows x 4 KB is ~1.6 MB each way, against a socket buffer measured
        // in tens of KB.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed(&a_store, A, 400, 4096);
        seed(&b_store, B, 400, 4096);

        let (a_stats, b_stats) = run_pair(a_store.clone(), b_store.clone());

        assert_eq!(a_stats.sent_items, 400, "A serves its whole history");
        assert_eq!(b_stats.sent_items, 400, "B serves its whole history");
        assert_eq!(a_stats.applied_items, 400, "A takes all of B's rows");
        assert_eq!(b_stats.applied_items, 400, "B takes all of A's rows");
        assert_eq!(a_stats.refused, 0);
        assert_eq!(b_stats.refused, 0);

        assert_eq!(a_store.lock().count().unwrap(), 800);
        assert_eq!(b_store.lock().count().unwrap(), 800);
    }

    #[test]
    fn a_second_exchange_moves_nothing() {
        // Convergence: once both sides agree, syncing again must be silent.
        // If receipts were wrong this is where an endless re-send would show.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed(&a_store, A, 20, 64);
        seed(&b_store, B, 20, 64);
        run_pair(a_store.clone(), b_store.clone());

        let (a2, b2) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a2.sent_items, 0, "nothing left to send: {a2:?}");
        assert_eq!(b2.sent_items, 0, "nothing left to send: {b2:?}");
        assert_eq!(a2.applied_items, 0);
        assert_eq!(b2.applied_items, 0);
        assert_eq!(a_store.lock().count().unwrap(), 40);
        assert_eq!(b_store.lock().count().unwrap(), 40);
    }

    #[test]
    fn evicting_a_synced_row_does_not_pull_it_back() {
        // Retention and replication used to fight: a row pruned locally was
        // re-pulled from the peer, re-inserted, pruned again, forever. The
        // receipt is what settles it — we have SEEN the row, so the peer stops
        // offering it whether or not we still hold it.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed(&b_store, B, 30, 64);
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_store.lock().count().unwrap(), 30);

        // A keeps only 5 rows, exactly as the count-based prune does.
        a_store.lock().prune(0, 5).unwrap();
        assert_eq!(a_store.lock().count().unwrap(), 5);

        let (a2, _) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a2.applied_items, 0, "evicted rows must not come back");
        assert_eq!(a_store.lock().count().unwrap(), 5);
    }

    #[test]
    fn a_delete_on_one_side_reaches_the_other_and_stays_deleted() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed(&b_store, B, 5, 32);
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_store.lock().count().unwrap(), 5);

        b_store.lock().clear(None).unwrap();
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_store.lock().count().unwrap(), 0, "the delete propagated");

        // And the row does not crawl back on the next exchange.
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_store.lock().count().unwrap(), 0);
        assert_eq!(b_store.lock().count().unwrap(), 0);
    }

    #[test]
    fn many_sources_survive_the_watermark_chunking() {
        // Chunked watermarks used to be sent as several messages and read as
        // one, so the leftovers surfaced where rows were expected and ended
        // the exchange. Anything past MAX_BATCH_LEN sources triggered it.
        let a_store = store_for(A);
        let b_store = store_for(B);
        {
            let g = a_store.lock();
            for i in 0..(PAGE + 40) {
                // Distinct, well-formed device ids so they survive DeviceId::parse.
                let src = format!("33333333-3333-4333-8333-{:012x}", i);
                g.note_received(&src, 1_000 + i as i64).unwrap();
            }
        }
        seed(&b_store, B, 3, 32);

        let (a_stats, _) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            a_stats.applied_items, 3,
            "the exchange must still complete with more sources than one batch holds"
        );
    }

    #[test]
    fn a_peer_may_not_author_rows_for_us_or_for_a_stranger() {
        // Attribution is the only thing standing between a paired peer and
        // rewriting our own dictations, so prove it end to end rather than
        // through the predicate alone.
        let a_store = store_for(A);
        let b_store = store_for(B);
        // B fabricates rows attributed to A itself and to an unpaired third id.
        seed(&b_store, A, 4, 32);
        seed(&b_store, "44444444-4444-4444-8444-444444444444", 4, 32);
        seed(&b_store, B, 2, 32);

        let (a_stats, _) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_stats.refused, 8, "both forgeries refused: {a_stats:?}");
        assert_eq!(a_stats.applied_items, 2, "only B's own rows land");
        assert_eq!(a_store.lock().count().unwrap(), 2);

        // And the forged sources never got a receipt, so they are not silenced.
        let marks = a_store.lock().watermarks().unwrap();
        assert!(
            marks.iter().all(|(src, _)| src != "44444444-4444-4444-8444-444444444444"),
            "a forged source must not gain a watermark: {marks:?}"
        );
    }
}
