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

pub fn exchange<S: Read + Write>(
    session: &mut Session<S>,
    store: &Arc<Mutex<Store>>,
    me: (&str, &str),
    kinds: Kinds,
    retention: Retention,
    attribution: &Attribution<'_>,
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

    // 2. Watermarks, both ways.
    let mine = store.lock().watermarks().map_err(|e| ReplicateError::Store(e.to_string()))?;
    // Chunked: the wire caps a batch at MAX_BATCH_LEN, and a store polluted
    // before attribution was enforced can hold more sources than that. Sending
    // one oversized message would fail every exchange with an opaque wire error
    // instead of degrading.
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
        session.send(&SyncMessage::Watermarks { entries: Vec::new() })?;
    } else {
        for chunk in marks.chunks(PAGE) {
            session.send(&SyncMessage::Watermarks { entries: chunk.to_vec() })?;
        }
    }
    let peer_marks: HashMap<String, i64> = match session.recv()? {
        SyncMessage::Watermarks { entries } => entries
            .into_iter()
            .map(|w| (w.source_device.as_str().to_string(), w.clock as i64))
            .collect(),
        _ => HashMap::new(),
    };

    // 3. Serve. Every source we hold ANYTHING for — live rows or only
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

    // 4. Drain whatever the peer sends until it says it is finished.
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
                    apply_item(store, it, &mut stats)?;
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

    Ok(stats)
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
}
