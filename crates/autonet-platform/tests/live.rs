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
//!
//! # What these tests may and may not assume
//!
//! Every assertion here has to hold on *any* machine that is working correctly.
//! "This host has Wi-Fi", "this host is online", "there is exactly one default
//! route" are all properties of a particular setup, and asserting one turns a
//! correctly-configured machine into a red test — a bug in the test, not in the
//! backend. Where a check only makes sense on some machines, the test either
//! narrows its scope (default routes only, non-tunnel interfaces only) or skips
//! with a printed explanation.
//!
//! # The macOS group
//!
//! The tests below marked `#[cfg(target_os = "macos")]` exist to falsify four
//! things the macOS routing parser currently asserts *from Apple's headers
//! rather than from a running system*: Darwin's four-byte sockaddr `ROUNDUP`,
//! the zero-length netmask of a default route, on-link gateways arriving as a
//! `sockaddr_dl`, and the `SIOCGIFAFLAG_IN6` union layout. Each of those fails
//! quietly — a wrong `ROUNDUP` produces plausible-looking wrong addresses, not
//! a panic — so the assertions are aimed at *garbage that still parses*: a
//! prefix that leaves host bits set, a gateway in the wrong family, a source
//! address bound to some other interface.
//!
//! One further cross-check, comparing `rtm_index` against `RTA_IFP`, cannot
//! live here: `Route` models a single interface index, so `RTA_IFP` never
//! crosses the platform boundary. It is an `#[ignore]`d unit test inside
//! `src/macos/route.rs` instead, and runs under the same command as these.

use autonet_core::model::{AddressScope, InterfaceKind, IpNetwork};
use autonet_platform::provider;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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

// ---------------------------------------------------------------------------
// Route shape — true of any kernel, so not gated
// ---------------------------------------------------------------------------

#[test]
#[ignore = "queries the live network"]
fn a_routes_destination_has_no_host_bits_below_its_prefix() {
    // `192.168.1.5/24` is not a network, it is an address with a prefix glued
    // to it, and it means the prefix and the destination were read from
    // different places. On macOS that is the single most likely symptom of the
    // netmask sockaddr being misread: the kernel truncates it to its
    // significant bytes, and counting the leading ones at the wrong offset
    // yields a prefix that has nothing to do with the destination beside it.
    let state = provider().unwrap().snapshot().unwrap();

    for route in &state.routes {
        let Some(network) = route.destination else {
            continue;
        };

        let max = match network.addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        assert!(
            network.prefix_len <= max,
            "route {route:?} claims a /{} prefix",
            network.prefix_len
        );

        assert_eq!(
            network_address(network),
            network.addr,
            "route {route:?} has host bits set below its own prefix"
        );

        // The family field and the destination must be the same family. They
        // come from different reads — one from the header's dump family, one
        // from the destination sockaddr — so a disagreement is a parse fault
        // rather than something a routing table can express.
        assert_eq!(
            autonet_core::model::Family::of(&network.addr),
            route.family,
            "route {route:?} declares a family its destination contradicts"
        );
    }
}

#[test]
#[ignore = "queries the live network"]
fn a_gateway_is_a_plausible_next_hop_in_the_routes_own_family() {
    // The gateway slot sits immediately after the destination in the sockaddr
    // walk, so it is the first thing to go wrong if the walk's stride is wrong.
    // `rtparse::route_parts` deliberately does *not* filter a gateway against
    // its route's family — its comment names this test as the reason — because
    // silently dropping a v6 gateway off a v4 route would hide exactly the bug
    // this is looking for.
    let state = provider().unwrap().snapshot().unwrap();

    for route in &state.routes {
        let Some(gateway) = route.gateway else {
            continue;
        };
        let interface = state
            .interface_by_index(route.interface_index)
            .expect("every route names an interface that exists");

        assert_eq!(
            autonet_core::model::Family::of(&gateway),
            route.family,
            "route {route:?} has a next hop in the wrong family"
        );

        // None of the following can be a next hop on any machine, correctly
        // configured or not. They are what arbitrary bytes look like once
        // something has read them as an address.
        assert!(
            !gateway.is_unspecified(),
            "route {route:?} has an unspecified next hop"
        );
        assert!(
            !gateway.is_multicast(),
            "route {route:?} has a multicast next hop"
        );
        match gateway {
            IpAddr::V4(v4) => {
                assert!(
                    !v4.is_broadcast(),
                    "route {route:?} has a broadcast next hop"
                );
                // `0.0.0.0/8` is "this network": never routable, and what a
                // small integer — an interface index, say — looks like once it
                // has been read as an IPv4 address.
                assert_ne!(
                    v4.octets()[0],
                    0,
                    "route {route:?} has a next hop inside 0.0.0.0/8"
                );
            }
            IpAddr::V6(_) => {}
        }
        assert!(
            !gateway.is_loopback() || interface.kind == InterfaceKind::Loopback,
            "route {route:?} sends traffic to a loopback address off {}",
            interface.name
        );
    }
}

#[test]
#[ignore = "queries the live network"]
fn a_default_route_normalises_and_names_a_source_bound_to_its_own_interface() {
    // Scoped to default routes so that an offline machine — no default route
    // at all — passes rather than failing on an empty table.
    let state = provider().unwrap().snapshot().unwrap();
    let mut seen = 0usize;

    for route in state.default_routes() {
        seen += 1;
        let interface = state
            .interface_by_index(route.interface_index)
            .expect("every route names an interface that exists");

        // Both backends normalise `0.0.0.0/0` and `::/0` to `None`, so
        // `autonet routes --json` reads identically on Linux and macOS. A
        // default route arriving as `Some(0.0.0.0/32)` would mean the mask was
        // read as a host route — the failure mode of getting the zero-length
        // netmask sockaddr wrong.
        assert!(
            route.destination.is_none(),
            "default route {route:?} was not normalised to a null destination"
        );

        // A default route out of a broadcast link needs a router to hand
        // packets to. Deliberately *not* asserted for point-to-point links: a
        // utun default route installed by WireGuard or Tailscale is routinely
        // on-link with no next hop, and demanding one there would fail on a Mac
        // that is working exactly as intended.
        if !interface.flags.point_to_point && interface.kind != InterfaceKind::Loopback {
            let Some(gateway) = route.gateway else {
                panic!("default route on {} has no next hop", interface.name);
            };

            // The next hop off a broadcast link is another machine — a router —
            // so it can never be an address this machine holds on that same
            // interface. If the sockaddr walk slipped by one slot, the gateway
            // would be read out of `RTA_IFA`, which is exactly the interface's
            // own address, and the result would otherwise look entirely
            // plausible. Point-to-point links are excluded above for a real
            // reason: macOS routinely installs a utun default route whose
            // gateway *is* the tunnel's own address, and asserting this there
            // would fail on a Mac with a VPN working correctly.
            assert!(
                !interface.addresses.iter().any(|a| a.ip == gateway),
                "default route on {} points its next hop at {gateway}, an address \
                 bound to that same interface",
                interface.name
            );
        }

        // The sharpest of the three. `RTA_IFA` is the *last* slot AutoNet
        // reads, so it is the furthest downstream of the zero-length netmask
        // that a wrong sockaddr stride would corrupt — and unlike a gateway,
        // its correct value is knowable: it must be an address this same
        // interface holds.
        if let Some(source) = route.preferred_source {
            assert!(
                interface.addresses.iter().any(|a| a.ip == source),
                "default route on {} prefers source {source}, which is not bound to it: {:?}",
                interface.name,
                interface.addresses
            );
        }
    }

    println!("checked {seen} default route(s)");
}

// ---------------------------------------------------------------------------
// macOS-specific invariants
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
#[test]
#[ignore = "queries the live network"]
fn a_temporary_address_is_a_plausible_privacy_address() {
    // The weakest of this file's four assumption checks, and worth saying so:
    // it catches a `SIOCGIFAFLAG_IN6` union layout that is wildly wrong, not
    // one that is subtly wrong, and it cannot assert a positive — a Mac with
    // IPv6 privacy extensions off, or no IPv6 at all, legitimately has zero
    // temporary addresses. What it does catch is the flag word being read from
    // the wrong offset, which sprays `IN6_IFF_TEMPORARY` across addresses that
    // could not possibly be privacy addresses.
    let state = provider().unwrap().snapshot().unwrap();
    let mut temporary = 0usize;

    for interface in &state.interfaces {
        for address in &interface.addresses {
            if !address.is_temporary {
                continue;
            }
            temporary += 1;

            assert!(
                address.ip.is_ipv6(),
                "{}: {} is IPv4 and cannot be a privacy address",
                interface.name,
                address.ip
            );
            // A privacy address is a globally scoped SLAAC address. Link-local
            // addresses are derived once and never rotate, and loopback is not
            // autoconfigured at all.
            assert_ne!(
                address.scope,
                AddressScope::LinkLocal,
                "{}: link-local {} cannot be a privacy address",
                interface.name,
                address.ip
            );
            assert_ne!(
                address.scope,
                AddressScope::Loopback,
                "{}: loopback {} cannot be a privacy address",
                interface.name,
                address.ip
            );
        }
    }

    println!("{temporary} temporary address(es); zero is a valid answer");
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "queries the live network"]
fn every_route_metric_is_one_the_service_order_could_have_produced() {
    // macOS has no per-route metric. Every value in this field is synthesized
    // by `servicerank` at a hundred points per position in the network service
    // order, capped at a thousand — so anything else means either a kernel
    // value leaked in or the map was never applied. The constants are restated
    // rather than imported because they are crate-private and this is an
    // integration test; `crates/autonet-platform/src/servicerank.rs` owns them.
    const PER_RANK: u32 = 100;
    const CAP: u32 = 1000;

    let state = provider().unwrap().snapshot().unwrap();

    for route in &state.routes {
        assert!(
            route.metric % PER_RANK == 0 && route.metric <= CAP,
            "route {route:?} has a metric no service-order rank produces"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "queries the live network"]
fn at_most_one_interface_can_hold_the_top_of_the_service_order() {
    // `scnetwork::service_order` hands out *dense* ranks starting at zero, so
    // exactly one interface can be rank 0 and carry metric 0. Two would mean
    // the BSD-name-to-index join failed and several interfaces collapsed onto
    // the same entry.
    //
    // Its limit, stated rather than glossed: an interface whose metric was
    // never populated also reads as 0, and `NetworkState` alone cannot tell
    // that apart from a genuine rank 0. This catches the join going wrong for
    // more than one interface, which is the common shape of that failure; it
    // does not catch the map being empty. An empty service order is a
    // legitimate answer anyway — it degrades ranking to the interface index,
    // which is what the code did before the service order existed.
    let state = provider().unwrap().snapshot().unwrap();

    let mut top: Vec<u32> = state
        .routes
        .iter()
        .filter(|route| route.metric == 0)
        .map(|route| route.interface_index)
        .collect();
    top.sort_unstable();
    top.dedup();

    assert!(
        top.len() <= 1,
        "interfaces {top:?} all claim the top of the service order"
    );
    println!("{} interface(s) at the top of the service order", top.len());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires two links of the same kind up at once"]
fn two_links_of_the_same_kind_are_ordered_by_the_service_order() {
    use autonet_core::config::SelectionConfig;
    use autonet_core::model::{Family, Interface, NetworkState};
    use autonet_core::select::select_address;

    // The case the service order exists for, and the one no CI runner can
    // stage: a built-in Ethernet port and a Thunderbolt dock, or two Wi-Fi
    // adapters. Nothing in the scoring policy separates two links of the same
    // kind, so before the service order the raw interface index decided — an
    // accident of enumeration order.
    let state = provider().unwrap().snapshot().unwrap();

    // Eligible: up, not loopback, owning an IPv4 default route with a next hop,
    // and carrying at least one peer-reachable IPv4 address.
    let eligible: Vec<(&Interface, u32, AddressScope)> = state
        .interfaces
        .iter()
        .filter(|interface| interface.kind != InterfaceKind::Loopback && !interface.state.is_down())
        .filter_map(|interface| {
            let metric = state.default_route_metric(interface.index, Family::V4)?;
            state.gateway_for(interface.index, Family::V4)?;

            // One scope per interface, or it is not comparable: the scope
            // weights are worth far more than the ten points a rank of service
            // order buys, so a Private address on one link and a Global one on
            // the other is not a test of the tie-break at all.
            let mut scopes = interface
                .addresses_of(Family::V4)
                .filter(|address| address.scope.is_reachable_by_peers())
                .map(|address| address.scope);
            let scope = scopes.next()?;
            if scopes.any(|other| other != scope) {
                return None;
            }
            Some((interface, metric, scope))
        })
        .collect();

    // A group is two or more eligible links of the same kind and scope whose
    // service-order ranks actually differ. Without differing metrics there is
    // no ordering to check.
    let group: Vec<&(&Interface, u32, AddressScope)> = eligible
        .iter()
        .find_map(|(interface, _, scope)| {
            let peers: Vec<&(&Interface, u32, AddressScope)> = eligible
                .iter()
                .filter(|(other, _, other_scope)| {
                    other.kind == interface.kind && other_scope == scope
                })
                .collect();
            let distinct = peers.iter().map(|(_, metric, _)| *metric).min()
                != peers.iter().map(|(_, metric, _)| *metric).max();
            (peers.len() >= 2 && distinct).then_some(peers)
        })
        .unwrap_or_default();

    if group.is_empty() {
        println!(
            "skipped: this machine has no two comparable links of the same kind \
             at different service-order ranks, so the tie-break cannot be \
             exercised here. Interfaces considered: {:?}",
            eligible
                .iter()
                .map(|(interface, metric, _)| (&interface.name, &interface.kind, metric))
                .collect::<Vec<_>>()
        );
        return;
    }

    // A state narrowed to just the group, built from the machine's own data, so
    // that metric and interface index are the only things left that differ.
    // Selecting over the whole snapshot would let an unrelated link win and
    // prove nothing about the ordering within this group.
    let narrowed = NetworkState::new(
        group
            .iter()
            .map(|(interface, _, scope)| {
                let mut trimmed = (*interface).clone();
                trimmed
                    .addresses
                    .retain(|address| address.family == Family::V4 && address.scope == *scope);
                trimmed
            })
            .collect(),
        group
            .iter()
            .filter_map(|(interface, _, _)| {
                state
                    .default_routes()
                    .find(|route| {
                        route.interface_index == interface.index && route.family == Family::V4
                    })
                    .cloned()
            })
            .collect(),
    );

    let best = group
        .iter()
        .min_by_key(|(_, metric, _)| *metric)
        .expect("the group is not empty");
    let selected = select_address(&narrowed, &SelectionConfig::default())
        .expect("a group built from real default routes should select something");

    println!(
        "same-kind group: {:?}",
        group
            .iter()
            .map(|(interface, metric, _)| (&interface.name, metric))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        selected.interface, best.0.name,
        "the service order lost to something else between two {} links",
        best.0.kind
    );

    // Only a demonstration, never a failure: if the best-ranked link also has
    // the lowest index, the assertion above would pass without the service
    // order being consulted at all, and the run should say so.
    let lowest_index = group
        .iter()
        .min_by_key(|(interface, _, _)| interface.index)
        .expect("the group is not empty");
    if lowest_index.0.index == best.0.index {
        println!(
            "note: {} is both best-ranked and lowest-indexed, so this run does \
             not distinguish the service order from the old index tie-break",
            best.0.name
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The network address of `network` — its own address with every host bit
/// cleared.
///
/// Equal to `network.addr` exactly when the destination really is a network.
fn network_address(network: IpNetwork) -> IpAddr {
    let prefix = u32::from(network.prefix_len);
    match network.addr {
        IpAddr::V4(v4) => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4(Ipv4Addr::from(u32::from(v4) & mask))
        }
        IpAddr::V6(v6) => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
        }
    }
}
