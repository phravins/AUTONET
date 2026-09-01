//! Checks that run against the machine's real network.
//!
//! Every test here is `#[ignore]`d. They exercise the one part of AutoNet that
//! genuinely cannot be tested from a fixture — the translation from kernel
//! structures into a `NetworkState` — and their results depend on which Wi-Fi
//! network the host happens to be on. Making CI depend on that would produce
//! failures that say nothing about the code.
//!
//! Run them deliberately:
//!
//! ```sh
//! cargo test -p autonet-platform -- --ignored --nocapture
//! ```

use autonet_core::model::{AddressScope, InterfaceKind};
use autonet_platform::provider;

#[test]
#[ignore = "queries the live network"]
fn the_snapshot_describes_a_plausible_machine() {
    let provider = provider().expect("a backend for this platform");
    let state = provider.snapshot().expect("a snapshot");

    assert_eq!(state.schema_version, autonet_core::SCHEMA_VERSION);
    assert!(
        state.captured_at.is_some(),
        "snapshot should be timestamped"
    );

    // Every operating system AutoNet supports has a loopback interface. If this
    // fails, the enumeration is broken rather than the machine being unusual.
    let loopback = state
        .interfaces
        .iter()
        .find(|i| i.kind == InterfaceKind::Loopback)
        .expect("a loopback interface");
    assert!(
        loopback
            .addresses
            .iter()
            .any(|a| a.scope == AddressScope::Loopback),
        "loopback carries no loopback address: {:?}",
        loopback.addresses
    );

    // Interface indexes are the join key for addresses and routes, so a
    // duplicate or a zero would silently corrupt the whole snapshot.
    let mut indexes: Vec<u32> = state.interfaces.iter().map(|i| i.index).collect();
    let count = indexes.len();
    indexes.sort_unstable();
    indexes.dedup();
    assert_eq!(indexes.len(), count, "duplicate interface indexes");
    assert!(!indexes.contains(&0), "interface index 0 is not valid");

    // Names must be unique too: config rules and `--interface` match on them.
    let mut names: Vec<&str> = state.interfaces.iter().map(|i| i.name.as_str()).collect();
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), count, "duplicate interface names");
}

#[test]
#[ignore = "queries the live network"]
fn every_route_points_at_an_interface_that_exists() {
    let state = provider().unwrap().snapshot().unwrap();

    for route in &state.routes {
        assert!(
            state.interface_by_index(route.interface_index).is_some(),
            "route {route:?} references a nonexistent interface"
        );
    }
}

#[test]
#[ignore = "queries the live network"]
fn addresses_agree_with_their_declared_family_and_scope() {
    use autonet_core::classify::classify_address;
    use autonet_core::model::Family;

    let state = provider().unwrap().snapshot().unwrap();

    for interface in &state.interfaces {
        for address in &interface.addresses {
            assert_eq!(
                address.family,
                Family::of(&address.ip),
                "{}: family disagrees with {}",
                interface.name,
                address.ip
            );
            assert_eq!(
                address.scope,
                classify_address(&address.ip),
                "{}: scope disagrees with {}",
                interface.name,
                address.ip
            );
            let max = if address.ip.is_ipv4() { 32 } else { 128 };
            assert!(
                address.prefix_len <= max,
                "{}: implausible prefix /{} on {}",
                interface.name,
                address.prefix_len,
                address.ip
            );
        }
    }
}

#[test]
#[ignore = "queries the live network"]
fn the_snapshot_survives_a_json_round_trip() {
    // The daemon and every SDK will consume exactly this encoding, so a live
    // snapshot containing something the fixtures do not cover must still
    // round-trip cleanly.
    let state = provider().unwrap().snapshot().unwrap();
    let encoded = serde_json::to_string(&state).unwrap();
    let decoded: autonet_core::model::NetworkState = serde_json::from_str(&encoded).unwrap();
    assert_eq!(state, decoded);
}

#[test]
#[ignore = "requires a connected machine"]
fn the_selected_address_is_one_the_kernel_agrees_with() {
    use autonet_core::config::SelectionConfig;
    use autonet_core::select::select_address;

    let state = provider().unwrap().snapshot().unwrap();
    let selected = select_address(&state, &SelectionConfig::default())
        .expect("this machine should have a usable IPv4 address");

    // The selected address must actually be bound to the interface AutoNet
    // named — the single most damaging kind of mistake, since the caller will
    // hand this pair to a bind() call.
    let interface = state
        .interface_by_name(&selected.interface)
        .expect("selected interface exists");
    assert!(
        interface.addresses.iter().any(|a| a.ip == selected.ip),
        "{} is not bound to {}",
        selected.ip,
        selected.interface
    );

    assert!(
        selected.scope.is_reachable_by_peers(),
        "{} is not an address another device can reach",
        selected.ip
    );

    println!("selected {} on {}", selected.ip, selected.interface);
}
