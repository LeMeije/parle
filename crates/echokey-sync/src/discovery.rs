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
            // The worker exits as soon as the daemon drops its event sender.
            let _ = worker.join();
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
fn translate(
    events: mdns_sd::Receiver<ServiceEvent>,
    tx: Sender<DiscoveryEvent>,
    own_id: DeviceId,
) {
    // mDNS removals identify a service by fullname, not by our device id, so we
    // remember which id each fullname resolved to.
    let mut known: HashMap<String, DeviceId> = HashMap::new();

    // Blocks. Ends when the daemon shuts down and drops its sender.
    while let Ok(event) = events.recv() {
        let out = match event {
            ServiceEvent::ServiceResolved(service) => match peer_from(&service) {
                Some(peer) if peer.id != own_id => {
                    known.insert(service.get_fullname().to_string(), peer.id.clone());
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
fn preferred_addr(service: &ResolvedService) -> Option<IpAddr> {
    let addrs: Vec<IpAddr> = service
        .get_addresses()
        .iter()
        .map(|ip| ip.to_ip_addr())
        .collect();
    addrs
        .iter()
        .find(|ip| ip.is_ipv4() && !ip.is_loopback())
        .or_else(|| addrs.iter().find(|ip| ip.is_ipv4()))
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
