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

use echokey_sync::{
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
use echokey_core::history::Store;

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
/// Read deadline once a session is authenticated. Longer than a handshake,
/// because a real exchange can be large, but never unbounded.
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);
/// Concurrent outbound dials. A flapping (or spoofed) mDNS record would
/// otherwise spawn a thread per sighting, without bound.
const MAX_DIALS: usize = 4;
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
trait ReleasesDial: Send + Sync {
    fn release_dial(&self, id: &str);
}

impl ReleasesDial for SyncManager {
    fn release_dial(&self, id: &str) {
        self.inner.lock().dialing.remove(id);
    }
}

/// Releases an outbound dial slot on drop, including on unwind.
struct DialGuard {
    owner: Arc<dyn ReleasesDial>,
    id: String,
}

impl Drop for DialGuard {
    fn drop(&mut self) {
        self.owner.release_dial(&self.id);
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
    pub last_seen: Option<i64>,
    pub online: bool,
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

struct Inner {
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
    /// Peers we owe one full re-offer of our history, because the user widened
    /// what this machine shares. See `set_kinds`.
    resend_owed: std::collections::HashMap<String, i64>,
    /// Set for the whole of start(), which binds a port and brings up mDNS and
    /// is far too slow to leave the "already running" test unguarded.
    starting: bool,
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
    settings: Arc<Mutex<echokey_core::settings::Settings>>,
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
        s: &echokey_core::settings::SyncSettings,
        store: Arc<Mutex<Store>>,
        settings: Arc<Mutex<echokey_core::settings::Settings>>,
        retention_days: u32,
    ) -> Arc<Self> {
        let m = Arc::new(Self {
            inner: Mutex::new(Inner {
                enabled: s.enabled,
                device_id: s.device_id.clone(),
                device_name: s.device_name.clone(),
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
                    })
                    .collect(),
                guard: PairingGuard::new(),
                discovery: None,
                port: 0,
                listen_stop: None,
                dialing: std::collections::HashSet::new(),
                // Restored, so a debt taken on before a quit is still owed.
                resend_owed: s
                    .resend_owed
                    .iter()
                    .map(|d| (d.device_id.clone(), d.from))
                    .collect(),
                starting: false,
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
        {
            let mut i = self.inner.lock();
            if i.listen_stop.is_some() || i.starting {
                return Ok(()); // already running, or another thread is bringing it up
            }
            i.starting = true;
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

        let id = match echokey_sync::DeviceId::parse(&device_id) {
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
                    .name("echokey-sync-discovery".into())
                    .spawn(move || {
                        for ev in rx {
                            let mut i = me.inner.lock();
                            match ev {
                                DiscoveryEvent::PeerFound(p) => {
                                    let id = p.id.as_str().to_string();
                                    if i.peers.len() >= MAX_PEERS && !i.peers.contains_key(&id) {
                                        continue;
                                    }
                                    let known = i.paired.iter().any(|d| d.id == id);
                                    let fresh = i.peers.insert(id.clone(), p).is_none();
                                    // Only dial on first sight, so a record
                                    // refresh does not start a new exchange
                                    // every few seconds.
                                    // First sight only, paired only, one dial
                                    // in flight per peer, and a hard cap. mDNS
                                    // is unsigned: a spoofed record flapping
                                    // between goodbye and announce would
                                    // otherwise spawn threads without bound.
                                    let room = i.dialing.len() < MAX_DIALS;
                                    if known && fresh && room && i.dialing.insert(id.clone()) {
                                        let me3 = me.clone();
                                        std::thread::spawn(move || {
                                            // RAII, for the same reason the
                                            // inbound path uses SlotGuard.
                                            // Removing the entry as a plain
                                            // statement after dial() meant any
                                            // unwind inside keystore::load,
                                            // the handshake or the exchange
                                            // skipped it: that peer was then
                                            // never dialled again for the life
                                            // of the process, and four such
                                            // panics exhausted MAX_DIALS so the
                                            // machine dialled nobody at all.
                                            let _slot = DialGuard {
                                                owner: me3.clone(),
                                                id: id.clone(),
                                            };
                                            me3.clone().dial(id.clone());
                                        });
                                    }
                                }
                                DiscoveryEvent::PeerLost(id) => {
                                    i.peers.remove(id.as_str());
                                }
                            }
                            drop(i);
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
            .name("echokey-sync-listen".into())
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
                            if me.inbound.load(Ordering::SeqCst) >= MAX_INBOUND {
                                tracing::debug!("sync: refusing connection, {MAX_INBOUND} already in flight");
                                drop(s); // closes it; the peer can retry
                                continue;
                            }
                            // A per-address share on top of the global budget.
                            // Without it the global budget is first-come, so
                            // eight sockets that connect and say nothing hold
                            // every slot for the whole timeout — and can be
                            // reopened forever, closing the listener to every
                            // real peer for eight sockets and zero bytes.
                            let src = s.peer_addr().map(|a| a.ip()).ok();
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
                            std::thread::spawn(move || {
                                // RAII: released on every exit including a
                                // panic. Decrementing after the call would leak
                                // a slot per unwind, and eight leaks close the
                                // listener to everyone.
                                let _slot = slot;
                                let _global = _global;
                                me2.serve(s);
                            });
                        }
                        Err(e) => tracing::debug!("sync: accept failed: {e}"),
                    }
                }
                tracing::info!("sync: listener stopped");
            });
        if let Err(e) = spawned {
            let msg = format!("Could not start the sync listener: {e}");
            self.fail(&msg);
            return Err(msg);
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
        self.inner.lock().enabled = false;
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
        // Take the daemon OUT under the lock and drop it outside. Discovery's
        // Drop unregisters over the network and joins its worker with no bound;
        // running that while holding `inner` would wedge every publish() and
        // the UI thread along with it.
        let (discovery, gen_stop, port) = {
            let mut i = self.inner.lock();
            i.peers.clear();
            i.guard.cancel();
            // `dialing` is deliberately NOT cleared. Its entries are owned by
            // live DialGuards; clearing it here let a new generation insert the
            // same peer and then have the OLD guard's drop remove it, so two
            // concurrent dials to one device were possible and the set
            // under-counted against MAX_DIALS. The guards clean up by
            // themselves as their threads unwind.
            (i.discovery.take(), i.listen_stop.take(), i.port)
        };
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
        match echokey_sync::Session::accept(s, &key) {
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

        let p = pair_flow::run(&mut s, PairingRole::Responder, &code, (&me_id, &me_name))
            .map_err(|_| "That code did not match. Check the digits and try again.".to_string())?;
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
                existing.name = p.device_name;
                existing.last_seen = Some(now_ms());
            } else {
                i.paired.push(UiPaired {
                    id: p.device_id,
                    name: p.device_name,
                    last_seen: Some(now_ms()),
                    online: true,
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
        }
        self.persist();
        self.publish();
        Ok(())
    }

    pub fn set_enabled(self: &Arc<Self>, on: bool) -> Result<(), String> {
        {
            let mut i = self.inner.lock();
            if i.enabled == on {
                return Ok(());
            }
            i.enabled = on;
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

    pub fn set_device_name(&self, name: &str) {
        self.inner.lock().device_name = name.to_string();
        self.publish();
    }

    /// Kept in step with history.retention_days by apply_settings.
    pub fn set_retention_days(&self, days: u32) {
        self.retention_days.store(days as usize, Ordering::SeqCst);
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
        let (enabled, name, dictations, clipboard, paired) = self.persistable();
        // Gathered BEFORE the settings lock is taken. Every other path locks
        // `inner` first and `settings` second; reaching back into `inner` from
        // under the settings lock here would be a genuine order inversion.
        let owed: Vec<echokey_core::settings::ResendDebt> = self
            .inner
            .lock()
            .resend_owed
            .iter()
            .map(|(id, from)| echokey_core::settings::ResendDebt {
                device_id: id.clone(),
                from: *from,
            })
            .collect();
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
        if let Err(e) = s.save(&echokey_core::settings::settings_path()) {
            tracing::warn!("sync: could not persist sync settings: {e}");
        }
    }

    pub fn persistable(&self) -> (bool, String, bool, bool, Vec<echokey_core::settings::PairedDevice>) {
        let i = self.inner.lock();
        (
            i.enabled,
            i.device_name.clone(),
            i.dictations,
            i.clipboard,
            i.paired
                .iter()
                .map(|d| echokey_core::settings::PairedDevice {
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
        mut session: echokey_sync::Session<S>,
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
        let resend_from = self.inner.lock().resend_owed.get(&peer_id).copied();
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
                if resend_all {
                    let mut i = self.inner.lock();
                    if stats.truncated {
                        // Resume where it stopped. Restarting from zero meant a
                        // history larger than the cap could never finish: every
                        // pass re-sent the same batch and stopped in the same
                        // place.
                        let from = stats.resend_progress.unwrap_or(0);
                        i.resend_owed.insert(peer_id.clone(), from);
                        drop(i);
                        tracing::info!(
                            "sync: re-offer to {peer_id} hit the batch cap; resuming from {from}"
                        );
                    } else {
                        i.resend_owed.remove(&peer_id);
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
        match self.store.lock().prune(days, max) {
            Ok(0) => {}
            Ok(n) => tracing::debug!("sync: pruned {n} rows after an exchange"),
            Err(e) => tracing::warn!("sync: prune after exchange failed: {e}"),
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
        let key = match keystore::load(&peer_id) {
            Ok(Some(k)) => k,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!("sync: keychain unavailable: {e}");
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
        match echokey_sync::Session::initiate(Timed::new(s, deadline.clone()), &key) {
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
