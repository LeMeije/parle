//! A DIAGNOSTIC, not a unit test. Run it deliberately:
//!
//! ```text
//! cargo test -p echokey-sync --test mdns_field_check -- --ignored --nocapture
//! ```
//!
//! It is `#[ignore]` because it puts real packets on the real network and its
//! result depends on the machine it runs on — a firewall, a captive network, or
//! a declined macOS local-network prompt all make it fail correctly. That is
//! information, not a broken build, so it must not sit in the ordinary suite.
//!
//! Why it exists: the sync feature has never run between two physical machines,
//! and `docs/SYNC_HANDOVER.md` records mDNS on macOS as the biggest unknown.
//! macOS runs its own mDNSResponder while the `mdns-sd` crate runs its own
//! stack, and macOS 14+ filters an app's mDNS traffic unless `Info.plist`
//! declares `NSBonjourServices` — silently, with discovery reporting no error
//! and simply never finding anyone. This answers "does the stack work here at
//! all" before anyone spends an afternoon on "why can the two machines not see
//! each other".
//!
//! Two independent `Discovery` instances, in one process, on this machine's
//! real interfaces. If they cannot find each other, two machines will not
//! either.

use std::time::{Duration, Instant};

use echokey_sync::{Discovery, DiscoveryConfig, DiscoveryEvent, DeviceId};

/// Hard bound. A discovery that has not happened in this long has not happened.
const WINDOW: Duration = Duration::from_secs(20);

#[test]
#[ignore = "diagnostic: puts real mDNS packets on the real network; run deliberately"]
fn mdns_two_instances_on_this_machine_can_find_each_other() {
    let one = DeviceId::parse("11111111-1111-4111-8111-111111111111").unwrap();
    let two = DeviceId::parse("22222222-2222-4222-8222-222222222222").unwrap();

    let (d1, rx1) = Discovery::start(&DiscoveryConfig {
        device_id: one.clone(),
        device_name: "Field check one".into(),
        port: 45_101,
    })
    .expect("mDNS daemon would not start");

    let (d2, rx2) = Discovery::start(&DiscoveryConfig {
        device_id: two.clone(),
        device_name: "Field check two".into(),
        port: 45_102,
    })
    .expect("mDNS daemon would not start");

    // Each must see the OTHER. Seeing only yourself proves the loopback path
    // and nothing about whether a second machine could ever be found.
    let mut one_saw_two = false;
    let mut two_saw_one = false;
    let deadline = Instant::now() + WINDOW;

    while Instant::now() < deadline && !(one_saw_two && two_saw_one) {
        let left = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(500));
        if let Ok(DiscoveryEvent::PeerFound(p)) = rx1.recv_timeout(left) {
            println!("instance one saw: {} ({}) at {}", p.name, p.id.short(), p.socket_addr());
            if p.id == two {
                one_saw_two = true;
            }
        }
        if let Ok(DiscoveryEvent::PeerFound(p)) = rx2.try_recv() {
            println!("instance two saw: {} ({}) at {}", p.name, p.id.short(), p.socket_addr());
            if p.id == one {
                two_saw_one = true;
            }
        }
    }

    drop(d1);
    drop(d2);

    assert!(
        one_saw_two && two_saw_one,
        "mDNS discovery did not work on this machine inside {WINDOW:?} \
         (one saw two: {one_saw_two}, two saw one: {two_saw_one}).\n\
         On macOS 14+ check, in order:\n\
         1. Info.plist declares NSBonjourServices = [_echokey._tcp] and \
            NSLocalNetworkUsageDescription — without them the OS filters this \
            silently and discovery just never finds anyone;\n\
         2. System Settings > Privacy & Security > Local Network has the app \
            (or your terminal, when running this test) switched ON;\n\
         3. the application firewall is not blocking incoming connections;\n\
         4. the network is not a guest/AP-isolated Wi-Fi, which blocks multicast."
    );
}
