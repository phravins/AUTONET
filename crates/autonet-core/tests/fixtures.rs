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

/// Where the fixtures live, and where their provenance is documented.
///
/// See `tests/fixtures/README.md`: the files there are not equally strong
/// evidence, and which ones are hand-built versus captured from a real machine
/// is recorded beside them rather than inferred from their names.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Every fixture stem in the directory, sorted for a deterministic order.
///
/// Enumerated rather than listed, so a fixture added later cannot quietly
/// escape the whole-corpus checks below by not being named in an array someone
/// forgot to update.
fn fixture_names() -> Vec<String> {
    let dir = fixtures_dir();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("could not list {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .expect("could not read a fixture directory entry")
                .path()
        })
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| {
            path.file_stem()
                .expect("a .json file has a stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no fixtures found in {}", dir.display());
    names
}

/// Load a fixture by stem, for example `"this-machine"`.
fn fixture(name: &str) -> NetworkState {
    let path = fixtures_dir().join(format!("{name}.json"));
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
    assert_eq!(
        pick("this-machine", &SelectionConfig::default()),
        "192.168.1.101"
    );
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
    assert_eq!(
        wifi_link_local.disqualified,
        Some(Disqualification::LinkLocal)
    );
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
    assert_eq!(
        pick("wifi-only", &SelectionConfig::default()),
        "192.168.1.101"
    );
}

#[test]
fn ethernet_only() {
    assert_eq!(
        pick("ethernet-only", &SelectionConfig::default()),
        "10.0.0.20"
    );
}

#[test]
fn ethernet_wins_when_a_laptop_is_docked() {
    // Both links are up and both carry a default route; the wired one is both
    // the better kind and the lower metric.
    assert_eq!(
        pick("wifi-and-ethernet", &SelectionConfig::default()),
        "10.0.0.20"
    );
}

// ---------------------------------------------------------------------------
// VPN
// ---------------------------------------------------------------------------

#[test]
fn a_vpn_does_not_hijack_the_lan_address() {
    // wg0 owns a default route at metric 50 — lower than Wi-Fi's 600 — so a
    // metric-only implementation would hand back 10.8.0.2, which nobody on the
    // LAN can reach.
    assert_eq!(
        pick("wifi-plus-vpn", &SelectionConfig::default()),
        "192.168.1.101"
    );
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
    assert_eq!(
        pick("cgnat-hotspot", &SelectionConfig::default()),
        "192.168.8.42"
    );
}

// ---------------------------------------------------------------------------
// macOS-shaped scenarios
//
// Synthetic, hand-built data. These are *not* captures from a Mac — see
// `tests/fixtures/README.md` for the provenance of every file in the corpus.
// They exercise combinations the `macos-latest` CI runner cannot produce (it has
// one interface, no Wi-Fi radio and no VPN), and they encode what the macOS
// backend is expected to emit rather than what it has been observed to emit.
// ---------------------------------------------------------------------------

/// A docked Mac: two Ethernet links at different service-order ranks, tunnel up.
///
/// This is the combination the macOS backend's two halves have to agree on.
/// Classification decides `utun4` is a `Vpn`; the network service order breaks
/// the tie between two links of the *same* kind, where the kind weights say
/// nothing at all. Each is unit-tested in the platform crate; neither has been
/// run through the selector together with the other until now.
///
/// `en5` deliberately carries the *worse* interface index and the *better*
/// service-order rank. Before the service order was consulted, the tie fell to
/// the raw index and `en0` won — so this fixture fails loudly if the metric is
/// ever dropped, instead of passing for the wrong reason. The final case below
/// proves that by flattening the metrics and watching the winner change.
///
/// Scores, from [`autonet_core::select::weights`]:
///
/// ```text
/// en5    1000 route + 250 ethernet + 150 family + 100 private + 25 gw -   0 = 1525
/// en0    1000       + 250          + 150        + 100         + 25   -  10 = 1515
/// utun4  1000       - 300 vpn      + 150        + 100         + 25   - 100 =  875
/// ```
///
/// Asserted as an ordering rather than as exact numbers, so retuning a weight
/// does not break this test spuriously.
#[test]
fn a_docked_mac_prefers_the_service_order_and_never_the_tunnel() {
    const VPN: u32 = 17;

    let winner = |state: &NetworkState| {
        select_address(state, &SelectionConfig::default())
            .expect("a docked Mac has a usable address")
            .ip
            .to_string()
    };

    // The higher-service-order link wins the tie between the two real links,
    // despite sitting at the higher interface index.
    assert_eq!(winner(&fixture("macos-dock-and-vpn")), "10.0.1.20");

    // And the tunnel loses to both of them, not merely to the winner.
    let selection = select(&fixture("macos-dock-and-vpn"), &SelectionConfig::default());
    let score_of = |name: &str| {
        selection
            .eligible()
            .find(|candidate| candidate.interface == name)
            .unwrap_or_else(|| panic!("{name} should still be eligible"))
            .score
    };
    assert!(
        score_of("utun4") < score_of("en5") && score_of("utun4") < score_of("en0"),
        "the tunnel outscored a real link: utun4 {}, en5 {}, en0 {}",
        score_of("utun4"),
        score_of("en5"),
        score_of("en0")
    );

    // Regardless of the VPN's own metric: give it the best rank there is.
    let mut best_ranked_vpn = fixture("macos-dock-and-vpn");
    for route in &mut best_ranked_vpn.routes {
        if route.interface_index == VPN {
            route.metric = 0;
        }
    }
    assert_eq!(winner(&best_ranked_vpn), "10.0.1.20");

    // And regardless of whether it owns a default route at all — the fixture
    // assumes ownership one way, so the other way is covered here rather than
    // in a near-duplicate file.
    let mut routeless_vpn = fixture("macos-dock-and-vpn");
    routeless_vpn
        .routes
        .retain(|route| route.interface_index != VPN);
    assert_eq!(winner(&routeless_vpn), "10.0.1.20");

    // Control: without the service order the two Ethernet links are identical,
    // and the tie falls back to the interface index — so `en0` wins. This is
    // what makes the first assertion a measurement of the service order rather
    // than a coincidence.
    let mut no_service_order = fixture("macos-dock-and-vpn");
    for route in &mut no_service_order.routes {
        route.metric = 0;
    }
    assert_eq!(winner(&no_service_order), "10.0.0.20");
}

/// The macOS counterpart of `wifi-and-ethernet`, with the ranks inverted.
///
/// In the Linux fixture both the kind weight and the route metric favour
/// Ethernet, so it cannot show which one decided. Here Wi-Fi is rank 0 and
/// Ethernet rank 1, so the metric favours Wi-Fi and only the kind weight favours
/// Ethernet:
///
/// ```text
/// en1 (ethernet, rank 1)  1000 + 250 + 150 + 100 + 25 - 10 = 1515
/// en0 (wireless, rank 0)  1000 + 200 + 150 + 100 + 25 -  0 = 1475
/// ```
///
/// Ethernet still wins, by 40. This is the end-to-end form of the property the
/// platform crate's `servicerank` module documents and unit-tests: the service
/// order is a tie-breaker between same-kind links and cannot overturn a category
/// difference, so **dragging Wi-Fi above Ethernet in System Settings does not
/// change AutoNet's answer.** If that is ever meant to change, it should change
/// here deliberately rather than be discovered on someone's laptop.
///
/// The fixture also names the Wi-Fi card `en0` and the wired port `en1`, which
/// is the inversion of the heuristic the backend is forbidden from using: a name
/// is a join key, never evidence about what a device is.
#[test]
fn the_macos_service_order_cannot_promote_wifi_over_ethernet() {
    assert_eq!(
        pick("macos-wifi-and-ethernet", &SelectionConfig::default()),
        "10.0.0.20"
    );
}

#[test]
fn every_fixture_round_trips_through_serde() {
    // Guards the JSON contract the SDKs will bind to: if a model change breaks
    // the wire format, it fails here rather than in someone's Python client.
    for name in fixture_names() {
        let state = fixture(&name);
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: NetworkState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(state, decoded, "{name} did not survive a round trip");
        assert_eq!(state.schema_version, autonet_core::SCHEMA_VERSION, "{name}");
    }
}

#[test]
fn selection_does_not_depend_on_input_ordering() {
    // Every fixture, including the ones that select nothing on purpose: an
    // ordering bug that turned "no usable address" into an answer would be as
    // wrong as one that changed which answer came back, so the outcome is
    // compared as an `Option` rather than unwrapped.
    fn outcome(state: &NetworkState) -> Option<(std::net::IpAddr, i32)> {
        select_address(state, &SelectionConfig::default())
            .ok()
            .map(|selected| (selected.ip, selected.score))
    }

    for name in fixture_names() {
        let forward = fixture(&name);
        let mut reversed = fixture(&name);
        reversed.interfaces.reverse();
        for i in &mut reversed.interfaces {
            i.addresses.reverse();
        }
        reversed.routes.reverse();

        assert_eq!(
            outcome(&forward),
            outcome(&reversed),
            "{name} depends on enumeration order"
        );
    }
}
