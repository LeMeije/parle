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
use super::replicate::{self, Attribution, Kinds, Retention};
use super::wire_tcp::{read_byte, read_frame, write_byte, write_frame, MODE_PAIR, MODE_SESSION};
use echokey_core::history::Store;

/// How long we will sit in a blocking read on an unauthenticated socket.
/// Without this a peer that connects and says nothing pins a thread forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Concurrent inbound handlers. One thread per connection with no ceiling lets
/// anyone on the LAN exhaust the process by opening sockets; a paired peer only
/// ever needs one.
const MAX_INBOUND: usize = 8;
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
    pairing_started: Option<Instant>,
    discovery: Option<Discovery>,
    port: u16,
    /// Stop flag for the CURRENT listener generation. Each start() gets its own
    /// so a listener left over from a previous enable cannot come back to life
    /// when the flag is cleared for a new one.
    listen_stop: Option<Arc<AtomicBool>>,
    /// Peers with a dial in flight, so a flapping record cannot spawn a thread
    /// per sighting.
    dialing: std::collections::HashSet<String>,
    /// Why sync is not working, surfaced to the UI instead of only the log.
    error: Option<String>,
}

pub struct SyncManager {
    inner: Mutex<Inner>,
    app: AppHandle,
    stop: Arc<AtomicBool>,
    store: Arc<Mutex<Store>>,
    inbound: Arc<AtomicUsize>,
    /// Mirrored from settings so the replication path never has to reach back
    /// into AppState (which would invert the lock order).
    retention_days: Arc<AtomicUsize>,
}

impl SyncManager {
    pub fn new(
        app: AppHandle,
        s: &echokey_core::settings::SyncSettings,
        store: Arc<Mutex<Store>>,
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
                pairing_started: None,
                discovery: None,
                port: 0,
                listen_stop: None,
                dialing: std::collections::HashSet::new(),
                error: None,
            }),
            app,
            stop: Arc::new(AtomicBool::new(false)),
            store,
            inbound: Arc::new(AtomicUsize::new(0)),
            retention_days: Arc::new(AtomicUsize::new(0)),
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
    pub fn start(self: Arc<Self>) -> Result<(), String> {
        {
            let i = self.inner.lock();
            if i.listen_stop.is_some() {
                return Ok(()); // already running
            }
        }
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
                                            me3.clone().dial(id.clone());
                                            me3.inner.lock().dialing.remove(&id);
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
                            me.inbound.fetch_add(1, Ordering::SeqCst);
                            let me2 = me.clone();
                            let slot = me.inbound.clone();
                            std::thread::spawn(move || {
                                me2.serve(s);
                                // Released however serve() exits, including on
                                // an early return, so a peer that connects and
                                // says nothing cannot leak a slot.
                                slot.fetch_sub(1, Ordering::SeqCst);
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

        self.inner.lock().error = None;
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
        self.stop.store(true, Ordering::SeqCst);
        // Take the daemon OUT under the lock and drop it outside. Discovery's
        // Drop unregisters over the network and joins its worker with no bound;
        // running that while holding `inner` would wedge every publish() and
        // the UI thread along with it.
        let (discovery, gen_stop, port) = {
            let mut i = self.inner.lock();
            i.peers.clear();
            i.guard.cancel();
            i.dialing.clear();
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
            }
        }
        self.publish();
    }

    // -- inbound --------------------------------------------------------------

    fn serve(self: Arc<Self>, mut s: TcpStream) {
        // Unauthenticated peer: never block on it indefinitely.
        let _ = s.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
        let _ = s.set_write_timeout(Some(HANDSHAKE_TIMEOUT));

        match read_byte(&mut s) {
            Ok(MODE_PAIR) => self.serve_pairing(s),
            Ok(MODE_SESSION) => self.serve_session(s),
            Ok(other) => tracing::debug!("sync: unknown mode byte {other:#04x}"),
            Err(e) => tracing::debug!("sync: no mode byte ({e})"),
        }
    }

    /// Someone is trying to pair with the code we are displaying.
    fn serve_pairing(self: Arc<Self>, mut s: TcpStream) {
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
        // Now charge it. Checking the budget only after the exchange completes
        // is a TOCTOU race: concurrent connections would each read the same
        // live code and each get a free guess.
        let code = {
            let mut i = self.inner.lock();
            match i.guard.reserve(Instant::now()) {
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

    fn serve_session(self: Arc<Self>, mut s: TcpStream) {
        // Peer announces which device it claims to be so we can find the key.
        let Ok(raw) = read_frame(&mut s) else { return };
        let Ok(peer_id) = String::from_utf8(raw) else { return };
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
        // The handshake is still unauthenticated work: keep the deadline ON
        // across it. Clearing it here previously let a peer that connects and
        // says nothing park this thread forever, and because the inbound slot
        // is only released when serve() returns, eight such packets saturated
        // MAX_INBOUND permanently and killed sync until restart.
        match echokey_sync::Session::accept(s, &key) {
            Ok(session) => {
                // Authenticated now, so a longer budget is reasonable — but
                // never unbounded: a peer that drops off the network mid
                // exchange without a FIN would otherwise hold this thread.
                let _ = session.get_ref().set_read_timeout(Some(SESSION_TIMEOUT));
                self.run_session(peer_id, session)
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
        i.pairing_started = Some(now);
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
                self.fail(&msg);
                return Err(msg);
            }
            if i.paired.iter().any(|d| d.id == p.device_id) {
                // Overwriting an existing entry would replace that device's key
                // and lock the real one out while attributing the newcomer's
                // rows to it. Unpair first, deliberately.
                let msg = format!(
                    "A device with that identity is already paired. Unpair \"{}\" first if you                      really mean to replace it.",
                    i.paired.iter().find(|d| d.id == p.device_id).map(|d| d.name.clone()).unwrap_or_default()
                );
                drop(i);
                self.fail(&msg);
                return Err(msg);
            }
        }
        if let Err(e) = keystore::store(&p.device_id, &p.key) {
            let msg = format!("Paired, but the key could not be saved to the keychain: {e}");
            self.fail(&msg);
            return Err(msg);
        }
        {
            let mut i = self.inner.lock();
            i.guard.cancel();
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
        self.publish();
        Ok(())
    }

    pub fn unpair(&self, device_id: &str) -> Result<(), String> {
        // Destroy the key first: if that fails we must not tell the user the
        // device is gone while the secret is still on disk.
        keystore::delete(device_id).map_err(|e| e.to_string())?;
        self.inner.lock().paired.retain(|d| d.id != device_id);
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
            self.stop.store(false, Ordering::SeqCst);
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

    pub fn set_kinds(&self, dictations: bool, clipboard: bool) {
        let mut i = self.inner.lock();
        i.dictations = dictations;
        i.clipboard = clipboard;
        drop(i);
        self.publish();
    }

    /// Snapshot of everything that belongs in settings.json.
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
    ) {
        let (me_id, me_name, kinds) = {
            let i = self.inner.lock();
            (
                i.device_id.clone(),
                i.device_name.clone(),
                Kinds { dictations: i.dictations, clipboard: i.clipboard },
            )
        };
        let retention = Retention { oldest_allowed: self.retention_floor() };
        // Only the handshake-proven peer, or another device we have paired
        // with, may author rows — and nobody may author rows as us.
        let known: Vec<String> = self.inner.lock().paired.iter().map(|d| d.id.clone()).collect();
        let attribution = Attribution { peer_id: &peer_id, local_id: &me_id, known: &known };
        // The store lock is taken inside the exchange, per statement, never
        // held across a socket read.
        match replicate::exchange(
            &mut session,
            &self.store,
            (&me_id, &me_name),
            kinds,
            retention,
            &attribution,
        ) {
            Ok(stats) => tracing::info!(
                "sync: {peer_id} sent {} items / {} tombstones, applied {} / {}, ignored {}",
                stats.sent_items,
                stats.sent_tombstones,
                stats.applied_items,
                stats.applied_tombstones,
                stats.ignored
            ),
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
        match echokey_sync::Session::initiate(s, &key) {
            Ok(session) => {
                let _ = session.get_ref().set_read_timeout(Some(SESSION_TIMEOUT));
                self.run_session(peer_id, session)
            }
            Err(e) => tracing::info!("sync: handshake with {peer_id} failed: {e}"),
        }
    }
}
