//! Ignored checks against the machine's live network.

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

    // Every supported platform has a loopback interface.
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

    // Interface indexes join addresses and routes.
    let mut indexes: Vec<u32> = state.interfaces.iter().map(|i| i.index).collect();
    let count = indexes.len();
    indexes.sort_unstable();
    indexes.dedup();
    assert_eq!(indexes.len(), count, "duplicate interface indexes");
    assert!(!indexes.contains(&0), "interface index 0 is not valid");

    // Names are used by configuration rules.
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

        // A default route out of a broadcast NIC needs a router to hand packets
        // to. Demanded of nothing else, for two separate reasons. A utun default
        // route installed by WireGuard or Tailscale is routinely on-link with no
        // next hop, so asserting it on a point-to-point link would fail on a Mac
        // that is working exactly as intended. And Windows reports a
        // legitimately gateway-less route as `None` rather than as the phantom
        // `0.0.0.0` it stores — `winroute::gateway` — so a Hyper-V switch or an
        // on-link-configured adapter would fail this on a machine that is
        // correct. Narrowed to the kinds the assertion was written for: it still
        // fires on the real NIC where a sockaddr-stride bug shows up.
        let demanding = matches!(
            interface.kind,
            InterfaceKind::Ethernet | InterfaceKind::Wireless
        ) && interface.flags.broadcast
            && !interface.flags.point_to_point;

        if demanding {
            let Some(gateway) = route.gateway else {
                panic!("default route on {} has no next hop", interface.name);
            };

            // The next hop off a broadcast link is another machine — a router —
            // so it can never be an address this machine holds on that same
            // interface. If the sockaddr walk slipped by one slot, the gateway
            // would be read out of `RTA_IFA`, which is exactly the interface's
            // own address, and the result would otherwise look entirely
            // plausible. Everything narrowed out above is narrowed out for a
            // real reason: macOS routinely installs a utun default route whose
            // gateway *is* the tunnel's own address.
            assert!(
                !interface.addresses.iter().any(|a| a.ip == gateway),
                "default route on {} points its next hop at {gateway}, an address \
                 bound to that same interface",
                interface.name
            );
        } else if route.gateway.is_none() {
            println!(
                "note: on-link default route on {} ({})",
                interface.name, interface.kind
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
// Windows-specific invariants
//
// Every one of these skips gracefully rather than assuming a configuration. A
// machine with IPv6 off, one NIC, or no tunnel is a correctly-configured
// machine, and a test that fails on it is a test bug rather than a backend one.
//
// None of them has ever run. There is no Windows hardware behind this file, and
// Milestone 2b Task 7 is the first time any of it executes.
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
#[test]
#[ignore = "queries the live network"]
fn the_loopback_adapter_is_classified_and_owns_no_reported_route() {
    // `winroute::is_reportable` drops the rows Windows flags as loopback and any
    // multicast destination, so that `autonet routes` reads like Linux's
    // unicast-only dump and macOS's flag filter. This is the only check that
    // filter will ever get against real rows.
    use autonet_core::model::Interface;

    let state = provider().unwrap().snapshot().unwrap();

    let loopbacks: Vec<&Interface> = state
        .interfaces
        .iter()
        .filter(|interface| interface.kind == InterfaceKind::Loopback)
        .collect();
    assert!(
        !loopbacks.is_empty(),
        "Windows always has a loopback pseudo-interface, and `wintype` did not \
         classify one: {:?}",
        state
            .interfaces
            .iter()
            .map(|interface| (&interface.name, &interface.kind))
            .collect::<Vec<_>>()
    );

    for loopback in &loopbacks {
        assert!(
            loopback.flags.loopback,
            "{} is classified loopback but not flagged loopback",
            loopback.name
        );
        assert!(
            loopback
                .addresses
                .iter()
                .any(|address| address.scope == AddressScope::Loopback),
            "{} carries no loopback address: {:?}",
            loopback.name,
            loopback.addresses
        );

        // If this fires, `MIB_IPFORWARD_ROW2.Loopback` is not the flag
        // `is_reportable` takes it for — worth learning either way.
        assert!(
            !state
                .routes
                .iter()
                .any(|route| route.interface_index == loopback.index),
            "a route survived the loopback filter on {}",
            loopback.name
        );
    }

    for route in &state.routes {
        if let Some(network) = route.destination {
            assert!(
                !network.addr.is_multicast(),
                "route {route:?} has a multicast destination the row filter should \
                 have dropped"
            );
        }
    }

    println!(
        "{} loopback interface(s), {} reported route(s)",
        loopbacks.len(),
        state.routes.len()
    );
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "queries the live network"]
fn a_dual_stack_adapter_keeps_both_route_families_on_one_interface() {
    // The failure Task 4's LUID join exists to prevent, against real data for
    // the first time.
    //
    // Asserting that both families "resolve to the same interface" would be
    // tautological: by the time the state is a `NetworkState` the join has
    // already happened and only an index survives. The shape the bug left behind
    // is not tautological. Reading `MIB_IPFORWARD_ROW2.InterfaceIndex` directly
    // gave an IPv6 row the adapter's `Ipv6IfIndex`, which on a dual-stack adapter
    // belongs to no interface at all — so exactly the IPv6 routes of exactly the
    // adapters where IPv6 works went missing, silently.
    //
    // The assumption this rests on, named rather than assumed: Windows installs
    // an on-link `fe80::/64` route for every IPv6-bound interface, so an adapter
    // holding an IPv6 address and no IPv6 route has had them dropped.
    use autonet_core::model::Family;

    let state = provider().unwrap().snapshot().unwrap();
    let mut dual_stack = 0usize;
    let mut considered = Vec::new();

    for interface in &state.interfaces {
        if interface.kind == InterfaceKind::Loopback {
            continue;
        }

        let has_v6_address = interface.addresses_of(Family::V6).next().is_some();
        let count = |family| {
            state
                .routes
                .iter()
                .filter(|route| route.interface_index == interface.index && route.family == family)
                .count()
        };
        let (v4, v6) = (count(Family::V4), count(Family::V6));
        considered.push((interface.name.clone(), has_v6_address, v4, v6));

        if !has_v6_address || v4 == 0 {
            continue;
        }

        assert!(
            v6 > 0,
            "{} holds an IPv6 address and {v4} IPv4 route(s) but no IPv6 route: the \
             LUID join dropped them onto an index no interface carries",
            interface.name
        );
        dual_stack += 1;
    }

    if dual_stack == 0 {
        println!(
            "skipped: no adapter here has both an IPv6 address and IPv4 routes, so \
             the join cannot be exercised. (name, has v6 address, v4 routes, v6 \
             routes): {considered:?}"
        );
        return;
    }

    println!("{dual_stack} dual-stack adapter(s) kept both route families: {considered:?}");
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "queries the live network"]
fn a_default_routes_next_hop_is_on_the_interface_it_names() {
    // `every_route_points_at_an_interface_that_exists` only asks whether the
    // index resolves to *something*. A route mis-joined onto a different real
    // adapter resolves perfectly well and is invisible to it — the residual half
    // of the LUID risk. A router is reachable on the link it sits on, so its
    // address falls inside one of that link's own prefixes, and that is knowable
    // from the snapshot alone.
    //
    // Residual risk, stated rather than glossed: a deliberately off-subnet
    // `onlink` gateway is legal, rare on Windows, and would fail this.
    use autonet_core::model::Family;

    let state = provider().unwrap().snapshot().unwrap();
    let mut checked = 0usize;

    for route in state.default_routes().filter(|r| r.family == Family::V4) {
        let Some(gateway) = route.gateway else {
            continue;
        };
        let interface = state
            .interface_by_index(route.interface_index)
            .expect("every route names an interface that exists");

        if interface.flags.point_to_point || interface.kind == InterfaceKind::Loopback {
            continue;
        }

        let prefixes: Vec<IpNetwork> = interface
            .addresses_of(Family::V4)
            .map(|address| IpNetwork::new(address.ip, address.prefix_len))
            .collect();
        if prefixes.is_empty() {
            continue;
        }

        checked += 1;
        assert!(
            prefixes.iter().any(|network| contains(*network, gateway)),
            "default route on {} points its next hop at {gateway}, outside every \
             prefix that interface holds ({prefixes:?}): the route may have been \
             joined to the wrong adapter",
            interface.name
        );
    }

    println!("checked {checked} IPv4 default-route gateway(s) against their own interface");
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "queries the live network"]
fn an_on_link_route_reports_no_gateway_rather_than_the_unspecified_address() {
    // Windows fills `NextHop` with `0.0.0.0` or `::` for a route that needs no
    // router, where Linux and macOS simply omit the attribute. Published as it
    // arrives it would print a gateway that does not exist and earn a
    // `has_gateway` bonus the route has not got; `winroute::gateway` maps it to
    // `None`.
    let state = provider().unwrap().snapshot().unwrap();

    if state.routes.is_empty() {
        println!("skipped: this machine reports no routes at all");
        return;
    }

    let mut on_link = Vec::new();
    for route in &state.routes {
        match route.gateway {
            Some(gateway) => assert!(
                !gateway.is_unspecified(),
                "route {route:?} publishes the unspecified address as a next hop"
            ),
            None => on_link.push(route),
        }

        // A default route arriving as `Some(0.0.0.0/0)` would mean the
        // normalisation `winroute::destination` performs did not happen.
        if route.is_default() {
            assert!(
                route.destination.is_none(),
                "default route {route:?} was not normalised to a null destination"
            );
        }
    }

    assert!(
        !on_link.is_empty(),
        "every one of this machine's {} routes claims a next hop; Windows gives each \
         IP-bound adapter an on-link subnet route, so `winroute::gateway` is \
         publishing 0.0.0.0 as a gateway",
        state.routes.len()
    );

    println!(
        "{} of {} route(s) are on-link, e.g. {:?}",
        on_link.len(),
        state.routes.len(),
        on_link[0]
    );
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "queries the live network"]
fn every_route_metric_includes_its_interfaces_own_metric() {
    // `windows::route::route_from` falls back to an interface metric of zero when
    // the LUID lookup misses, and `winroute::effective_metric` is a sum — so a
    // metric of zero on a real adapter means the interface metric contributed
    // nothing. That is precisely the inertness Task 4 was written to prevent,
    // where every default route ties at zero and the tie-break falls to whatever
    // order Windows walked its NDIS tables.
    //
    // The assumption, DOCUMENTED only: Windows' automatic metrics start at 5 and
    // its UI enforces a minimum of 1, so 0 is not a value a bound IP interface
    // reports. If this fires on a healthy machine then that assumption is what is
    // wrong, and learning so is worth more than the test passing.
    //
    // What it does *not* catch, stated plainly: whether `MIB_IPFORWARD_ROW2.Metric`
    // is the offset Microsoft documents or already the effective value. Both
    // readings produce a plausible sum and the model carries no separate
    // interface-metric field, so no assertion over `Route.metric` can separate
    // them. Only comparing the numbers printed below against `netsh interface
    // ipv4 show interfaces` on the machine itself can.
    let state = provider().unwrap().snapshot().unwrap();
    let mut metrics = Vec::new();

    for route in &state.routes {
        let interface = state
            .interface_by_index(route.interface_index)
            .expect("every route names an interface that exists");

        if interface.kind == InterfaceKind::Loopback || interface.state.is_down() {
            continue;
        }

        assert!(
            route.metric > 0,
            "route {route:?} on {} has an effective metric of zero: either the LUID \
             join missed this adapter or its interface metric read as zero",
            interface.name
        );
        metrics.push((interface.name.as_str(), route.family, route.metric));
    }

    println!("route metrics by interface: {metrics:?}");
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires a connected machine"]
fn repeated_snapshots_select_the_same_interface() {
    // `adapters::walk` sorts by name precisely so that `GetAdaptersAddresses`'
    // enumeration order cannot reach the tie-break. This asserts nothing about
    // *which* interface wins — that depends on the machine, and asserting it
    // would be a test of one setup rather than of the code. It asserts only that
    // the answer does not move.
    use autonet_core::config::SelectionConfig;
    use autonet_core::model::NetworkState;
    use autonet_core::select::select_address;

    let provider = provider().expect("a backend for this platform");
    let first = provider.snapshot().expect("a snapshot");
    let second = provider.snapshot().expect("a second snapshot");

    // Sorted rather than compared positionally: reordering is exactly what this
    // test is looking for, but it has to be told apart from the network itself
    // changing between the two calls.
    let shape = |state: &NetworkState| {
        let mut lines: Vec<String> = state
            .interfaces
            .iter()
            .map(|interface| {
                format!(
                    "if {} {} {}",
                    interface.index, interface.name, interface.kind
                )
            })
            .chain(state.routes.iter().map(|route| format!("rt {route:?}")))
            .collect();
        lines.sort();
        lines
    };

    if shape(&first) != shape(&second) {
        println!(
            "skipped: the network changed between the two snapshots — a DHCP renew or \
             a Wi-Fi roam is not a bug, and there is nothing to compare"
        );
        return;
    }

    // `CoreError` carries no `PartialEq`, so the comparison is over `.ok()`.
    // That treats every failure as one answer rather than distinguishing the
    // variants — which is the right reading here: a machine with nothing to
    // select must keep selecting nothing, and an offline machine is a legitimate
    // machine to run this on.
    let config = SelectionConfig::default();
    let once = select_address(&first, &config).ok();

    assert_eq!(
        once,
        select_address(&second, &config).ok(),
        "two snapshots of an unchanged network selected different addresses"
    );
    assert_eq!(
        once,
        select_address(&first, &config).ok(),
        "one snapshot selected differently on a second pass"
    );

    match once {
        Some(selected) => println!("stable: {} on {}", selected.ip, selected.interface),
        None => println!("stable: this machine selects no address"),
    }
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires two links of the same kind up at once"]
fn two_links_of_the_same_kind_are_ordered_by_the_interface_metric() {
    use autonet_core::config::SelectionConfig;
    use autonet_core::model::{Family, Interface, NetworkState};
    use autonet_core::select::select_address;

    // The case the interface metric exists for, and the one no CI runner can
    // stage: a laptop on a dock with its built-in NIC also up, or two Wi-Fi
    // adapters. Nothing in the scoring policy separates two links of the same
    // kind, so without the metric the raw interface index decides — an accident
    // of enumeration order. Direct mirror of the macOS service-order test above.
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

            // One scope per interface, or it is not comparable: the scope weights
            // dwarf what the metric tie-break is worth. On Windows that gap is
            // wider than on macOS — automatic metrics of 5 against 35 come to a
            // three-point difference once `METRIC_DIVISOR` has divided it, where
            // a rank of macOS service order is worth ten. It decides the tie
            // deterministically, and it decides it by a hair.
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
    // metrics actually differ. Without differing metrics there is no ordering to
    // check.
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
            "skipped: this machine has no two comparable links of the same kind at \
             different interface metrics, so the tie-break cannot be exercised here. \
             Interfaces considered: {:?}",
            eligible
                .iter()
                .map(|(interface, metric, _)| (&interface.name, &interface.kind, metric))
                .collect::<Vec<_>>()
        );
        return;
    }

    // A state narrowed to just the group, built from the machine's own data, so
    // that metric and interface index are the only things left that differ.
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
        "the interface metric lost to something else between two {} links",
        best.0.kind
    );

    // Only a demonstration, never a failure: if the best-metric link also has the
    // lowest index, the assertion above would pass without the metric being
    // consulted at all, and the run should say so.
    let lowest_index = group
        .iter()
        .min_by_key(|(interface, _, _)| interface.index)
        .expect("the group is not empty");
    if lowest_index.0.index == best.0.index {
        println!(
            "note: {} is both best-metric and lowest-indexed, so this run does not \
             distinguish the interface metric from the old index tie-break",
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

/// Whether `ip` sits inside `network`.
///
/// Built on [`network_address`] rather than beside it: two addresses share a
/// network exactly when masking both at the same prefix gives the same answer.
/// Gated because only the Windows section uses it, and an ungated helper nothing
/// calls is dead code under `-D warnings`.
#[cfg(target_os = "windows")]
fn contains(network: IpNetwork, ip: IpAddr) -> bool {
    if network.addr.is_ipv4() != ip.is_ipv4() {
        return false;
    }
    network_address(network) == network_address(IpNetwork::new(ip, network.prefix_len))
}
