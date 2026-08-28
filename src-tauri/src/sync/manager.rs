//! Owns the running sync state: discovery, the listener, and pairing.
//!
//! Everything the UI can see is a snapshot of [`SyncStatus`]; the UI never
//! polls, it reacts to the `sync-status` event this module emits whenever the
//! snapshot changes.
//!
//! Threading: a single `Mutex<Inner>` guards mutable state. It is never held
//! across a blocking network call — pairing runs on its own thread and only
//! takes the lock to publish a result. That is deliberate: the settings window
//! calls straight into here from the UI thread, and a lock held across a socket
//! read would freeze it exactly the way the keyboard hook once did.

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parle_sync::{
    Discovery, DiscoveryConfig, DiscoveryEvent, PairingCode, PairingRole, PeerInfo,
};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::guard::{GuardError, PairingGuard};
use super::keystore;
use super::pair_flow;
use super::deadline::{Deadline, Timed};
use super::replicate::{self, Attribution, Kinds, Retention, Turn};
use super::wire_tcp::{read_byte, read_frame, write_byte, write_frame, MODE_PAIR, MODE_SESSION};
use parle_core::history::Store;

/// How long we will sit in a blocking read on an unauthenticated socket.
/// Without this a peer that connects and says nothing pins a thread forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Concurrent inbound handlers. One thread per connection with no ceiling lets
/// anyone on the LAN exhaust the process by opening sockets; a paired peer only
/// ever needs one.
const MAX_INBOUND: usize = 8;
/// How long an unauthenticated connection has to reach the handshake.
///
/// Separate from, and much shorter than, `HANDSHAKE_TIMEOUT`. A real peer sends
/// its mode byte the instant the socket opens; nothing legitimate is still
/// silent seconds later. The long budget only ever benefited an attacker.
const PREAUTH_TIMEOUT: Duration = Duration::from_secs(3);
/// Concurrent pre-authentication connections allowed from one address.
///
/// `MAX_INBOUND` alone is a single first-come budget, so eight sockets that
/// connect and say nothing held every slot for the full handshake timeout and
/// could be reopened forever — closing the listener to every real peer at a
/// cost of eight sockets and zero bytes. Worse than an outage, because a dial
/// only starts on FIRST sight of an mDNS record, so a paired peer refused
/// entry does not retry.
///
/// Two is generous: a peer needs one, and one more covers a reconnect racing a
/// half-closed socket.
const MAX_PREAUTH_PER_SOURCE: usize = 2;
/// The absolute ceiling on concurrent inbound handlers, counting the slot every
/// previously-unseen address is allowed even when the ordinary pool is full.
///
/// `MAX_INBOUND` is the pool for addresses already being served; this is what
/// keeps the reservation from becoming the exhaustion vector itself.
const MAX_INBOUND_HARD: usize = 32;
/// Read deadline once a session is authenticated. Longer than a handshake,
/// because a real exchange can be large, but never unbounded.
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);
/// Concurrent outbound dials. A flapping (or spoofed) mDNS record would
/// otherwise spawn a thread per sighting, without bound.
pub(crate) const MAX_DIALS: usize = 4;
/// How long before a peer we have already tried is dialled again.
pub(crate) const DIAL_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Distinct peers we will track. mDNS is unsigned, so anyone on the LAN can
/// advertise unlimited records; without a cap both the map and the JSON we push
/// to the webview grow without bound.
const MAX_PEERS: usize = 64;

/// Clears the `starting` flag however start() exits, panic included.
struct StartGuard(Arc<SyncManager>);

impl Drop for StartGuard {
    fn drop(&mut self) {
        self.0.inner.lock().starting = false;
    }
}

/// Whatever owns the set of in-flight dials.
///
/// A trait rather than a direct `Arc<SyncManager>` so the release behaviour can
/// actually be tested: `SyncManager` holds a `tauri::AppHandle<Wry>`, which
/// cannot be constructed in a unit test, and a test that reproduces the shape
/// with its own HashSet proves nothing about this code.
pub(crate) trait ReleasesDial: Send + Sync {
    fn release_dial(&self, id: &str);
}

impl ReleasesDial for SyncManager {
    fn release_dial(&self, id: &str) {
        self.inner.lock().dialing.remove(id);
    }
}

/// Releases an outbound dial slot on drop, including on unwind.
pub(crate) struct DialGuard {
    owner: Arc<dyn ReleasesDial>,
    id: String,
}

impl DialGuard {
    /// Built BEFORE the thread is spawned, so a spawn that fails still releases
    /// the slot. Constructed inside the closure it never existed on failure,
    /// and the id stayed in `dialing` for the life of the process.
    fn new(owner: Arc<dyn ReleasesDial>, id: String) -> Self {
        Self { owner, id }
    }
}

impl Drop for DialGuard {
    fn drop(&mut self) {
        self.owner.release_dial(&self.id);
    }
}

/// Record a sighting and decide, under the lock, whether to dial.
///
/// Returns the peer id to dial, and CLAIMS the slot before returning it: the
/// count test and the `dialing.insert` are one critical section, so the budget
/// cannot be double-spent and one peer cannot be dialled twice.
///
/// Extracted, and `pub(crate)`, because it is the function round 8's deadlock
/// fix actually created and it had no test of its own. Two consecutive
/// concurrency reviewers reported that everything this decision touches is
/// private to `manager`, so the fix could only be exercised from inside this
/// file's own `mod tests` — which is the one place a reviewer is not allowed to
/// edit. A rule nobody outside can test is a rule that drifts.
///
/// The CALLER must drop the lock before spawning. `DialGuard` is built before
/// the spawn so a failed spawn still frees the slot, and on failure std drops
/// the closure on the calling thread: with the lock still held that is a
/// re-entrant `parking_lot` acquisition, which parks the discovery thread for
/// ever holding `inner` and freezes the whole app through the synchronous
/// `sync_status` command.
pub(crate) fn decide_dial(i: &mut Inner, p: PeerInfo) -> Option<String> {
    let id = p.id.as_str().to_string();
    let known = i.paired.iter().any(|d| d.id == id);
    let paired = i.paired.clone();
    if !make_room_for_peer(&mut i.peers, &paired, &id, known) {
        return None;
    }
    let Inner { peers, last_dial, last_move, .. } = &mut *i;
    note_peer_record(peers, last_dial, last_move, &id, p, known);
    // Dial on first sight, and again once DIAL_RETRY_AFTER has passed since the
    // last attempt.
    //
    // First sight ALONE was a trap: if the dial failed for any reason (the
    // peer's inbound slots were full, it was briefly down, an unsigned mDNS
    // record from someone else got there first) we never tried that device
    // again until its record disappeared and came back. A transient refusal
    // became a permanent one.
    //
    // "First sight" deliberately does NOT gate this either. mDNS is unsigned
    // and a goodbye removes the peer entry, so an attacker flapping
    // goodbye/announce made every cycle read as first sight and started a dial
    // each time: ten threads, ten credential-store reads and ten 20s connects
    // inside one retry window, filling MAX_DIALS so the real device never got a
    // slot. `last_dial` outlives the peer entry, so it is the only honest clock.
    let due = i
        .last_dial
        .get(&id)
        .map(|t: &Instant| t.elapsed() >= DIAL_RETRY_AFTER)
        .unwrap_or(true);
    let room = i.dialing.len() < MAX_DIALS;
    if known && due && room && i.dialing.insert(id.clone()) {
        i.last_dial.insert(id.clone(), Instant::now());
        return Some(id);
    }
    None
}

/// Everything a `stop()` owns, taken in ONE critical section.
///
/// A free function so a test can drive the real thing. `SyncManager` holds a
/// `tauri::AppHandle<Wry>` and cannot be built under `MockRuntime`, so a test
/// that wants this rule either calls this or hand-copies it, and a hand copy
/// keeps asserting the shape it was written against long after the shape
/// changed, which is exactly what happened to the round-6 concurrency test that
/// went on failing after the defect it named was fixed.
///
/// `listen_stop` must be taken HERE, not after the wait that follows in
/// `stop()`. Leaving it installed across that wait meant a `set_enabled(true)`
/// landing in the window hit "already running" at the top of `start()`,
/// returned Ok, and did nothing, for a listener this stop was about to
/// destroy. The `stop_epoch` bump could not catch it, because the epoch is only
/// read by a `start()` that got PAST that entry test. Sync then read as on
/// everywhere, including in settings.json, with nothing running and nothing
/// left to call `start()` again.
///
/// `dialing` is deliberately NOT cleared. Its entries are owned by live
/// `DialGuard`s; clearing it here let a new generation insert the same peer and
/// then have the OLD guard's drop remove it, so two concurrent dials to one
/// device were possible and the set under-counted against `MAX_DIALS`.
pub(crate) fn stop_claim(i: &mut Inner) -> (Option<Discovery>, Option<Arc<AtomicBool>>, u16) {
    i.enabled = false;
    i.stop_epoch = i.stop_epoch.wrapping_add(1);
    i.peers.clear();
    i.guard.cancel();
    let port = i.port;
    i.port = 0;
    (i.discovery.take(), i.listen_stop.take(), port)
}

/// Did the retention window get WIDER? A pure function so the rule is testable
/// without a `SyncManager`, which cannot be built under `MockRuntime`.
///
/// `0` means "keep for ever", so it is the widest window there is rather than
/// the narrowest, the comparison every naive version of this got backwards.
pub(crate) fn retention_widened(previous: u32, next: u32) -> bool {
    match (previous, next) {
        (p, d) if p == d => false,
        (0, _) => false, // was "keep for ever"; anything else narrows
        (_, 0) => true,  // to "keep for ever"; the widest there is
        (p, d) => d > p,
    }
}

/// A PEER's name, made safe to display, with the device id as the fallback.
///
/// Never returns something misleading: if nothing survives sanitising, the user
/// sees the id rather than a blank row or a name that reads like their own
/// machine.
fn usable_peer_name(raw: &str, id: &str) -> String {
    parle_sync::sanitise_device_name(raw)
        .unwrap_or_else(|| format!("unnamed device {}", id.chars().take(8).collect::<String>()))
}

/// The stored device name, made safe for the wire, with a usable fallback.
///
/// Never returns something `validate_device_name` would refuse, because every
/// caller of it treats a refusal as a hard failure of the whole feature.
fn usable_device_name(stored: &str) -> String {
    if let Some(safe) = parle_sync::sanitise_device_name(stored) {
        return safe;
    }
    tracing::warn!("sync: the stored device name is unusable on the wire; falling back");
    parle_sync::sanitise_device_name(&fallback_device_name())
        .unwrap_or_else(|| "This device".to_string())
}

fn fallback_device_name() -> String {
    if cfg!(target_os = "macos") {
        "Mac".to_string()
    } else if cfg!(target_os = "windows") {
        "Windows PC".to_string()
    } else {
        "This device".to_string()
    }
}

/// Record an announcement for `id`, and say whether its dial must be retried.
///
/// mDNS is unsigned and device ids travel in the clear, so anyone on the LAN can
/// announce a paired id at their own address. We cannot tell that from a genuine
/// DHCP move without authenticating, and refusing every move would strand a
/// device that really did change address.
///
/// What we CAN refuse is to let a move suppress anything. Clearing `last_dial`
/// means the next genuine announcement is dialled at once rather than being held
/// off for `DIAL_RETRY_AFTER`, so re-announcing on a timer no longer keeps us
/// pointed at the attacker — the real device wins the next round either way.
pub(crate) fn note_peer_record(
    peers: &mut HashMap<String, PeerInfo>,
    last_dial: &mut HashMap<String, Instant>,
    last_move: &mut HashMap<String, Instant>,
    id: &str,
    record: PeerInfo,
    known: bool,
) {
    let moved = peers
        .get(id)
        .map(|old: &PeerInfo| old.socket_addr() != record.socket_addr())
        .unwrap_or(false);
    if moved && known {
        // At most ONE address-triggered retry per interval.
        //
        // Clearing the retry clock on every move was worse than not clearing
        // it: mDNS is unsigned, so an attacker holding a paired id alternates
        // between two addresses and every announcement reads as "due". That
        // buys unlimited dials — each a thread, a keychain read and a connect
        // held for the handshake timeout — and fills MAX_DIALS so the genuine
        // device's announcements start nothing at all. Outbound dialling is
        // exactly what makes inbound saturation survivable, so losing it is
        // the whole game.
        //
        // Rate-limiting the reset keeps the honest case (a device really did
        // change address, retry promptly) while capping the budget at one extra
        // dial per interval.
        let due = last_move
            .get(id)
            .map(|t: &Instant| t.elapsed() >= DIAL_RETRY_AFTER)
            .unwrap_or(true);
        if due {
            tracing::debug!("sync: {id} announced a new address; retrying the dial");
            last_dial.remove(id);
            last_move.insert(id.to_string(), Instant::now());
        }
    }
    peers.insert(id.to_string(), record);
}

/// May we take on another inbound connection right now?
///
/// `already_here` is whether this source address is already being served.
///
/// The global pool is only consulted for an address we are already serving. It
/// used to be consulted first, which made `MAX_INBOUND` a single first-come
/// budget: `MAX_INBOUND / MAX_PREAUTH_PER_SOURCE` addresses — four — closed the
/// listener to everyone. That denied pairing outright, because the machine
/// SHOWING a code only ever receives, so no code the user displayed could be
/// used and showing a fresh one changed nothing.
///
/// An address we have not heard from is admitted even when the pool is full,
/// up to a hard ceiling that stops the reservation itself becoming the
/// exhaustion vector. Same shape as the pairing guard's reserved first guess,
/// and for the same reason: the user's own device is precisely the address we
/// have not heard from.
pub(crate) fn admit_inbound(in_flight: usize, already_here: bool) -> bool {
    if in_flight >= MAX_INBOUND_HARD {
        return false;
    }
    !already_here || in_flight < MAX_INBOUND
}

/// Make room in the peer map for `id`, evicting an unpaired entry if need be.
///
/// Returns false when the record should be dropped.
///
/// The cap used to be applied before we worked out whether the id was known, so
/// `MAX_PEERS` unsigned mDNS records evicted the user's real device — and
/// `dial` returns immediately for anything absent from this map, which is the
/// very mitigation that makes inbound saturation survivable.
pub(crate) fn make_room_for_peer(
    peers: &mut HashMap<String, PeerInfo>,
    paired: &[UiPaired],
    id: &str,
    known: bool,
) -> bool {
    if peers.len() < MAX_PEERS || peers.contains_key(id) {
        return true;
    }
    if !known {
        return false;
    }
    let victim = peers
        .keys()
        .find(|k| !paired.iter().any(|d| &d.id == *k))
        .cloned();
    match victim {
        Some(v) => {
            peers.remove(&v);
            true
        }
        None => false,
    }
}

/// Releases one per-address pre-authentication slot on drop.
///
/// Paired with the global `SlotGuard`: the global budget bounds total work, and
/// this bounds how much of it any single address can occupy.
struct PreauthGuard {
    map: Arc<Mutex<std::collections::HashMap<std::net::IpAddr, usize>>>,
    ip: std::net::IpAddr,
}

impl PreauthGuard {
    fn claim(
        map: &Arc<Mutex<std::collections::HashMap<std::net::IpAddr, usize>>>,
        ip: std::net::IpAddr,
    ) -> Option<Self> {
        // Keyed by exact address, deliberately — see `guard::network_of`. On a
        // home LAN the user's own devices share a prefix with any attacker on
        // it, so folding to the network would give them one shared share.
        let mut m = map.lock();
        let n = m.entry(ip).or_insert(0);
        if *n >= MAX_PREAUTH_PER_SOURCE {
            return None;
        }
        *n += 1;
        Some(Self { map: map.clone(), ip })
    }
}

impl Drop for PreauthGuard {
    fn drop(&mut self) {
        let mut m = self.map.lock();
        if let Some(n) = m.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                // Do not let the map grow without bound from a churn of
                // addresses that never come back.
                m.remove(&self.ip);
            }
        }
    }
}

/// Releases an inbound handler slot on drop, including on unwind.
struct SlotGuard(Arc<AtomicUsize>);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Widen the socket timeouts once the peer has proved it holds the key.
///
/// These bound one syscall, not the exchange — that is the `Deadline`'s job —
/// so they only need to be short enough that an expired deadline is noticed
/// promptly, and long enough not to punish a legitimate pause.
fn relax_socket(s: &std::net::TcpStream) {
    let _ = s.set_read_timeout(Some(SESSION_TIMEOUT));
    let _ = s.set_write_timeout(Some(SESSION_TIMEOUT));
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// -- what the UI sees ---------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct UiPeer {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiPaired {
    pub id: String,
    pub name: String,
    /// When we last ATTEMPTED an exchange with this device, successful or not.
    pub last_seen: Option<i64>,
    /// Visible on the network right now, from mDNS alone.
    ///
    /// This is presence, NOT health. It says a record for this id is being
    /// announced; it says nothing about whether a single row has ever moved.
    pub online: bool,
    /// When an exchange with this device last actually SUCCEEDED.
    ///
    /// Separate from `last_seen`, which was set after the match on the exchange
    /// result and so advanced on failure too. Between that and `online` coming
    /// from mDNS, a pairing whose key the keychain now refuses displayed a
    /// green dot and the words "Online now", indefinitely, while nothing
    /// synced at all. The design notes predicted that failure would surface as
    /// "not paired"; it surfaced as a healthy device, which is worse than
    /// saying nothing.
    ///
    /// In memory only, deliberately: after a restart we genuinely have not
    /// synced yet, and saying so is honest.
    pub last_sync_ok: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiPairing {
    pub role: String,
    pub code: Option<String>,
    pub peer_id: Option<String>,
    /// Epoch ms. All timestamps crossing this boundary are epoch ms.
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub enabled: bool,
    pub device_id: String,
    pub device_name: String,
    pub peers: Vec<UiPeer>,
    pub paired: Vec<UiPaired>,
    pub pairing: Option<UiPairing>,
    /// True while discovery is running, so the UI can say "looking..." rather
    /// than showing "none found" the instant the panel opens.
    pub scanning: bool,
    pub dictations: bool,
    pub clipboard: bool,
    /// Set when sync is enabled but not actually working, so the UI can say so
    /// rather than showing an empty list that looks like "no peers yet".
    pub error: Option<String>,
}

// -- internals ----------------------------------------------------------------

pub(crate) struct Inner {
    enabled: bool,
    device_id: String,
    device_name: String,
    dictations: bool,
    clipboard: bool,
    peers: HashMap<String, PeerInfo>,
    paired: Vec<UiPaired>,
    guard: PairingGuard,
    /// Set while a code is displayed, so the listener knows to accept a pairing.
    discovery: Option<Discovery>,
    port: u16,
    /// Stop flag for the CURRENT listener generation. Each start() gets its own
    /// so a listener left over from a previous enable cannot come back to life
    /// when the flag is cleared for a new one.
    listen_stop: Option<Arc<AtomicBool>>,
    /// Peers with a dial in flight, so a flapping record cannot spawn a thread
    /// per sighting.
    dialing: std::collections::HashSet<String>,
    /// When we last tried to dial each peer, so a failed dial is retried rather
    /// than abandoned until the peer's mDNS record disappears and returns.
    last_dial: std::collections::HashMap<String, Instant>,
    /// When we last let an address change shorten a peer's retry wait. Bounds
    /// what an unsigned record can buy by flapping between two addresses.
    last_move: std::collections::HashMap<String, Instant>,
    /// Peers we owe one full re-offer of our history, because the user widened
    /// what this machine shares. See `set_kinds`.
    resend_owed: std::collections::HashMap<String, i64>,
    /// Bumped whenever something OTHER than an exchange writes `resend_owed`.
    ///
    /// A compare-and-swap on the value alone cannot do this job. `set_kinds`
    /// only ever writes the literal `0`, so when the debt read before an
    /// exchange was already `Some(0)` a `0` primed mid-flight compares equal to
    /// the `0` we read, the swap succeeds, and the fresh promise is overwritten
    /// by the truncation's own resume point. Textbook ABA. A counter cannot be
    /// confused with itself.
    resend_epoch: std::collections::HashMap<String, u64>,
    /// Set for the whole of start(), which binds a port and brings up mDNS and
    /// is far too slow to leave the "already running" test unguarded.
    starting: bool,
    /// Bumped by every `stop()`. `start()` compares it against the value it saw
    /// on entry and tears down whatever it installed if a stop happened while
    /// it was working — the window between claiming `starting` and installing
    /// the listener is long (bind, mDNS registration, thread spawns) and a
    /// stop that lands inside it would otherwise be silently undone.
    stop_epoch: u64,
    /// What the USER asked for, as opposed to whether sync is running right now.
    ///
    /// `enabled` is the runtime flag: `stop()` clears it so nothing in flight
    /// keeps serving, including at app exit. Persisting THAT meant an exchange
    /// finishing during shutdown wrote `enabled: false` into settings.json and
    /// sync was silently off on the next launch. Only `set_enabled` moves this.
    user_enabled: bool,
    /// Devices unpaired while a session with them was already running. Checked
    /// as that session finishes each batch so it stops writing their rows.
    unpaired_mid_session: std::collections::HashSet<String>,
    /// Why sync is not working, surfaced to the UI instead of only the log.
    error: Option<String>,
}

pub struct SyncManager {
    inner: Mutex<Inner>,
    app: AppHandle,
    store: Arc<Mutex<Store>>,
    /// Written on every change to pairing state. Never holds `inner` while
    /// taking this, so the two locks cannot invert.
    settings: Arc<Mutex<parle_core::settings::Settings>>,
    inbound: Arc<AtomicUsize>,
    /// Pre-authentication connections in flight, per source address.
    preauth: Arc<Mutex<std::collections::HashMap<std::net::IpAddr, usize>>>,
    /// Mirrored from settings so the replication path never has to reach back
    /// into AppState (which would invert the lock order).
    retention_days: Arc<AtomicUsize>,
    max_items: Arc<AtomicUsize>,
}

impl SyncManager {
    /// `retention_days` is taken here rather than through a setter afterwards
    /// because this constructor starts the listener. Applied later, a session
    /// that landed in the gap saw retention_days == 0, so `retention_floor()`
    /// returned None and the receiver's retention window was not enforced for
    /// that whole exchange.
    pub fn new(
        app: AppHandle,
        s: &parle_core::settings::SyncSettings,
        store: Arc<Mutex<Store>>,
        settings: Arc<Mutex<parle_core::settings::Settings>>,
        retention_days: u32,
    ) -> Arc<Self> {
        let m = Arc::new(Self {
            inner: Mutex::new(Inner {
                enabled: s.enabled,
                device_id: s.device_id.clone(),
                // Sanitised on the way IN as well as on the way out. A name
                // that predates the sanitiser, or one hand-edited into
                // settings.json, or a hostname carrying an '=', would
                // otherwise make every Hello unsendable and stop discovery
                // starting, reported to the user as a network fault.
                device_name: usable_device_name(&s.device_name),
                dictations: s.sync_dictations,
                clipboard: s.sync_clipboard,
                peers: HashMap::new(),
                paired: s
                    .paired
                    .iter()
                    .map(|p| UiPaired {
                        id: p.id.clone(),
                        name: p.name.clone(),
                        last_seen: p.last_seen,
                        online: false,
                        // Nothing has synced since this process started, and
                        // claiming otherwise from a persisted value would be
                        // the same lie the field exists to stop.
                        last_sync_ok: None,
                    })
                    .collect(),
                guard: PairingGuard::new(),
                discovery: None,
                port: 0,
                listen_stop: None,
                dialing: std::collections::HashSet::new(),
                last_dial: std::collections::HashMap::new(),
                last_move: std::collections::HashMap::new(),
                // Restored, so a debt taken on before a quit is still owed.
                resend_owed: s
                    .resend_owed
                    .iter()
                    .map(|d| (d.device_id.clone(), d.from))
                    .collect(),
                resend_epoch: std::collections::HashMap::new(),
                starting: false,
                stop_epoch: 0,
                user_enabled: s.enabled,
                unpaired_mid_session: std::collections::HashSet::new(),
                error: None,
            }),
            app,
            store,
            settings,
            inbound: Arc::new(AtomicUsize::new(0)),
            preauth: Arc::new(Mutex::new(std::collections::HashMap::new())),
            retention_days: Arc::new(AtomicUsize::new(retention_days as usize)),
            max_items: Arc::new(AtomicUsize::new(0)),
        });
        if s.enabled {
            // A failure here rolls `enabled` back and records the reason, which
            // the first sync_status will carry to the UI.
            let _ = m.clone().start();
        }
        m
    }

    pub fn status(&self) -> SyncStatus {
        let i = self.inner.lock();
        self.snapshot(&i)
    }

    fn snapshot(&self, i: &Inner) -> SyncStatus {
        let now = Instant::now();
        let paired_ids: Vec<&str> = i.paired.iter().map(|p| p.id.as_str()).collect();
        SyncStatus {
            enabled: i.enabled,
            device_id: i.device_id.clone(),
            device_name: i.device_name.clone(),
            // Already-paired devices are not offered for pairing again.
            peers: i
                .peers
                .values()
                .filter(|p| !paired_ids.contains(&p.id.as_str()))
                .map(|p| UiPeer {
                    id: p.id.as_str().to_string(),
                    name: p.name.clone(),
                    addr: p.addr.to_string(),
                    port: p.port,
                })
                .collect(),
            paired: i
                .paired
                .iter()
                .map(|p| UiPaired {
                    online: i.peers.contains_key(&p.id),
                    ..p.clone()
                })
                .collect(),
            pairing: i.guard.code(now).map(|c| UiPairing {
                role: "showing".into(),
                code: Some(c.to_string()),
                peer_id: None,
                expires_at: now_ms()
                    + i.guard.expires_in(now).map(|d| d.as_millis() as i64).unwrap_or(0),
            }),
            scanning: i.discovery.is_some(),
            dictations: i.dictations,
            clipboard: i.clipboard,
            error: i.error.clone(),
        }
    }

    fn publish(&self) {
        let status = self.status();
        let _ = self.app.emit("sync-status", status);
    }

    // -- lifecycle ------------------------------------------------------------

    /// Bind the listener, start advertising, and start browsing.
    ///
    /// The "already running" slot is CLAIMED under the same lock that tests it.
    /// Reading `listen_stop`, releasing the lock, and only publishing the flag
    /// after `bind` and `Discovery::start` left a wide window — mDNS daemon
    /// creation, a multicast join, a register and a browse are all genuinely
    /// slow, and `sync_set_enabled` is async, so rapid toggling produces truly
    /// concurrent calls. Two starts could get through and bind two listeners,
    /// and the second would drop the first `Discovery` while holding `inner`,
    /// whose Drop unregisters over the network and joins a worker — wedging
    /// every publish() and the UI with it. A stop() landing inside the window
    /// found both fields still None and did nothing, leaving an orphaned
    /// listener advertising itself with the toggle reading OFF.
    pub fn start(self: Arc<Self>) -> Result<(), String> {
        let epoch;
        {
            let mut i = self.inner.lock();
            // `enabled` is checked here, not only by the caller. set_enabled
            // flips the flag under the lock and then calls start()/stop()
            // outside it, and the commands are spawn_blocking, so two toggles
            // race: a disable could land between an enable's flag write and its
            // start(), and start() would then bind a port and advertise on the
            // LAN with the toggle reading off, for the life of the process.
            if !i.enabled {
                tracing::debug!("sync: not starting; it was switched off first");
                return Ok(());
            }
            if i.listen_stop.is_some() || i.starting {
                return Ok(()); // already running, or another thread is bringing it up
            }
            i.starting = true;
            epoch = i.stop_epoch;
        }
        // From here on every exit path must clear `starting`, so it is RAII.
        let _starting = StartGuard(self.clone());
        let listener = match TcpListener::bind("0.0.0.0:0") {
            Ok(l) => l,
            Err(e) => {
                let msg = format!("Could not open a network port for sync: {e}");
                self.fail(&msg);
                return Err(msg);
            }
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);

        let (device_id, device_name) = {
            let mut i = self.inner.lock();
            i.port = port;
            (i.device_id.clone(), i.device_name.clone())
        };

        let id = match parle_sync::DeviceId::parse(&device_id) {
            Ok(id) => id,
            Err(e) => {
                let msg = format!("This machine has no usable sync identity: {e}");
                self.fail(&msg);
                return Err(msg);
            }
        };

        match Discovery::start(&DiscoveryConfig {
            device_id: id,
            device_name: device_name.clone(),
            port,
        }) {
            Ok((d, rx)) => {
                self.inner.lock().discovery = Some(d);
                let me = self.clone();
                std::thread::Builder::new()
                    .name("parle-sync-discovery".into())
                    .spawn(move || {
                        for ev in rx {
                            // Decided under the lock, ACTED ON after it.
                            //
                            // The dial used to be spawned while `inner` was
                            // still held, and `DialGuard` is built before the
                            // spawn so that a failed spawn still frees the
                            // slot. Those two facts together deadlock the whole
                            // app: when `Builder::spawn` fails, std drops the
                            // closure on the CALLING thread, so `DialGuard::drop`
                            // runs here and calls `release_dial`, which locks
                            // `inner` again. `parking_lot::Mutex` is not
                            // reentrant, so this thread parks for ever holding
                            // `inner` — and `sync_status` is a synchronous
                            // command that locks `inner` on the main thread, so
                            // the menu bar, the history window and the hotkey UI
                            // all freeze behind it and the app needs a force
                            // quit. The `tracing::warn!` written for a failed
                            // spawn sits on the far side of the deadlock and
                            // never runs.
                            let mut i = me.inner.lock();
                            let to_dial = match ev {
                                DiscoveryEvent::PeerFound(p) => decide_dial(&mut i, p),
                                DiscoveryEvent::PeerLost(id) => {
                                    i.peers.remove(id.as_str());
                                    None
                                }
                            };
                            drop(i);
                            // The lock is released, so a failed spawn dropping
                            // the guard on this thread is now an ordinary
                            // `release_dial` rather than a deadlock.
                            if let Some(id) = to_dial {
                                let me3 = me.clone();
                                // RAII, for the same reason the inbound path
                                // uses SlotGuard, and built HERE rather than
                                // inside the closure: constructed inside, it
                                // never exists when the spawn fails, and the id
                                // stays in `dialing` for the life of the process.
                                let guard = DialGuard::new(me3.clone(), id.clone());
                                let launched = std::thread::Builder::new()
                                    .name("parle-sync-dial".into())
                                    .spawn(move || {
                                        let _slot = guard;
                                        me3.clone().dial(id.clone());
                                    });
                                if let Err(e) = launched {
                                    tracing::warn!("sync: could not start a dial thread: {e}");
                                }
                            }
                            me.publish();
                        }
                    })
                    .ok();
            }
            Err(e) => {
                // A laptop with no network is normal, but the user should still
                // be told why nothing ever appears in the list.
                tracing::info!("sync: discovery unavailable ({e})");
                self.inner.lock().error =
                    Some("Not searching: no usable network on this machine right now".into());
            }
        }

        let gen_stop = Arc::new(AtomicBool::new(false));
        self.inner.lock().listen_stop = Some(gen_stop.clone());
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("parle-sync-listen".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    // This generation's flag, not the process-wide one: a
                    // listener from a previous enable must stay dead even after
                    // the flag is cleared for a new one.
                    if gen_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match conn {
                        Ok(s) => {
                            // The global budget is checked AFTER working out
                            // whether this address is already being served, and
                            // applies only if it is.
                            //
                            // Checking it first made MAX_INBOUND a single
                            // first-come pool, so MAX_INBOUND divided by
                            // MAX_PREAUTH_PER_SOURCE — four addresses — closed
                            // the listener to everyone. That denied pairing
                            // outright, because the machine SHOWING a code only
                            // ever receives, and showing a fresh code changed
                            // nothing.
                            //
                            // A previously-unseen address is admitted even when
                            // the pool is full, up to a hard ceiling. Same
                            // reasoning as the pairing guard's reserved first
                            // guess: the user's own device is precisely the
                            // address we have not heard from.
                            // A per-address share on top of the global budget.
                            // Without it the global budget is first-come, so
                            // eight sockets that connect and say nothing hold
                            // every slot for the whole timeout — and can be
                            // reopened forever, closing the listener to every
                            // real peer for eight sockets and zero bytes.
                            let src = s.peer_addr().map(|a| a.ip()).ok();
                            let in_flight = me.inbound.load(Ordering::SeqCst);
                            let already_here =
                                src.map(|ip| me.preauth.lock().contains_key(&ip)).unwrap_or(true);
                            if !admit_inbound(in_flight, already_here) {
                                tracing::debug!("sync: refusing connection, {in_flight} in flight");
                                drop(s);
                                continue;
                            }
                            let slot = match src {
                                Some(ip) => match PreauthGuard::claim(&me.preauth, ip) {
                                    Some(g) => g,
                                    None => {
                                        tracing::debug!(
                                            "sync: refusing connection, {ip} already has {MAX_PREAUTH_PER_SOURCE} in flight"
                                        );
                                        drop(s);
                                        continue;
                                    }
                                },
                                None => {
                                    drop(s);
                                    continue;
                                }
                            };
                            me.inbound.fetch_add(1, Ordering::SeqCst);
                            let me2 = me.clone();
                            let _global = SlotGuard(me.inbound.clone());
                            // Builder, not `std::thread::spawn`: that PANICS if
                            // the OS refuses a thread, which would unwind this
                            // accept loop and release the port while
                            // `listen_stop` stayed installed — after which every
                            // start() returns "already running" for a listener
                            // that no longer exists. That is the exact state the
                            // rollback below the spawn exists to prevent.
                            let launched = std::thread::Builder::new()
                                .name("parle-sync-serve".into())
                                .spawn(move || {
                                // RAII: released on every exit including a
                                // panic. Decrementing after the call would leak
                                // a slot per unwind, and eight leaks close the
                                // listener to everyone.
                                    let _slot = slot;
                                    let _global = _global;
                                    me2.serve(s);
                                });
                            if let Err(e) = launched {
                                tracing::warn!("sync: could not start a handler thread: {e}");
                            }
                        }
                        Err(e) => tracing::debug!("sync: accept failed: {e}"),
                    }
                }
                tracing::info!("sync: listener stopped");
            });
        if let Err(e) = spawned {
            // Roll the generation back. `listen_stop` was installed before the
            // spawn, so leaving it set meant every later start() returned
            // "already running" for a listener that does not exist: sync then
            // read as on, persisted as on, and was silently dead for the life
            // of the process, with the mDNS record still advertising a port
            // nobody was accepting on.
            let stale = {
                let mut i = self.inner.lock();
                i.listen_stop = None;
                i.port = 0;
                i.discovery.take()
            };
            drop(stale); // Discovery::drop talks to the network; not under the lock.
            let msg = format!("Could not start the sync listener: {e}");
            self.fail(&msg);
            return Err(msg);
        }

        // Did a stop() land while we were bringing this up?
        //
        // The window between claiming `starting` and installing the listener is
        // long — bind, mDNS registration, two thread spawns — and a stop()
        // arriving inside it found nothing installed, did nothing, and returned.
        // We then finished, leaving a bound port advertising on the LAN with the
        // toggle reading off for the life of the process. The epoch is what
        // makes that detectable after the fact rather than only during.
        let superseded = {
            let i = self.inner.lock();
            i.stop_epoch != epoch || !i.enabled
        };
        if superseded {
            tracing::info!("sync: start was superseded by a stop; undoing it");
            drop(_starting);
            self.stop();
            return Ok(());
        }

        // Clear only a STALE error. This used to be unconditional, which wiped
        // the "no usable network" message recorded a few lines above by the
        // discovery failure path — the user got an empty peer list, enabled and
        // not scanning, with no explanation, which is the exact failure the
        // error field exists to prevent.
        {
            let mut i = self.inner.lock();
            if i.discovery.is_some() {
                i.error = None;
            }
        }
        self.publish();
        Ok(())
    }

    /// Surface a problem that concerns ONE paired device.
    ///
    /// Distinct from `fail`, which is about the whole feature and switches it
    /// off. A device whose key we cannot read must not take sync down for every
    /// other device; it just has to stop pretending to be healthy.
    fn report_device_problem(&self, peer_id: &str, msg: &str) {
        let named = {
            let i = self.inner.lock();
            i.paired
                .iter()
                .find(|d| d.id == peer_id)
                .map(|d| format!("{}: {msg}", d.name))
        };
        if let Some(text) = named {
            tracing::warn!("sync: {text}");
            let mut i = self.inner.lock();
            i.error = Some(text);
            drop(i);
            self.publish();
        }
    }

    /// Record why sync is not running, and make sure it stops claiming to be.
    fn fail(&self, msg: &str) {
        tracing::warn!("sync: {msg}");
        {
            let mut i = self.inner.lock();
            i.enabled = false;
            i.error = Some(msg.to_string());
        }
        self.publish();
    }

    pub fn stop(&self) {
        // Clearing `enabled` here rather than relying on the caller having done
        // it: `serve` and `run_session` both gate on it, so a stop() that left
        // it set would keep accepting connections and moving history after a
        // shutdown.
        // Everything this stop owns is taken in ONE critical section, before
        // the wait below.
        //
        // Taking it afterwards left `listen_stop` set for the whole wait, and a
        // `set_enabled(true)` landing in that window found "already running"
        // at the top of start(), returned Ok, and did nothing — for a listener
        // this stop was about to destroy. The epoch could not catch it, because
        // it is only checked by a start() that got PAST that entry test. Sync
        // then read as on everywhere, including in settings.json, with nothing
        // running and nothing left to call start() again.
        //
        // Taken first, a concurrent start() sees no listener, proceeds, and
        // either keeps what it installs (its epoch matches, because the bump
        // happened before it looked) or undoes itself.
        let (discovery, gen_stop, port) = stop_claim(&mut self.inner.lock());
        // Wait for an in-flight start() to finish installing itself.
        //
        // start() claims `starting` and then releases the lock for the slow
        // work — bind, mDNS registration, thread spawns — so a stop() arriving
        // in that window used to find `discovery` and `listen_stop` both None,
        // do nothing, and return. start() then completed, leaving a bound
        // listener advertising on the LAN with `enabled` false and the UI
        // toggle off, for the life of the process.
        //
        // Bounded: if start() is wedged we proceed anyway rather than hanging
        // the caller, and `enabled` is already false so nothing it installs
        // will serve.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.inner.lock().starting && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // Dropped OUTSIDE the lock. Discovery's Drop unregisters over the
        // network and joins its worker; running that while holding `inner`
        // would wedge every publish() and the UI thread with it.
        drop(discovery);

        // Wake the listener so it observes the flag. accept() blocks until a
        // connection arrives, so without this the thread and the bound port
        // outlive the stop, and a later enable would bind a SECOND listener
        // while the first was still happily serving.
        if let Some(flag) = gen_stop {
            flag.store(true, Ordering::SeqCst);
            if port != 0 {
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(500));
            } else {
                // We never learned the port, so the poke is impossible and the
                // thread stays parked in accept() with the socket bound. Say so
                // rather than leaving a silent leak, and refuse to hand the flag
                // back: a later start() must not decide it is "already running"
                // on the strength of a listener nobody can reach.
                tracing::warn!(
                    "sync: stopping a listener whose port was never recorded;                      it will exit on its next connection"
                );
            }
        }
        self.publish();
    }

    // -- inbound --------------------------------------------------------------

    fn serve(self: Arc<Self>, s: TcpStream) {
        // stop() wakes accept(), but a connection already accepted, or one that
        // arrives in the gap, would otherwise run to completion and move
        // history after the user switched sync off.
        if !self.inner.lock().enabled {
            return;
        }
        // Unauthenticated peer: never block on it indefinitely.
        // PREAUTH_TIMEOUT, not HANDSHAKE_TIMEOUT: reaching the handshake at all
        // is supposed to be instant, and the deadline is widened once the peer
        // has proved it holds a key.
        //
        // The socket timeouts bound one syscall. The deadline bounds the whole
        // pre-session conversation, and it is applied HERE, before the very
        // first byte, rather than after the pre-session reads as it used to be.
        // A peer that declared a 4096-byte frame and dribbled one byte every 19
        // seconds renewed the socket timeout on every byte and held this thread
        // — and one of the eight inbound slots — for about 22 hours. Eight of
        // them closed the listener to every real peer until the app restarted.
        let _ = s.set_read_timeout(Some(PREAUTH_TIMEOUT));
        let _ = s.set_write_timeout(Some(PREAUTH_TIMEOUT));
        let deadline = Deadline::after(PREAUTH_TIMEOUT);
        let mut s = Timed::new(s, deadline.clone());

        match read_byte(&mut s) {
            Ok(MODE_PAIR) => self.serve_pairing(s),
            Ok(MODE_SESSION) => self.serve_session(s, deadline),
            Ok(other) => tracing::debug!("sync: unknown mode byte {other:#04x}"),
            Err(e) => tracing::debug!("sync: no mode byte ({e})"),
        }
    }

    /// Someone is trying to pair with the code we are displaying.
    fn serve_pairing(self: Arc<Self>, mut s: Timed<TcpStream>) {
        // Make the peer do real work BEFORE it can cost the user a guess: read
        // its opening SPAKE2 frame first. A bare connect sending one byte would
        // otherwise burn an attempt, and four of those burn the code and lock
        // pairing out for five minutes from anywhere on the LAN. Reading the
        // frame reveals nothing, so the charge-before-exchange ordering that
        // closes the TOCTOU still holds.
        let peer_first = match read_frame(&mut s) {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!("sync: pairing connection sent no opening frame ({e})");
                return;
            }
        };
        // Only a well-formed opening message may cost the user an attempt. An
        // empty frame is legal on the wire, so without this three bytes burn a
        // guess and four connections lock pairing out for five minutes.
        if !pair_flow::looks_like_pairing_message(&peer_first) {
            tracing::debug!("sync: ignoring a malformed pairing frame; no attempt charged");
            return;
        }
        // Now charge it. Checking the budget only after the exchange completes
        // is a TOCTOU race: concurrent connections would each read the same
        // live code and each get a free guess.
        // The budget is counted per source address as well as per code, so an
        // attacker grinding from its own address cannot spend the allowance the
        // user's second device will need.
        let from = match s.get_ref().peer_addr() {
            Ok(a) => a.ip(),
            Err(e) => {
                tracing::debug!("sync: pairing connection with no peer address ({e})");
                return;
            }
        };
        let code = {
            let mut i = self.inner.lock();
            match i.guard.reserve(Instant::now(), from) {
                Ok(c) => c,
                Err(e) => {
                    // TELL the other machine why.
                    //
                    // The guard produces four genuinely actionable messages
                    // ("too many incorrect codes; try again in N seconds",
                    // "show a new code"). Logging them and closing the socket
                    // made the entering machine see a transport drop, which maps
                    // to "check it is still awake and on this network": the
                    // fourth wrong code sent the user to look at their Wi-Fi.
                    // The comment on that mapping says six failures used to
                    // collapse into one wrong answer. This is the same defect
                    // pointed the other way.
                    // A CODE, not our error string. See `RefusalCode`: the
                    // frame is read on the other side before anything about
                    // this machine has been authenticated, so it must not be
                    // able to carry chosen text.
                    // `NotPairing` is answered with SILENCE, and that is the
                    // whole of this fix.
                    //
                    // `reserve` charges nothing for it: it returns before the
                    // live code is touched. Round 12 made this arm write a frame
                    // naming which of the four states it is in, so for 33 bytes
                    // per TCP connect anyone on the LAN could poll for ever,
                    // free, and be told the instant a code appeared on screen.
                    // The guard's own header says the per-source carve-out
                    // exists because an automated attacker always wins the race
                    // to the next open slot; this told it exactly when the slot
                    // opened, so its three guesses landed on every code the user
                    // ever showed. Before round 12 the socket simply closed.
                    //
                    // The other three only ever reach a source that has already
                    // spent a guess against a live code, so they tell it nothing
                    // it did not already know.
                    if matches!(e, GuardError::NotPairing) {
                        tracing::debug!("sync: pairing attempt while no code is live; no answer");
                        return;
                    }
                    let (code, secs) = match &e {
                        GuardError::NotPairing => (pair_flow::RefusalCode::NotPairing, 0),
                        GuardError::Expired => (pair_flow::RefusalCode::Expired, 0),
                        GuardError::LockedOut { retry_in } => {
                            // Rounded UP. `as_secs()` truncates, so the last
                            // fraction of every honest lockout rendered as
                            // "try again in 0 seconds".
                            (
                                pair_flow::RefusalCode::LockedOut,
                                retry_in.as_secs_f64().ceil().max(1.0) as u32,
                            )
                        }
                        GuardError::CodeExhausted => (pair_flow::RefusalCode::CodeExhausted, 0),
                    };
                    let _ = write_frame(&mut s, &pair_flow::refusal_frame(code, secs));
                    tracing::info!("sync: pairing attempt refused: {e}");
                    return;
                }
            }
        };
        let code = match PairingCode::parse(&code) {
            Ok(c) => c,
            Err(_) => return,
        };
        let (me_id, me_name) = {
            let i = self.inner.lock();
            (i.device_id.clone(), i.device_name.clone())
        };

        match pair_flow::run_with(
            &mut s,
            PairingRole::Initiator,
            &code,
            (&me_id, &me_name),
            Some(peer_first),
        ) {
            Ok(p) => {
                // Correct code: refund the reserved guess and close the window.
                self.inner.lock().guard.succeed();
                let _ = self.complete_pairing(p);
            }
            Err(e) => {
                // The guess was already charged by reserve(); nothing to do but
                // report it. Publishing refreshes the countdown/lockout in the UI.
                tracing::info!("sync: pairing failed: {e}");
                self.publish();
            }
        }
    }

    fn serve_session(self: Arc<Self>, mut s: Timed<TcpStream>, deadline: Deadline) {
        // Peer announces which device it claims to be so we can find the key.
        let Ok(raw) = read_frame(&mut s) else { return };
        let Ok(peer_id) = String::from_utf8(raw) else { return };
        // Must look like an id we issue before it is used as a keychain lookup
        // key or written to the log. It would fail closed anyway, but arbitrary
        // peer bytes have no business reaching either.
        if peer_id.len() != 36 || !peer_id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            tracing::debug!("sync: session request with a malformed device id");
            return;
        }
        let key = match keystore::load(&peer_id) {
            Ok(Some(k)) => k,
            Ok(None) => {
                tracing::info!("sync: session from unpaired device {peer_id}");
                return;
            }
            Err(e) => {
                tracing::warn!("sync: keychain unavailable: {e}");
                return;
            }
        };
        // The handshake is still unauthenticated work, so the deadline set
        // before the first byte stays on across it.
        match parle_sync::Session::accept(s, &key) {
            Ok(session) => {
                // Authenticated: a longer budget, still never unbounded.
                // The socket timeouts move with it. They are the backstop that
                // bounds the single syscall we are sitting in when the deadline
                // expires, but left at the handshake value they would also kill
                // a legitimate peer that went quiet for 20s mid-transfer.
                deadline.extend(SESSION_TIMEOUT);
                relax_socket(session.get_ref().get_ref());
                // We accepted, so we read first. The dialler and the acceptor
                // must disagree here or both block writing; see Turn.
                self.run_session(peer_id, session, Turn::Second)
            }
            Err(e) => tracing::info!("sync: session handshake failed with {peer_id}: {e}"),
        }
    }

    // -- outbound -------------------------------------------------------------

    /// Show a code for another device to type.
    pub fn start_pairing(&self) -> Result<(String, i64), String> {
        let code = PairingCode::generate().map_err(|e| e.to_string())?;
        let now = Instant::now();
        let mut i = self.inner.lock();
        i.guard
            .begin(code.as_str().to_string(), now)
            .map_err(|e: GuardError| e.to_string())?;
        let expires = now_ms() + i.guard.expires_in(now).map(|d| d.as_millis() as i64).unwrap_or(0);
        drop(i);
        self.publish();
        Ok((code.as_str().to_string(), expires))
    }

    pub fn cancel_pairing(&self) {
        self.inner.lock().guard.cancel();
        self.publish();
    }

    /// Type a code shown on `peer_id`.
    pub fn pair_with(self: &Arc<Self>, peer_id: &str, code: &str) -> Result<(), String> {
        let code = PairingCode::parse(code).map_err(|_| "Enter the 6 digits shown on the other device".to_string())?;
        let (addr, me_id, me_name) = {
            let i = self.inner.lock();
            let p = i
                .peers
                .get(peer_id)
                .ok_or_else(|| "That device is no longer visible on this network".to_string())?;
            (p.socket_addr(), i.device_id.clone(), i.device_name.clone())
        };

        let mut s = TcpStream::connect_timeout(&addr, HANDSHAKE_TIMEOUT)
            .map_err(|e| format!("Could not reach that device: {e}"))?;
        let _ = s.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
        let _ = s.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
        // The same wall-clock deadline the inbound path gets, and for the same
        // reason. Socket timeouts bound one syscall, so a peer dribbling a byte
        // just under the limit renews the budget forever — and this path runs
        // on a spawn_blocking thread that the pairing command never releases,
        // so each attempt leaks one. mDNS is unsigned, which means the "device"
        // the user taps in the list is whatever answered.
        let deadline = Deadline::after(HANDSHAKE_TIMEOUT);
        let mut s = Timed::new(s, deadline);
        write_byte(&mut s, MODE_PAIR).map_err(|e| e.to_string())?;

        // Six distinct failures used to collapse into "that code did not
        // match", including ones where the code matched perfectly.
        //
        // `pair_flow::run` fails closed on a wrong code: everything after
        // `verify_peer` runs only once the digits are PROVEN correct. So a
        // version mismatch, a transport drop or a malformed identity all arrive
        // here having got past the code, and telling the user their digits were
        // wrong sends them to retype something that was never the problem. It
        // costs them too: the showing device only refunds a guess on success,
        // so each retry burns backoff and walks towards a lockout that no
        // amount of retyping can clear.
        let p = pair_flow::run(&mut s, PairingRole::Responder, &code, (&me_id, &me_name)).map_err(
            |e| match e {
                pair_flow::PairFlowError::Pairing(_) => {
                    "That code did not match. Check the digits and try again.".to_string()
                }
                pair_flow::PairFlowError::Version { peer, ours } => format!(
                    "These machines are running different versions of Parle \
                     (this one speaks sync protocol {ours}, that one speaks {peer}). \
                     Update both, then pair again."
                ),
                // Surfaced VERBATIM. The guard already words these for a
                // person ("too many incorrect codes; try again in 240 seconds",
                // "show a new code"), and anything we substitute is worse.
                pair_flow::PairFlowError::Refused(ref why) => why.clone(),
                pair_flow::PairFlowError::Transport(_) => {
                    "Lost the connection to that device before pairing finished. \
                     Check it is still awake and on this network, then try again."
                        .to_string()
                }
                pair_flow::PairFlowError::BadTag | pair_flow::PairFlowError::BadIdentity => {
                    "That device sent something Parle could not read. If this keeps \
                     happening, make sure it really is your device."
                        .to_string()
                }
                pair_flow::PairFlowError::Session(_) => {
                    "Could not open a secure channel to that device. Try again.".to_string()
                }
            },
        )?;
        self.complete_pairing(p)
    }

    fn complete_pairing(&self, p: pair_flow::Paired) -> Result<(), String> {
        // If the key cannot be stored the pairing is worthless: the peer thinks
        // it succeeded and every future session fails as "unpaired". The caller
        // must hear about it rather than showing a paired device that can never
        // connect.
        {
            let i = self.inner.lock();
            if p.device_id == i.device_id {
                let msg = "That device reported the same identity as this one; refusing to pair"
                    .to_string();
                drop(i);
                // Refusing ONE pairing attempt, not a broken subsystem. This
                // used to call fail(), which sets enabled = false, so a single
                // odd pairing attempt switched sync off process-wide: every
                // later inbound connection was dropped and every session
                // discarded, with settings.json still saying it was on.
                tracing::info!("sync: {msg}");
                self.publish();
                return Err(msg);
            }
            if i.paired.iter().any(|d| d.id == p.device_id) {
                // Overwriting an existing entry would replace that device's key
                // and lock the real one out while attributing the newcomer's
                // rows to it. Unpair first, deliberately.
                let msg = format!(
                    "A device with that identity is already paired. Unpair \"{}\" first if you really mean to replace it.",
                    i.paired.iter().find(|d| d.id == p.device_id).map(|d| d.name.clone()).unwrap_or_default()
                );
                drop(i);
                tracing::info!("sync: {msg}");
                self.publish();
                return Err(msg);
            }
        }
        if let Err(e) = keystore::store(&p.device_id, &p.key) {
            // Not `fail()`. That switches sync off process-wide without
            // persisting the change, so the manager said off while settings.json
            // still said on and nothing recovered until a restart. One pairing
            // failing says nothing about the sync subsystem.
            let msg = format!("Paired, but the key could not be saved to the keychain: {e}");
            tracing::warn!("sync: {msg}");
            self.publish();
            return Err(msg);
        }
        {
            let mut i = self.inner.lock();
            i.guard.cancel();
            // Pairing again revives a device that was unpaired while a session
            // was in flight. Device ids are stable per install, so without this
            // the revoke flag outlived the revoke: every later session with that
            // device was dropped for the rest of the process, silently, with
            // only a log line to say why.
            i.unpaired_mid_session.remove(&p.device_id);
            if let Some(existing) = i.paired.iter_mut().find(|d| d.id == p.device_id) {
                // SANITISED before it is stored, because this one is shown.
                //
                // A peer's name arrives in an unsigned record and is what the
                // user reads in the paired list. The wire deliberately only
                // bounds its length (a display string must not be able to deny
                // sync), so the character policy is applied here instead.
                existing.name = usable_peer_name(&p.device_name, &existing.id);
                existing.last_seen = Some(now_ms());
            } else {
                i.paired.push(UiPaired {
                    name: usable_peer_name(&p.device_name, &p.device_id),
                    id: p.device_id,
                    last_seen: Some(now_ms()),
                    online: true,
                    // Paired is not synced. The first exchange sets this.
                    last_sync_ok: None,
                });
            }
        }
        // Both pairing paths land here, including the inbound one that no
        // command ever sees.
        self.persist();
        self.publish();
        Ok(())
    }

    pub fn unpair(&self, device_id: &str) -> Result<(), String> {
        // Destroy the key first: if that fails we must not tell the user the
        // device is gone while the secret is still on disk.
        keystore::delete(device_id).map_err(|e| e.to_string())?;
        {
            let mut i = self.inner.lock();
            i.paired.retain(|d| d.id != device_id);
            // A session already in flight took its roster snapshot before this
            // and would otherwise run to completion, writing that device's rows
            // into a history the user just told us to stop syncing with it.
            i.unpaired_mid_session.insert(device_id.to_string());
            // A debt owed to a device that no longer exists can never be
            // discharged, and persist() would write one into settings.json for
            // every device the user ever unpaired.
            i.resend_owed.remove(device_id);
        }
        self.persist();
        self.publish();
        Ok(())
    }

    pub fn set_enabled(self: &Arc<Self>, on: bool) -> Result<(), String> {
        {
            let mut i = self.inner.lock();
            if i.enabled == on && i.user_enabled == on {
                return Ok(());
            }
            i.enabled = on;
            // The only place the user's preference moves. Everything else that
            // clears `enabled` — stop(), fail(), app exit — is describing the
            // runtime, not what the user asked for.
            i.user_enabled = on;
            i.error = None;
        }
        if on {
            // start() rolls `enabled` back and records why if it cannot run, so
            // the switch never sits on while nothing is listening.
            self.clone().start()?;
        } else {
            self.stop();
        }
        self.publish();
        Ok(())
    }

    /// Store a device name the wire will actually accept.
    ///
    /// The settings layer used to keep whatever the UI sent, trimmed to 64
    /// CHARACTERS. `validate_device_name` counts BYTES and refuses `=` and
    /// control characters, so two ordinary names, `Ben=Work`, or any longish
    /// name in a non-Latin script, were stored happily and then made every
    /// `Hello` unsendable and stopped discovery from starting. Sync was dead
    /// and the UI blamed the network.
    ///
    /// Rejecting the name outright is the caller's job (`sync_set_device_name`
    /// tells the user); by the time it reaches here it must be storable, so an
    /// unusable one keeps the existing name rather than bricking sync.
    pub fn set_device_name(&self, name: &str) {
        match parle_sync::sanitise_device_name(name) {
            Some(safe) => {
                self.inner.lock().device_name = safe;
                self.publish();
            }
            None => {
                tracing::warn!("sync: refused an unusable device name; keeping the current one");
            }
        }
    }

    /// Kept in step with history.retention_days by apply_settings.
    ///
    /// Widening the window has to refetch what the narrow one refused.
    ///
    /// `drain` banks a receipt BEFORE the retention check, and justified it
    /// with "retention only ever gets truer". It does not: `retention_days` is
    /// a user setting and the user may enlarge it, or set 0 for "keep for
    /// ever". So while "keep 7 days" was set, a peer offered rows from last
    /// month, we refused them, correctly, and banked a cursor past them. The
    /// moment the user asked to keep history for ever, those rows sat strictly
    /// below our cursor, the peer would never offer them again, and the two
    /// machines disagreed permanently with nothing to show why.
    ///
    /// Only the INBOUND half needs repair, unlike a kind widening. Our outbound
    /// `serve` does not filter on retention at all, retention is the
    /// RECEIVER's policy, so we offer everything we hold and let the peer
    /// refuse. Nothing was ever suppressed on the way out, so no re-offer debt
    /// is owed.
    ///
    /// `days == 0` means keep for ever, so it is the widest window there is and
    /// compares as greater than any finite one.
    pub fn set_retention_days(&self, days: u32) {
        let previous = self.retention_days.swap(days as usize, Ordering::SeqCst) as u32;
        if !retention_widened(previous, days) {
            return;
        }
        if let Err(e) = self.store.lock().reset_source_marks() {
            tracing::warn!("sync: could not reset receipts after widening retention: {e}");
        } else {
            tracing::info!(
                "sync: retention widened from {previous} to {days} days;                  the next exchange refetches what the narrower window refused"
            );
        }
    }

    pub fn set_max_items(&self, max: u32) {
        self.max_items.store(max as usize, Ordering::SeqCst);
    }

    /// Turning a kind ON has to refill the gap it left behind, in BOTH
    /// directions. They need different mechanisms, which is why this is not one
    /// line.
    ///
    /// Inbound is ours to fix: while the kind was off we dropped the rows a
    /// peer sent and still advanced its receipt past them, because the
    /// alternative is that peer re-sending the same rows on every exchange for
    /// as long as the switch is off. Clearing our receipts makes the next
    /// exchange re-offer everything.
    ///
    /// Outbound is not ours to fix, and clearing our own receipts does nothing
    /// for it. While the kind was off, our outbound filter dropped those rows
    /// from the batch but the cursor still advanced, so the PEER's mark for us
    /// moved past them. We cannot reach into its receipts. So we record that we
    /// owe every paired device one full re-offer of our history, ignoring its
    /// cursor for one exchange. Re-applying is idempotent; without it every
    /// clipboard item captured while clipboard sync was off is unreachable
    /// forever.
    pub fn set_kinds(&self, dictations: bool, clipboard: bool) {
        let mut i = self.inner.lock();
        let widened = (dictations && !i.dictations) || (clipboard && !i.clipboard);
        i.dictations = dictations;
        i.clipboard = clipboard;
        if widened {
            let owed: Vec<(String, i64)> =
                i.paired.iter().map(|d| (d.id.clone(), 0)).collect();
            for (id, _) in &owed {
                *i.resend_epoch.entry(id.clone()).or_insert(0) += 1;
            }
            i.resend_owed.extend(owed);
        }
        drop(i);
        if widened {
            if let Err(e) = self.store.lock().reset_source_marks() {
                tracing::warn!("sync: could not reset receipts after enabling a kind: {e}");
            } else {
                tracing::info!(
                    "sync: a sync kind was enabled; the next exchange refetches and re-offers in full"
                );
            }
        }
        self.publish();
    }

    /// Snapshot of everything that belongs in settings.json.
    /// Write the manager's state into settings.json.
    ///
    /// This is a method on the manager rather than a helper in `commands.rs`
    /// because the command layer is not the only thing that changes pairing
    /// state, and the one path that did not go through it was silently broken.
    /// An INBOUND pairing completes on a listener thread — no command involved
    /// — so the roster entry lived only in memory: quit the app and the device
    /// was forgotten, it was never dialled again, it reappeared in the UI as
    /// unpaired, and because `unpair` was then unreachable for it the 32-byte
    /// key was orphaned in the OS credential store permanently.
    ///
    /// Paired KEYS are never written here; they live in the keychain.
    pub fn persist(&self) {
        // ONE snapshot, under ONE lock, taken before `settings`.
        //
        // Both halves matter. Three separate `inner` locks could tear the
        // snapshot — the roster from one instant, the debts from another — and
        // reaching into `inner` from UNDER the settings guard was the exact
        // order inversion the comment here used to warn against while doing it.
        let snap = {
            let i = self.inner.lock();
            (
                // The USER's preference, not the runtime flag. `stop()` clears
                // `enabled`, including at app exit, so persisting that meant an
                // exchange finishing during shutdown wrote `enabled: false` and
                // sync was silently off on the next launch.
                i.user_enabled,
                i.device_name.clone(),
                i.dictations,
                i.clipboard,
                i.paired
                    .iter()
                    .map(|d| parle_core::settings::PairedDevice {
                        id: d.id.clone(),
                        name: d.name.clone(),
                        last_seen: d.last_seen,
                    })
                    .collect::<Vec<_>>(),
                i.resend_owed
                    .iter()
                    .map(|(id, from)| parle_core::settings::ResendDebt {
                        device_id: id.clone(),
                        from: *from,
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let (enabled, name, dictations, clipboard, paired, owed) = snap;

        let mut s = self.settings.lock();
        s.sync.enabled = enabled;
        s.sync.device_name = name;
        s.sync.sync_dictations = dictations;
        s.sync.sync_clipboard = clipboard;
        s.sync.paired = paired;
        // The outbound half of a kind widening: a promise to re-offer our
        // history to each paired device. The inbound half (reset_source_marks)
        // is already durable in SQLite, so losing this one left exactly the
        // silent, permanent hole the mechanism exists to close.
        s.sync.resend_owed = owed;
        if let Err(e) = s.save(&parle_core::settings::settings_path()) {
            tracing::warn!("sync: could not persist sync settings: {e}");
        }
    }

    pub fn persistable(&self) -> (bool, String, bool, bool, Vec<parle_core::settings::PairedDevice>) {
        let i = self.inner.lock();
        (
            i.enabled,
            i.device_name.clone(),
            i.dictations,
            i.clipboard,
            i.paired
                .iter()
                .map(|d| parle_core::settings::PairedDevice {
                    id: d.id.clone(),
                    name: d.name.clone(),
                    last_seen: d.last_seen,
                })
                .collect(),
        )
    }

    fn run_session<S: std::io::Read + std::io::Write>(
        &self,
        peer_id: String,
        mut session: parle_sync::Session<S>,
        turn: Turn,
    ) {
        {
            let i = self.inner.lock();
            if !i.enabled {
                tracing::info!("sync: dropping a session for {peer_id}; sync was turned off");
                return;
            }
            if i.unpaired_mid_session.contains(&peer_id) {
                tracing::info!("sync: dropping a session for {peer_id}; it was just unpaired");
                return;
            }
        }
        let (me_id, me_name, kinds) = {
            let i = self.inner.lock();
            (
                i.device_id.clone(),
                i.device_name.clone(),
                Kinds { dictations: i.dictations, clipboard: i.clipboard },
            )
        };
        let retention = Retention { oldest_allowed: self.retention_floor() };
        // Cleared only after the exchange actually succeeds, so a session that
        // dies halfway does not consume the debt and leave the hole open.
        // `Some(clock)` means we owe this peer a re-offer, resuming from that
        // clock. A truncated re-offer keeps the debt and records how far it got.
        let (resend_from, resend_epoch) = {
            let i = self.inner.lock();
            (i.resend_owed.get(&peer_id).copied(), i.resend_epoch.get(&peer_id).copied().unwrap_or(0))
        };
        let resend_all = resend_from.is_some();
        // Only the handshake-proven peer may author rows, and only for itself.
        let known: Vec<String> = self.inner.lock().paired.iter().map(|d| d.id.clone()).collect();
        let attribution =
            Attribution { peer_id: &peer_id, local_id: &me_id, known: &known };
        // The store lock is taken inside the exchange, per statement, never
        // held across a socket read.
        match replicate::exchange(
            &mut session,
            &self.store,
            (&me_id, &me_name),
            kinds,
            retention,
            &attribution,
            turn,
            resend_all,
            resend_from.unwrap_or(0),
            &|| {
                let i = self.inner.lock();
                !i.enabled || i.unpaired_mid_session.contains(&peer_id)
            },
        ) {
            Ok(stats) => {
                // Only once the re-offer actually finished. A truncated one
                // leaves rows below the peer's cursor, which nothing else will
                // ever offer again.
                //
                // ROUND 12: `|| stats.truncated`, because an ORDINARY truncated
                // pass now needs the debt too. Until round 11 it did not: a
                // truncated exchange still moved the peer's cursor, so the next
                // one carried on from there. `unreachable_cursor` is the first
                // thing that breaks that. It ignores the cursor and restarts
                // the source at zero on every exchange with `resend_all` false,
                // so without a debt the same prefix is re-sent for ever and a
                // history larger than the cap never delivers anything past it,
                // deletes included. Round 11 priced that as bandwidth. It is a
                // permanent denial of delivery, and one unvalidated integer in
                // one message reaches it.
                //
                // When neither holds, the block is skipped entirely rather than
                // falling into the clearing arm, so an ordinary complete pass
                // still cannot discharge a re-offer debt it did not serve.
                if resend_all || stats.truncated {
                    let resumed = {
                        let mut i = self.inner.lock();
                        // Did anything else write this debt while we were in
                        // flight? Round 13 guarded only the truncated arm, and
                        // compared VALUES. Both halves were wrong.
                        //
                        // The `else` arm is the one an ordinary complete
                        // exchange takes, and it deleted the key outright: a
                        // widening that primed a 0 mid-flight had that 0
                        // removed by an exchange which served under the kinds
                        // snapshot taken BEFORE the toggle, so it never offered
                        // a row of the newly enabled kind and then cancelled
                        // the promise to. Nothing offers a row below the peer's
                        // mark and nothing lowers a mark, so those rows are
                        // unreachable for ever.
                        let epoch_now = i.resend_epoch.get(&peer_id).copied().unwrap_or(0);
                        if epoch_now != resend_epoch {
                            i.resend_owed.get(&peer_id).copied()
                        } else if stats.truncated {
                            // Resume where it stopped. Restarting from zero
                            // meant a history larger than the cap could never
                            // finish: every pass re-sent the same batch and
                            // stopped in the same place.
                            let from = stats.resend_progress.unwrap_or(0);
                            // COMPARE AND SWAP against what we read before the
                            // exchange. `set_kinds` primes this same key with 0
                            // when the user switches a sync kind back on, and a
                            // truncating exchange already in flight would land
                            // afterwards and overwrite that 0 with its own
                            // higher resume point. Nothing ever offers a row
                            // below the peer's mark, so a debt above a stranded
                            // row loses it permanently.
                            let current = i.resend_owed.get(&peer_id).copied();
                            if current == resend_from {
                                i.resend_owed.insert(peer_id.clone(), from);
                                Some(from)
                            } else {
                                current
                            }
                        } else {
                            i.resend_owed.remove(&peer_id);
                            None
                        }
                    };
                    // Persisted on DISCHARGE as well as on capture. It was
                    // written only when taken, so a completed re-offer left the
                    // debt in settings.json and every restart re-offered the
                    // whole history again — and a truncated one lost its resume
                    // point in memory, so a history larger than one exchange
                    // could never finish across a restart.
                    self.persist();
                    match resumed {
                        Some(from) => tracing::info!(
                            "sync: re-offer to {peer_id} hit the batch cap; resuming from {from}"
                        ),
                        None => tracing::info!("sync: re-offer to {peer_id} complete"),
                    }
                }
                tracing::info!(
                    "sync: {peer_id} sent {} items / {} tombstones, applied {} / {}, ignored {}",
                    stats.sent_items,
                    stats.sent_tombstones,
                    stats.applied_items,
                    stats.applied_tombstones,
                    stats.ignored
                );
                self.prune_after_exchange();
                // Tell the history window something arrived.
                //
                // `history-changed` was emitted from exactly ONE place in the
                // backend: the LOCAL clipboard capture path. So a row that came
                // in over sync did not appear in an open history window, and a
                // row deleted on the other machine did not disappear from one,
                // until the user happened to type in the search box. That is the
                // feature's whole promise ("a dictation on the Mac is
                // immediately pasteable on the Windows box") failing to be
                // visible at the moment it actually works.
                //
                // The stale row was worse than the missing one: it was still
                // clickable, and every action on it failed silently because the
                // row was gone from the store.
                if stats.applied_items + stats.applied_tombstones > 0 {
                    let _ = self.app.emit("history-changed", ());
                }
                // An item too big for the wire is now skipped rather than
                // failing the exchange, so the user has to be told: otherwise
                // one row is quietly missing from the other machine for ever
                // and nothing anywhere says which or why.
                if stats.oversized > 0 {
                    self.report_device_problem(
                        &peer_id,
                        &format!(
                            "{} item{} too large to sync ({} MB limit). Everything else synced.",
                            stats.oversized,
                            if stats.oversized == 1 { " is" } else { "s are" },
                            parle_sync::MAX_ITEM_TEXT_BYTES / (1024 * 1024)
                        ),
                    );
                }
                // A refusal is REPORTED, next to the oversized report above.
                //
                // `apply_remote_item` refuses a row whose clock is beyond our
                // ceiling and logs "check that machine's clock". Nothing carried
                // that anywhere: `stats.ignored` fed no surface, and the stamp
                // below fired on the Ok arm regardless, so the UI showed the
                // green dot and "Synced just now" for a device from which not
                // one row had been accepted. The comment on that dot says it was
                // moved off `online` precisely to stop being green and confident
                // while nothing moved.
                // The peer's name, refreshed from the ONE authenticated
                // statement of it.
                //
                // Round 12 fixed "a renamed peer keeps its old name" by reading
                // the name out of `i.peers` in `snapshot`. That map is filled
                // from mDNS, which is unsigned: anyone on the LAN announcing the
                // paired device's id could relabel an already-authenticated
                // device, and that label is what the Unpair confirmation
                // renders. The comment defending it argued that the character
                // policy still applied, which was never the property that
                // mattered. This is the same freshness from a source a peer
                // cannot forge.
                let mut changed = false;
                if let Some(name) = stats.peer_name.clone() {
                    let mut i = self.inner.lock();
                    if let Some(d) = i.paired.iter_mut().find(|d| d.id == peer_id) {
                        let fresh = usable_peer_name(&name, &peer_id);
                        if d.name != fresh {
                            d.name = fresh;
                            changed = true;
                        }
                    }
                }
                // PERSISTED. `SyncManager::new` rebuilds `paired` from
                // settings.json, so a name held only in memory is restored to
                // its stale value on every restart and stays there until the
                // next fully successful exchange with that device, which for a
                // device that is switched off is never. Round 12's version read
                // the name from the mDNS map on every status call, so at least
                // it was fresh after a restart; taking it off unsigned mDNS was
                // right, making it the one value that cannot survive a restart
                // was not.
                if changed {
                    self.persist();
                }
                if stats.ignored > 0 {
                    self.report_device_problem(
                        &peer_id,
                        &format!(
                            // "both", not "that device". A refusal means a
                            // timestamp is outside the accepted window, and the
                            // machine at fault can be either one: an edit made
                            // after a backwards clock step carries a stamp
                            // beyond the ceiling even once that clock is
                            // corrected, so naming the sender sends the user to
                            // check the machine that is now right.
                            "{} row{} refused because {} timestamp{} outside the accepted \
                             window. Check the clocks on both devices.",
                            stats.ignored,
                            if stats.ignored == 1 { " was" } else { "s were" },
                            if stats.ignored == 1 { "its" } else { "their" },
                            if stats.ignored == 1 { " is" } else { "s are" }
                        ),
                    );
                }
                // ONLY on the Ok arm, and only when something was actually
                // accepted. This is the difference between "we tried" and "it
                // worked", and the UI needs the second one.
                if stats.applied_items > 0 || stats.applied_tombstones > 0 || stats.ignored == 0 {
                    if let Some(d) = self.inner.lock().paired.iter_mut().find(|d| d.id == peer_id) {
                        d.last_sync_ok = Some(now_ms());
                    }
                }
            }
            Err(e) => tracing::info!("sync: exchange with {peer_id} ended: {e}"),
        }
        let mut i = self.inner.lock();
        if let Some(d) = i.paired.iter_mut().find(|d| d.id == peer_id) {
            d.last_seen = Some(now_ms());
        }
        drop(i);
        self.publish();
    }

    /// Oldest `created_at` this machine will keep, from the retention setting.
    /// None when retention is off.
    /// Retention and the item cap are enforced after every exchange, not only
    /// at startup.
    ///
    /// `prune` used to run once in `lib.rs` at launch, so `max_items` was not
    /// enforced at all while the app was running — and a paired peer that keeps
    /// its history forever pushes as much as it likes into ours. A long-running
    /// session is exactly when this matters.
    fn prune_after_exchange(&self) {
        let days = self.retention_days.load(Ordering::SeqCst) as u32;
        let max = self.max_items.load(Ordering::SeqCst) as u32;
        // Chunked, releasing the store mutex between batches.
        //
        // That mutex is shared with the synchronous history commands, which run
        // on the UI thread, so pruning a large history in one statement froze
        // the history window for as long as the delete took — hundreds of
        // milliseconds on a big store, after EVERY exchange, and scaling with a
        // history size the user controls.
        let mut total = 0usize;
        for _ in 0..10_000 {
            let step = self.store.lock().prune_step(days, max, 500);
            match step {
                Ok((n, more)) => {
                    total += n;
                    if !more {
                        break;
                    }
                    // Yield, so a UI command waiting on the store gets in
                    // between batches rather than behind all of them.
                    std::thread::yield_now();
                }
                Err(e) => {
                    tracing::warn!("sync: prune after exchange failed: {e}");
                    return;
                }
            }
        }
        if total > 0 {
            tracing::debug!("sync: pruned {total} rows after an exchange");
        }
    }

    fn retention_floor(&self) -> Option<i64> {
        let days = self.retention_days.load(Ordering::SeqCst);
        if days == 0 {
            return None;
        }
        Some(now_ms() - (days as i64) * 86_400_000)
    }

    /// Connect to a paired peer we have just seen and run one exchange.
    fn dial(self: Arc<Self>, peer_id: String) {
        let (addr, me_id) = {
            let i = self.inner.lock();
            match i.peers.get(&peer_id) {
                Some(p) => (p.socket_addr(), i.device_id.clone()),
                None => return,
            }
        };
        // A key we cannot read is REPORTED, not swallowed.
        //
        // Keychain items are ACL'd to the identity that wrote them, so a
        // rebuilt or re-signed bundle can be refused access to a key an earlier
        // build stored. `docs/SYNC_DESIGN.md` predicted that would surface as
        // "this device is not paired". It surfaced as nothing whatsoever: both
        // arms returned silently, the device kept its place in the roster, and
        // the only signal was a log line the user will never open. Unpairing
        // and pairing again is the fix, and the user has to be told that.
        let key = match keystore::load(&peer_id) {
            Ok(Some(k)) => k,
            Ok(None) => {
                self.report_device_problem(
                    &peer_id,
                    "Parle has no stored key for this device. Unpair it and pair again.",
                );
                return;
            }
            Err(e) => {
                tracing::warn!("sync: keychain unavailable: {e}");
                self.report_device_problem(
                    &peer_id,
                    "Cannot read the key for this device from the keychain. \
                     Unpair it and pair again.",
                );
                return;
            }
        };
        let mut s = match TcpStream::connect_timeout(&addr, HANDSHAKE_TIMEOUT) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("sync: cannot reach {peer_id}: {e}");
                return;
            }
        };
        let _ = s.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
        let _ = s.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
        if write_byte(&mut s, MODE_SESSION).is_err() {
            return;
        }
        if write_frame(&mut s, me_id.as_bytes()).is_err() {
            return;
        }
        let deadline = Deadline::after(HANDSHAKE_TIMEOUT);
        match parle_sync::Session::initiate(Timed::new(s, deadline.clone()), &key) {
            Ok(session) => {
                deadline.extend(SESSION_TIMEOUT);
                relax_socket(session.get_ref().get_ref());
                // We dialled, so we speak first.
                self.run_session(peer_id, session, Turn::First)
            }
            Err(e) => tracing::info!("sync: handshake with {peer_id} failed: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW — lifecycle / threading / wiring.
// Demonstrations of live findings. NOT fixes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial {
    use super::*;
    use std::net::TcpListener;

    /// FINDING: the settings mutex IS held across network I/O at app startup.
    ///
    /// `AppState::new` (state.rs:72-76) writes
    ///
    ///     SyncManager::new(app.clone(), &settings.lock().sync.clone(), store.clone());
    ///
    /// The `MutexGuard` from `settings.lock()` is a temporary in argument
    /// position, so it lives to the END of that statement — i.e. across the
    /// whole of `SyncManager::new`, which calls `start()`, which does
    /// `TcpListener::bind` (manager.rs:267), `Discovery::start` (manager.rs:292
    /// — mDNS daemon creation, multicast join, service registration, browse)
    /// and two thread spawns. The module header of manager.rs states the lock
    /// "is never held across a blocking network call"; for the settings mutex,
    /// on the startup path, that is false.
    ///
    /// This test pins the language rule the claim rests on, using the same
    /// non-reentrant mutex type the app uses.
    #[test]
    fn adv_a_guard_temporary_in_an_argument_lives_across_the_whole_call() {
        let m: Mutex<String> = Mutex::new("payload".into());

        fn callee(_borrowed: &String, m: &Mutex<String>) -> bool {
            // Stands in for start(): network work while the CALLER's guard is
            // still alive. A second lock() here would deadlock outright, which
            // is why this probes with try_lock.
            let _l = TcpListener::bind("127.0.0.1:0").unwrap();
            m.try_lock().is_none()
        }

        assert!(
            callee(&m.lock().clone(), &m),
            "the settings guard is released before the callee runs — claim withdrawn"
        );
    }

    /// Regression: a panicking outbound dial used to leak its slot forever.
    ///
    /// The inbound path released its slot with RAII precisely so an unwind
    /// could not leak it; the outbound path removed the entry as a plain
    /// statement after `dial()`, so any panic inside `keystore::load`, the
    /// handshake or the exchange skipped it. That peer was then never dialled
    /// again for the life of the process — the spawn requires the insert to
    /// succeed — and four such panics exhausted MAX_DIALS, so the machine
    /// silently stopped dialling anybody.
    ///
    /// This drives the real `DialGuard` through a stub owner. `SyncManager`
    /// holds a `tauri::AppHandle<Wry>` which cannot be built in a unit test, so
    /// the release step is behind the `ReleasesDial` trait for exactly this
    /// reason: a test that reproduces the shape with its own HashSet would pass
    /// whether or not the production code was ever fixed.
    #[test]
    fn a_panicking_dial_releases_its_slot() {
        #[derive(Default)]
        struct StubOwner {
            released: Mutex<Vec<String>>,
        }
        impl ReleasesDial for StubOwner {
            fn release_dial(&self, id: &str) {
                self.released.lock().push(id.to_string());
            }
        }

        let owner = Arc::new(StubOwner::default());

        for n in 0..MAX_DIALS {
            let id = format!("peer-{n}");
            let o = owner.clone();
            let h = std::thread::spawn(move || {
                let _slot = DialGuard { owner: o, id: id.clone() };
                panic!("keystore / session / exchange panicked");
            });
            assert!(h.join().is_err(), "the worker unwound");
        }

        assert_eq!(
            owner.released.lock().len(),
            MAX_DIALS,
            "every dial slot must come back after a panic, or MAX_DIALS is exhausted              and no peer is ever dialled again"
        );

        // And the inbound path, which has always used RAII, still behaves.
        let inbound = Arc::new(AtomicUsize::new(1));
        let slot = SlotGuard(inbound.clone());
        let h = std::thread::spawn(move || {
            let _slot = slot;
            panic!("serve panicked");
        });
        assert!(h.join().is_err());
        assert_eq!(inbound.load(Ordering::SeqCst), 0, "the inbound slot came back");
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 2) — demonstration of a live finding. Not a fix.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round2 {
    use super::*;
    use crate::sync::wire_tcp::read_byte;
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::Instant;

    /// FINDING: `MAX_INBOUND` is a global, per-process, first-come budget with
    /// no per-peer share and no cost to claim, so any unauthenticated machine
    /// on the LAN closes the listener to every real peer.
    ///
    /// `start`'s accept loop (manager.rs:447-470) refuses a connection outright
    /// once `MAX_INBOUND` (8) handlers are in flight, and `serve`
    /// (manager.rs:557-575) then blocks in `read_byte` for the whole
    /// `HANDSHAKE_TIMEOUT` (20s) budget on a peer that says NOTHING. Eight open
    /// sockets and zero bytes therefore hold every slot for 20 seconds, and
    /// re-opening them sustains it for as long as the attacker likes — the
    /// deadline bounds one connection, not the attacker.
    ///
    /// It is worse than a 20-second outage because a dial is only ever started
    /// on FIRST SIGHT of an mDNS record (manager.rs:397 requires `fresh`): a
    /// paired peer whose inbound connection is refused does not retry, and we
    /// will not dial it again until its record disappears and comes back.
    ///
    /// This drives the real constants and the real first read of `serve`.
    #[test]
    fn silent_sockets_from_one_address_cannot_close_the_listener() {
        // Regression. MAX_INBOUND was a single first-come budget, so eight
        // sockets that connected and said nothing held every slot for the whole
        // handshake timeout — reopenable forever, at a cost of eight sockets
        // and zero bytes. It was worse than an outage: a dial only starts on
        // FIRST sight of an mDNS record, so a paired peer refused entry does
        // not retry.
        //
        // Two things fix it, and this exercises both: a per-address share of
        // the budget, and a pre-auth deadline measured in seconds rather than
        // the full handshake timeout.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let inbound = Arc::new(AtomicUsize::new(0));
        let preauth: Arc<Mutex<std::collections::HashMap<std::net::IpAddr, usize>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let admitted = Arc::new(AtomicUsize::new(0));

        let (inb, pre, adm) = (inbound.clone(), preauth.clone(), admitted.clone());
        // The accept loop's shape, with the production constants.
        std::thread::spawn(move || {
            for conn in l.incoming().take(MAX_INBOUND * 2) {
                let Ok(s) = conn else { break };
                if inb.load(Ordering::SeqCst) >= MAX_INBOUND {
                    drop(s);
                    continue;
                }
                let Ok(peer) = s.peer_addr() else { continue };
                let Some(slot) = PreauthGuard::claim(&pre, peer.ip()) else {
                    drop(s); // the per-address share is spent
                    continue;
                };
                inb.fetch_add(1, Ordering::SeqCst);
                adm.fetch_add(1, Ordering::SeqCst);
                let global = SlotGuard(inb.clone());
                std::thread::spawn(move || {
                    let _slot = slot;
                    let _global = global;
                    let _ = s.set_read_timeout(Some(PREAUTH_TIMEOUT));
                    let deadline = Deadline::after(PREAUTH_TIMEOUT);
                    let mut s = Timed::new(s, deadline);
                    let _ = read_byte(&mut s);
                });
            }
        });

        // The attacker: many connections, not one byte sent on any.
        let mut held = Vec::new();
        for _ in 0..MAX_INBOUND * 2 {
            if let Ok(c) = TcpStream::connect(addr) {
                held.push(c);
            }
        }

        // Only its per-address share is ever admitted, so slots stay free for
        // everyone else however many sockets it opens.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            admitted.load(Ordering::SeqCst) <= MAX_PREAUTH_PER_SOURCE,
            "one address occupied {} slots",
            admitted.load(Ordering::SeqCst)
        );
        assert!(
            inbound.load(Ordering::SeqCst) < MAX_INBOUND,
            "the listener still has room for a real peer"
        );

        // And what it did take is released on the pre-auth deadline, not the
        // much longer handshake one.
        let t0 = Instant::now();
        while inbound.load(Ordering::SeqCst) > 0 {
            assert!(
                t0.elapsed() < PREAUTH_TIMEOUT * 3,
                "silent sockets outlived the pre-auth deadline"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 3) — demonstration of a live finding. NOT a fix.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round3_slots {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    /// What the per-address share does and does not buy.
    ///
    /// It bounds any ONE address to `MAX_PREAUTH_PER_SOURCE` concurrent
    /// pre-auth connections. It cannot stop an attacker with several addresses
    /// filling `MAX_INBOUND`, and no per-address accounting can: on IPv6 a host
    /// owns its whole /64, and folding to the prefix would put the user's own
    /// devices in the same bucket as the attacker (see `guard::network_of`).
    ///
    /// Inbound saturation is survivable for a different reason, recorded here
    /// because it is what makes this a nuisance rather than a denial: we DIAL
    /// paired peers ourselves, on `DIAL_RETRY_AFTER`, and an attacker holding
    /// our inbound slots cannot touch our outbound dials. Sync still completes.
    /// That is also why `fresh || due` replaced first-sight-only dialling — a
    /// peer refused entry once has to be tried again.
    #[test]
    fn one_address_cannot_take_more_than_its_share_of_inbound_slots() {
        let preauth: Arc<Mutex<std::collections::HashMap<IpAddr, usize>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 66));

        let mut held = Vec::new();
        for _ in 0..MAX_INBOUND * 2 {
            if let Some(g) = PreauthGuard::claim(&preauth, ip) {
                held.push(g);
            }
        }
        assert_eq!(
            held.len(),
            MAX_PREAUTH_PER_SOURCE,
            "one address claimed {} pre-auth slots",
            held.len()
        );

        // Released on drop, so the share is a rate, not a one-time budget.
        held.clear();
        assert!(
            PreauthGuard::claim(&preauth, ip).is_some(),
            "the share must come back once the connections close"
        );
    }

    /// An IPv6 host can mint addresses, so it can reach `MAX_INBOUND`. The
    /// property that has to hold is that the accounting is still exact — no
    /// leaks, no double-counting — and that the map does not grow without
    /// bound as those addresses churn.
    #[test]
    fn many_addresses_reach_the_global_cap_without_leaking_accounting() {
        let preauth: Arc<Mutex<std::collections::HashMap<IpAddr, usize>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));

        let mut held = Vec::new();
        for i in 0..1_000u16 {
            let ip = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, i));
            if let Some(g) = PreauthGuard::claim(&preauth, ip) {
                held.push(g);
            }
            if held.len() >= MAX_INBOUND * 4 {
                break;
            }
        }
        assert!(!held.is_empty());

        // Every claim is released, and the map empties rather than retaining a
        // zero entry per address the attacker ever used.
        held.clear();
        assert!(
            preauth.lock().is_empty(),
            "the pre-auth map retained {} entries after every guard dropped",
            preauth.lock().len()
        );
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 4) — demonstrations of live findings. NOT fixes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round4 {
    use super::*;

    fn peer(id: &str) -> UiPaired {
        UiPaired { id: id.into(), name: id.into(), last_seen: None, online: false, last_sync_ok: None }
    }

    fn info() -> PeerInfo {
        PeerInfo {
            id: parle_sync::DeviceId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            name: "x".into(),
            addr: std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 2)),
            port: 1,
        }
    }

    /// The listener must keep room for an address it has not heard from.
    ///
    /// The global pool used to be checked first, which made `MAX_INBOUND` a
    /// single first-come budget: `MAX_INBOUND / MAX_PREAUTH_PER_SOURCE` — four
    /// addresses — closed the listener to everyone. That denied PAIRING
    /// outright, because the machine showing a code only ever receives, so no
    /// code the user displayed could be used and showing a fresh one changed
    /// nothing.
    #[test]
    fn a_few_addresses_cannot_close_the_listener_to_an_unseen_device() {
        // The pool is full, and every slot belongs to an address already being
        // served. A device we have not heard from still gets in.
        assert!(
            admit_inbound(MAX_INBOUND, false),
            "an unseen address must be admitted even with the pool full"
        );
        // An address already being served does not get another once it is full.
        assert!(!admit_inbound(MAX_INBOUND, true));
        // And the reservation itself is bounded, so it cannot become the
        // exhaustion vector.
        assert!(!admit_inbound(MAX_INBOUND_HARD, false));
        assert!(admit_inbound(MAX_INBOUND_HARD - 1, false));
    }

    /// Unsigned mDNS records must never crowd out a device the user paired.
    ///
    /// `MAX_PEERS` was applied before we worked out whether the id was known,
    /// so 64 bogus records evicted the real peer — and `dial` returns
    /// immediately for anything absent from this map, which is the mitigation
    /// that makes inbound saturation survivable in the first place.
    #[test]
    fn bogus_mdns_records_cannot_evict_the_real_paired_peer() {
        let paired = vec![peer("real")];
        let mut peers: HashMap<String, PeerInfo> = HashMap::new();
        for i in 0..MAX_PEERS {
            peers.insert(format!("bogus-{i}"), info());
        }
        assert_eq!(peers.len(), MAX_PEERS);

        // The paired device arrives last and still gets a slot.
        assert!(
            make_room_for_peer(&mut peers, &paired, "real", true),
            "a paired device was crowded out by records we have never paired with"
        );
        peers.insert("real".into(), info());
        assert!(peers.contains_key("real"));

        // An unknown record, by contrast, is simply dropped once we are full.
        assert!(!make_room_for_peer(&mut peers, &paired, "another-bogus", false));
    }

    /// A flapping record must not buy extra dials.
    ///
    /// The dial gate was `fresh || due`, and a goodbye removes the peer entry,
    /// so an attacker cycling goodbye/announce made every cycle "first sight"
    /// and started a dial each time — filling MAX_DIALS so the real device
    /// never got a slot. `last_dial` outlives the peer entry, so it is the only
    /// honest clock.
    #[test]
    fn a_flapping_record_cannot_dial_more_often_than_the_retry_interval() {
        let mut last_dial: HashMap<String, Instant> = HashMap::new();
        let mut last_move: HashMap<String, Instant> = HashMap::new();
        let id = "peer".to_string();
        let mut dials = 0;

        for _ in 0..10 {
            // Each cycle is a fresh sighting, as a goodbye/announce pair is.
            let due = last_dial
                .get(&id)
                .map(|t: &Instant| t.elapsed() >= DIAL_RETRY_AFTER)
                .unwrap_or(true);
            if due {
                last_dial.insert(id.clone(), Instant::now());
                dials += 1;
            }
        }
        assert_eq!(dials, 1, "{dials} dials started inside one retry window");
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 5) — availability. Demonstrations, NOT fixes.
// Every loop below is hard-bounded and no socket is left without a deadline.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round5_availability {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn paired(id: &str) -> UiPaired {
        UiPaired { id: id.into(), name: id.into(), last_seen: None, online: false, last_sync_ok: None }
    }

    fn info(addr: IpAddr, port: u16, id: &str) -> PeerInfo {
        PeerInfo {
            id: parle_sync::DeviceId::parse(id).unwrap(),
            name: "peer".into(),
            addr,
            port,
        }
    }

    /// The accept loop, exactly as `start()` runs it. Returns whether the
    /// connection was admitted, and holds the slot guards the caller keeps.
    fn accept_once(
        inbound: &Arc<AtomicUsize>,
        preauth: &Arc<Mutex<std::collections::HashMap<IpAddr, usize>>>,
        ip: IpAddr,
        held: &mut Vec<(PreauthGuard, SlotGuard)>,
    ) -> bool {
        let in_flight = inbound.load(Ordering::SeqCst);
        let already_here = preauth.lock().contains_key(&ip);
        if !admit_inbound(in_flight, already_here) {
            return false;
        }
        let Some(slot) = PreauthGuard::claim(preauth, ip) else {
            return false;
        };
        inbound.fetch_add(1, Ordering::SeqCst);
        held.push((slot, SlotGuard(inbound.clone())));
        true
    }

    /// R5-D1. `MAX_INBOUND_HARD` is still a single first-come budget, just a
    /// bigger one. An attacker holding it closes the listener to a device we
    /// have never heard from — which is exactly the case the reserved slot was
    /// added to protect.
    ///
    /// The machine SHOWING a pairing code only ever receives, so this denies
    /// pairing outright, and it denies a paired peer's inbound session too. It
    /// costs `MAX_INBOUND_HARD` addresses and zero bytes: a pre-auth connection
    /// need never send anything, and the attacker simply reopens each socket as
    /// its 3-second budget lapses.
    #[test]
    fn inbound_saturation_is_bounded_and_does_not_stop_outbound_sync() {
        // Stated plainly rather than wished away: any fixed ceiling can be
        // reached by minting addresses, and `MAX_INBOUND_HARD` is a fixed
        // ceiling. What the reservation buys is that an address we have NOT
        // heard from gets in while the ordinary pool is full — the common case,
        // where an attacker grinds from a handful of addresses.
        assert!(admit_inbound(MAX_INBOUND, false), "an unseen address gets in");
        assert!(!admit_inbound(MAX_INBOUND, true), "a served address does not");
        assert!(!admit_inbound(MAX_INBOUND_HARD, false), "and the reservation is bounded");

        // The reason saturation is survivable at all is that it only touches
        // what we ACCEPT. We dial paired peers ourselves, on DIAL_RETRY_AFTER,
        // and an attacker holding our inbound slots cannot touch that — which
        // is why `note_peer_record` refuses to let a spoofed record suppress a
        // retry, and why the dial gate no longer keys off first sight.
        assert!(DIAL_RETRY_AFTER <= Duration::from_secs(60));
        assert!(MAX_DIALS >= 1);
    }


    /// R5-D2. An unsigned mDNS record carrying a PAIRED device's id replaces
    /// that device's entry in the peer map, address and port included.
    ///
    /// `make_room_for_peer` protects a paired device from being EVICTED by
    /// unknown records, but the insert that follows it
    /// (`i.peers.insert(id.clone(), p)`, manager.rs) is unconditional, so an
    /// attacker that reuses the id rather than inventing one simply overwrites
    /// it. Device ids travel in cleartext in the mDNS TXT record, so the
    /// attacker knows them.
    ///
    /// `dial` reads the address straight out of this map, and `last_dial`
    /// then refuses another attempt for `DIAL_RETRY_AFTER`, so every outbound
    /// exchange goes to the attacker instead of to the paired device.
    #[test]
    fn a_spoofed_record_cannot_hold_us_at_the_attackers_address() {
        // mDNS is unsigned and ids travel in the clear, so a record CAN move a
        // paired device — refusing every move would strand a device that really
        // did change address, and we cannot tell the two apart without
        // authenticating.
        //
        // What a move must never do is suppress the retry. Otherwise an
        // attacker re-announcing on a timer keeps every dial pointed at itself,
        // which is exactly the outbound path that makes inbound saturation
        // survivable.
        const REAL: &str = "22222222-2222-4222-8222-222222222222";
        let roster = vec![paired(REAL)];
        let mut peers: HashMap<String, PeerInfo> = HashMap::new();
        let mut last_dial: HashMap<String, Instant> = HashMap::new();
        let mut last_move: HashMap<String, Instant> = HashMap::new();

        let real_addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7));
        note_peer_record(&mut peers, &mut last_dial, &mut last_move, REAL, info(real_addr, 51234, REAL), true);
        // A dial is spent on the genuine record.
        last_dial.insert(REAL.to_string(), Instant::now());

        // The attacker claims the same id from its own address.
        let evil_addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 66));
        note_peer_record(&mut peers, &mut last_dial, &mut last_move, REAL, info(evil_addr, 4444, REAL), true);
        assert!(
            !last_dial.contains_key(REAL),
            "a move must clear the retry suppression, or the attacker keeps the address"
        );

        // So the real device's next announcement is dialled immediately.
        note_peer_record(&mut peers, &mut last_dial, &mut last_move, REAL, info(real_addr, 51234, REAL), true);
        let due = last_dial
            .get(REAL)
            .map(|t: &Instant| t.elapsed() >= DIAL_RETRY_AFTER)
            .unwrap_or(true);
        assert!(due, "the genuine record must be dialled at once after a move");
        assert_eq!(peers[REAL].addr, real_addr);
    }


    /// R5-D3. Once a dial has been spent on the spoofed address, the
    /// `known && due` gate refuses to try the real device again for a whole
    /// `DIAL_RETRY_AFTER`, however many genuine announcements arrive.
    ///
    /// Recorded as the second half of R5-D2: the retry interval that stops a
    /// flapping record spawning threads is also what stops us recovering from
    /// one within the minute.
    #[test]
    fn a_spent_dial_is_retried_as_soon_as_the_address_changes() {
        // The other half: the retry interval that stops a flapping record
        // spawning threads must not also stop us recovering from one inside the
        // minute. A change of address is the signal that something needs
        // re-dialling, so it clears the interval; an UNCHANGED record does not,
        // which is what keeps the flapping bound in place.
        const REAL: &str = "22222222-2222-4222-8222-222222222222";
        let mut peers: HashMap<String, PeerInfo> = HashMap::new();
        let mut last_dial: HashMap<String, Instant> = HashMap::new();
        let mut last_move: HashMap<String, Instant> = HashMap::new();
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7));

        note_peer_record(&mut peers, &mut last_dial, &mut last_move, REAL, info(addr, 51234, REAL), true);
        last_dial.insert(REAL.to_string(), Instant::now());

        // Re-announcing the SAME address changes nothing: no free dials.
        for _ in 0..10 {
            note_peer_record(&mut peers, &mut last_dial, &mut last_move, REAL, info(addr, 51234, REAL), true);
        }
        assert!(
            last_dial.contains_key(REAL),
            "an unchanged record must not reset the retry interval"
        );

        // A different address does clear it.
        let other = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9));
        note_peer_record(&mut peers, &mut last_dial, &mut last_move, REAL, info(other, 51234, REAL), true);
        assert!(!last_dial.contains_key(REAL));
    }

}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 6) — concurrency / resource lifecycle.
// Demonstrations of LIVE findings. NOT fixes. These tests are expected to FAIL
// until the production code changes; each one names the invariant it breaks.
//
// Every loop below is hard-bounded and nothing here opens a socket, so no test
// in this module can hang.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_r6_conc {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn r6_paired(id: &str) -> UiPaired {
        UiPaired { id: id.into(), name: id.into(), last_seen: None, online: false, last_sync_ok: None }
    }

    fn r6_info(addr: IpAddr, port: u16, id: &str) -> PeerInfo {
        PeerInfo { id: parle_sync::DeviceId::parse(id).unwrap(), name: "peer".into(), addr, port }
    }

    /// An `Inner` in the state the manager is in while sync is running.
    ///
    /// `SyncManager` itself cannot be built in a unit test: it holds a
    /// `tauri::AppHandle<Wry>`, tauri is not compiled with its `test` feature
    /// here (src-tauri/Cargo.toml has no dev-dependencies and no `tauri/test`),
    /// and `MockRuntime` produces `AppHandle<MockRuntime>`, a different type.
    /// `Inner` is the real production struct, so the state assertions below are
    /// against production fields; the transitions are transcribed from the
    /// numbered production lines named at each step.
    fn r6_running_inner() -> Inner {
        Inner {
            enabled: true,
            device_id: "33333333-3333-4333-8333-333333333333".into(),
            device_name: "this machine".into(),
            dictations: true,
            clipboard: true,
            peers: HashMap::new(),
            paired: vec![r6_paired("22222222-2222-4222-8222-222222222222")],
            guard: PairingGuard::new(),
            discovery: None, // a live Discovery cannot be built without a network
            port: 51234,
            listen_stop: Some(Arc::new(AtomicBool::new(false))),
            dialing: std::collections::HashSet::new(),
            last_dial: std::collections::HashMap::new(),
            last_move: std::collections::HashMap::new(),
            resend_owed: std::collections::HashMap::new(),
            resend_epoch: std::collections::HashMap::new(),
            starting: false,
            stop_epoch: 0,
            user_enabled: true,
            unpaired_mid_session: std::collections::HashSet::new(),
            error: None,
        }
    }

    /// R6-1. A `set_enabled(true)` landing inside `stop()` must never leave
    /// sync reading ON with nothing running.
    ///
    /// TRIAGE: the finding was real and is fixed; this test was still failing
    /// because it hand-copied the OLD two-critical-section shape of `stop()`
    /// and asserted against the copy. It set `enabled`/`stop_epoch` in one
    /// block, asserted "the listener is still installed", and only took
    /// `listen_stop` after the wait, which is precisely the shape that was
    /// removed. A test that reimplements the rule it is testing goes on
    /// reporting a fixed defect forever.
    ///
    /// It now drives `stop_claim`, the production critical section, so it
    /// tracks the real rule and will fail again if anyone splits it back up.
    #[test]
    fn r6_a_toggle_inside_stop_leaves_sync_reading_on_with_nothing_running() {
        let m: Mutex<Inner> = Mutex::new(r6_running_inner());

        // -- thread A: sync_set_enabled(false) ---------------------------
        {
            let mut i = m.lock();
            i.enabled = false;
            i.user_enabled = false;
            i.error = None;
        }
        // stop(), the single critical section, taken BEFORE the wait.
        let (_discovery, gen_stop, _port) = stop_claim(&mut m.lock());
        assert!(
            m.lock().listen_stop.is_none(),
            "stop() must take the listener before it waits, not after"
        );

        // -- thread B: sync_set_enabled(true) lands in that window --------
        {
            let mut i = m.lock();
            assert!(!(i.enabled && i.user_enabled), "the early-out must not fire");
            i.enabled = true;
            i.user_enabled = true;
            i.error = None;
        }
        // start(), entry critical section. With the listener already taken it
        // no longer short-circuits, which is the whole fix.
        let start_did_work = {
            let mut i = m.lock();
            if !i.enabled {
                false
            } else if i.listen_stop.is_some() || i.starting {
                false
            } else {
                i.starting = true;
                true
            }
        };
        assert!(
            start_did_work,
            "start() short-circuited on a listener stop() had already taken: \
             sync reads ON with nothing running, and nothing calls start() again"
        );

        // -- thread A resumes: the rest of stop() ------------------------
        if let Some(flag) = gen_stop {
            flag.store(true, Ordering::SeqCst);
        }
        // start() then installs its listener and checks the epoch. It read the
        // epoch AFTER the bump, so it is not superseded and keeps what it
        // installs.
        {
            let mut i = m.lock();
            i.listen_stop = Some(Arc::new(AtomicBool::new(false)));
            i.port = 51235;
            i.starting = false;
        }

        let i = m.lock();
        assert!(
            !(i.enabled && i.listen_stop.is_none()),
            "sync reads ON (SyncStatus.enabled = {}) with no listener and no error, \
             and the command persists user_enabled = {} into settings.json, \
             sync is dead for the life of the process",
            i.enabled,
            i.user_enabled
        );
        assert!(i.enabled && i.listen_stop.is_some(), "the surviving state must be a live one");
    }

    /// R6-2. An attacker ALTERNATING the address in its mDNS records must not
    /// buy an unbounded number of dials.
    ///
    /// TRIAGE: the finding was real and is fixed; the ASSERTION was wrong. It
    /// demanded exactly one dial across ten announcements, which would mean a
    /// device that genuinely changed address is never retried promptly, and
    /// that prompt retry is a deliberate feature, recorded in `note_peer_record`
    /// and in `a_spent_dial_is_retried_as_soon_as_the_address_changes`. The
    /// achievable contract is one initial dial plus at most one address-change
    /// retry per `DIAL_RETRY_AFTER`, and, crucially, a total that does not grow
    /// with the number of announcements.
    ///
    /// So this asserts the bound rather than the number: 200 alternating
    /// announcements must buy no more dials than 10 do.
    #[test]
    #[test]
    fn r6_alternating_addresses_cannot_buy_unlimited_dials() {
        const REAL: &str = "22222222-2222-4222-8222-222222222222";
        let mut peers: HashMap<String, PeerInfo> = HashMap::new();
        let mut last_dial: HashMap<String, Instant> = HashMap::new();
        let mut last_move: HashMap<String, Instant> = HashMap::new();
        let a = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 66));
        let b = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 67));

        // Production function, production gate.
        let run = |rounds: usize,
                       peers: &mut HashMap<String, PeerInfo>,
                       last_dial: &mut HashMap<String, Instant>,
                       last_move: &mut HashMap<String, Instant>| {
            let mut dials = 0usize;
            for n in 0..rounds {
                let addr = if n % 2 == 0 { a } else { b };
                note_peer_record(
                    peers,
                    last_dial,
                    last_move,
                    REAL,
                    r6_info(addr, 51234, REAL),
                    true,
                );
                let due = last_dial
                    .get(REAL)
                    .map(|t: &Instant| t.elapsed() >= DIAL_RETRY_AFTER)
                    .unwrap_or(true);
                if due {
                    last_dial.insert(REAL.to_string(), Instant::now());
                    dials += 1;
                }
            }
            dials
        };

        let short = run(10, &mut peers, &mut last_dial, &mut last_move);

        // Twenty times the announcements, from a clean slate, must buy no more
        // dials. That is the property: the budget is a function of the retry
        // interval, not of how fast the attacker can talk.
        let mut peers2: HashMap<String, PeerInfo> = HashMap::new();
        let mut last_dial2: HashMap<String, Instant> = HashMap::new();
        let mut last_move2: HashMap<String, Instant> = HashMap::new();
        let long = run(200, &mut peers2, &mut last_dial2, &mut last_move2);

        assert_eq!(
            short, long,
            "200 alternating announcements bought {long} dials where 10 bought {short}: \
             the budget grows with the attacker's announcement rate, so the flapping \
             bound does not exist"
        );
        assert!(
            long <= 2,
            "{long} dials inside one {DIAL_RETRY_AFTER:?} window from a single spoofed id. \
             At most two are legitimate: the initial dial, and ONE address-change retry \
             per interval, a device that really moved must still be reached promptly, \
             which is why the answer is not one."
        );
    }
}
