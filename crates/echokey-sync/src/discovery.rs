//! LAN discovery over mDNS: advertise `_echokey._tcp` and browse for peers.
//!
//! Discovery is *unauthenticated by nature*. mDNS records are unsigned, so
//! anything reported here is a hint about where to dial, never proof of who is
//! there. Identity is settled by [`crate::pairing`] and [`crate::session`].
//!
//! Nothing here polls. The mDNS daemon owns its own socket thread; we own one
//! thread that blocks on its event channel and translates events for the app.
//! Both stop when [`Discovery::shutdown`] is called (or the value is dropped).
//!
//! A machine with no usable network is a normal, expected state — a laptop on a
//! plane. Every failure path returns an error; none of them panic.

use std::collections::HashMap;
use std::net::IpAddr;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::identity::{validate_device_name, DeviceId, IdentityError, PeerInfo};
use crate::wire::PROTOCOL_VERSION;

/// The service type both devices advertise and browse.
pub const SERVICE_TYPE: &str = "_echokey._tcp.local.";

/// TXT key carrying the device's UUID.
pub const TXT_KEY_ID: &str = "id";
/// TXT key carrying the friendly device name.
pub const TXT_KEY_NAME: &str = "name";
/// TXT key carrying the replication protocol version.
pub const TXT_KEY_VERSION: &str = "v";

/// How many discovery events we buffer for a slow consumer before dropping the
/// oldest news on the floor. mDNS re-announces, so a dropped event is
/// self-healing; an unbounded queue fed by the LAN is not.
const EVENT_QUEUE: usize = 128;

/// How long we wait for the mDNS daemon to confirm it has stopped.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("mDNS error: {0}")]
    Mdns(#[from] mdns_sd::Error),
    #[error(transparent)]
    Identity(#[from] IdentityError),
}

/// What this device advertises about itself.
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    pub device_id: DeviceId,
    pub device_name: String,
    /// TCP port the sync listener is bound to.
    pub port: u16,
}

/// Peer arrivals and departures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// A peer was resolved. May repeat for the same peer as records refresh.
    PeerFound(PeerInfo),
    /// A peer's advertisement went away.
    PeerLost(DeviceId),
}

/// A running advertise + browse pair. Dropping it stops both.
pub struct Discovery {
    daemon: Option<ServiceDaemon>,
    fullname: String,
    worker: Option<JoinHandle<()>>,
}

impl Discovery {
    /// Start advertising and browsing.
    ///
    /// Returns the handle plus the channel discovery events arrive on. Dropping
    /// the receiver stops the worker thread at its next event.
    pub fn start(
        config: &DiscoveryConfig,
    ) -> Result<(Self, Receiver<DiscoveryEvent>), DiscoveryError> {
        validate_device_name(&config.device_name)?;

        let daemon = ServiceDaemon::new()?;

        let mut txt = HashMap::new();
        txt.insert(TXT_KEY_ID.to_string(), config.device_id.to_string());
        txt.insert(TXT_KEY_NAME.to_string(), config.device_name.clone());
        txt.insert(TXT_KEY_VERSION.to_string(), PROTOCOL_VERSION.to_string());

        // `()` for the addresses plus `enable_addr_auto` lets mdns-sd track the
        // host's real addresses as interfaces come and go — which is what makes
        // "started on no network, then joined wifi" work without a restart.
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name(config),
            &host_name(&config.device_id),
            (),
            config.port,
            txt,
        )?
        .enable_addr_auto();
        let fullname = service.get_fullname().to_string();

        daemon.register(service)?;
        let events = daemon.browse(SERVICE_TYPE)?;

        let (tx, rx) = bounded(EVENT_QUEUE);
        let own_id = config.device_id.clone();
        let worker = std::thread::Builder::new()
            .name("echokey-sync-discovery".into())
            .spawn(move || translate(events, tx, own_id))
            .map_err(|e| DiscoveryError::Mdns(mdns_sd::Error::Msg(e.to_string())))?;

        Ok((
            Self {
                daemon: Some(daemon),
                fullname,
                worker: Some(worker),
            },
            rx,
        ))
    }

    /// Stop advertising, stop browsing, join the worker thread.
    pub fn shutdown(mut self) -> Result<(), DiscoveryError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), DiscoveryError> {
        if let Some(daemon) = self.daemon.take() {
            // Best effort: tell the LAN we are going away. If the network is
            // already gone this fails, and that is fine.
            if let Err(e) = daemon.unregister(&self.fullname) {
                tracing::debug!("mdns unregister failed: {e}");
            }
            match daemon.shutdown() {
                // Bounded wait, so a wedged daemon cannot wedge app shutdown.
                Ok(done) => {
                    let _ = done.recv_timeout(SHUTDOWN_GRACE);
                }
                Err(e) => tracing::debug!("mdns shutdown failed: {e}"),
            }
        }
        if let Some(worker) = self.worker.take() {
            // The worker exits as soon as the daemon drops its event sender —
            // but only if it does. Joining unconditionally undid the bounded
            // wait immediately above it: this runs on the main thread during
            // RunEvent::Exit, so a daemon that never dropped its sender meant
            // the app simply never quit.
            //
            // Detaching costs nothing here. The thread holds no lock we care
            // about and the process is going away; the only thing that mattered
            // was not blocking on it.
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                tracing::debug!("mdns worker still running at shutdown; detaching rather than blocking");
            }
        }
        Ok(())
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Blocking translation loop: mDNS events in, [`DiscoveryEvent`] out.
/// Fullname-to-device-id entries kept while translating mDNS events.
///
/// Every one comes from an unauthenticated announcement, so this is a bound on
/// what the LAN can make us hold. Generous next to any real network.
const MAX_TRACKED_RECORDS: usize = 512;

fn translate(
    events: mdns_sd::Receiver<ServiceEvent>,
    tx: Sender<DiscoveryEvent>,
    own_id: DeviceId,
) {
    // mDNS removals identify a service by fullname, not by our device id, so we
    // remember which id each fullname resolved to.
    //
    // Bounded, and it has to be: every entry is created by an unauthenticated
    // announcement from the network, keyed by a name the announcer chooses, and
    // removed only by a goodbye that same announcer sends. Anyone on the LAN
    // could otherwise make this grow until the process ran out of memory.
    //
    // Past the cap the OLDEST entry goes. Losing one costs a `PeerLost` event
    // for a device that later disappears — the peer simply lingers in the UI
    // list until the next restart — which is a far smaller problem than an
    // unbounded map.
    let mut known: HashMap<String, DeviceId> = HashMap::new();
    let mut order: std::collections::VecDeque<String> = std::collections::VecDeque::new();

    // Blocks. Ends when the daemon shuts down and drops its sender.
    while let Ok(event) = events.recv() {
        let out = match event {
            ServiceEvent::ServiceResolved(service) => match peer_from(&service) {
                Some(peer) if peer.id != own_id => {
                    let name = service.get_fullname().to_string();
                    if known.insert(name.clone(), peer.id.clone()).is_none() {
                        order.push_back(name);
                        while order.len() > MAX_TRACKED_RECORDS {
                            if let Some(old) = order.pop_front() {
                                known.remove(&old);
                            }
                        }
                    }
                    DiscoveryEvent::PeerFound(peer)
                }
                // Our own advertisement, or a record that is not one of ours.
                _ => continue,
            },
            ServiceEvent::ServiceRemoved(_, fullname) => match known.remove(&fullname) {
                Some(id) => DiscoveryEvent::PeerLost(id),
                None => continue,
            },
            _ => continue,
        };

        match tx.try_send(out) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                tracing::debug!("discovery consumer is behind; dropping an event")
            }
            // Nobody is listening any more: stop the thread rather than spin.
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
}

/// Build a [`PeerInfo`] from a resolved record, or `None` if it is not a
/// well-formed EchoKey advertisement.
fn peer_from(service: &ResolvedService) -> Option<PeerInfo> {
    let props = service.get_properties();
    let id = DeviceId::parse(props.get_property_val_str(TXT_KEY_ID)?).ok()?;
    let name = props.get_property_val_str(TXT_KEY_NAME)?.to_string();
    validate_device_name(&name).ok()?;
    let addr = preferred_addr(service)?;
    Some(PeerInfo {
        id,
        name,
        addr,
        port: service.get_port(),
    })
}

/// Pick one address to dial: routable IPv4 first (what a home LAN actually
/// uses), then any IPv4, then anything at all.
/// Is this an address we are willing to dial?
///
/// mDNS records are unsigned, so the address in one is simply whatever the
/// sender put there. Without this check a spoofed announcement carrying a
/// paired device's id and an arbitrary A record sent us off to any host and
/// port the attacker liked — including a public one, which breaks the
/// "LAN-local only, no relay" rule outright, and which we would then hand our
/// device id to in cleartext before the Noise handshake.
///
/// The handshake still protects the CONTENTS: an attacker without the paired
/// key gets nothing out of the connection. This is about not making it in the
/// first place.
fn is_lan_addr(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        // Unique-local and link-local only; no global IPv6.
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

fn preferred_addr(service: &ResolvedService) -> Option<IpAddr> {
    let addrs: Vec<IpAddr> = service
        .get_addresses()
        .iter()
        .map(|ip| ip.to_ip_addr())
        .filter(|ip| !ip.is_loopback() && is_lan_addr(ip))
        .collect();
    addrs
        .iter()
        .find(|ip| ip.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
}

/// mDNS instance name. Unique per device (the id suffix) but still readable in
/// a generic Bonjour browser.
fn instance_name(config: &DiscoveryConfig) -> String {
    // A DNS label is at most 63 bytes; leave room for the "-xxxxxxxx" suffix.
    let mut name = config.device_name.clone();
    name.truncate(
        (0..=name.len().min(54))
            .rev()
            .find(|i| name.is_char_boundary(*i))
            .unwrap_or(0),
    );
    format!("{}-{}", name, config.device_id.short())
}

/// SRV target for our own advertisement. Derived from the device id so it is
/// stable and cannot collide with another EchoKey install.
fn host_name(id: &DeviceId) -> String {
    format!("echokey-{}.local.", id.short())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DiscoveryConfig {
        DiscoveryConfig {
            device_id: DeviceId::parse("3f2b1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d").unwrap(),
            device_name: "Ben's G14".into(),
            port: 51_234,
        }
    }

    #[test]
    fn starts_and_stops_without_panicking() {
        // On a machine with no usable network the daemon may refuse to start.
        // That is a legitimate outcome; what must never happen is a panic.
        match Discovery::start(&config()) {
            Ok((discovery, events)) => {
                assert!(events.try_recv().is_err(), "no peer should exist yet");
                discovery.shutdown().expect("clean shutdown");
            }
            Err(e) => eprintln!("discovery unavailable on this host: {e}"),
        }
    }

    #[test]
    fn dropping_the_handle_also_stops_it() {
        if let Ok((discovery, _events)) = Discovery::start(&config()) {
            drop(discovery);
        }
    }

    #[test]
    fn instance_and_host_names_are_legal_labels() {
        let mut config = config();
        config.device_name = "x".repeat(crate::identity::MAX_DEVICE_NAME_BYTES);
        let instance = instance_name(&config);
        assert!(
            instance.len() <= 63,
            "instance label was {}",
            instance.len()
        );
        assert!(instance.ends_with("-3f2b1c4d"));
        assert_eq!(host_name(&config.device_id), "echokey-3f2b1c4d.local.");
    }

    #[test]
    fn a_multibyte_device_name_is_truncated_on_a_char_boundary() {
        let mut config = config();
        config.device_name = "é".repeat(32); // 64 bytes
        let instance = instance_name(&config);
        assert!(instance.len() <= 63);
        assert!(instance.is_char_boundary(instance.len() - 9));
    }
}

// ---------------------------------------------------------------------------
// ADVERSARIAL REVIEW (round 4) — demonstration of a live finding. NOT a fix.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod adversarial_round4 {
    use super::*;
    use mdns_sd::{ResolvedService, ServiceEvent};

    /// The fullname an attacker chooses for its `n`th announcement. A DNS
    /// instance label may be up to 63 bytes, so this is a fraction of what one
    /// record can actually cost us.
    fn fullname(n: usize) -> String {
        format!("evil-{n:010}.{SERVICE_TYPE}")
    }

    fn resolved(n: usize) -> ResolvedService {
        let id = format!("{n:08x}-0000-4000-8000-000000000000");
        let mut props = HashMap::new();
        props.insert(TXT_KEY_ID.to_string(), id);
        props.insert(TXT_KEY_NAME.to_string(), "Peer".to_string());
        props.insert(TXT_KEY_VERSION.to_string(), PROTOCOL_VERSION.to_string());
        ServiceInfo::new(
            SERVICE_TYPE,
            &format!("evil-{n:010}"),
            &format!("evil-{n}.local."),
            "192.168.1.66",
            5000,
            props,
        )
        .expect("a well-formed advertisement")
        .as_resolved_service()
    }

    /// FINDING (round 4, MEDIUM): `translate`'s `known` map is unbounded, and
    /// every entry in it is chosen by whoever is on the LAN.
    ///
    /// mDNS records are unsigned. `translate` (discovery.rs:174-210) inserts one
    /// `fullname -> DeviceId` entry per resolved service and removes it only on
    /// a `ServiceRemoved` that the ADVERTISER sends. An attacker announcing
    /// distinct instance names simply never sends one, so the map grows for as
    /// long as it keeps talking, at a size it picks (a DNS instance label is up
    /// to 63 bytes and the id is a 36-byte String).
    ///
    /// `manager.rs` caps its own peer map at `MAX_PEERS` for exactly this
    /// reason — "anyone on the LAN can advertise unlimited records; without a
    /// cap both the map and the JSON we push to the webview grow without bound"
    /// (manager.rs:67-70). That cap is applied one layer ABOVE this map, so it
    /// does not bound it. The channel between the two is bounded
    /// (`EVENT_QUEUE`), which only means the events are dropped downstream while
    /// the entry is retained here anyway.
    ///
    /// The test proves absence of any cap without measuring memory: it feeds
    /// `RECORDS` announcements, then a goodbye for every one of them, and counts
    /// the `PeerLost` events. An entry can only produce `PeerLost` if it was
    /// still being held, so `RECORDS` of them means nothing was ever evicted.
    /// Drives the real `translate` over real channels.
    #[test]
    fn r4_the_fullname_map_is_unbounded_and_lan_controlled() {
        const RECORDS: usize = 100_000;
        /// Conservative bytes retained per entry: fullname + id + map overhead.
        const PER_ENTRY: usize = 64 + 36 + 48;

        let (tx_ev, rx_ev) = flume::unbounded::<ServiceEvent>();
        let (tx_out, rx_out) = crossbeam_channel::bounded(EVENT_QUEUE);
        let own = DeviceId::parse("ffffffff-0000-4000-8000-000000000000").unwrap();

        // Drain continuously so the bounded channel is never what stops this.
        let counter = std::thread::spawn(move || {
            let (mut found, mut lost) = (0usize, 0usize);
            // Hard bound: at most one event per message we ever send.
            for _ in 0..(RECORDS * 2 + 16) {
                match rx_out.recv() {
                    Ok(DiscoveryEvent::PeerLost(_)) => lost += 1,
                    Ok(DiscoveryEvent::PeerFound(_)) => found += 1,
                    Err(_) => break,
                }
            }
            (found, lost)
        });

        for n in 0..RECORDS {
            tx_ev
                .send(ServiceEvent::ServiceResolved(Box::new(resolved(n))))
                .unwrap();
        }
        for n in 0..RECORDS {
            tx_ev
                .send(ServiceEvent::ServiceRemoved(
                    SERVICE_TYPE.to_string(),
                    fullname(n),
                ))
                .unwrap();
        }
        drop(tx_ev);

        translate(rx_ev, tx_out, own);
        let (found, lost) = counter.join().expect("counter thread");

        // Guard against a false pass: if the synthesised records were not
        // recognised at all, nothing would ever be inserted and `lost` would be
        // zero for the wrong reason.
        // A floor, not an equality. The output channel is bounded and
        // `translate` uses `try_send`, so under load a handful of PeerFounds are
        // legitimately dropped; demanding all 100,000 made this test fail in a
        // busy suite and pass on its own. The floor still cannot be met if the
        // synthesised records were not recognised at all, which is the false
        // pass it is here to prevent: that case yields zero.
        assert!(
            found > RECORDS / 2,
            "precondition: every synthesised announcement must be a well-formed \
             EchoKey record that translate actually accepts, but only {found} of \
             {RECORDS} were"
        );

        // A cap of any sane size (MAX_PEERS, the equivalent one layer up, is 64)
        // would have evicted almost everything, leaving almost nothing still
        // held to lose. A tenth is a generous threshold; the observed figure is
        // essentially all of them, the handful missing being `try_send` drops on
        // the bounded channel.
        // Measured against `found`, not `RECORDS`. A dropped `try_send` lowers
        // both counts together, so the ratio survives it; a cap crushes `lost`
        // alone, which is the thing being asserted.
        assert!(
            lost < found / 10,
            "{lost} of {RECORDS} attacker-chosen mDNS instance names were still \
             being held when their goodbyes arrived, so nothing is ever evicted: \
             `known` in discovery.rs:181 has no cap. At roughly {PER_ENTRY} bytes \
             an entry that is ~{} MiB of retained state bought with nothing but \
             multicast announcements.",
            lost * PER_ENTRY / (1024 * 1024)
        );
    }
}
