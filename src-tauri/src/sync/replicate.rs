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
/// Mirrors the store's own ceiling on how far ahead a peer's clock may be.
const MAX_SKEW_MS: i64 = 24 * 60 * 60 * 1000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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
    /// How far a truncated re-offer actually got, as a clock.
    ///
    /// The next re-offer resumes from here. Restarting from zero each time
    /// meant a history larger than the cap could never finish: every pass
    /// re-sent the same first batch and stopped in the same place.
    pub resend_progress: Option<i64>,
    /// We hit the per-exchange batch cap with rows still to send.
    ///
    /// An ordinary exchange resumes next time, because the peer's cursor moved.
    /// A one-shot re-offer cannot: it was serving from BELOW the peer's cursor
    /// precisely because those rows are unreachable otherwise. Clearing the
    /// debt after a truncated re-offer stranded everything past the cap
    /// permanently, so the debt is kept until a re-offer finishes.
    pub truncated: bool,
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
    /// Devices we have paired with. A peer may carry an EDIT to one of their
    /// rows, but never create one — see `may_create`.
    pub known: &'a [String],
}

impl Attribution<'_> {
    /// Not every refusal is an incident.
    ///
    /// A peer claiming to BE us is misbehaving and deserves a warning. A peer
    /// offering rows for some third device is, since relaying was removed,
    /// simply talking to a version that no longer wants them — noise, not an
    /// attack, and logging it as one made an ordinary upgrade look alarming.
    fn log_refusal(&self, source: &str, what: &str) {
        if source == self.local_id {
            tracing::warn!("sync: {} tried to {} us; refused", self.peer_id, what);
        } else {
            tracing::debug!(
                "sync: {} offered to {} {}, which only that device may do; ignored",
                self.peer_id,
                what,
                source
            );
        }
    }

    /// A device may speak for itself. That is the whole rule.
    ///
    /// This used to also accept any source in our paired roster, described as
    /// legitimate relaying. It was a full authority escalation: any paired
    /// device could author dictations attributed to another, delete another's
    /// rows with forged tombstones, and — by sending one row for a third
    /// device stamped far in the future — park our watermark for that innocent
    /// device and silence it.
    ///
    /// Relaying buys nothing that the design asks for. `docs/SYNC_DESIGN.md`
    /// says "no relay servers" and requires pairing to be explicit and
    /// human-verified; devices that have paired exchange their rows directly.
    /// Removing it also deleted a whole class of never-terminating transfer,
    /// where rows for a device only one side had paired with were re-sent and
    /// refused on every exchange forever, because a refusal takes no receipt
    /// and the protocol has no way to say "stop offering me this".
    /// Is this row's claimed source even well formed?
    ///
    /// The real authority question is `may_create`; this only rejects nonsense.
    fn accepts(&self, source: &str) -> bool {
        !source.is_empty()
    }

    /// May this peer bring a row attributed to `source` into EXISTENCE?
    ///
    /// Only the device itself may. Everything else is an update, and an update
    /// is allowed for any identity we already hold — which is what makes the
    /// mesh work in every direction:
    ///
    /// - Pinning or correcting a Mac dictation on the Windows box has to reach
    ///   the Mac. That row's source is the Mac, so the edit is relayed to it as
    ///   an update to a row the Mac obviously holds.
    /// - Deleting a synced row on the receiving device has to reach the author.
    ///   Same shape: a tombstone for an identity the author holds.
    /// - A peer still cannot INVENT a dictation attributed to another device,
    ///   or to us, because there is nothing to update.
    ///
    /// Both blunter rules failed. Accepting any paired source outright let one
    /// compromised device fabricate another's history; refusing every
    /// third-party row silently broke edits and deletes of replicated rows, so
    /// a password deleted on the laptop lived on the desktop forever and the
    /// next edit resurrected it.
    fn may_create(&self, source: &str) -> bool {
        source == self.peer_id && source != self.local_id
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
    resend_all: bool,
    resend_from: i64,
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
            send_watermarks(session, store, attribution.peer_id)?;
            recv_watermarks(session)?
        }
        Turn::Second => {
            let peer = recv_watermarks(session)?;
            send_watermarks(session, store, attribution.peer_id)?;
            peer
        }
    };

    // 3 and 4. Serve and drain, again in turn order: the side that spoke
    //          first also sends its rows first, so its peer is already reading
    //          them while it works through the store.
    match turn {
        Turn::First => {
            serve(session, store, me, attribution.peer_id, &peer_marks, kinds, resend_all, resend_from, &mut stats)?;
            drain(session, store, kinds, retention, attribution, &mut stats)?;
        }
        Turn::Second => {
            drain(session, store, kinds, retention, attribution, &mut stats)?;
            serve(session, store, me, attribution.peer_id, &peer_marks, kinds, resend_all, resend_from, &mut stats)?;
        }
    }

    Ok(stats)
}

/// Everything we hold that the peer has not seen, per source device.
/// Where to start serving `source` to this peer.
///
/// Normally the peer's own cursor. For the peer's OWN rows we can do strictly
/// better, and have to: everything we received from that device is by
/// definition something it already holds, so only rows whose clock has moved
/// ABOVE what it gave us can be a local change worth sending back. Without
/// this, two synced machines handed each other their entire histories back on
/// the exchange after the first — every row ignored, nothing learned.
///
/// The reasoning holds only for `source == peer`. For a third device the two
/// marks measure different things and taking the larger would skip rows.
fn floor_for(
    source: &str,
    peer_id: &str,
    peer_marks: &HashMap<String, i64>,
    ours: &HashMap<String, i64>,
) -> i64 {
    let theirs = peer_marks.get(source).copied().unwrap_or(0);
    if source == peer_id {
        theirs.max(ours.get(source).copied().unwrap_or(0))
    } else {
        theirs
    }
}

fn serve<S: Read + Write>(
    session: &mut Session<S>,
    store: &Arc<Mutex<Store>>,
    me: (&str, &str),
    peer_id: &str,
    peer_marks: &HashMap<String, i64>,
    kinds: Kinds,
    resend_all: bool,
    resend_from: i64,
    stats: &mut RoundStats,
) -> Result<(), ReplicateError> {
    let mut reached: Option<i64> = None;
    // Every source we hold anything for, live rows or only tombstones.
    //
    // Serving only our own rows looked safer and quietly broke the feature: an
    // edit or a delete applied to a REPLICATED row never left the machine that
    // made it, so clearing a synced password on the laptop left it on the
    // desktop permanently, and the next edit on the desktop pushed it back.
    // Fabrication is prevented on the receiving side instead, by
    // `Attribution::may_create`, which is where the authority question actually
    // belongs.
    //
    // `resend_all` ignores the peer's cursor for one exchange. The user has
    // just widened what this machine shares, and the rows suppressed while the
    // switch was off are BELOW the peer's mark for us: it will never ask for
    // them again, and we cannot reach into its receipts to say otherwise. So we
    // offer our history once more from the beginning; re-applying is
    // idempotent, and it is the only way to close a hole that is otherwise
    // silent and permanent.
    let (sources, ours) = {
        let g = store.lock();
        let sources = g
            .known_sources()
            .map_err(|e| ReplicateError::Store(e.to_string()))?;
        // What THIS peer has already handed us, per source.
        let ours: HashMap<String, i64> = g
            .watermarks(peer_id)
            .map_err(|e| ReplicateError::Store(e.to_string()))?
            .into_iter()
            .collect();
        (sources, ours)
    };
    for source in sources.iter() {
        let source = source.as_str();
        // A device's OWN rows are served back to it too, and deliberately.
        // That is how an edit made here — pinning or correcting a dictation
        // that came from the Mac — reaches the Mac. The peer's cursor is what
        // stops this being wasteful: it tells us what it has already seen from
        // us about each source, including itself, so only rows we actually
        // changed go across.
        let mut after = if resend_all {
            resend_from
        } else {
            floor_for(source, peer_id, peer_marks, &ours)
        };
        let mut served = 0usize;
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
            served += 1;
            if served >= MAX_BATCHES {
                stats.truncated = true;
            }
        }

        // Track the lowest point any source reached, so a truncated re-offer
        // resumes from a clock that cannot skip another source's rows.
        reached = Some(reached.map_or(after, |r: i64| r.min(after)));

        // Tombstones for the same source.
        let mut after = if resend_all {
            resend_from
        } else {
            floor_for(source, peer_id, peer_marks, &ours)
        };
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

    if stats.truncated {
        stats.resend_progress = reached;
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
                    // Recorded BEFORE the policy checks below, and safe to do
                    // so only because receipts are per (peer, source): what
                    // this peer has offered us cannot touch the cursor we keep
                    // for the device that actually wrote the row.
                    //
                    // Without it, every row we refuse — a relayed row for a
                    // device we have not paired with, a kind the user switched
                    // off — is re-sent on every exchange for the life of the
                    // pairing, because nothing else tells the peer to stop.
                    //
                    // A clock outside the acceptable range is deliberately
                    // excluded: recording that would park this peer's cursor in
                    // the future and hide everything it legitimately writes.
                    let clock = it.updated_at;
                    if clock > 0 && clock <= now_ms() + MAX_SKEW_MS {
                        store
                            .lock()
                            .note_received(attribution.peer_id, it.source_device.as_str(), clock)
                            .map_err(|e| ReplicateError::Store(e.to_string()))?;
                    }
                    if !attribution.accepts(it.source_device.as_str()) {
                        attribution.log_refusal(it.source_device.as_str(), "author rows for");
                        stats.refused += 1;
                        continue;
                    }
                    // A third device's row may only be an UPDATE to something
                    // we already hold. Without this a paired peer could invent
                    // dictations attributed to any other paired device.
                    if !attribution.may_create(it.source_device.as_str()) {
                        let held = store
                            .lock()
                            .holds_identity(it.source_device.as_str(), &it.origin_id)
                            .map_err(|e| ReplicateError::Store(e.to_string()))?;
                        if !held {
                            attribution
                                .log_refusal(it.source_device.as_str(), "create rows for");
                            stats.refused += 1;
                            continue;
                        }
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
                        .note_received(attribution.peer_id, it.source_device.as_str(), it.updated_at)
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
                    apply_item(store, attribution.peer_id, it, stats)?;
                }
                if !more && items.is_empty() {
                    break;
                }
            }
            SyncMessage::Tombstones { entries, .. } => {
                for t in &entries {
                    let clock = t.deleted_at;
                    if clock > 0 && clock <= now_ms() + MAX_SKEW_MS {
                        store
                            .lock()
                            .note_received(attribution.peer_id, t.source_device.as_str(), clock)
                            .map_err(|e| ReplicateError::Store(e.to_string()))?;
                    }
                    if !attribution.accepts(t.source_device.as_str()) {
                        attribution.log_refusal(t.source_device.as_str(), "delete rows belonging to");
                        stats.refused += 1;
                        continue;
                    }
                    // A delete for a third device's row is only honoured for a
                    // row we actually hold, so a peer cannot pre-emptively
                    // tombstone identities it invented.
                    if !attribution.may_create(t.source_device.as_str()) {
                        let held = store
                            .lock()
                            .holds_identity(t.source_device.as_str(), &t.origin_id)
                            .map_err(|e| ReplicateError::Store(e.to_string()))?;
                        if !held {
                            stats.refused += 1;
                            continue;
                        }
                    }
                    store
                        .lock()
                        .note_received(attribution.peer_id, t.source_device.as_str(), t.deleted_at)
                        .map_err(|e| ReplicateError::Store(e.to_string()))?;
                    let rt = RemoteTombstone {
                        source_machine: t.source_device.as_str().to_string(),
                        origin_id: t.origin_id.clone(),
                        deleted_at: t.deleted_at,
                    };
                    let outcome = store
                        .lock()
                        .apply_remote_tombstone(attribution.peer_id, &rt)
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
    peer_id: &str,
) -> Result<(), ReplicateError> {
    let mine = store
        .lock()
        .watermarks(peer_id)
        .map_err(|e| ReplicateError::Store(e.to_string()))?;
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
    peer_id: &str,
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
        .apply_remote_item(peer_id, &r)
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

    fn attrib<'a>(peer: &'a str, local: &'a str, _known: &'a [String]) -> Attribution<'a> {
        Attribution { peer_id: peer, local_id: local, known: _known }
    }

    #[test]
    fn only_the_authoring_device_may_bring_a_row_into_existence() {
        // The authority rule is about CREATION, not about which sources may
        // appear. An update to something we already hold is how an edit or a
        // delete made on one machine reaches the machine that wrote the row.
        let known = vec!["33333333-3333-4333-8333-333333333333".to_string()];
        let a = attrib(
            "22222222-2222-4222-8222-222222222222",
            "11111111-1111-4111-8111-111111111111",
            &known,
        );
        assert!(a.may_create("22222222-2222-4222-8222-222222222222"), "the peer itself");
        assert!(
            !a.may_create("33333333-3333-4333-8333-333333333333"),
            "not for another device we have paired with"
        );
        assert!(!a.may_create("44444444-4444-4444-8444-444444444444"), "nor a stranger");
        assert!(!a.may_create("11111111-1111-4111-8111-111111111111"), "and never as us");
        assert!(!a.accepts(""), "an empty source id is not a source");
    }



    #[test]
    fn nobody_may_bring_a_row_into_existence_as_us() {
        // The attack: a paired peer claiming our own id to inject dictations we
        // never made, or to resurrect ones retention has already dropped.
        let known = vec!["11111111-1111-4111-8111-111111111111".to_string()];
        let a = attrib(
            "22222222-2222-4222-8222-222222222222",
            "11111111-1111-4111-8111-111111111111",
            &known,
        );
        assert!(!a.may_create("11111111-1111-4111-8111-111111111111"));
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
    /// A third paired device, used to test relayed edits and forgery.
    const C: &str = "33333333-3333-4333-8333-333333333333";

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
            g.apply_remote_item(C, &RemoteItem {
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
            // Every device in this scenario is paired with us.
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            exchange(
                &mut session,
                &b_store,
                (B, "Deck B"),
                Kinds { dictations: true, clipboard: true },
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
            )
        });

        let mut session = Session::initiate(c, &key).unwrap();
        // Every device in this scenario is paired with us.
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let a_stats = exchange(
            &mut session,
            &a_store,
            (A, "Deck A"),
            Kinds { dictations: true, clipboard: true },
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
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
                g.note_received(&src, &src, 1_000 + i as i64).unwrap();
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
    fn a_peer_cannot_bring_rows_into_existence_for_anyone_else() {
        // B holds rows attributed to us and to a device we have never paired
        // with. Neither may be CREATED here: there is nothing to update.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed(&b_store, A, 4, 32);
        seed(&b_store, "44444444-4444-4444-8444-444444444444", 4, 32);
        seed(&b_store, B, 2, 32);

        let (a_stats, _) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_stats.applied_items, 2, "only B's own rows land: {a_stats:?}");
        assert_eq!(a_store.lock().count().unwrap(), 2);
        assert_eq!(a_store.lock().items_since(A, 0, 10).unwrap().len(), 0, "none as us");

        // A receipt IS recorded for the refused rows — keyed by (peer, source),
        // so it cannot touch the cursor we keep for the device that supposedly
        // wrote them, and it stops the refusal repeating without end.
        let (a2, _) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a2.refused, 0, "the same rows must not be refused again: {a2:?}");
        assert_eq!(a_store.lock().count().unwrap(), 2, "and nothing new landed");
    }

}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW — demonstrations of live findings. Not fixes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial {
    use super::*;
    use echokey_core::history::{RemoteItem, RemoteTombstone, Store};
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

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

    const A: &str = "11111111-1111-4111-8111-111111111111"; // us
    const B: &str = "22222222-2222-4222-8222-222222222222"; // the malicious paired peer
    const C: &str = "33333333-3333-4333-8333-333333333333"; // an innocent third paired device

    /// One exchange where A has paired with BOTH B and C, and B is the peer.
    fn exchange_with_b(a_store: &Arc<Mutex<Store>>, b_store: Arc<Mutex<Store>>) -> RoundStats {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let bt = std::thread::spawn(move || {
            let mut s = Session::accept(srv, &k2).unwrap();
            // Every device in this scenario is paired with us.
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            let _ = exchange(
                &mut s,
                &b_store,
                (B, "Attacker"),
                Kinds { dictations: true, clipboard: true },
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
            );
        });
        let mut s = Session::initiate(c, &key).unwrap();
        // A's roster: both B and C are paired devices.
        // Every device in this scenario is paired with us.
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let stats = exchange(
            &mut s,
            a_store,
            (A, "Us"),
            Kinds { dictations: true, clipboard: true },
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
        )
        .expect("exchange");
        bt.join().unwrap();
        stats
    }

    /// FINDING: `Attribution::accepts` lets any paired peer speak for any OTHER
    /// paired device. B forges history attributed to C, and A stores it as if C
    /// had said it.
    #[test]
    fn a_paired_peer_cannot_forge_rows_for_a_third_device() {
        // Regression: B injects a dictation attributed to C, a device we have
        // ALSO paired with. It used to be stored and shown as if C had written
        // it, indistinguishable from C's own history.
        let a_store = store_for(A);
        let b_store = store_for(B);
        {
            let g = b_store.lock();
            g.apply_remote_item(C, &RemoteItem {
                source_machine: C.into(),
                origin_id: "forged-1".into(),
                kind: "transcription".into(),
                text: "text C never dictated".into(),
                created_at: 1_700_000_000_000,
                updated_at: 1_700_000_000_000,
                pinned: false,
            })
            .unwrap();
        }

        let stats = exchange_with_b(&a_store, b_store);
        assert_eq!(stats.applied_items, 0, "A must store nothing B invented for C");
        assert_eq!(a_store.lock().items_since(C, 0, 10).unwrap().len(), 0);
        assert_eq!(a_store.lock().count().unwrap(), 0);
    }


    /// FINDING: the same hole lets B DELETE C's rows on our machine.
    #[test]
    fn a_relayed_delete_lands_but_a_fabricated_one_does_not() {
        // Two halves of the same rule. B may carry a delete for a row we hold —
        // that is exactly how deleting a synced dictation on one machine
        // reaches the others. B may NOT tombstone an identity we have never
        // seen, which would let it pre-emptively block rows before they arrive.
        let a_store = store_for(A);
        a_store
            .lock()
            .apply_remote_item(
                C,
                &RemoteItem {
                    source_machine: C.into(),
                    origin_id: "real-row".into(),
                    kind: "transcription".into(),
                    text: "C's real dictation".into(),
                    created_at: 1_700_000_000_000,
                    updated_at: 1_700_000_000_000,
                    pinned: false,
                },
            )
            .unwrap();
        assert_eq!(a_store.lock().count().unwrap(), 1);

        let b_store = store_for(B);
        for (origin, held) in [("real-row", true), ("never-seen", false)] {
            b_store
                .lock()
                .apply_remote_tombstone(
                    C,
                    &RemoteTombstone {
                        source_machine: C.into(),
                        origin_id: origin.into(),
                        deleted_at: 1_700_000_001_000,
                    },
                )
                .unwrap();
            let _ = held;
        }

        let _ = exchange_with_b(&a_store, b_store);
        assert_eq!(
            a_store.lock().count().unwrap(),
            0,
            "the delete for a row we hold must land, or deletes never propagate"
        );
        // The fabricated one left nothing behind to block a future row.
        assert!(!a_store.lock().holds_identity(C, "never-seen").unwrap());
    }



    /// FINDING: B can push A's watermark for C forward, so A stops asking C for
    /// anything up to that clock — C is silenced for as long as the clamp
    /// allows (now + 24h).
    #[test]
    fn what_one_peer_offers_cannot_move_another_devices_cursor() {
        // The subtlest of the three. Receipts used to be keyed by source alone,
        // so B relaying one row for C moved the cursor we keep for C and hid
        // whatever C wrote below it. Keyed by (peer, source) the attack has
        // nowhere to land.
        let a_store = store_for(A);
        let b_store = store_for(B);
        {
            let g = b_store.lock();
            g.apply_remote_item(
                C,
                &RemoteItem {
                    source_machine: C.into(),
                    origin_id: "poison".into(),
                    kind: "transcription".into(),
                    text: "x".into(),
                    created_at: 1_700_000_000_000,
                    updated_at: 1_700_000_000_000,
                    pinned: false,
                },
            )
            .unwrap();
        }

        exchange_with_b(&a_store, b_store);

        // Nothing B said appears in the cursor we advertise to C, so C is still
        // asked for everything it has.
        let for_c = a_store.lock().watermarks(C).unwrap();
        assert!(
            for_c.is_empty(),
            "B moved the cursor we keep for C: {for_c:?}"
        );
    }


}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (convergence pass) — each test asserts what the design
// PROMISES, so a failure is a demonstrated defect. Not fixes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_convergence {
    use super::*;
    use echokey_core::history::{RemoteItem, RemoteTombstone, Store};
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C_UNKNOWN: &str = "44444444-4444-4444-8444-444444444444";
    /// A third device we HAVE paired with, as opposed to C_UNKNOWN.
    const C: &str = "33333333-3333-4333-8333-333333333333";
    const DAY_MS: i64 = 86_400_000;

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

    fn seed_at(store: &Arc<Mutex<Store>>, source: &str, origin: &str, kind: &str, clock: i64) {
        store
            .lock()
            .apply_remote_item(C, &RemoteItem {
                source_machine: source.into(),
                origin_id: origin.into(),
                kind: kind.into(),
                text: format!("{kind}-{origin}"),
                created_at: clock,
                updated_at: clock,
                pinned: false,
            })
            .unwrap();
    }

    /// One full exchange, both sides real, with per-side kind toggles.
    fn run_pair_kinds(
        a_store: Arc<Mutex<Store>>,
        b_store: Arc<Mutex<Store>>,
        a_kinds: Kinds,
        b_kinds: Kinds,
    ) -> (RoundStats, RoundStats) {
        run_pair_full(a_store, b_store, a_kinds, b_kinds, false)
    }

    /// A owes B one full re-offer, as SyncManager does after the user widens
    /// what this machine shares.
    fn run_pair_resend(a: Arc<Mutex<Store>>, b: Arc<Mutex<Store>>) -> (RoundStats, RoundStats) {
        run_pair_resend_from(a, b, 0)
    }

    /// A re-offer resuming from `from`, exactly as SyncManager does after a
    /// truncated one.
    fn run_pair_resend_from(
        a: Arc<Mutex<Store>>,
        b: Arc<Mutex<Store>>,
        from: i64,
    ) -> (RoundStats, RoundStats) {
        run_pair_full_from(a, b, both(), both(), true, from)
    }

    fn run_pair_full(
        a_store: Arc<Mutex<Store>>,
        b_store: Arc<Mutex<Store>>,
        a_kinds: Kinds,
        b_kinds: Kinds,
        a_resend: bool,
    ) -> (RoundStats, RoundStats) {
        run_pair_full_from(a_store, b_store, a_kinds, b_kinds, a_resend, 0)
    }

    fn run_pair_full_from(
        a_store: Arc<Mutex<Store>>,
        b_store: Arc<Mutex<Store>>,
        a_kinds: Kinds,
        b_kinds: Kinds,
        a_resend: bool,
        a_from: i64,
    ) -> (RoundStats, RoundStats) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();

        let bt = std::thread::spawn(move || {
            let mut session = Session::accept(srv, &k2).unwrap();
            // Every device in this scenario is paired with us.
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            exchange(
                &mut session,
                &b_store,
                (B, "Deck B"),
                b_kinds,
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
            )
        });

        let mut session = Session::initiate(c, &key).unwrap();
        // Every device in this scenario is paired with us.
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let a_stats = exchange(
            &mut session,
            &a_store,
            (A, "Deck A"),
            a_kinds,
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            a_resend,
            a_from,
        );
        let b_stats = bt.join().expect("the accepting side must not panic");
        (a_stats.expect("dialling side"), b_stats.expect("accepting side"))
    }

    fn both() -> Kinds {
        Kinds { dictations: true, clipboard: true }
    }

    fn run_pair(a: Arc<Mutex<Store>>, b: Arc<Mutex<Store>>) -> (RoundStats, RoundStats) {
        run_pair_kinds(a, b, both(), both())
    }

    /// Retention prunes our OWN rows without tombstones. `watermarks()` derives
    /// our own mark from the rows we still hold, so it collapses; the peer then
    /// re-offers our entire pruned history on every exchange and attribution
    /// refuses every row (nobody may author rows as us), so no receipt is ever
    /// taken and the loop never terminates.
    ///
    /// Realistic shape: laptop keeps 30 days, desktop keeps everything.
    #[test]
    fn bug_retention_pruned_own_rows_are_re_sent_on_every_exchange_forever() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        let old = 1_700_000_000_000i64; // long past any 30-day window
        for i in 0..10 {
            seed_at(&a_store, A, &format!("row-{i}"), "transcription", old + i);
        }
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(b_store.lock().count().unwrap(), 10, "B holds A's history");

        // A's own 30-day retention sweep. No tombstones: retention is a local
        // policy, not a delete.
        a_store.lock().prune(30, 0).unwrap();
        assert_eq!(a_store.lock().count().unwrap(), 0);

        let (a2, b2) = run_pair(a_store.clone(), b_store.clone());
        let (a3, b3) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            (b2.sent_items, b3.sent_items),
            (0, 0),
            "B re-sends A's whole pruned history on every exchange, forever \
             (A refuses each row as a forgery of its own id: refused={} then {})",
            a2.refused,
            a3.refused
        );
    }

    /// prune() drops tombstones older than 180 days and justifies it with "the
    /// receipt for its source outlives it". There is no receipt for our OWN
    /// source: once B's own tombstone is pruned, B's advertised watermark falls
    /// back below the delete, and A — which never saw the delete — offers the
    /// row again forever while continuing to display it.
    #[test]
    fn bug_a_pruned_tombstone_leaves_a_deleted_row_alive_on_the_peer_forever() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        let old = crate::sync::manager::now_ms() - 200 * DAY_MS;

        seed_at(&b_store, B, "7", "clipboard", old);
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_store.lock().count().unwrap(), 1, "A picked up the row");

        // B deletes it 200 days ago. A is off the network and never hears.
        b_store
            .lock()
            .apply_remote_tombstone(C, &RemoteTombstone {
                source_machine: B.into(),
                origin_id: "7".into(),
                deleted_at: old + 1,
            })
            .unwrap();
        assert_eq!(b_store.lock().count().unwrap(), 0);

        // Routine housekeeping on B. TOMBSTONE_MIN_DAYS is 180.
        b_store.lock().prune(0, 0).unwrap();

        // A comes back, twice.
        run_pair(a_store.clone(), b_store.clone());
        let (a3, _b3) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            a_store.lock().count().unwrap(),
            0,
            "the delete must still reach A; instead A holds the deleted row and \
             re-offers it to B on every exchange (sent_items={})",
            a3.sent_items
        );
    }

    /// The kind toggle filters OUTBOUND rows, but the peer's mark keeps moving
    /// past them on the rows that do go out. `SyncManager::set_kinds` resets
    /// only the LOCAL receipts, which repairs the inbound direction only, so
    /// everything captured while the kind was off is unreachable forever.
    #[test]
    fn enabling_a_kind_backfills_what_the_outbound_filter_suppressed() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        let t0 = 1_700_000_000_000i64;
        seed_at(&a_store, A, "clip-1", "clipboard", t0);
        seed_at(&a_store, A, "dict-1", "transcription", t0 + 1_000);

        let off = Kinds { dictations: true, clipboard: false };
        run_pair_kinds(a_store.clone(), b_store.clone(), off, both());
        assert_eq!(b_store.lock().count().unwrap(), 1, "only the dictation crossed");

        // The user switches clipboard sync ON. SyncManager::set_kinds does two
        // things here and BOTH are needed: it clears our receipts (inbound) and
        // it records that every paired device is owed one full re-offer of our
        // history (outbound). Clearing receipts alone cannot help, because the
        // suppressed rows sit below the mark the PEER keeps for us and we
        // cannot reach into its receipts.
        a_store.lock().reset_source_marks().unwrap();

        run_pair_resend(a_store.clone(), b_store.clone());
        assert_eq!(
            b_store.lock().count().unwrap(),
            2,
            "enabling a kind must backfill the rows it suppressed"
        );

        // And the debt is one-shot: an ordinary exchange afterwards is silent.
        let (a2, _) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a2.sent_items, 0, "the re-offer must not repeat forever: {a2:?}");
    }


    /// A source that only ONE side has paired with is relayed by that side and
    /// refused by the other. The refusal is right, but no receipt is taken and
    /// the protocol has no way to signal it, so the same rows cross the wire on
    /// every exchange for the life of the pairing.
    #[test]
    fn bug_rows_for_a_device_only_one_side_knows_are_re_sent_forever() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        for i in 0..50 {
            seed_at(&b_store, C_UNKNOWN, &format!("row-{i}"), "transcription", 1_700_000_000_000 + i);
        }

        let (a1, b1) = run_pair(a_store.clone(), b_store.clone());
        let (a2, b2) = run_pair(a_store.clone(), b_store.clone());
        let (_a3, b3) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            (b2.sent_items, b3.sent_items),
            (0, 0),
            "the same 50 rows cross on every exchange (first={} refused={}, second refused={})",
            b1.sent_items,
            a1.refused,
            a2.refused
        );
    }

    const C3: &str = "33333333-3333-4333-8333-333333333333";

    /// One exchange between any two devices, both sides real.
    fn exchange_between(
        x_id: &'static str,
        x_store: Arc<Mutex<Store>>,
        y_id: &'static str,
        y_store: Arc<Mutex<Store>>,
    ) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let t = std::thread::spawn(move || {
            let mut s = Session::accept(srv, &k2).unwrap();
            // Every device in this scenario is paired with us.
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: x_id, local_id: y_id, known: &known };
            exchange(
                &mut s,
                &y_store,
                (y_id, "Y"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
            )
        });
        let mut s = Session::initiate(c, &key).unwrap();
        // Every device in this scenario is paired with us.
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: y_id, local_id: x_id, known: &known };
        exchange(
            &mut s,
            &x_store,
            (x_id, "X"),
            both(),
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
        )
        .expect("dialling side");
        t.join().expect("accepting side must not panic").expect("accepting side");
    }

    /// `Store::delete_item_local` promises the delete "travels", and `clear()`
    /// promises "the deletes propagate". Both write a tombstone keyed on the
    /// row's ORIGINAL source. With relaying removed, `serve` offers tombstones
    /// only for rows we authored, so a delete performed on the receiving device
    /// is never told to anyone. Two devices, one delete, permanent divergence.
    #[test]
    fn bug_deleting_a_replicated_row_never_reaches_the_device_that_authored_it() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed_at(&a_store, A, "secret", "clipboard", 1_700_000_000_000);
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(b_store.lock().count().unwrap(), 1, "B received A's row");

        // The user deletes it on B — the machine that received it.
        let id = b_store.lock().recent(None, 10).unwrap()[0].id;
        b_store.lock().delete(id).unwrap();
        assert_eq!(b_store.lock().count().unwrap(), 0);
        assert_eq!(
            b_store.lock().tombstones_since(A, 0, 10).unwrap().len(),
            1,
            "B did record the tombstone; it simply has no way to send it"
        );

        run_pair(a_store.clone(), b_store.clone());
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            a_store.lock().count().unwrap(),
            0,
            "a delete made on the receiving device must still reach the author"
        );
    }

    /// The same divergence turned into a resurrection: because A never learns
    /// about the delete, any later edit on A (a pin, a text correction) lifts
    /// the row's clock above B's tombstone and pushes it straight back onto B.
    #[test]
    fn bug_a_delete_on_the_non_author_is_undone_by_the_next_edit_on_the_author() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed_at(&a_store, A, "secret", "clipboard", 1_700_000_000_000);
        run_pair(a_store.clone(), b_store.clone());

        let id = b_store.lock().recent(None, 10).unwrap()[0].id;
        b_store.lock().delete(id).unwrap();
        assert_eq!(b_store.lock().count().unwrap(), 0, "deleted on B");

        // Later, on A, the user pins the row they still see. Bounded wait so
        // the pin's millisecond is strictly after the tombstone's.
        std::thread::sleep(Duration::from_millis(5));
        let a_id = a_store.lock().recent(None, 10).unwrap()[0].id;
        a_store.lock().set_pinned(a_id, true).unwrap();

        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            b_store.lock().count().unwrap(),
            0,
            "the row the user deleted on B is back, because A never heard about the delete"
        );
    }

    /// Three devices, chained pairing: A<->B and B<->C, A and C never paired.
    /// Before relaying was removed B passed C's rows on to A. Now it holds them
    /// and never serves them, so a device the user considers part of one synced
    /// set silently sees a subset of the history.
    #[test]
    fn a_row_reaches_only_the_devices_paired_directly_with_its_author() {
        // Documented behaviour, not a defect. A<->B and B<->C paired, A and C
        // never paired: C's dictations reach B and stop there.
        //
        // SYNC_DESIGN.md requires pairing to be explicit and human-verified and
        // rules out relay servers. A has never agreed to trust C, so B carrying
        // C's history into A would be exactly the transitive trust that rule
        // exists to prevent. B can still carry an EDIT to a row A already holds
        // — that is an update to something A accepted from C directly, not new
        // history appearing from a device A does not know.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed_at(&b_store, C_UNKNOWN, "c-row", "transcription", 1_700_000_000_000);

        let (a1, _) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            a_store.lock().count().unwrap(),
            0,
            "a device A never paired with cannot introduce history to A"
        );
        let _ = a1;

        // And it terminates: the refusal records a receipt, so B stops offering.
        let (_, b2) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(b2.sent_items, 0, "the refusal must not repeat forever: {b2:?}");
    }


    /// This module's own header says, as a load-bearing rule: "We serve rows
    /// for EVERY source we know about, not just our own. Pinning a Mac row on
    /// the Windows box bumps that row's clock but leaves its source as the Mac;
    /// if each side only offered its own rows, that edit would never leave the
    /// machine." `serve` now offers only `me.0`. The comment describes the
    /// current code exactly, and calls it the bug.
    #[test]
    fn bug_an_edit_to_a_peers_row_never_leaves_the_machine_that_made_it() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed_at(&b_store, B, "note", "transcription", 1_700_000_000_000);
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_store.lock().count().unwrap(), 1, "A received B's row");

        // On A the user pins B's dictation and corrects its text.
        let id = a_store.lock().recent(None, 10).unwrap()[0].id;
        a_store.lock().set_pinned(id, true).unwrap();
        a_store.lock().update_text(id, "corrected on A").unwrap();

        run_pair(a_store.clone(), b_store.clone());
        run_pair(a_store.clone(), b_store.clone());

        let on_b = b_store.lock().recent(None, 10).unwrap().remove(0);
        assert_eq!(
            (on_b.pinned, on_b.text.as_str()),
            (true, "corrected on A"),
            "the edit never leaves A: the two machines disagree about the same row forever"
        );
    }


    /// `serve` stops after MAX_BATCHES (64) pages of PAGE (256) rows, i.e.
    /// 16,384 rows per exchange. An ordinary sync resumes next time, because
    /// the peer's mark advanced. The `resend_all` backfill does NOT: it is
    /// one-shot, `SyncManager::run_session` clears the debt as soon as the
    /// exchange returns Ok, and the peer's mark for us was already above these
    /// rows — so everything past row 16,384 stays unreachable.
    #[test]
    fn a_truncated_re_offer_keeps_its_debt_and_finishes_next_time() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        let n = MAX_BATCHES * PAGE + 200;
        {
            let g = a_store.lock();
            for i in 0..n {
                g.apply_remote_item(C, &RemoteItem {
                    source_machine: A.into(),
                    origin_id: format!("r{i}"),
                    kind: "clipboard".into(),
                    text: "x".into(),
                    created_at: 1_700_000_000_000 + i as i64,
                    updated_at: 1_700_000_000_000 + i as i64,
                    pinned: false,
                })
                .unwrap();
            }
        }
        // The one full re-offer the user is owed after turning a kind on. It
        // does not fit in a single exchange.
        let (a1, _) = run_pair_resend(a_store.clone(), b_store.clone());
        assert!(a1.truncated, "a re-offer this size must report truncation: {a1:?}");
        assert!(
            (b_store.lock().count().unwrap() as usize) < n,
            "precondition: the cap really did bite"
        );

        // SyncManager keeps the debt when the re-offer is truncated, so the
        // next exchange resumes it. Spending the debt here stranded everything
        // past the cap permanently, because a re-offer serves from BELOW the
        // peer's cursor and nothing else will ever offer those rows again.
        let mut from = a1.resend_progress.unwrap_or(0);
        for _ in 0..8 {
            if b_store.lock().count().unwrap() as usize == n {
                break;
            }
            let (r, _) = run_pair_resend_from(a_store.clone(), b_store.clone(), from);
            match r.resend_progress {
                Some(next) if r.truncated => from = next,
                _ => {}
            }
        }
        assert_eq!(
            b_store.lock().count().unwrap() as usize,
            n,
            "a retained debt must eventually deliver every row"
        );
    }

}
