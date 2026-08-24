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
use super::replicate::{self, Kinds};
use super::wire_tcp::{read_byte, read_frame, write_byte, write_frame, MODE_PAIR, MODE_SESSION};
use echokey_core::history::Store;

/// How long we will sit in a blocking read on an unauthenticated socket.
/// Without this a peer that connects and says nothing pins a thread forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Concurrent inbound handlers. One thread per connection with no ceiling lets
/// anyone on the LAN exhaust the process by opening sockets; a paired peer only
/// ever needs one.
const MAX_INBOUND: usize = 8;

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
}

pub struct SyncManager {
    inner: Mutex<Inner>,
    app: AppHandle,
    stop: Arc<AtomicBool>,
    store: Arc<Mutex<Store>>,
    inbound: Arc<AtomicUsize>,
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
            }),
            app,
            stop: Arc::new(AtomicBool::new(false)),
            store,
            inbound: Arc::new(AtomicUsize::new(0)),
        });
        if s.enabled {
            m.clone().start();
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
        }
    }

    fn publish(&self) {
        let status = self.status();
        let _ = self.app.emit("sync-status", status);
    }

    // -- lifecycle ------------------------------------------------------------

    /// Bind the listener, start advertising, and start browsing.
    pub fn start(self: Arc<Self>) {
        {
            let i = self.inner.lock();
            if i.discovery.is_some() {
                return; // already running
            }
        }
        let listener = match TcpListener::bind("0.0.0.0:0") {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("sync: cannot bind a listener ({e}); sync stays off");
                return;
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
                tracing::warn!("sync: device id is unusable ({e}); sync stays off");
                return;
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
                                    let known = i.paired.iter().any(|d| d.id == id);
                                    let fresh = i.peers.insert(id.clone(), p).is_none();
                                    // Only dial on first sight, so a record
                                    // refresh does not start a new exchange
                                    // every few seconds.
                                    if known && fresh {
                                        let me3 = me.clone();
                                        std::thread::spawn(move || me3.dial(id));
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
                // A laptop with no network is normal; sync simply finds nobody.
                tracing::info!("sync: discovery unavailable ({e}); pairing by hand only");
            }
        }

        let me = self.clone();
        std::thread::Builder::new()
            .name("echokey-sync-listen".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    if me.stop.load(Ordering::SeqCst) {
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
            })
            .ok();

        self.publish();
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let mut i = self.inner.lock();
        i.discovery = None; // Drop shuts the daemon down
        i.peers.clear();
        i.guard.cancel();
        drop(i);
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
        // Charge the guess up front. Reading the code and checking the budget
        // afterwards is a TOCTOU race: concurrent connections would each read
        // the same live code and each get a free guess.
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

        match pair_flow::run(&mut s, PairingRole::Initiator, &code, (&me_id, &me_name)) {
            Ok(p) => {
                // Correct code: refund the reserved guess and close the window.
                self.inner.lock().guard.succeed();
                self.complete_pairing(p);
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
        // A session lives longer than a handshake; drop the short timeout.
        let _ = s.set_read_timeout(None);
        match echokey_sync::Session::accept(s, &key) {
            Ok(session) => self.run_session(peer_id, session),
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
        self.complete_pairing(p);
        Ok(())
    }

    fn complete_pairing(&self, p: pair_flow::Paired) {
        if let Err(e) = keystore::store(&p.device_id, &p.key) {
            tracing::error!("sync: could not save the paired key: {e}");
            return;
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
    }

    pub fn unpair(&self, device_id: &str) -> Result<(), String> {
        // Destroy the key first: if that fails we must not tell the user the
        // device is gone while the secret is still on disk.
        keystore::delete(device_id).map_err(|e| e.to_string())?;
        self.inner.lock().paired.retain(|d| d.id != device_id);
        self.publish();
        Ok(())
    }

    pub fn set_enabled(self: &Arc<Self>, on: bool) {
        {
            let mut i = self.inner.lock();
            if i.enabled == on {
                return;
            }
            i.enabled = on;
        }
        if on {
            self.stop.store(false, Ordering::SeqCst);
            self.clone().start();
        } else {
            self.stop();
        }
        self.publish();
    }

    pub fn set_device_name(&self, name: &str) {
        self.inner.lock().device_name = name.to_string();
        self.publish();
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
        // The store lock is taken inside the exchange, per statement, never
        // held across a socket read.
        match replicate::exchange(&mut session, &self.store, (&me_id, &me_name), kinds) {
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
        let _ = s.set_read_timeout(None);
        match echokey_sync::Session::initiate(s, &key) {
            Ok(session) => self.run_session(peer_id, session),
            Err(e) => tracing::info!("sync: handshake with {peer_id} failed: {e}"),
        }
    }
}
