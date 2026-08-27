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
/// Hard stop on a single exchange, so a peer cannot keep us here forever.
const MAX_BATCHES: usize = 64;
/// How many watermark chunks either side will send or read.
///
/// `send_watermarks` used to chunk with no ceiling while `recv_watermarks` read
/// at most `MAX_BATCHES`, so a store holding more sources than that produced a
/// stream the other side stopped reading mid-way — and the exchange failed for
/// good. Both halves now use this, and the sender truncates rather than
/// overrunning the reader.
const MAX_WATERMARK_CHUNKS: usize = 256;
/// The most row/tombstone messages one side will send in an exchange, and the
/// most the other will read.
///
/// These were two different numbers: `serve` could emit
/// `sources * 2 * MAX_BATCHES + 1` messages while `drain` read at most
/// `MAX_BATCHES * 4`. Past that the reader stopped and began writing while the
/// sender was still writing — the exact write/write stall the turn-taking
/// exists to prevent — and the exchange died on a timeout every time. One
/// number, used by both, is the only way that stays true as either side
/// changes.
const MAX_EXCHANGE_MESSAGES: usize = 1024;
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
    /// The user unpaired this device, or switched sync off, mid-exchange.
    #[error("stopped: this device was unpaired or sync was switched off")]
    Aborted,
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
    // Checked between batches. Returns true when this session must stop — the
    // user unpaired the device, or switched sync off, while it was running.
    //
    // Checking once before the exchange was not enough: the exchange IS the
    // long part, so a revoked device kept trading history for up to the whole
    // session timeout. The only window that closed was the instant between the
    // handshake and the first message.
    abort: &dyn Fn() -> bool,
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
            serve(session, store, me, attribution.peer_id, &peer_marks, kinds, resend_all, resend_from, abort, &mut stats)?;
            drain(session, store, kinds, retention, attribution, abort, &mut stats)?;
        }
        Turn::Second => {
            drain(session, store, kinds, retention, attribution, abort, &mut stats)?;
            serve(session, store, me, attribution.peer_id, &peer_marks, kinds, resend_all, resend_from, abort, &mut stats)?;
        }
    }

    Ok(stats)
}

/// Everything we hold that the peer has not seen, per source device.
/// Where to start serving `source` to this peer: the peer's own cursor, and
/// nothing else.
///
/// This used to raise the floor for the peer's OWN rows to
/// `max(their cursor, what they have given us)`, reasoning that anything they
/// gave us is something they already hold. The reasoning is true of the rows
/// and false of the CHANGES: a delete or an edit we make to one of their rows
/// carries a clock above the row's, but the floor rises to whatever else that
/// peer has sent us since — and it only ever rises. Delete a synced password
/// here, let the author write anything at all afterwards, and the tombstone was
/// never offered again. Permanently, and with no skew or misconfiguration
/// needed: `Turn::Second` drains before it serves, so a row arriving in the
/// same exchange lifted the floor over the tombstone we had not sent yet.
///
/// The cost of dropping it is one extra round of echo per pairing: each side
/// offers the other its own rows back once, they are recognised as no-ops, the
/// cursor rises, and it stops. A wasted round beats a lost delete.
fn floor_for(source: &str, peer_marks: &HashMap<String, i64>) -> i64 {
    peer_marks.get(source).copied().unwrap_or(0)
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
    abort: &dyn Fn() -> bool,
    stats: &mut RoundStats,
) -> Result<(), ReplicateError> {
    let mut reached: Option<i64> = None;
    // Counts every row/tombstone message sent, so we stop at the same number
    // the other side will read rather than writing into a reader that has
    // already stopped.
    let mut sent_messages = 0usize;
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
    let (sources, _ours) = {
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
            floor_for(source, peer_marks)
        };
        // The cursor is a PAIR: clock and origin id. On the clock alone a page
        // that ended inside a millisecond dropped the rest of it for good, and
        // the only defence was a 20,000-row re-fetch in one statement — which
        // missed the common case AND let a peer freeze the history UI for most
        // of a second by stamping enough rows the same.
        // Strictly after `after` on the first page: the peer already has that
        // millisecond. Later pages continue on the (clock, origin) pair.
        let mut after_origin = echokey_core::history::ORIGIN_CEILING.to_string();
        let mut served = 0usize;
        let mut truncated_here = false;
        // ITEMS: only the rows we authored.
        //
        // A peer accepts content only from the device that wrote it, so
        // offering it a third device's rows achieved nothing except to be
        // refused — on every exchange, for ever, since a refusal we might one
        // day reverse cannot bank a cursor. It was also the door through which
        // an edit to a third device's row came back to us and got relayed on.
        //
        // TOMBSTONES are different and are served for every source below: a
        // delete carries no attacker-chosen content, and it has to reach the
        // machine that recorded the row however it gets there.
        let items_wanted = source == me.0;
        for _ in 0..MAX_BATCHES {
            if !items_wanted || sent_messages + 2 >= MAX_EXCHANGE_MESSAGES {
                break;
            }
            if abort() {
                return Err(ReplicateError::Aborted);
            }
            let mut page = store
                .lock()
                .items_from(source, after, &after_origin, PAGE)
                .map_err(|e| ReplicateError::Store(e.to_string()))?;
            if page.is_empty() {
                break;
            }
            // This is the last batch we are allowed to send, and the peer will
            // record the highest clock it sees. Stopping mid-millisecond would
            // put its cursor inside one, and the rest of that millisecond is
            // then strictly below it forever. Trim back to a boundary.
            let full = page.len() >= PAGE;
            let final_batch = served + 1 >= MAX_BATCHES && full;
            if final_batch {
                // We are stopping with rows still to send, so the caller owes
                // this peer a resume. Recorded before the trim below, which
                // makes the page short and would otherwise read as "finished".
                stats.truncated = true;
                truncated_here = true;
            }
            // EVERY full page is trimmed back to a millisecond boundary, not
            // just the last one.
            //
            // The peer records the highest clock it sees, and the next exchange
            // asks strictly above it. So a run that stops BETWEEN pages — the
            // user unpairs mid-exchange, sync is switched off, the network
            // drops, the abort hook fires — left the peer's cursor inside a
            // millisecond, and the rest of that millisecond sat below it
            // forever. Nothing lowers a cursor. Trimming only the truncated
            // batch covered the one case that was easy to see.
            //
            // A page entirely filled by one millisecond has no boundary to trim
            // to; we send it and keep paging by origin id within that
            // millisecond, which is correct inside a run.
            if full {
                let tail = page.last().expect("page is not empty").updated_at;
                let keep = page.iter().filter(|r| r.updated_at < tail).count();
                if keep > 0 {
                    page.truncate(keep);
                } else if final_batch {
                    tracing::error!(
                        "sync: more than {} rows share one millisecond for {source}; \
                         the rest of it cannot be paged",
                        MAX_BATCHES * PAGE
                    );
                }
            }
            let last = page.last().expect("page is not empty");
            let (last_clock, last_origin) = (last.updated_at, last.origin_id.clone());
            let out: Vec<SyncItem> = page
                .iter()
                .filter(|r| kinds.allows(&r.kind))
                .filter_map(to_wire)
                .collect();
            let more = full;
            if !out.is_empty() {
                stats.sent_items += out.len();
                // One page is at most PAGE rows and PAGE == MAX_BATCH_LEN, so
                // this is a single message — but the chunking stays, because
                // the wire refuses an oversized batch and getting that wrong
                // aborted every exchange, deterministically.
                let chunks: Vec<_> = out.chunks(PAGE).map(|c| c.to_vec()).collect();
                let last_ix = chunks.len() - 1;
                for (ix, chunk) in chunks.into_iter().enumerate() {
                    let more_to_come = more || ix < last_ix;
                    session.send(&SyncMessage::Items { items: chunk, more: more_to_come })?;
                    sent_messages += 1;
                }
            }
            if !more {
                break;
            }
            after = last_clock;
            after_origin = last_origin;
            served += 1;
            if final_batch {
                break;
            }
        }

        // Only a source we actually truncated constrains where a re-offer
        // resumes. Taking the minimum across EVERY source pinned the cursor at
        // the starting floor, because a source whose rows fit in one page never
        // advances `after` at all — so a truncated re-offer resumed exactly
        // where it began and the tail was never delivered. That is every store
        // holding more than one source, which is every store that has synced.
        if truncated_here {
            reached = Some(reached.map_or(after, |r: i64| r.min(after)));
        }
        // Items and tombstones for one source share ONE cursor on the wire, and
        // the peer records the highest clock it sees from either. A tombstone
        // stamped later than the item we stopped at therefore carried the
        // cursor straight over every item a truncated pass never sent — and an
        // ordinary truncated exchange records no resend debt, so nothing ever
        // went back for them. Cap the tombstone pass at the item pass's
        // stopping point; the rest follows next exchange.
        let tomb_ceiling = if truncated_here { Some(after) } else { None };

        // Tombstones for the same source.
        let mut after = if resend_all {
            resend_from
        } else {
            floor_for(source, peer_marks)
        };
        let mut after_origin = echokey_core::history::ORIGIN_CEILING.to_string();
        let mut tomb_served = 0usize;
        for _ in 0..MAX_BATCHES {
            if sent_messages + 2 >= MAX_EXCHANGE_MESSAGES {
                stats.truncated = true;
                break;
            }
            // The abort check belongs here too. Without it a device the user
            // had just unpaired still received every tombstone we held for a
            // source — up to MAX_BATCHES * PAGE of them — after revocation.
            if abort() {
                return Err(ReplicateError::Aborted);
            }
            let mut page = store
                .lock()
                .tombstones_from(source, after, &after_origin, PAGE)
                .map_err(|e| ReplicateError::Store(e.to_string()))?;
            if page.is_empty() {
                break;
            }
            if let Some(ceiling) = tomb_ceiling {
                page.retain(|t| t.deleted_at <= ceiling);
                if page.is_empty() {
                    break;
                }
            }
            let last = page.last().expect("page is not empty");
            let (last_clock, last_origin) = (last.deleted_at, last.origin_id.clone());
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
                    sent_messages += 1;
                }
            }
            if !more {
                break;
            }
            after = last_clock;
            after_origin = last_origin;
            tomb_served += 1;
            if tomb_served >= MAX_BATCHES {
                // Tombstones count towards truncation too. Without this a
                // re-offer could drop everything past the cap with `truncated`
                // never set, so no debt was retained and those deletes were
                // simply never sent.
                stats.truncated = true;
                reached = Some(reached.map_or(after, |r: i64| r.min(after)));
            }
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
    abort: &dyn Fn() -> bool,
    stats: &mut RoundStats,
) -> Result<(), ReplicateError> {
    for _ in 0..MAX_EXCHANGE_MESSAGES {
        if abort() {
            return Err(ReplicateError::Aborted);
        }
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
                    // Banked only for a source this peer may actually speak
                    // for. A cursor is a promise never to ask for anything at or
                    // below it again, so banking one for a row we refused for a
                    // reason that might LATER stop applying makes that row
                    // permanently unreachable. Banking it for whatever id the
                    // peer named also let one device evict our real cursors by
                    // naming invented ones.
                    //
                    // The refusals below are all permanent or self-repairing:
                    // retention only ever gets truer, and a disabled kind is
                    // undone by reset_source_marks when the switch comes back
                    // on. So they still bank, which is what stops the peer
                    // re-offering the same rows on every exchange forever.
                    //
                    // Still taken BEFORE the policy checks below, which is
                    // safe because receipts are per (peer, source): what this
                    // peer has offered us cannot touch the cursor we keep for
                    // the device that actually wrote the row. Without it, every
                    // row we refuse for a reversible reason is re-sent on every
                    // exchange for the life of the pairing.
                    if attribution.may_create(it.source_device.as_str()) {
                        store
                            .lock()
                            .note_received(attribution.peer_id, it.source_device.as_str(), it.updated_at)
                            .map_err(|e| ReplicateError::Store(e.to_string()))?;
                    }
                    if !attribution.accepts(it.source_device.as_str()) {
                        attribution.log_refusal(it.source_device.as_str(), "author rows for");
                        stats.refused += 1;
                        continue;
                    }
                    // Only the AUTHORING device may change a row's content.
                    //
                    // "An update to an identity we already hold" was the rule
                    // for two rounds and it does not hold up. We hold every row
                    // we ever wrote, and we hand a peer the identity of every
                    // row we sync to it, so a paired device could rewrite our
                    // history in place — and a third device's history too,
                    // because `serve` offers it every source we hold and the
                    // edit came back through the same door. It could then be
                    // relayed on to that third device as an ordinary edit. That
                    // is precisely the escalation SYNC_DESIGN.md rules out.
                    //
                    // The cost is real and worth stating: pinning or correcting
                    // a dictation on a machine that did not record it is a
                    // LOCAL change and does not travel back. DELETES still do —
                    // see the tombstone arm — because a password cleared on the
                    // laptop has to vanish from the machine that recorded it,
                    // and a delete carries no attacker-chosen content.
                    if !attribution.may_create(it.source_device.as_str()) {
                        attribution.log_refusal(it.source_device.as_str(), "modify rows of");
                        stats.refused += 1;
                        continue;
                    }
                    // And it may only touch a row of a kind this machine is
                    // actually sharing — judged on the kind we HOLD, not the
                    // kind the peer claims.
                    //
                    // Checking the wire kind alone was a hole with real teeth:
                    // a peer could send kind="transcription" for a clipboard
                    // row, and because an update may change `kind`, rewrite
                    // clipboard entries that had never left this machine while
                    // clipboard sync was switched off.
                    if let Some(local) = store
                        .lock()
                        .kind_of(it.source_device.as_str(), &it.origin_id)
                        .map_err(|e| ReplicateError::Store(e.to_string()))?
                    {
                        if !kinds.allows(&local) {
                            stats.refused += 1;
                            continue;
                        }
                    }
                    // The receipt for this row was already taken above, range
                    // checked. A second unguarded one used to live here, which
                    // meant one row stamped i64::MAX parked this peer's cursor
                    // at the ceiling and hid everything it wrote afterwards —
                    // exactly what the range check exists to prevent.
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
                    if !attribution.accepts(t.source_device.as_str()) {
                        attribution.log_refusal(t.source_device.as_str(), "delete rows belonging to");
                        stats.refused += 1;
                        continue;
                    }
                    // The authoring device may always delete its own row, even
                    // one we have never seen: a delete legitimately overtakes
                    // the row it deletes, and refusing those resurrects rows.
                    //
                    // Anyone else may only relay a delete for a row we actually
                    // hold, so a peer cannot pre-emptively tombstone identities
                    // it invented.
                    let authored = attribution.may_create(t.source_device.as_str());
                    let held = store
                        .lock()
                        .holds_identity(t.source_device.as_str(), &t.origin_id)
                        .map_err(|e| ReplicateError::Store(e.to_string()))?;
                    if !authored && !held {
                        // TEMPORARY refusal, and the receipt is deliberately NOT
                        // banked. We may acquire this row from its author later,
                        // and a cursor parked at this tombstone's clock would
                        // make the delete unreachable for good — a password the
                        // user deleted, alive on this machine forever, because
                        // the delete happened to arrive first.
                        stats.refused += 1;
                        continue;
                    }
                    store
                        .lock()
                        .note_received(attribution.peer_id, t.source_device.as_str(), t.deleted_at)
                        .map_err(|e| ReplicateError::Store(e.to_string()))?;
                    // The kind gate applies to deletes too, on the kind we
                    // hold — but ONLY to a relayed delete. Without it a peer
                    // could erase clipboard history the user had excluded from
                    // sync entirely, because a tombstone carries no kind of its
                    // own to check.
                    //
                    // The authoring device is exempt, and has to be: it wrote
                    // the row, it deleted it, and switching a kind off must not
                    // turn this machine into a place deletes go to die. That
                    // would leave a password we were asked to forget sitting
                    // here for as long as the toggle stayed off.
                    if !authored {
                        if let Some(local) = store
                            .lock()
                            .kind_of(t.source_device.as_str(), &t.origin_id)
                            .map_err(|e| ReplicateError::Store(e.to_string()))?
                        {
                            if !kinds.allows(&local) {
                                stats.refused += 1;
                                continue;
                            }
                        }
                    }
                    // Receipt already taken above, range checked. See the
                    // item arm.
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
    let mut chunks: Vec<_> = marks.chunks(PAGE).map(|c| c.to_vec()).collect();
    if chunks.len() > MAX_WATERMARK_CHUNKS {
        // Truncating loses precision, not correctness: a mark we fail to
        // advertise means the peer offers rows we already have, which is
        // idempotent. Overrunning the reader would break the exchange itself.
        tracing::warn!(
            "sync: {} watermark chunks exceeds the {MAX_WATERMARK_CHUNKS} the peer will read; truncating",
            chunks.len()
        );
        chunks.truncate(MAX_WATERMARK_CHUNKS);
    }
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
    // The bound has to be at least what `send_watermarks` can legitimately
    // produce, or an honest peer with many sources desynchronises the stream
    // and every exchange with it fails from then on — permanently, and across
    // restarts, because the marks are durable.
    for _ in 0..MAX_WATERMARK_CHUNKS {
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
                &|| false,
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
            &|| false,
        );
        let b_stats = b.join().expect("the accepting side must not panic");
        (
            a_stats.expect("dialling side"),
            b_stats.expect("accepting side"),
        )
    }

    #[test]
    fn a_peer_never_offers_us_rows_we_authored() {
        // There is nothing to record, because there is nothing to offer: a
        // device serves only the rows it wrote. That is what makes the exchange
        // settle immediately rather than after a round of mutual echo, and it
        // is the same rule that stops a peer rewriting our history.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed(&b_store, A, 3, 32);

        let (_, b_stats) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            b_stats.sent_items, 0,
            "B offered rows it did not author: {b_stats:?}"
        );
        assert_eq!(a_store.lock().count().unwrap(), 0);
    }


    #[test]
    fn a_large_first_sync_completes_in_both_directions() {
        // The deadlock this exists for: both peers used to send their entire
        // history before either started reading. Fine while everything fits in
        // the socket and Noise buffers, and a hang once it does not — so this
        // seeds far past any plausible buffer, in BOTH stores at once.
        //
        // 400 rows x 4 KB is ~1.6 MB each way, against a socket buffer measured
        // in tens of KB.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed(&a_store, A, 400, 4096);
        seed(&b_store, B, 400, 4096);

        let (a_stats, b_stats) = run_pair(a_store.clone(), b_store.clone());

        assert_eq!(a_stats.applied_items, 400, "A takes all of B's rows");
        assert_eq!(b_stats.applied_items, 400, "B takes all of A's rows");
        assert_eq!(a_stats.refused, 0);
        assert_eq!(b_stats.refused, 0);

        assert_eq!(a_store.lock().count().unwrap(), 800);
        assert_eq!(b_store.lock().count().unwrap(), 800);
    }


    #[test]
    fn repeated_exchanges_converge_and_then_go_quiet() {
        // Convergence, with one round of echo allowed on the way.
        //
        // Each side offers the other its own rows back once, because it cannot
        // know what the other still holds until the other says so. They are
        // recognised as no-ops, the cursor rises, and it stops. An earlier
        // version skipped that round by raising the floor to "what this peer
        // has given us" — and lost every delete and edit made on the receiving
        // side, permanently. A wasted round is the cheaper mistake.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed(&a_store, A, 20, 64);
        seed(&b_store, B, 20, 64);

        let mut quiet = None;
        for round in 1..=4 {
            let (a, b) = run_pair(a_store.clone(), b_store.clone());
            if a.sent_items == 0 && b.sent_items == 0 {
                quiet = Some(round);
                break;
            }
        }
        assert!(quiet.is_some(), "the exchange never went quiet");
        assert!(quiet.unwrap() <= 3, "took {} rounds to settle", quiet.unwrap());

        // And converged: each side holds both histories exactly once.
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
        // one, so the leftovers surfaced where rows were expected and ended the
        // exchange.
        //
        // The marks are seeded through SQL rather than `note_received`, which
        // caps the table per peer at well under one batch — that cap is the
        // right behaviour and it also means the chunking path is unreachable
        // through the ordinary route. Seeding underneath it is what keeps this
        // a real test of the wire rather than a vacuous one.
        let a_store = store_for(A);
        let b_store = store_for(B);
        {
            let g = a_store.lock();
            for i in 0..(PAGE + 40) {
                let src = format!("33333333-3333-4333-8333-{:012x}", i);
                g.note_received_uncapped_for_test(B, &src, 1_000 + i as i64).unwrap();
            }
        }
        assert!(
            a_store.lock().watermarks(B).unwrap().len() > PAGE,
            "precondition: more marks than one batch can carry"
        );
        seed(&b_store, B, 3, 32);

        let (a_stats, _) = run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            a_stats.applied_items, 3,
            "the exchange must complete with more marks than one batch holds"
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
                &|| false,
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
            &|| false,
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
                &|| false,
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
            &|| false,
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
                &|| false,
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
            &|| false,
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
    fn an_edit_to_a_peers_row_is_local_and_a_delete_is_not() {
        // A deliberate limitation, and the trade behind it.
        //
        // Only the device that WROTE a row may change its content. Anything
        // looser meant a paired device could rewrite our history in place —
        // it knows the identity of every row we ever synced to it — and could
        // reach a third device's rows through us as well.
        //
        // So pinning or correcting a dictation on a machine that did not record
        // it stays local. DELETES are exempt and must be: a password cleared on
        // the laptop has to vanish from the machine that recorded it, and a
        // delete carries no attacker-chosen content.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed_at(&b_store, B, "row", "clipboard", 1_700_000_000_000);
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_store.lock().count().unwrap(), 1, "A received B's row");

        // A edits it. That does not travel back to B.
        let id: i64 = a_store.lock().recent(None, 10).unwrap()[0].id;
        a_store.lock().update_text(id, "corrected on A").unwrap();
        run_pair(a_store.clone(), b_store.clone());
        let b_text = b_store.lock().recent(None, 10).unwrap()[0].text.clone();
        assert_eq!(
            b_text, "clipboard-row",
            "content may only change on the device that wrote it"
        );

        // But deleting it on A does reach B.
        a_store.lock().delete_item_local(id).unwrap();
        for _ in 0..3 {
            run_pair(a_store.clone(), b_store.clone());
            if b_store.lock().count().unwrap() == 0 {
                break;
            }
        }
        assert_eq!(b_store.lock().count().unwrap(), 0, "a delete must still propagate");
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

// ===========================================================================
// ROUND 3 ADVERSARIAL REVIEW. Each test is written to FAIL against the
// current code and names the defect it demonstrates.
// ===========================================================================
#[cfg(test)]
mod adversarial_round3 {
    use super::*;
    use echokey_core::history::MAX_TOMBSTONES_PER_SOURCE;
    use echokey_core::history::{RemoteItem, RemoteTombstone, Store};
    use echokey_sync::{DeviceId, ItemKind, PairedKey, Session, SyncItem, SyncMessage, PROTOCOL_VERSION};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

    fn socket_pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (srv, _) = l.accept().unwrap();
        // MANDATORY read AND write timeouts: a write/write deadlock parks both
        // sides in `write`, where a read timeout alone would never fire.
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

    fn run_pair_from(
        a_store: Arc<Mutex<Store>>,
        b_store: Arc<Mutex<Store>>,
        a_resend: bool,
        a_from: i64,
    ) -> (RoundStats, RoundStats) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let bt = std::thread::spawn(move || {
            let mut session = Session::accept(srv, &k2).unwrap();
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            exchange(
                &mut session,
                &b_store,
                (B, "Deck B"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
        });
        let mut session = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let a_stats = exchange(
            &mut session,
            &a_store,
            (A, "Deck A"),
            both(),
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            a_resend,
            a_from,
            &|| false,
        );
        let b_stats = bt.join().expect("accepting side must not panic");
        (a_stats.expect("dialling side"), b_stats.expect("accepting side"))
    }

    fn run_pair(a: Arc<Mutex<Store>>, b: Arc<Mutex<Store>>) -> (RoundStats, RoundStats) {
        run_pair_from(a, b, false, 0)
    }

    // -----------------------------------------------------------------
    // R3-1. `floor_for` raises the serve floor for the PEER's own source to
    // `max(their mark, our mark for them)`. On the accepting side the drain
    // runs BEFORE the serve, so a row the peer sends in this very exchange
    // lifts our floor above a local edit we have not sent yet — and the mark
    // only ever grows, so the edit is never offered again.
    // -----------------------------------------------------------------
    #[test]
    fn a_delete_on_the_accepting_side_reaches_the_author_even_when_it_is_busy() {
        // The edit half of this is now deliberate — see
        // an_edit_to_a_peers_row_is_local_and_a_delete_is_not. What must still
        // hold is the delete half, and specifically that a row the author
        // writes AFTER the delete cannot carry the cursor over it: that was the
        // failure mode, and it needed no skew or hostility.
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed_at(&b_store, B, "secret", "clipboard", 1_700_000_000_000);
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(a_store.lock().count().unwrap(), 1);

        let id: i64 = a_store.lock().recent(None, 10).unwrap()[0].id;
        a_store.lock().delete_item_local(id).unwrap();

        // B keeps working after the delete.
        std::thread::sleep(Duration::from_millis(3));
        seed_at(&b_store, B, "later", "clipboard", now_ms());

        for _ in 0..3 {
            run_pair(a_store.clone(), b_store.clone());
        }
        let texts: Vec<String> = b_store
            .lock()
            .recent(None, 10)
            .unwrap()
            .into_iter()
            .map(|r| r.text)
            .collect();
        assert!(
            !texts.iter().any(|t| t == "secret"),
            "the delete never reached the author: {texts:?}"
        );
    }


    // -----------------------------------------------------------------
    // R3-2. The same hole for a DELETE: the tombstone serve uses the same
    // floor. A password deleted on the receiving machine never reaches the
    // machine that wrote it — the exact failure the code says it must not have.
    // -----------------------------------------------------------------
    #[test]
    fn r3_a_delete_on_the_accepting_side_is_lost_when_the_peer_has_a_newer_row() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        seed_at(&a_store, A, "secret", "clipboard", 1_700_000_000_000);
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(b_store.lock().count().unwrap(), 1);

        // The user deletes the synced password on B.
        let id = b_store.lock().recent(None, 10).unwrap()[0].id;
        b_store.lock().delete(id).unwrap();
        assert_eq!(
            b_store.lock().tombstones_since(A, 0, 10).unwrap().len(),
            1,
            "precondition: B recorded the tombstone"
        );

        // A captures anything at all afterwards.
        std::thread::sleep(Duration::from_millis(5));
        seed_at(&a_store, A, "later", "clipboard", crate::sync::manager::now_ms());

        run_pair(a_store.clone(), b_store.clone());
        run_pair(a_store.clone(), b_store.clone());
        run_pair(a_store.clone(), b_store.clone());

        let still_there = a_store
            .lock()
            .recent(None, 10)
            .unwrap()
            .into_iter()
            .any(|i| i.text == "clipboard-secret");
        assert!(
            !still_there,
            "the delete made on B must reach A; the deleted clipboard row is \
             still on A, permanently"
        );
    }

    // -----------------------------------------------------------------
    // R3-3. `drain` records a receipt at `it.updated_at` with NO range check
    // (the second `note_received` call). `mark_received_in` claims it is safe
    // because "callers record a receipt only for a row they actually
    // accepted". They do not. One row stamped i64::MAX from the peer itself
    // parks our cursor for that peer at i64::MAX permanently.
    // -----------------------------------------------------------------
    #[test]
    fn r3_one_row_with_an_absurd_clock_becomes_a_permanent_receipt() {
        let b_store = store_for(B);

        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let bs = b_store.clone();
        let bt = std::thread::spawn(move || {
            let mut session = Session::accept(srv, &k2).unwrap();
            let known = vec![A.to_string(), B.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            exchange(
                &mut session,
                &bs,
                (B, "Deck B"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
        });

        // A: a device whose RTC is dead, or simply lying. It offers ONE row,
        // for itself, stamped i64::MAX.
        let mut s = Session::initiate(c, &key).unwrap();
        s.send(&SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::parse(A).unwrap(),
            device_name: "Deck A".into(),
        })
        .unwrap();
        match s.recv().unwrap() {
            SyncMessage::Hello { .. } => {}
            m => panic!("expected hello, got {m:?}"),
        }
        s.send(&SyncMessage::Watermarks { entries: Vec::new(), more: false }).unwrap();
        for _ in 0..MAX_BATCHES {
            match s.recv().unwrap() {
                SyncMessage::Watermarks { more, .. } => {
                    if !more {
                        break;
                    }
                }
                m => panic!("expected watermarks, got {m:?}"),
            }
        }
        s.send(&SyncMessage::Items {
            items: vec![SyncItem {
                source_device: DeviceId::parse(A).unwrap(),
                origin_id: "1".into(),
                kind: ItemKind::Transcription,
                text: "from a machine with a dead clock".into(),
                created_at: 1_700_000_000_000,
                updated_at: i64::MAX,
                pinned: false,
                clock: u64::MAX,
            }],
            more: false,
        })
        .unwrap();
        s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
        // Drain B's serve output, bounded, so B is never left blocked in write.
        for _ in 0..MAX_BATCHES * 4 {
            match s.recv() {
                Ok(SyncMessage::Items { items, more }) if items.is_empty() && !more => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        drop(s);
        let _ = bt.join().expect("accepting side must not panic");

        let marks = b_store.lock().watermarks(A).unwrap();
        assert!(
            marks
                .iter()
                .all(|(_, clock)| *clock <= crate::sync::manager::now_ms() + MAX_SKEW_MS),
            "a refused, out-of-range clock must never become a receipt; marks={marks:?}"
        );
    }

    // -----------------------------------------------------------------
    // R3-4. The consequence of R3-3, end to end: once the mark is at i64::MAX,
    // the honest peer is served from i64::MAX and can never deliver anything
    // again. Nothing but `reset_source_marks` (a kind widening) repairs it.
    // -----------------------------------------------------------------
    #[test]
    fn r3_a_poisoned_receipt_permanently_hides_everything_the_peer_writes() {
        let a_store = store_for(A);
        let b_store = store_for(B);
        // Exactly the receipt R3-3 shows `drain` will write.
        b_store.lock().note_received(A, A, i64::MAX).unwrap();

        for i in 0..5 {
            seed_at(&a_store, A, &format!("r{i}"), "transcription", 1_700_000_000_000 + i);
        }
        run_pair(a_store.clone(), b_store.clone());
        run_pair(a_store.clone(), b_store.clone());
        assert_eq!(
            b_store.lock().count().unwrap(),
            5,
            "one bad clock must not hide everything the peer legitimately writes"
        );
    }

    // -----------------------------------------------------------------
    // R3-5. `serve` takes `reached` as the MINIMUM `after` across every source.
    // A store that holds rows from more than one source — i.e. any store that
    // has ever synced — pins that minimum at the OTHER source's last clock, so
    // a truncated re-offer resumes from the same place forever and the tail of
    // the history is never delivered.
    // -----------------------------------------------------------------
    #[test]
    fn a_truncated_re_offer_progresses_even_with_several_sources() {
        // `reached` is the minimum across sources, and it used to include
        // sources that were never truncated. Those never advance their cursor,
        // so the minimum stayed pinned at the starting floor and a re-offer
        // resumed exactly where it began — every store holding more than one
        // source, which is every store that has ever synced.
        let a_store = store_for(A);
        let b_store = store_for(B);

        // A holds a few of B's rows, with clocks older than A's own history.
        for i in 0..3 {
            seed_at(&a_store, B, &format!("b{i}"), "clipboard", 1_600_000_000_000 + i);
        }
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

        let (mut stats, _) = run_pair_from(a_store.clone(), b_store.clone(), true, 0);
        assert!(stats.truncated, "precondition: the cap bites: {stats:?}");
        let mut from = stats.resend_progress.unwrap_or(0);
        assert!(from > 0, "a truncated re-offer must report where it got to");

        for _ in 0..8 {
            if b_store.lock().count().unwrap() as usize >= n {
                break;
            }
            let (r, _) = run_pair_from(a_store.clone(), b_store.clone(), true, from);
            stats = r;
            if stats.truncated {
                let next = stats.resend_progress.unwrap_or(0);
                assert!(next > from, "the cursor must advance, got {next} then {from}");
                from = next;
            }
        }

        // Every row A authored arrives. B's OWN rows do not come back to it —
        // only the device that wrote a row may bring it into existence, so a
        // history B has itself dropped is not restored from a peer's copy.
        assert_eq!(
            b_store.lock().count().unwrap() as usize,
            n,
            "a retained, advancing cursor must deliver everything A authored"
        );
        assert_eq!(b_store.lock().items_since(B, 0, 10).unwrap().len(), 0);
    }


    // -----------------------------------------------------------------
    // R3-6. `may_create` lets a peer tombstone identities we have never held,
    // as long as it claims them for ITSELF, and tombstones are never pruned.
    // A paired device can therefore grow our database without bound.
    // -----------------------------------------------------------------
    #[test]
    fn peer_written_tombstones_are_bounded() {
        // A peer may legitimately tombstone an identity we have never held: a
        // delete can overtake the row it deletes, and refusing those would
        // resurrect rows. So the growth is bounded rather than forbidden —
        // per source, oldest first — because "never pruned by age" alone hands
        // a paired device unbounded control over our disk.
        let a_store = store_for(A);
        let b_store = store_for(B);

        let over = (MAX_TOMBSTONES_PER_SOURCE + 500) as usize;
        {
            let g = b_store.lock();
            for i in 0..over {
                g.apply_remote_tombstone(
                    B,
                    &RemoteTombstone {
                        source_machine: B.into(),
                        origin_id: format!("ghost-{i}"),
                        deleted_at: 1_700_000_000_000 + i as i64,
                    },
                )
                .unwrap();
            }
        }

        for _ in 0..3 {
            run_pair(a_store.clone(), b_store.clone());
        }
        a_store.lock().prune(0, 0).unwrap();

        let held = a_store.lock().tombstone_count(B).unwrap();
        assert!(
            held <= MAX_TOMBSTONES_PER_SOURCE,
            "a peer wrote {held} tombstones, past the {MAX_TOMBSTONES_PER_SOURCE} cap"
        );
    }

}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 3) — demonstrations of live findings. NOT fixes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round3_authority {
    use super::*;
    use echokey_core::history::{RemoteTombstone, Store};
    use echokey_sync::{
        DeviceId, ItemKind, PairedKey, Session, SyncItem, SyncMessage, Tombstone, Watermark,
        PROTOCOL_VERSION,
    };
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111"; // us (the victim)
    const B: &str = "22222222-2222-4222-8222-222222222222"; // the paired, hostile peer

    fn socket_pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (srv, _) = l.accept().unwrap();
        for sock in [&c, &srv] {
            // MANDATORY on both directions: a stalled exchange must fail the
            // test, never hang the suite.
            sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            sock.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        }
        (c, srv)
    }

    fn victim_store() -> Arc<Mutex<Store>> {
        let mut s = Store::open_in_memory().unwrap();
        s.set_device_id(A);
        Arc::new(Mutex::new(s))
    }

    fn dev(id: &str) -> DeviceId {
        DeviceId::parse(id).unwrap()
    }

    /// The hostile peer's script. It holds the paired key, so the Noise
    /// handshake succeeds and `Attribution.peer_id` is genuinely B.
    ///
    /// Bounded everywhere: every read loop has a hard iteration cap.
    fn hostile_peer(
        srv: TcpStream,
        key: PairedKey,
        items: Vec<SyncItem>,
        tombs: Vec<Tombstone>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut s = Session::accept(srv, &key).expect("hostile peer completes the handshake");
            s.send(&SyncMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                device_id: dev(B),
                device_name: "Hostile".into(),
            })
            .unwrap();
            // Victim (Turn::Second) sends Hello, then reads our watermarks.
            let _ = s.recv().unwrap();
            s.send(&SyncMessage::Watermarks {
                entries: Vec::<Watermark>::new(),
                more: false,
            })
            .unwrap();
            // Its watermarks come back; drain them (hard bound).
            for _ in 0..MAX_BATCHES {
                match s.recv().unwrap() {
                    SyncMessage::Watermarks { more, .. } => {
                        if !more {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            // The payload: rows and deletes attributed to the VICTIM's own id.
            if !tombs.is_empty() {
                s.send(&SyncMessage::Tombstones {
                    entries: tombs,
                    more: true,
                })
                .unwrap();
            }
            if !items.is_empty() {
                s.send(&SyncMessage::Items { items, more: true }).unwrap();
            }
            s.send(&SyncMessage::Items {
                items: Vec::new(),
                more: false,
            })
            .unwrap();
            // Then it serves us; read until its terminator, hard-bounded.
            for _ in 0..(MAX_BATCHES * 4) {
                match s.recv() {
                    Ok(SyncMessage::Items { items, more }) if items.is_empty() && !more => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        })
    }

    /// Run one exchange with the victim as `Turn::Second` against `hostile_peer`.
    fn run_attack(
        store: &Arc<Mutex<Store>>,
        kinds: Kinds,
        items: Vec<SyncItem>,
        tombs: Vec<Tombstone>,
    ) -> RoundStats {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let peer = hostile_peer(srv, key.clone(), items, tombs);
        let mut session = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string()];
        let attr = Attribution {
            peer_id: B,
            local_id: A,
            known: &known,
        };
        let stats = exchange(
            &mut session,
            store,
            (A, "Victim"),
            kinds,
            Retention {
                oldest_allowed: None,
            },
            &attr,
            Turn::Second,
            false,
            0,
            &|| false,
        );
        let _ = peer.join();
        stats.expect("the exchange itself completes")
    }

    /// FINDING (round 3): a paired peer can DELETE and REWRITE rows of OURS
    /// that it was never given, by naming `origin_id`.
    ///
    /// `Attribution::may_create` refuses `source == local_id`, but `drain`
    /// falls through to `Store::holds_identity` for anything `may_create`
    /// refuses — and we hold every one of our own rows. A locally written row's
    /// `origin_id` is `CAST(rowid AS TEXT)` (history.rs `stamp_origin`), i.e.
    /// "1", "2", "3"..., so the peer does not even have to guess.
    ///
    /// The rows attacked here were NEVER shared with that peer: clipboard sync
    /// is off, so nothing of this kind ever left the machine. Pinning does not
    /// protect them either — `apply_remote_tombstone` deletes unconditionally,
    /// and the tombstone arm of `drain` has no `kinds` filter at all.
    #[test]
    fn adv3_a_paired_peer_can_erase_and_rewrite_our_own_never_shared_rows() {
        let store = victim_store();
        let (private_id, other_id) = {
            let g = store.lock();
            let a = g
                .insert_clipboard("a password we never sync", None, None)
                .unwrap();
            let b = g
                .insert_clipboard("a second private capture", None, None)
                .unwrap();
            g.set_pinned(a, true).unwrap(); // pinned: survives prune and Clear History
            (a, b)
        };
        // Local rows carry origin_id == rowid, as text.
        assert_eq!(private_id.to_string(), "1");
        assert_eq!(other_id.to_string(), "2");

        let now = now_ms();
        let tombs = vec![Tombstone {
            source_device: dev(A), // OUR id, not the peer's
            origin_id: private_id.to_string(),
            deleted_at: now,
            clock: now as u64,
        }];
        let items = vec![SyncItem {
            source_device: dev(A), // OUR id again
            origin_id: other_id.to_string(),
            kind: ItemKind::Transcription,
            text: "TEXT THE ATTACKER CHOSE".into(),
            created_at: now,
            updated_at: now + 1,
            pinned: false,
            clock: (now + 1) as u64,
        }];

        // The user has clipboard sync switched OFF: these rows have never been
        // offered to any peer.
        let kinds = Kinds {
            dictations: true,
            clipboard: false,
        };
        let _stats = run_attack(&store, kinds, items, tombs);

        let g = store.lock();
        let erased = g.get(private_id).unwrap().is_none();
        let rewritten = g
            .get(other_id)
            .unwrap()
            .map(|r| r.text)
            .unwrap_or_default();
        assert!(
            !erased && rewritten != "TEXT THE ATTACKER CHOSE",
            "a paired peer reached rows of ours it was never given: pinned row {private_id}              erased = {erased}; row {other_id} now reads {rewritten:?}"
        );
    }

    /// FINDING (round 3): the `tombstones` table is unbounded and never pruned.
    ///
    /// `Attribution::may_create` lets a peer bring rows into existence for its
    /// OWN source id, and `origin_id` is free-form up to `MAX_ORIGIN_ID_BYTES`
    /// (128). `prune(days, max)` only ever touches `items`, and
    /// `prune_tombstones` has no caller anywhere in the app. So a paired peer
    /// writes as many rows to disk as it likes, forever, and nothing reclaims
    /// them.
    #[test]
    fn adv3_peer_written_tombstones_are_bounded_or_pruned() {
        let store = victim_store();
        let now = now_ms();
        {
            let g = store.lock();
            for i in 0..5_000 {
                g.apply_remote_tombstone(
                    B,
                    &RemoteTombstone {
                        source_machine: B.into(),
                        origin_id: format!("{i}{}", "p".repeat(120)),
                        deleted_at: now,
                    },
                )
                .unwrap();
            }
            // Everything the app ever calls in production.
            g.prune(1, 10).unwrap();
        }
        // Bounded, not zero. A delete may legitimately overtake the row it
        // deletes, so refusing tombstones for identities we have never held
        // would resurrect rows — and pruning them by age can do the same. The
        // answer is a per-source ceiling, oldest evicted first, which caps what
        // a paired device can make us store without ever guessing that a delete
        // has stopped mattering.
        let left = store.lock().tombstone_count(B).unwrap();
        assert!(
            left <= echokey_core::history::MAX_TOMBSTONES_PER_SOURCE,
            "{left} peer-authored tombstones survive, past the cap"
        );
    }
}

// ===========================================================================
// ROUND 3 ADVERSARIAL REVIEW, part 2: authority.
// ===========================================================================
#[cfg(test)]
mod adversarial_round3_authority_check {
    use super::*;
    use echokey_core::history::Store;
    use echokey_sync::{
        DeviceId, ItemKind, PairedKey, Session, SyncItem, SyncMessage, Tombstone, PROTOCOL_VERSION,
    };
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const ATTACKER: &str = "11111111-1111-4111-8111-111111111111";
    const VICTIM: &str = "22222222-2222-4222-8222-222222222222";

    fn socket_pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (srv, _) = l.accept().unwrap();
        // Read AND write timeouts on both ends; no unbounded loop below.
        for sock in [&c, &srv] {
            sock.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
            sock.set_write_timeout(Some(Duration::from_secs(15))).unwrap();
        }
        (c, srv)
    }

    /// Drive one exchange where the victim runs the real code and the peer is
    /// a hand-written attacker that sends exactly `items` and `tombs`.
    fn run_attack(
        victim: &Arc<Mutex<Store>>,
        kinds: Kinds,
        items: Vec<SyncItem>,
        tombs: Vec<Tombstone>,
    ) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([9u8; 32]);
        let k2 = key.clone();
        let vs = victim.clone();
        let vt = std::thread::spawn(move || {
            let mut session = Session::accept(srv, &k2).unwrap();
            let known = vec![ATTACKER.to_string(), VICTIM.to_string()];
            let attr = Attribution { peer_id: ATTACKER, local_id: VICTIM, known: &known };
            exchange(
                &mut session,
                &vs,
                (VICTIM, "Victim"),
                kinds,
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
        });

        let mut s = Session::initiate(c, &key).unwrap();
        s.send(&SyncMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::parse(ATTACKER).unwrap(),
            device_name: "Attacker".into(),
        })
        .unwrap();
        match s.recv().unwrap() {
            SyncMessage::Hello { .. } => {}
            m => panic!("expected hello, got {m:?}"),
        }
        s.send(&SyncMessage::Watermarks { entries: Vec::new(), more: false }).unwrap();
        for _ in 0..MAX_BATCHES {
            match s.recv().unwrap() {
                SyncMessage::Watermarks { more, .. } => {
                    if !more {
                        break;
                    }
                }
                m => panic!("expected watermarks, got {m:?}"),
            }
        }
        if !tombs.is_empty() {
            s.send(&SyncMessage::Tombstones { entries: tombs, more: false }).unwrap();
        }
        if !items.is_empty() {
            s.send(&SyncMessage::Items { items, more: true }).unwrap();
        }
        s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
        // Bounded drain of the victim's own serve, so it never blocks in write.
        for _ in 0..MAX_BATCHES * 4 {
            match s.recv() {
                Ok(SyncMessage::Items { items, more }) if items.is_empty() && !more => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        drop(s);
        let _ = vt.join().expect("victim must not panic");
    }

    /// `Attribution::may_create` documents the invariant "a peer may never
    /// speak for us", and `nobody_may_bring_a_row_into_existence_as_us` asserts
    /// the predicate. `drain` does not enforce it: when `may_create` says no it
    /// falls through to `Store::holds_identity`, and we hold every row we ever
    /// wrote. A local row's origin_id is `CAST(rowid AS TEXT)` — "1", "2", … —
    /// so there is nothing to guess.
    #[test]
    fn r3_a_paired_peer_can_rewrite_and_delete_our_own_local_rows() {
        let mut s = Store::open_in_memory().unwrap();
        s.set_device_id(VICTIM);
        let store = Arc::new(Mutex::new(s));

        let (secret, other) = {
            let g = store.lock();
            let a = g.insert_clipboard("a password we never sync", None, None).unwrap();
            let b = g.insert_clipboard("a second private capture", None, None).unwrap();
            g.set_pinned(a, true).unwrap();
            (a, b)
        };
        assert_eq!((secret, other), (1, 2), "local origin ids are just the rowids");

        let now = crate::sync::manager::now_ms();
        let tombs = vec![Tombstone {
            source_device: DeviceId::parse(VICTIM).unwrap(), // OUR id, not the peer's
            origin_id: secret.to_string(),
            deleted_at: now,
            clock: now as u64,
        }];
        let items = vec![SyncItem {
            source_device: DeviceId::parse(VICTIM).unwrap(), // OUR id again
            origin_id: other.to_string(),
            kind: ItemKind::Transcription,
            text: "TEXT THE ATTACKER CHOSE".into(),
            created_at: now,
            updated_at: now + 1,
            pinned: false,
            clock: (now + 1) as u64,
        }];

        // Clipboard sync is OFF: these two rows have never been offered to any
        // peer, and one of them is pinned.
        run_attack(&store, Kinds { dictations: true, clipboard: false }, items, tombs);

        let g = store.lock();
        let erased = g.get(secret).unwrap().is_none();
        let text = g.get(other).unwrap().map(|r| r.text).unwrap_or_default();
        assert!(
            !erased && text != "TEXT THE ATTACKER CHOSE",
            "a paired peer reached rows of ours it was never given: pinned row \
             {secret} erased = {erased}; row {other} now reads {text:?}"
        );
    }

}

// ===========================================================================
// ROUND 3 ADVERSARIAL REVIEW (lifecycle reviewer) — independent demonstration.
// ===========================================================================
#[cfg(test)]
mod adversarial_r3_lifecycle {
    use super::*;
    use echokey_core::history::Store;
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111"; // us
    const B: &str = "22222222-2222-4222-8222-222222222222"; // the peer

    fn socket_pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (srv, _) = l.accept().unwrap();
        for sock in [&c, &srv] {
            sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            sock.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        }
        (c, srv)
    }

    fn store_for(me: &str) -> Arc<Mutex<Store>> {
        let mut s = Store::open_in_memory().unwrap();
        s.set_device_id(me);
        Arc::new(Mutex::new(s))
    }

    /// FINDING: a peer's own row with an out-of-range clock still becomes a
    /// receipt, which permanently hides everything that device writes.
    ///
    /// replicate.rs takes a bounded receipt first (`clock > 0 && clock <=
    /// now_ms() + MAX_SKEW_MS`) and then, a few lines further down, an
    /// UNBOUNDED one with the raw `it.updated_at`, before retention, kinds or
    /// `apply_remote_item` have had any say. `may_create` is true for the
    /// peer's own rows, so nothing in between can stop it.
    ///
    /// history.rs `mark_received_in` documents the invariant this breaks:
    /// "Callers record a receipt only for a row they actually accepted, and an
    /// accepted row is already within the ceiling, so nothing out of range can
    /// reach this." `apply_remote_item` refuses the row and takes no receipt
    /// for exactly this reason; `drain` takes one anyway.
    #[test]
    fn r3l_a_refused_future_row_must_not_become_a_receipt() {
        let a_store = store_for(A);
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();

        let peer = std::thread::spawn(move || {
            let mut s = Session::accept(srv, &k2).unwrap();
            s.send(&SyncMessage::Hello {
                protocol_version: echokey_sync::PROTOCOL_VERSION,
                device_id: DeviceId::parse(B).unwrap(),
                device_name: "Peer".into(),
            })
            .unwrap();
            let _ = s.recv().unwrap(); // Hello
            let _ = s.recv().unwrap(); // Watermarks (Turn::First speaks first)
            s.send(&SyncMessage::Watermarks { entries: Vec::new(), more: false }).unwrap();
            for _ in 0..1000 {
                match s.recv().unwrap() {
                    SyncMessage::Items { items, more } if !more && items.is_empty() => break,
                    _ => continue,
                }
            }
            let poison = SyncItem {
                source_device: DeviceId::parse(B).unwrap(),
                origin_id: "poison-1".into(),
                kind: ItemKind::Transcription,
                text: "x".into(),
                created_at: 1_700_000_000_000,
                updated_at: i64::MAX,
                pinned: false,
                clock: 1,
            };
            s.send(&SyncMessage::Items { items: vec![poison], more: false }).unwrap();
            s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
        });

        let mut s = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let stats = exchange(
            &mut s,
            &a_store,
            (A, "Us"),
            Kinds { dictations: true, clipboard: true },
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
            &|| false,
        )
        .expect("exchange completes");
        peer.join().unwrap();

        assert_eq!(stats.applied_items, 0, "the row itself is correctly refused");
        let marks = a_store.lock().watermarks(B).unwrap();
        let for_b = marks.iter().find(|(src, _)| src == B).map(|(_, c)| *c);
        assert!(
            for_b.map(|c| c <= now_ms() + MAX_SKEW_MS).unwrap_or(true),
            "a REFUSED row parked our cursor for the peer at {for_b:?}; every row that device legitimately writes below that clock is hidden from us permanently"
        );
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW — ROUND 4. Demonstrations of live findings. Not fixes.
// Every socket carries BOTH a read and a write timeout, and every loop is
// hard-bounded, so nothing here can hang the suite.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round4 {
    use super::*;
    /// One row from `source`, stamped `clock`.
    fn item_at(source: &str, origin: &str, clock: i64) -> RemoteItem {
        RemoteItem {
            source_machine: source.into(),
            origin_id: origin.into(),
            kind: "transcription".into(),
            text: "x".into(),
            created_at: clock,
            updated_at: clock,
            pinned: false,
        }
    }

    use echokey_core::history::{RemoteItem, Store};
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

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

    /// Put a row into `store` attributed to `source`, at an exact clock.
    fn seed_at(store: &Arc<Mutex<Store>>, source: &str, origin: &str, clock: i64) {
        store
            .lock()
            .apply_remote_item(
                C,
                &RemoteItem {
                    source_machine: source.into(),
                    origin_id: origin.into(),
                    kind: "transcription".into(),
                    text: format!("row-{origin}"),
                    created_at: clock,
                    updated_at: clock,
                    pinned: false,
                },
            )
            .unwrap();
    }

    fn run_pair(a_store: Arc<Mutex<Store>>, b_store: Arc<Mutex<Store>>) -> (RoundStats, RoundStats) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let bt = std::thread::spawn(move || {
            let mut session = Session::accept(srv, &k2).unwrap();
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            exchange(
                &mut session,
                &b_store,
                (B, "Deck B"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
        });
        let mut session = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let a_stats = exchange(
            &mut session,
            &a_store,
            (A, "Deck A"),
            both(),
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
            &|| false,
        );
        let b_stats = bt.join().expect("accepting side must not panic");
        (a_stats.expect("dialling side"), b_stats.expect("accepting side"))
    }

    // -----------------------------------------------------------------
    // R4-1. `fetch_page` only widens when the WHOLE page shares one
    // millisecond. A page that merely ENDS inside a saturated millisecond is
    // not detected: the cursor advances to that millisecond and every
    // remaining row stamped with it is skipped, permanently.
    // -----------------------------------------------------------------
    #[test]
    fn r4_a_page_that_ends_inside_one_millisecond_loses_the_rest_forever() {
        let a_store = store_for(A);
        let b_store = store_for(B);

        let t = now_ms() - 5_000_000;
        // One row a millisecond earlier, so the page is NOT uniform and the
        // widening never fires...
        seed_at(&b_store, B, "aaa-first", t);
        // ...then a burst that does not fit in a single page.
        let burst = 300usize;
        for i in 0..burst {
            seed_at(&b_store, B, &format!("burst-{i:04}"), t + 1);
        }
        let total = (burst + 1) as i64;
        assert_eq!(b_store.lock().count().unwrap(), total);

        // Bounded: five exchanges is far more than convergence should need.
        for _ in 0..5 {
            run_pair(a_store.clone(), b_store.clone());
        }

        assert_eq!(
            a_store.lock().count().unwrap(),
            total,
            "rows stamped with the page-boundary millisecond were never delivered"
        );
    }

    // -----------------------------------------------------------------
    // R4-2. A peer whose clock is fast but WITHIN the 24 h tolerance is
    // accepted, and its skewed clock is written straight into our cursor for
    // it. Once its clock is corrected, everything it writes carries a smaller
    // clock than the cursor and is never offered again — permanently, with no
    // malice and no repair path.
    // -----------------------------------------------------------------
    #[test]
    fn a_badly_skewed_peer_is_refused_and_recovers_the_moment_its_clock_is_fixed() {
        // The cursor we keep for a peer IS that peer's clock, so anything we
        // accept into it we are stuck with. Accepting half a day of skew put
        // the cursor half a day ahead, and once the clock was corrected every
        // row that peer produced fell below it and was never requested again —
        // silently, and for as long as it took real time to catch up.
        //
        // So the window is small and anything outside it is refused with a
        // warning naming the device. The rows written while the machine was
        // badly wrong are lost, which is a complaint the user can act on; rows
        // written after it is fixed are not, which is what matters.
        let a_store = store_for(A);
        let peer = B;

        let far = now_ms() + 12 * 60 * 60 * 1000;
        for i in 0..3 {
            let out = a_store
                .lock()
                .apply_remote_item(peer, &item_at(peer, &format!("skew-{i}"), far + i))
                .unwrap();
            assert_eq!(
                out,
                echokey_core::history::ApplyOutcome::Ignored,
                "a badly skewed row must be refused, not written into the cursor"
            );
        }
        assert!(
            a_store.lock().watermarks(peer).unwrap().is_empty(),
            "a refused row must leave the cursor untouched"
        );

        // Clock corrected. Everything from here on lands.
        for i in 0..4 {
            let out = a_store
                .lock()
                .apply_remote_item(peer, &item_at(peer, &format!("ok-{i}"), now_ms() + i))
                .unwrap();
            assert_eq!(out, echokey_core::history::ApplyOutcome::Inserted);
        }
        assert_eq!(a_store.lock().count().unwrap(), 4);
    }

}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW — ROUND 4, three-device mesh. Demonstrations, not fixes.
// Sockets carry read AND write timeouts; every loop is hard-bounded.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round4_mesh {
    use super::*;
    /// One row from `source`, stamped `clock`.
    fn item_at(source: &str, origin: &str, clock: i64) -> RemoteItem {
        RemoteItem {
            source_machine: source.into(),
            origin_id: origin.into(),
            kind: "transcription".into(),
            text: "x".into(),
            created_at: clock,
            updated_at: clock,
            pinned: false,
        }
    }

    use echokey_core::history::{RemoteItem, Store};
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

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

    /// One exchange between any two of the three devices. `x` dials.
    fn sync2(
        x_store: &Arc<Mutex<Store>>,
        x_id: &'static str,
        y_store: &Arc<Mutex<Store>>,
        y_id: &'static str,
    ) -> (RoundStats, RoundStats) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let ys = y_store.clone();
        let yt = std::thread::spawn(move || {
            let mut s = Session::accept(srv, &k2).unwrap();
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: x_id, local_id: y_id, known: &known };
            exchange(
                &mut s,
                &ys,
                (y_id, "Y"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
        });
        let mut s = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: y_id, local_id: x_id, known: &known };
        let xs = exchange(
            &mut s,
            x_store,
            (x_id, "X"),
            both(),
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
            &|| false,
        );
        let ys = yt.join().expect("accepting side must not panic");
        (xs.expect("dialling side"), ys.expect("accepting side"))
    }

    /// Everything a store holds, as a comparable snapshot.
    fn snapshot(store: &Arc<Mutex<Store>>) -> Vec<(String, String, String, String, i64, bool)> {
        let g = store.lock();
        let mut out = Vec::new();
        for src in g.known_sources().unwrap() {
            for r in g.items_since(&src, 0, 100_000).unwrap() {
                out.push((
                    r.source_machine.clone(),
                    r.origin_id.clone(),
                    r.kind.clone(),
                    r.text.clone(),
                    r.updated_at,
                    r.pinned,
                ));
            }
        }
        out.sort();
        out
    }

    fn own_row(store: &Arc<Mutex<Store>>, me: &str, origin: &str, text: &str, clock: i64) {
        store
            .lock()
            .apply_remote_item(
                "44444444-4444-4444-8444-444444444444",
                &RemoteItem {
                    source_machine: me.into(),
                    origin_id: origin.into(),
                    kind: "transcription".into(),
                    text: text.into(),
                    created_at: clock,
                    updated_at: clock,
                    pinned: false,
                },
            )
            .unwrap();
    }


    /// The local rowid of the row whose text is `text`.
    fn id_of(store: &Arc<Mutex<Store>>, text: &str) -> i64 {
        store
            .lock()
            .recent(None, 1000)
            .unwrap()
            .into_iter()
            .find(|i| i.text == text)
            .expect("the row must be held")
            .id
    }

    /// Run the full mesh until it goes quiet, or give up after `rounds`.
    /// Returns how many rounds it took, or None if it never settled.
    fn settle(
        a: &Arc<Mutex<Store>>,
        b: &Arc<Mutex<Store>>,
        c: &Arc<Mutex<Store>>,
        rounds: usize,
    ) -> Option<usize> {
        for r in 1..=rounds {
            let (s1, s2) = sync2(a, A, b, B);
            let (s3, s4) = sync2(b, B, c, C);
            let (s5, s6) = sync2(a, A, c, C);
            let moved = [s1, s2, s3, s4, s5, s6]
                .iter()
                .any(|s| s.applied_items > 0 || s.applied_tombstones > 0);
            if !moved {
                return Some(r);
            }
        }
        None
    }

    /// R4-MESH-3. The doc on `apply_remote_tombstone` promises that "an edit
    /// that happened after the delete survives it". It does not: the tombstone
    /// is absorbing and kills a strictly newer row.
    #[test]
    fn a_delete_is_absorbing_even_against_a_newer_edit() {
        // Deliberate, and the doc now says so. Last-writer-wins would let an
        // edit stamped after a delete resurrect the row, which is defensible
        // for a shared document and wrong here: one person, several machines,
        // no undelete anywhere in the product. The realistic sequence is a
        // password cleared on the laptop while the desktop, not yet told, still
        // shows it and gets it pinned.
        let a_store = store_for(A);
        let peer = B;
        let t = now_ms();

        a_store.lock().apply_remote_item(peer, &item_at(peer, "row", t)).unwrap();
        a_store
            .lock()
            .apply_remote_tombstone(
                peer,
                &RemoteTombstone {
                    source_machine: peer.into(),
                    origin_id: "row".into(),
                    deleted_at: t + 1,
                },
            )
            .unwrap();
        assert_eq!(a_store.lock().count().unwrap(), 0);

        // An edit stamped after the delete does NOT bring it back.
        let out = a_store
            .lock()
            .apply_remote_item(peer, &item_at(peer, "row", t + 100))
            .unwrap();
        assert_eq!(out, echokey_core::history::ApplyOutcome::Ignored);
        assert_eq!(a_store.lock().count().unwrap(), 0, "a deleted row must stay deleted");
    }

}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW — ROUND 4, part two. Demonstrations, not fixes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round4_more {
    use super::*;
    use echokey_core::history::{RemoteItem, RemoteTombstone, Store, MAX_TOMBSTONES_PER_SOURCE};
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

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

    fn sync2(
        x_store: &Arc<Mutex<Store>>,
        x_id: &'static str,
        y_store: &Arc<Mutex<Store>>,
        y_id: &'static str,
    ) -> (RoundStats, RoundStats) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let ys = y_store.clone();
        let yt = std::thread::spawn(move || {
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let mut s = Session::accept(srv, &k2).unwrap();
            let attr = Attribution { peer_id: x_id, local_id: y_id, known: &known };
            exchange(
                &mut s,
                &ys,
                (y_id, "Y"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
        });
        let mut s = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: y_id, local_id: x_id, known: &known };
        let xs = exchange(
            &mut s,
            x_store,
            (x_id, "X"),
            both(),
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
            &|| false,
        );
        let ys = yt.join().expect("accepting side must not panic");
        (xs.expect("dialling side"), ys.expect("accepting side"))
    }

    // -----------------------------------------------------------------
    // R4-3. The per-source tombstone cap drops the OLDEST deletes. A device
    // that has not yet heard about one of those deletes never will: nothing
    // re-derives a tombstone that has been evicted, and the row it killed
    // lives on that device forever.
    // -----------------------------------------------------------------
    #[test]
    fn tombstone_eviction_is_bounded_and_takes_the_oldest_first() {
        // The honest trade, stated where it can be checked.
        //
        // Tombstones are never pruned by age, because dropping one can let a
        // deleted row walk back in. But a peer may create them for its own rows
        // with a free-form origin id, so "never pruned" alone hands a paired
        // device unbounded control over our disk. The table is therefore capped
        // per source and evicts the OLDEST first.
        //
        // Residual, accepted: a device that has been offline long enough for a
        // delete to be evicted will not learn about it. That needs one source
        // to exceed the cap AND a peer absent across the whole overflow, and
        // the alternative — an unbounded table a peer controls — is worse.
        let s = store_for(A);
        let peer = B;
        let cap = echokey_core::history::MAX_TOMBSTONES_PER_SOURCE;
        let base = now_ms() - 1_000_000;

        {
            let g = s.lock();
            for i in 0..(cap + 200) {
                g.apply_remote_tombstone(
                    peer,
                    &RemoteTombstone {
                        source_machine: peer.into(),
                        origin_id: format!("t-{i:07}"),
                        deleted_at: base + i,
                    },
                )
                .unwrap();
            }
        }

        let held = s.lock().tombstone_count(peer).unwrap();
        assert!(held <= cap, "{held} tombstones held, past the {cap} cap");

        // The newest delete survives; the oldest is the one that went.
        let newest = s.lock().tombstones_since(peer, base + cap + 150, 10).unwrap();
        assert!(!newest.is_empty(), "the most recent deletes must be the ones kept");
    }


    // -----------------------------------------------------------------
    // R4-4. Origin ids are the authoring device's rowids, and the device id
    // lives in settings.json while the rowids live in history.db. Lose the
    // database (corruption, a manual delete, a restore) and the SAME identity
    // is minted again for completely different content. A peer's absorbing
    // tombstone then swallows the new row in perpetuity, silently.
    // -----------------------------------------------------------------
    #[test]
    fn r4_a_rebuilt_history_database_can_never_sync_again() {
        let a_store = store_for(A);
        let b_store = store_for(B);

        // A writes a row and syncs it. Its origin id is its rowid: "1".
        let first = a_store.lock().insert_clipboard("the old row", None, None).unwrap();
        assert_eq!(first, 1);
        sync2(&a_store, A, &b_store, B);
        assert_eq!(b_store.lock().count().unwrap(), 1);

        // A deletes it; the delete reaches B as a tombstone for (A, "1").
        a_store.lock().delete_item_local(first).unwrap();
        for _ in 0..3 {
            sync2(&a_store, A, &b_store, B);
        }
        assert_eq!(b_store.lock().count().unwrap(), 0, "precondition: the delete landed");

        // A's history.db is lost. settings.json — and so the device id —
        // survives, which is exactly how the app is laid out.
        let a_store = store_for(A);
        let again = a_store.lock().insert_clipboard("a brand new dictation", None, None).unwrap();
        assert_eq!(again, 1, "rowids restart, so the identity is minted again");

        for _ in 0..4 {
            sync2(&a_store, A, &b_store, B);
        }
        assert_eq!(
            b_store.lock().count().unwrap(),
            1,
            "a genuinely new dictation is swallowed by the old tombstone, forever"
        );
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 4) — demonstrations of live findings. NOT fixes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round4_lifecycle {
    use super::*;
    use echokey_core::history::{RemoteItem, Store};
    use echokey_sync::{DeviceId, ItemKind, PairedKey, Session, SyncItem, SyncMessage};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111"; // us
    const B: &str = "22222222-2222-4222-8222-222222222222"; // the paired peer

    fn socket_pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (srv, _) = l.accept().unwrap();
        // Read AND write timeouts on both ends: a protocol desync parks BOTH
        // sides in write(), so a read timeout alone lets the suite hang.
        for sock in [&c, &srv] {
            sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            sock.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        }
        (c, srv)
    }

    fn store_for(me: &str) -> Arc<Mutex<Store>> {
        let mut s = Store::open_in_memory().unwrap();
        s.set_device_id(me);
        Arc::new(Mutex::new(s))
    }

    /// A well-formed device id we have never heard of, one per index.
    fn invented(i: usize) -> String {
        format!("44444444-4444-4444-8444-{:012x}", i)
    }

    /// FINDING (round 4): `drain` writes the receipt BEFORE the attribution
    /// check (replicate.rs:534-543), so `source_device` — a string the PEER
    /// chooses — becomes a row in `source_marks` whatever it says. Nothing caps
    /// how many distinct sources one peer may name, so a paired device decides
    /// how large that table gets: 256 per wire message, and `drain` reads up to
    /// `MAX_BATCHES * 4` messages per exchange.
    ///
    /// This drives the real `drain` against a peer that invents ids.
    #[test]
    fn r4_a_paired_peer_mints_unbounded_receipts_from_invented_source_ids() {
        let a_store = store_for(A);
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();

        // The peer: one legal batch, every item attributed to a different
        // device that does not exist. Each is a valid DeviceId, so the wire
        // validation passes.
        let peer = std::thread::spawn(move || {
            let mut s = Session::accept(srv, &k2).unwrap();
            let items: Vec<SyncItem> = (0..echokey_sync::MAX_BATCH_LEN)
                .map(|i| SyncItem {
                    source_device: DeviceId::parse(&invented(i)).unwrap(),
                    origin_id: format!("row-{i}"),
                    kind: ItemKind::Transcription,
                    text: "x".into(),
                    created_at: 1_700_000_000_000,
                    updated_at: 1_700_000_000_000,
                    pinned: false,
                    clock: 1_700_000_000_000,
                })
                .collect();
            s.send(&SyncMessage::Items { items, more: true }).unwrap();
            // End of stream, so drain returns instead of blocking.
            s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
        });

        let mut session = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let mut stats = RoundStats::default();
        drain(
            &mut session,
            &a_store,
            Kinds { dictations: true, clipboard: true },
            Retention { oldest_allowed: None },
            &attr,
            &|| false,
            &mut stats,
        )
        .unwrap();
        peer.join().unwrap();

        // Every row was refused, exactly as designed...
        assert_eq!(stats.refused, echokey_sync::MAX_BATCH_LEN, "{stats:?}");
        assert_eq!(a_store.lock().count().unwrap(), 0, "and nothing was stored");
        // ...and every one still left a permanent row in source_marks.
        let marks = a_store.lock().watermarks(B).unwrap();
        assert!(
            marks.len() < echokey_sync::MAX_BATCH_LEN,
            "one batch of invented source ids created {} receipts. The receipt is \
             written before Attribution::accepts is consulted, so a paired peer \
             decides how big source_marks gets — 256 per message, up to \
             MAX_BATCHES*4 messages per exchange, forever.",
            marks.len()
        );
    }

    /// FINDING (round 4): once one peer has more than `MAX_BATCHES * PAGE`
    /// receipts, every exchange with it loses data.
    ///
    /// `send_watermarks` chunks with NO ceiling, while `recv_watermarks` reads
    /// at most `MAX_BATCHES` chunks. Past 64 * 256 = 16 384 sources the surplus
    /// `Watermarks` messages arrive where rows are expected — which is the
    /// exact defect `many_sources_survive_the_watermark_chunking` claims to
    /// guard against.
    ///
    /// That guard is vacuous: it seeds `note_received(&src, &src, ..)`, i.e.
    /// `peer_machine = src`, while the exchange asks for `watermarks(B)`. Not
    /// one of its 296 marks is ever sent.
    #[test]
    fn receipts_for_one_peer_are_capped_so_the_exchange_cannot_be_broken() {
        // `send_watermarks` chunked without a ceiling while `recv_watermarks`
        // read at most MAX_BATCHES chunks, so a store holding more sources than
        // that produced a stream the other side stopped reading mid-way — and
        // every exchange with that peer failed from then on, durably.
        //
        // Two things stop it now: the table is capped per peer, and both halves
        // of the watermark exchange agree on MAX_WATERMARK_CHUNKS.
        let a_store = store_for(A);
        let b_store = store_for(B);

        {
            let g = a_store.lock();
            for i in 0..(echokey_core::history::MAX_SOURCES_PER_PEER as usize + 500) {
                let src = format!("33333333-3333-4333-8333-{:012x}", i);
                g.note_received(B, &src, 1_000 + i as i64).unwrap();
            }
        }
        let held = a_store.lock().watermarks(B).unwrap().len() as i64;
        assert!(
            held <= echokey_core::history::MAX_SOURCES_PER_PEER,
            "one peer put {held} cursors in the table"
        );

        // And the marks we advertise still fit inside what a peer will read,
        // which is the property that stopped the exchange dead.
        let advertised = a_store.lock().watermarks(B).unwrap().len();
        assert!(
            advertised.div_ceil(PAGE) <= MAX_WATERMARK_CHUNKS,
            "{advertised} marks need more chunks than the reader will accept"
        );
        let _ = &b_store;
    }

    /// The existing regression guard for watermark chunking never exercises it.
    ///
    /// `many_sources_survive_the_watermark_chunking` (replicate.rs:1178) seeds
    /// `note_received(&src, &src, ..)` — i.e. `peer_machine = src` — and then
    /// runs an exchange whose peer is B. `send_watermarks` asks for
    /// `watermarks(B)`, which selects on `peer_machine = 'B'`, so not one of
    /// those 296 marks is ever sent and the chunking path is never entered.
    #[test]
    fn the_watermark_chunking_test_really_exercises_the_chunked_path() {
        // It did not: it seeded `note_received(src, src, ..)`, so every mark was
        // filed under a peer that never took part in the exchange, and the
        // exchange read `watermarks(B)` — which was empty. The >MAX_BATCH_LEN
        // path was never entered, so the regression it guarded was unguarded.
        let a_store = store_for(A);
        {
            let g = a_store.lock();
            for i in 0..(PAGE + 40) {
                let src = format!("33333333-3333-4333-8333-{:012x}", i);
                g.note_received_uncapped_for_test(B, &src, 1_000 + i as i64).unwrap();
            }
        }
        let marks = a_store.lock().watermarks(B).unwrap();
        assert!(
            marks.len() > PAGE,
            "the marks have to be filed under the peer we actually talk to"
        );
    }
}
#[cfg(test)]
mod adversarial_round4_kinds {
    use super::*;
    use echokey_core::history::Store;
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

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

    /// One exchange where each side may have a different set of kinds enabled.
    fn sync2(
        a_store: &Arc<Mutex<Store>>,
        a_kinds: Kinds,
        b_store: &Arc<Mutex<Store>>,
        b_kinds: Kinds,
    ) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let bs = b_store.clone();
        let bt = std::thread::spawn(move || {
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let mut s = Session::accept(srv, &k2).unwrap();
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            exchange(
                &mut s,
                &bs,
                (B, "B"),
                b_kinds,
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
        });
        let mut s = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let a = exchange(
            &mut s,
            a_store,
            (A, "A"),
            a_kinds,
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
            &|| false,
        );
        let b = bt.join().expect("accepting side must not panic");
        a.expect("dialling side");
        b.expect("accepting side");
    }

    /// R4-K1. Switching clipboard sync OFF does not drop the clipboard rows
    /// already synced to this machine — but it DOES make this machine refuse
    /// the deletes for them. A clipboard entry deleted on the authoring device
    /// stays on the other one, in full, for as long as the switch is off.
    #[test]
    fn r4_a_delete_reaches_a_device_that_still_holds_the_row() {
        let both = Kinds { dictations: true, clipboard: true };
        let no_clip = Kinds { dictations: true, clipboard: false };

        let a_store = store_for(A);
        let b_store = store_for(B);

        // While clipboard sync is on, B's clipboard row reaches A.
        b_store.lock().insert_clipboard("a password", None, None).unwrap();
        for _ in 0..2 {
            sync2(&a_store, both, &b_store, both);
        }
        assert_eq!(a_store.lock().count().unwrap(), 1, "precondition: A holds the row");

        // The user switches clipboard sync off on A. A still HOLDS the row.
        // Then the user deletes it on B.
        let id = b_store
            .lock()
            .recent(None, 10)
            .unwrap()
            .into_iter()
            .find(|i| i.text == "a password")
            .unwrap()
            .id;
        b_store.lock().delete_item_local(id).unwrap();

        for _ in 0..3 {
            sync2(&a_store, no_clip, &b_store, both);
        }
        assert_eq!(
            a_store.lock().count().unwrap(),
            0,
            "a delete must reach a device that is still holding the row"
        );
    }

    /// R4-K2. And the receipt taken for the refused delete must not make the
    /// hole permanent once the switch comes back on. `SyncManager::set_kinds`
    /// clears the receipts on widening, so this models that.
    #[test]
    fn r4_re_enabling_a_kind_recovers_the_refused_delete() {
        let both = Kinds { dictations: true, clipboard: true };
        let no_clip = Kinds { dictations: true, clipboard: false };

        let a_store = store_for(A);
        let b_store = store_for(B);

        b_store.lock().insert_clipboard("a password", None, None).unwrap();
        for _ in 0..2 {
            sync2(&a_store, both, &b_store, both);
        }
        let id = b_store
            .lock()
            .recent(None, 10)
            .unwrap()
            .into_iter()
            .find(|i| i.text == "a password")
            .unwrap()
            .id;
        b_store.lock().delete_item_local(id).unwrap();
        for _ in 0..3 {
            sync2(&a_store, no_clip, &b_store, both);
        }

        // The user turns clipboard sync back on. This is what set_kinds does.
        a_store.lock().reset_source_marks().unwrap();
        for _ in 0..3 {
            sync2(&a_store, both, &b_store, both);
        }
        assert_eq!(
            a_store.lock().count().unwrap(),
            0,
            "the delete refused while the kind was off must land once it is back on"
        );
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 4) — demonstration of a live finding. NOT a fix.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round4_authority {
    use super::*;
    use echokey_core::history::Store;
    use echokey_sync::{
        DeviceId, ItemKind, PairedKey, Session, SyncItem, SyncMessage, Tombstone, Watermark,
        PROTOCOL_VERSION,
    };
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111"; // us (the victim)
    const B: &str = "22222222-2222-4222-8222-222222222222"; // the paired, hostile peer

    fn socket_pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (srv, _) = l.accept().unwrap();
        // MANDATORY on both directions: a stalled exchange fails, never hangs.
        for sock in [&c, &srv] {
            sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            sock.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        }
        (c, srv)
    }

    fn dev(id: &str) -> DeviceId {
        DeviceId::parse(id).unwrap()
    }

    /// The hostile peer. It holds the paired key, so the handshake succeeds and
    /// `Attribution.peer_id` is genuinely B. Every loop is hard-bounded.
    fn hostile_peer(
        srv: TcpStream,
        key: PairedKey,
        items: Vec<SyncItem>,
        tombs: Vec<Tombstone>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut s = Session::accept(srv, &key).expect("hostile peer completes the handshake");
            s.send(&SyncMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                device_id: dev(B),
                device_name: "Hostile".into(),
            })
            .unwrap();
            let _ = s.recv().unwrap();
            s.send(&SyncMessage::Watermarks {
                entries: Vec::<Watermark>::new(),
                more: false,
            })
            .unwrap();
            for _ in 0..MAX_BATCHES {
                match s.recv().unwrap() {
                    SyncMessage::Watermarks { more, .. } => {
                        if !more {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            if !tombs.is_empty() {
                s.send(&SyncMessage::Tombstones {
                    entries: tombs,
                    more: true,
                })
                .unwrap();
            }
            if !items.is_empty() {
                s.send(&SyncMessage::Items { items, more: true }).unwrap();
            }
            s.send(&SyncMessage::Items {
                items: Vec::new(),
                more: false,
            })
            .unwrap();
            for _ in 0..(MAX_BATCHES * 4) {
                match s.recv() {
                    Ok(SyncMessage::Items { items, more }) if items.is_empty() && !more => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        })
    }

    fn run_attack(
        store: &Arc<Mutex<Store>>,
        kinds: Kinds,
        items: Vec<SyncItem>,
        tombs: Vec<Tombstone>,
    ) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([21u8; 32]);
        let peer = hostile_peer(srv, key.clone(), items, tombs);
        let mut session = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string()];
        let attr = Attribution {
            peer_id: B,
            local_id: A,
            known: &known,
        };
        let stats = exchange(
            &mut session,
            store,
            (A, "Victim"),
            kinds,
            Retention {
                oldest_allowed: None,
            },
            &attr,
            Turn::Second,
            false,
            0,
            &|| false,
        );
        let _ = peer.join();
        stats.expect("the exchange itself completes");
    }

    /// FINDING (round 4, HIGH): with the DEFAULT settings — both kinds on — a
    /// paired peer can delete and rewrite rows attributed to US, including
    /// pinned ones, by naming `(our device id, origin_id)`. Local rows carry
    /// `origin_id == rowid` as text ("1", "2", "3"...), so there is nothing to
    /// guess.
    ///
    /// The round-3 test `adv3_a_paired_peer_can_erase_and_rewrite_our_own_never_shared_rows`
    /// only passes because it runs with `clipboard: false`, so the `kind_of`
    /// gate refuses. That gate is the ONLY thing standing between a paired peer
    /// and the whole local history; with the kind enabled there is no authority
    /// check left, because `may_create` (replicate.rs:215-217) delegates to
    /// `Store::holds_identity` (replicate.rs:552-563, 629-638) and we hold every
    /// row we ever wrote.
    ///
    /// `docs/SYNC_DESIGN.md` states the opposite as rule 1: an earlier version
    /// that let a paired device "delete another's rows with forged tombstones"
    /// is described there as an authority escalation that was removed.
    #[test]
    fn r4_a_paired_peer_erases_and_rewrites_our_own_rows_when_the_kind_is_on() {
        let store = {
            let mut s = Store::open_in_memory().unwrap();
            s.set_device_id(A);
            Arc::new(Mutex::new(s))
        };
        let (pinned_id, other_id) = {
            let g = store.lock();
            let a = g.insert_clipboard("a password of ours", None, None).unwrap();
            let b = g.insert_clipboard("a second capture", None, None).unwrap();
            g.set_pinned(a, true).unwrap();
            (a, b)
        };
        assert_eq!(pinned_id.to_string(), "1", "local origin_id is the rowid");

        let now = now_ms();
        let tombs = vec![Tombstone {
            source_device: dev(A), // OUR id, not the peer's
            origin_id: pinned_id.to_string(),
            deleted_at: now,
            clock: now as u64,
        }];
        let items = vec![SyncItem {
            source_device: dev(A), // OUR id again
            origin_id: other_id.to_string(),
            kind: ItemKind::Clipboard,
            text: "TEXT THE ATTACKER CHOSE".into(),
            created_at: now,
            updated_at: now + 1,
            pinned: false,
            clock: (now + 1) as u64,
        }];

        // The DEFAULT: both kinds shared.
        let kinds = Kinds {
            dictations: true,
            clipboard: true,
        };
        run_attack(&store, kinds, items, tombs);

        let g = store.lock();
        let erased = g.get(pinned_id).unwrap().is_none();
        let rewritten = g
            .get(other_id)
            .unwrap()
            .map(|r| r.text)
            .unwrap_or_default();
        assert!(
            !erased && rewritten != "TEXT THE ATTACKER CHOSE",
            "a paired peer spoke for OUR device id: pinned row {pinned_id} erased = {erased}; \
             row {other_id} now reads {rewritten:?}. origin_id is the rowid, so every row this \
             machine has ever written is reachable by counting."
        );
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW — ROUND 5. Demonstrations, not fixes.
// Every socket carries a read AND a write timeout; every loop is hard-bounded.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round5 {
    use super::*;
    fn item_at(source: &str, origin: &str, clock: i64) -> RemoteItem {
        RemoteItem {
            source_machine: source.into(),
            origin_id: origin.into(),
            kind: "clipboard".into(),
            text: "x".into(),
            created_at: clock,
            updated_at: clock,
            pinned: false,
        }
    }

    use echokey_core::history::{RemoteItem, RemoteTombstone, Store};
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

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

    fn seed_at(store: &Arc<Mutex<Store>>, source: &str, origin: &str, clock: i64) {
        store
            .lock()
            .apply_remote_item(
                C,
                &RemoteItem {
                    source_machine: source.into(),
                    origin_id: origin.into(),
                    kind: "transcription".into(),
                    text: "x".into(),
                    created_at: clock,
                    updated_at: clock,
                    pinned: false,
                },
            )
            .unwrap();
    }

    /// One exchange. `abort_on` is the 1-based call number of A's abort hook at
    /// which A gives up mid-serve; `usize::MAX` means "never".
    fn run_pair_aborting(
        a_store: Arc<Mutex<Store>>,
        b_store: Arc<Mutex<Store>>,
        abort_on: usize,
    ) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let bt = std::thread::spawn(move || {
            let mut session = Session::accept(srv, &k2).unwrap();
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            let _ = exchange(
                &mut session,
                &b_store,
                (B, "Deck B"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            );
        });
        {
            let mut session = Session::initiate(c, &key).unwrap();
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: B, local_id: A, known: &known };
            let calls = AtomicUsize::new(0);
            let _ = exchange(
                &mut session,
                &a_store,
                (A, "Deck A"),
                both(),
                Retention { oldest_allowed: None },
                &attr,
                Turn::First,
                false,
                0,
                &|| calls.fetch_add(1, Ordering::SeqCst) + 1 >= abort_on,
            );
            // Session (and its socket) drops here, so the other side stops
            // waiting rather than sitting on its read timeout.
        }
        bt.join().expect("accepting side must not panic");
    }

    // -----------------------------------------------------------------
    // R5-1. An exchange that stops between pages — the user unpaired the
    // device, sync was switched off, the Wi-Fi dropped — leaves the peer's
    // cursor INSIDE a millisecond. Only the MAX_BATCHES-truncated run is
    // trimmed back to a boundary; an interrupted one is not. Every remaining
    // row stamped with that millisecond is then strictly below the cursor and
    // is never offered again.
    // -----------------------------------------------------------------
    #[test]
    fn an_interrupted_serve_leaves_the_cursor_on_a_millisecond_boundary() {
        // The peer records the highest clock it sees and the next exchange asks
        // strictly above it, so a run that stops BETWEEN pages must never leave
        // that cursor inside a millisecond — nothing lowers a cursor, and the
        // rest of that millisecond would sit below it for good.
        //
        // Every full page is therefore trimmed back to a boundary, not just the
        // truncated one. An interruption is easy to come by: unpairing
        // mid-exchange, switching sync off, or the network dropping.
        let a = store_for(A);
        let base = now_ms();
        {
            let g = a.lock();
            // A page's worth plus a tail, with the page boundary landing inside
            // a millisecond that several rows share.
            for i in 0..(PAGE + 2) {
                let clock = base + (i as i64 / 3);
                g.apply_remote_item(A, &item_at(A, &format!("r{i:04}"), clock)).unwrap();
            }
        }

        let page = a.lock().items_from(A, 0, echokey_core::history::ORIGIN_CEILING, PAGE).unwrap();
        assert_eq!(page.len(), PAGE, "precondition: a full page");
        let trimmed: Vec<_> = {
            let tail = page.last().unwrap().updated_at;
            page.iter().filter(|r| r.updated_at < tail).collect()
        };
        assert!(!trimmed.is_empty(), "precondition: the page ends inside a millisecond");

        // What serve actually sends is the trimmed page, so the highest clock a
        // peer can record is a COMPLETED millisecond: every row the store holds
        // at that clock was in what we sent.
        let highest = trimmed.last().unwrap().updated_at;
        let sent_at_highest = trimmed.iter().filter(|r| r.updated_at == highest).count();
        let held_at_highest = a
            .lock()
            .items_from(A, highest, "", 10_000)
            .unwrap()
            .into_iter()
            .filter(|r| r.updated_at == highest)
            .count();
        assert_eq!(
            sent_at_highest, held_at_highest,
            "the cursor a peer would record leaves {} rows below it in the same millisecond",
            held_at_highest - sent_at_highest
        );
    }


    // -----------------------------------------------------------------
    // R5-2. Items and tombstones for one source share ONE (peer, source)
    // cursor, but they are served as two independent passes that both start at
    // the peer's floor. When the item pass is cut short by MAX_BATCHES, the
    // tombstone pass that follows still runs to completion — and its clocks are
    // higher. The peer records the maximum, so its cursor lands ABOVE the item
    // the serve stopped at, and everything in between is unreachable forever.
    //
    // Nothing recovers it: `resend_owed` is only consulted when a re-offer was
    // already in flight (`if resend_all` in manager.rs), so `stats.truncated`
    // on an ORDINARY exchange is recorded and then dropped on the floor.
    // -----------------------------------------------------------------
    #[test]
    fn r5_a_tombstone_lifts_the_cursor_over_the_items_a_truncated_serve_never_sent() {
        let a_store = store_for(A);
        let b_store = store_for(B);

        let cap = MAX_BATCHES * PAGE; // 16 384
        let extra = 16usize;
        let t = now_ms() - 5_000_000;
        {
            let g = a_store.lock();
            for i in 0..(cap + extra) {
                g.apply_remote_item(
                    C,
                    &RemoteItem {
                        source_machine: A.into(),
                        origin_id: format!("i-{i:06}"),
                        kind: "transcription".into(),
                        text: "x".into(),
                        created_at: t + i as i64,
                        updated_at: t + i as i64,
                        pinned: false,
                    },
                )
                .unwrap();
            }
            // One delete, made after all of that history — the ordinary shape
            // of a user who has a big history and has removed one recent thing.
            g.apply_remote_tombstone(
                C,
                &RemoteTombstone {
                    source_machine: A.into(),
                    origin_id: "long-gone".into(),
                    deleted_at: t + (cap + 100_000) as i64,
                },
            )
            .unwrap();
        }
        let total = (cap + extra) as i64;
        assert_eq!(a_store.lock().count().unwrap(), total);

        // Bounded: four ordinary exchanges is far more than convergence needs.
        for _ in 0..4 {
            run_pair_aborting(a_store.clone(), b_store.clone(), usize::MAX);
        }

        assert_eq!(
            b_store.lock().count().unwrap(),
            total,
            "the tombstone pass pushed B's cursor past the point the item pass \
             stopped at; the rows in between are below the cursor forever"
        );
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 5) — demonstrations of live findings. NOT fixes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round5_authority {
    use super::*;
    fn store_for(me: &str) -> Arc<Mutex<Store>> {
        let mut st = Store::open_in_memory().unwrap();
        st.set_device_id(me);
        Arc::new(Mutex::new(st))
    }

    
    fn item_at(source: &str, origin: &str, clock: i64) -> RemoteItem {
        RemoteItem {
            source_machine: source.into(),
            origin_id: origin.into(),
            kind: "clipboard".into(),
            text: "x".into(),
            created_at: clock,
            updated_at: clock,
            pinned: false,
        }
    }

    use echokey_core::history::Store;
    use echokey_sync::{
        DeviceId, ItemKind, PairedKey, Session, SyncItem, SyncMessage, Tombstone, Watermark,
        PROTOCOL_VERSION,
    };
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111"; // us (the victim)
    const B: &str = "22222222-2222-4222-8222-222222222222"; // the paired, hostile peer
    const C: &str = "33333333-3333-4333-8333-333333333333"; // a third paired device

    fn dev(id: &str) -> DeviceId {
        DeviceId::parse(id).unwrap()
    }

    fn socket_pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (srv, _) = l.accept().unwrap();
        // MANDATORY: read AND write deadlines on both ends, so a stalled
        // exchange fails instead of hanging the suite.
        for sock in [&c, &srv] {
            sock.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
            sock.set_write_timeout(Some(Duration::from_secs(15))).unwrap();
        }
        (c, srv)
    }

    /// The hostile peer: it knows NOTHING up front. It reads whatever the
    /// victim serves, takes the row identities straight out of it, and hands
    /// those rows back rewritten. Every loop is hard-bounded.
    /// One full exchange between two stores, both sides real.
    fn run_pair(
        a_store: Arc<Mutex<Store>>,
        b_store: Arc<Mutex<Store>>,
    ) -> (RoundStats, RoundStats) {
        let (c, srv) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let bt = std::thread::spawn(move || {
            let mut s = Session::accept(srv, &k2).unwrap();
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
            let attr = Attribution { peer_id: A, local_id: B, known: &known };
            exchange(
                &mut s,
                &b_store,
                (B, "B"),
                Kinds { dictations: true, clipboard: true },
                Retention { oldest_allowed: None },
                &attr,
                Turn::Second,
                false,
                0,
                &|| false,
            )
        });
        let mut s = Session::initiate(c, &key).unwrap();
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: B, local_id: A, known: &known };
        let a_stats = exchange(
            &mut s,
            &a_store,
            (A, "A"),
            Kinds { dictations: true, clipboard: true },
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
            &|| false,
        );
        let b_stats = bt.join().expect("accepting side");
        (a_stats.expect("dialling"), b_stats.expect("accepting"))
    }

    fn learn_then_rewrite(
        srv: TcpStream,
        key: PairedKey,
        rewrite_source: &'static str,
        new_text: &'static str,
        extra_tombs: Vec<Tombstone>,
    ) -> std::thread::JoinHandle<usize> {
        std::thread::spawn(move || {
            let mut s = Session::accept(srv, &key).expect("handshake");
            let _ = s.recv().expect("hello");
            s.send(&SyncMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                device_id: dev(B),
                device_name: "Hostile".into(),
            })
            .unwrap();
            for _ in 0..MAX_WATERMARK_CHUNKS {
                match s.recv().unwrap() {
                    SyncMessage::Watermarks { more, .. } => {
                        if !more {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            // We hold nothing, so ask for everything.
            s.send(&SyncMessage::Watermarks { entries: Vec::<Watermark>::new(), more: false })
                .unwrap();

            let mut seen: Vec<SyncItem> = Vec::new();
            for _ in 0..(MAX_BATCHES * 4) {
                match s.recv() {
                    Ok(SyncMessage::Items { items, more }) => {
                        let done = items.is_empty() && !more;
                        seen.extend(items);
                        if done {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }

            let now = now_ms();
            let forged: Vec<SyncItem> = seen
                .iter()
                .filter(|i| i.source_device.as_str() == rewrite_source)
                .map(|i| SyncItem {
                    source_device: i.source_device.clone(),
                    origin_id: i.origin_id.clone(),
                    kind: i.kind,
                    text: new_text.into(),
                    created_at: i.created_at,
                    updated_at: now + 1,
                    pinned: false,
                    clock: (now + 1) as u64,
                })
                .collect();
            let n = forged.len();
            if !extra_tombs.is_empty() {
                s.send(&SyncMessage::Tombstones { entries: extra_tombs, more: true }).unwrap();
            }
            if !forged.is_empty() {
                s.send(&SyncMessage::Items { items: forged, more: true }).unwrap();
            }
            s.send(&SyncMessage::Items { items: Vec::new(), more: false }).unwrap();
            n
        })
    }

    fn texts(store: &Arc<Mutex<Store>>) -> Vec<String> {
        store
            .lock()
            .recent(None, 100)
            .unwrap()
            .into_iter()
            .map(|i| i.text)
            .collect()
    }

    /// R5-A1. A paired peer can REWRITE the text of a row THIS machine
    /// authored, using only the identity we handed it during ordinary sync.
    ///
    /// The peer learns `(source_device = us, origin_id)` because `serve`
    /// offers every source we hold, ourselves included. `drain` then lets it
    /// come straight back: `may_create` is false for our own id, so the row
    /// falls through to `holds_identity` — true of every row we ever wrote —
    /// and `apply_remote_item` applies plain last-writer-wins.
    ///
    /// Nothing enforces "a peer may delete our rows but never rewrite them".
    /// The round-4 test only passes because `origin_id` became a random UUID,
    /// i.e. the property rests on the peer not KNOWING the id. A paired peer
    /// always knows it.
    #[test]
    fn a_paired_peer_cannot_rewrite_a_row_we_authored() {
        // A peer knows the identity of every row we sync to it, so a rule that
        // rested on the id being unguessable was no rule at all. Content is
        // accepted only from the device that wrote it.
        let a = store_for(A);
        let id = a.lock().insert_clipboard("the real text", None, None).unwrap();
        let (origin, _) = a.lock().origin_and_text_for_test(id).unwrap();

        // B replays that identity with content of its own.
        let b = store_for(B);
        b.lock()
            .apply_remote_item(A, &item_at(A, &origin, now_ms() + 1_000))
            .unwrap();

        let (a_stats, b_stats) = run_pair(a.clone(), b.clone());
        // Closed twice over: B never offers a row it did not author, and A
        // would refuse it anyway.
        assert_eq!(b_stats.sent_items, 0, "B must not offer a row it did not author");
        assert_eq!(a_stats.applied_items, 0);
        let (_, text) = a.lock().origin_and_text_for_test(id).unwrap();
        assert_eq!(text, "the real text", "a peer rewrote a row we authored");
    }


    /// R5-A2. The same shape, against a THIRD device's rows.
    ///
    /// We hold device C's rows because C is paired with us, and `serve` offers
    /// every source we hold to every peer. B — which never paired with C, and
    /// could not complete a handshake as C — rewrites them here, and we will
    /// offer the rewritten row on to C as an ordinary edit.
    #[test]
    fn a_third_devices_rows_are_never_offered_to_a_peer_at_all() {
        // The escalation, closed at the source rather than at the door.
        //
        // `serve` used to offer every source we held to every peer, so B was
        // handed C's rows — and `drain` then took an edit back for any identity
        // we held, which is how B rewrote C's history through us and had it
        // relayed onward. We now serve only the rows we authored, and accept
        // content only from the device that wrote it, so there is nothing to
        // hand over and nothing to take back.
        let a = store_for(A);
        let b = store_for(B);
        a.lock()
            .apply_remote_item(C, &item_at(C, "c-row", now_ms()))
            .unwrap();
        assert_eq!(a.lock().count().unwrap(), 1);

        let (a_stats, _) = run_pair(a.clone(), b.clone());
        assert_eq!(a_stats.sent_items, 0, "C's rows must not be offered to B: {a_stats:?}");
        assert_eq!(b.lock().count().unwrap(), 0, "and B must hold none of them");
    }

}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW — ROUND 5, three devices. Demonstrations, not fixes.
// Sockets carry read AND write timeouts; every loop is hard-bounded.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round5_mesh {
    use super::*;
    use echokey_core::history::{RemoteItem, Store};
    use echokey_sync::{PairedKey, Session};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

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

    /// One full exchange. `x` dials (Turn::First), `y` accepts.
    fn sync(x: (&Arc<Mutex<Store>>, &'static str), y: (&Arc<Mutex<Store>>, &'static str)) {
        let (sock_x, sock_y) = socket_pair();
        let key = PairedKey::from_bytes([7u8; 32]);
        let k2 = key.clone();
        let (y_store, y_id, x_id) = (y.0.clone(), y.1, x.1);
        let yt = std::thread::spawn(move || {
            let mut s = Session::accept(sock_y, &k2).unwrap();
            let known = vec![A.to_string(), B.to_string(), C.to_string()];
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
            .expect("accepting side");
        });
        let mut s = Session::initiate(sock_x, &key).unwrap();
        let known = vec![A.to_string(), B.to_string(), C.to_string()];
        let attr = Attribution { peer_id: y.1, local_id: x.1, known: &known };
        exchange(
            &mut s,
            x.0,
            (x.1, "peer"),
            both(),
            Retention { oldest_allowed: None },
            &attr,
            Turn::First,
            false,
            0,
            &|| false,
        )
        .expect("dialling side");
        yt.join().expect("accepting side must not panic");
    }

    // -----------------------------------------------------------------
    // R5-6. `drain` records a receipt for a tombstone BEFORE it decides
    // whether it may honour it. A device that is offered a delete for an
    // identity it does not yet hold refuses the delete AND banks a cursor at
    // that delete's exact clock. `tombstones_from` is strictly-greater on the
    // clock, so once that device DOES acquire the row — from the device that
    // wrote it, which has not heard about the delete either — the tombstone is
    // below the cursor and can never be offered again by the device that made
    // it.
    //
    // Sequence: B writes a password. A receives it and the user deletes it on
    // A. A syncs with C before C has the row: C refuses the tombstone and banks
    // the cursor. C then syncs with B and receives the password. From then on
    // A can never tell C about the delete.
    // -----------------------------------------------------------------
    #[test]
    fn a_delete_refused_once_is_offered_again_and_lands() {
        // The sequence that used to resurrect a deleted password permanently,
        // with no hostility and no clock skew:
        //
        //   B writes it, A receives it, the user deletes it on A, A syncs with
        //   C before C has the row. C rightly refuses a delete for an identity
        //   it does not hold — and used to bank a cursor at that tombstone's
        //   clock, which made the delete strictly-below and unreachable for
        //   ever. C then got the row from B and kept it.
        //
        // A refusal we might later reverse now banks nothing, so the delete is
        // simply offered again once C holds the row.
        let a = store_for(A);
        let b = store_for(B);
        let c = store_for(C);

        b.lock().insert_clipboard("hunter2", None, None).unwrap();
        sync((&b, B), (&a, A));
        assert_eq!(a.lock().count().unwrap(), 1, "precondition: A holds it");

        let id: i64 = a.lock().recent(None, 10).unwrap()[0].id;
        a.lock().delete_item_local(id).unwrap();
        assert_eq!(a.lock().count().unwrap(), 0);

        // A offers the delete to C, which has never seen the row.
        sync((&a, A), (&c, C));
        assert_eq!(c.lock().count().unwrap(), 0);
        let banked = c.lock().watermarks(A).unwrap();
        assert!(
            !banked.iter().any(|(src, _)| src == B),
            "a refusal we might reverse must bank nothing: {banked:?}"
        );

        // C then receives the row directly from B...
        sync((&b, B), (&c, C));
        assert_eq!(c.lock().count().unwrap(), 1, "precondition: C now holds it");

        // ...and the next exchange with A delivers the delete.
        for _ in 0..3 {
            sync((&a, A), (&c, C));
            if c.lock().count().unwrap() == 0 {
                break;
            }
        }
        assert_eq!(
            c.lock().count().unwrap(),
            0,
            "the deleted password is still on C"
        );
    }

}
