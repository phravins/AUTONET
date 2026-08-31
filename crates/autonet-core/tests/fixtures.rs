//! Fixture-driven tests for the selection engine.
//!
//! Every scenario here is a JSON snapshot in `tests/fixtures/` at the repository
//! root. Nothing in this file touches the operating system, which is the point:
//! switching Wi-Fi networks while the suite runs must not change a single
//! result. Live checks belong in the platform crate, behind `#[ignore]`.
//!
//! Fixtures state each address's `scope` explicitly rather than deriving it.
//! That keeps the two concerns separate — `classify.rs` unit tests decide what
//! `172.17.0.1` *is*, and these decide what the engine *does* with it.

use std::path::PathBuf;

use autonet_core::config::SelectionConfig;
use autonet_core::model::{FamilyPreference, NetworkState};
use autonet_core::select::{select, select_address, Disqualification};

/// Load a fixture by stem, for example `"this-machine"`.
fn fixture(name: &str) -> NetworkState {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read fixture {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("could not parse fixture {}: {e}", path.display()))
}

/// The address the engine picks for a fixture under a given configuration.
fn pick(name: &str, config: &SelectionConfig) -> String {
    select_address(&fixture(name), config)
        .unwrap_or_else(|e| panic!("{name}: expected a selection, got: {e}"))
        .ip
        .to_string()
}

fn ipv6() -> SelectionConfig {
    SelectionConfig {
        prefer_family: FamilyPreference::Ipv6,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// The headline case
// ---------------------------------------------------------------------------

/// The development host: one Wi-Fi card buried under nine bridges, eleven
/// veth pairs, a down Ethernet port, and a pile of link-local IPv6.
///
/// A naive "first address found" implementation returns `172.20.0.1` here.
#[test]
fn this_machine_selects_the_wifi_lan_address() {
    assert_eq!(pick("this-machine", &SelectionConfig::default()), "192.168.1.101");
}

#[test]
fn this_machine_reports_the_right_interface_and_gateway() {
    let s = select_address(&fixture("this-machine"), &SelectionConfig::default()).unwrap();
    assert_eq!(s.interface, "wlo1");
    assert_eq!(s.interface_index, 3);
    assert_eq!(s.gateway.unwrap().to_string(), "192.168.1.1");
    assert_eq!(s.prefix_len, 24);
    assert_eq!(s.url(3000, "http"), "http://192.168.1.101:3000");
}

#[test]
fn this_machine_rejects_every_bridge_veth_and_loopback() {
    let selection = select(&fixture("this-machine"), &SelectionConfig::default());

    // Exactly one candidate survives: the Wi-Fi IPv4 address.
    let eligible: Vec<_> = selection.eligible().collect();
    assert_eq!(eligible.len(), 1, "expected a single eligible candidate");
    assert_eq!(eligible[0].address.ip.to_string(), "192.168.1.101");

    // And the rejections are for the reasons we expect, not by accident.
    let reason_for = |name: &str| {
        selection
            .candidates
            .iter()
            .find(|c| c.interface == name)
            .unwrap_or_else(|| panic!("no candidate for {name}"))
            .disqualified
    };
    assert_eq!(reason_for("lo"), Some(Disqualification::Loopback));
    assert_eq!(reason_for("docker0"), Some(Disqualification::InterfaceDown));
    assert_eq!(reason_for("virbr0"), Some(Disqualification::InterfaceDown));
    assert_eq!(
        reason_for("br-18642d3532b2"),
        Some(Disqualification::SyntheticWithoutRoute)
    );
    // A veth is both a routeless container device and carries only a
    // link-local address. Interface-level checks run first, so the reported
    // reason is the broader one.
    assert_eq!(
        reason_for("veth7faf9d0"),
        Some(Disqualification::SyntheticWithoutRoute)
    );
    // The Wi-Fi card's own fe80:: address is rejected on scope, since the
    // interface itself is perfectly good.
    let wifi_link_local = selection
        .candidates
        .iter()
        .find(|c| c.interface == "wlo1" && c.address.ip.to_string().starts_with("fe80"))
        .unwrap();
    assert_eq!(wifi_link_local.disqualified, Some(Disqualification::LinkLocal));
}

#[test]
fn this_machine_ipv6_returns_the_global_address_never_a_link_local_one() {
    let s = select_address(&fixture("this-machine"), &ipv6()).unwrap();
    assert_eq!(s.ip.to_string(), "2606:4700:4700::1111");
    assert_eq!(s.interface, "wlo1");
    // The one mistake that would make the result useless.
    assert!(!s.ip.to_string().starts_with("fe80"));
}

// ---------------------------------------------------------------------------
// Ordinary topologies
// ---------------------------------------------------------------------------

#[test]
fn wifi_only() {
    assert_eq!(pick("wifi-only", &SelectionConfig::default()), "192.168.1.101");
}

#[test]
fn ethernet_only() {
    assert_eq!(pick("ethernet-only", &SelectionConfig::default()), "10.0.0.20");
}

#[test]
fn ethernet_wins_when_a_laptop_is_docked() {
    // Both links are up and both carry a default route; the wired one is both
    // the better kind and the lower metric.
    assert_eq!(pick("wifi-and-ethernet", &SelectionConfig::default()), "10.0.0.20");
}

// ---------------------------------------------------------------------------
// VPN
// ---------------------------------------------------------------------------

#[test]
fn a_vpn_does_not_hijack_the_lan_address() {
    // wg0 owns a default route at metric 50 — lower than Wi-Fi's 600 — so a
    // metric-only implementation would hand back 10.8.0.2, which nobody on the
    // LAN can reach.
    assert_eq!(pick("wifi-plus-vpn", &SelectionConfig::default()), "192.168.1.101");
}

#[test]
fn allowing_vpns_stops_the_penalty_without_promoting_them() {
    // `allow_vpn` means "stop penalising", not "prefer". Wi-Fi still wins on
    // interface kind; demanding the tunnel is what `prefer_interfaces` is for.
    let allowed = SelectionConfig {
        allow_vpn: true,
        ..Default::default()
    };
    assert_eq!(pick("wifi-plus-vpn", &allowed), "192.168.1.101");

    let preferred = SelectionConfig {
        allow_vpn: true,
        prefer_interfaces: vec!["wg0".into()],
        ..Default::default()
    };
    assert_eq!(pick("wifi-plus-vpn", &preferred), "10.8.0.2");
}

// ---------------------------------------------------------------------------
// Cases where the honest answer is "nothing"
// ---------------------------------------------------------------------------

#[test]
fn a_machine_with_only_loopback_yields_an_error() {
    let err = select_address(&fixture("loopback-only"), &SelectionConfig::default()).unwrap_err();
    assert!(err.to_string().contains("loopback"), "got: {err}");
}

#[test]
fn a_disconnected_machine_yields_an_error() {
    assert!(select_address(&fixture("disconnected"), &SelectionConfig::default()).is_err());
}

#[test]
fn docker_alone_is_not_an_answer() {
    // Offline laptop with containers running. `172.17.0.1` is reachable from
    // nowhere, so reporting failure beats reporting a plausible-looking lie.
    assert!(select_address(&fixture("docker-only"), &SelectionConfig::default()).is_err());
}

#[test]
fn docker_alone_is_an_answer_when_explicitly_requested() {
    let config = SelectionConfig {
        allow_container: true,
        ..Default::default()
    };
    assert_eq!(pick("docker-only", &config), "172.17.0.1");
}

// ---------------------------------------------------------------------------
// Family handling
// ---------------------------------------------------------------------------

#[test]
fn an_ipv6_only_machine_reports_nothing_when_ipv4_is_demanded() {
    // Surprising but correct: silently substituting an IPv6 address for the
    // IPv4 one the caller asked for would break them further downstream.
    assert!(select_address(&fixture("ipv6-only"), &SelectionConfig::default()).is_err());
}

#[test]
fn an_ipv6_only_machine_answers_when_ipv6_or_any_is_requested() {
    assert_eq!(pick("ipv6-only", &ipv6()), "2606:4700:4700::1111");

    let any = SelectionConfig {
        prefer_family: FamilyPreference::Any,
        ..Default::default()
    };
    assert_eq!(pick("ipv6-only", &any), "2606:4700:4700::1111");
}

#[test]
fn stable_ipv6_addresses_beat_privacy_temporaries() {
    // A temporary address is rotated out from under whoever you gave it to.
    let s = select_address(&fixture("ipv6-only"), &ipv6()).unwrap();
    assert_eq!(s.ip.to_string(), "2606:4700:4700::1111");
}

// ---------------------------------------------------------------------------
// Scoring nuances
// ---------------------------------------------------------------------------

#[test]
fn cgnat_loses_to_an_ordinary_private_address() {
    // Phone hotspot on carrier-grade NAT alongside a real LAN: the CGNAT
    // address is routable outward but generally not reachable inward.
    assert_eq!(pick("cgnat-hotspot", &SelectionConfig::default()), "192.168.8.42");
}

#[test]
fn every_fixture_round_trips_through_serde() {
    // Guards the JSON contract the SDKs will bind to: if a model change breaks
    // the wire format, it fails here rather than in someone's Python client.
    for name in [
        "this-machine",
        "wifi-only",
        "ethernet-only",
        "wifi-and-ethernet",
        "wifi-plus-vpn",
        "docker-only",
        "loopback-only",
        "disconnected",
        "ipv6-only",
        "cgnat-hotspot",
    ] {
        let state = fixture(name);
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: NetworkState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(state, decoded, "{name} did not survive a round trip");
        assert_eq!(state.schema_version, autonet_core::SCHEMA_VERSION, "{name}");
    }
}

#[test]
fn selection_does_not_depend_on_input_ordering() {
    for name in ["this-machine", "wifi-and-ethernet", "cgnat-hotspot"] {
        let forward = fixture(name);
        let mut reversed = fixture(name);
        reversed.interfaces.reverse();
        for i in &mut reversed.interfaces {
            i.addresses.reverse();
        }
        reversed.routes.reverse();

        let a = select_address(&forward, &SelectionConfig::default()).unwrap();
        let b = select_address(&reversed, &SelectionConfig::default()).unwrap();
        assert_eq!(a.ip, b.ip, "{name} depends on enumeration order");
        assert_eq!(a.score, b.score, "{name} score is order-dependent");
    }
}
