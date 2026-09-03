//! Turning the macOS network service order into a route metric.
//!
//! macOS has no per-route metric: `rt_metrics.rmx_hopcount` reads 0 for every
//! route on Darwin, so every route ties and `NetworkState::default_route_for`
//! falls back to raw *interface index* — an accident of enumeration order.
//! macOS's own answer to "which link is preferred" is the network service order
//! in System Settings ▸ Network, and this module turns a position in that list
//! into the metric the selector already consumes.
//!
//! **The metric is synthesized, not a kernel fact.** On Linux `Route::metric` is
//! a number the kernel stores; on macOS it is derived here, and
//! `autonet routes --json` shows the same field either way.
//!
//! **It is only a tie-breaker.** `METRIC_DIVISOR` is 10, so one rank costs 10
//! points — enough to separate two links the selector cannot otherwise tell
//! apart, and deliberately not enough to overturn `KIND_ETHERNET` (250) beating
//! `KIND_WIRELESS` (200). So dragging Wi-Fi above Ethernet in System Settings
//! does not change AutoNet's answer; what this fixes is two links of the same
//! kind, such as a built-in port and a Thunderbolt dock.
//!
//! Not gated to macOS, so these decisions and their tests run on the Linux job.
//! Only the query that *produces* the order is macOS-only; see
//! [`crate::macos::scnetwork`].

use std::collections::HashMap;

use autonet_core::model::Interface;
use autonet_core::select::weights;

/// Metric cost of each step down the service order.
///
/// 100 divided by `METRIC_DIVISOR` (10) is 10 points per rank — the same order
/// of magnitude Linux route metrics land on, where a wired link at 100 and Wi-Fi
/// at 600 differ by 50 points.
const METRIC_PER_RANK: u32 = 100;

/// The metric for an interface the service order does not mention.
///
/// The worst value the selector will act on: `METRIC_CAP` is where `select.rs`
/// stops increasing the penalty, so no larger number would mean anything.
///
/// An unlisted interface can therefore never silently *win* a tie, while staying
/// nowhere near disqualifying — 10 points, which a link carrying the default
/// route (+1000) shrugs off. Who lands here: `utun` devices, which SC does not
/// enumerate; `lo0`, disqualified as loopback long before scoring; and an
/// adapter plugged in but not yet configured as a network service, which must
/// stay selectable when it is the only real link.
pub(crate) const UNRANKED: u32 = weights::METRIC_CAP;

/// The metric for a given position in the service order.
///
/// `None` means the interface was not in the order at all. Ranks past the cap
/// saturate rather than wrapping, which makes a deep service order and an
/// absent interface indistinguishable — intentional, since both mean "the user
/// has expressed no useful preference for this link".
pub(crate) fn metric_for_rank(rank: Option<u32>) -> u32 {
    match rank {
        Some(rank) => rank.saturating_mul(METRIC_PER_RANK).min(UNRANKED),
        None => UNRANKED,
    }
}

/// Resolve a BSD-name-keyed service order into the index-keyed metrics the
/// route walk needs.
///
/// SystemConfiguration knows interfaces by BSD name; routing messages carry only
/// the kernel's index. The `getifaddrs` walk produced both, so this joins
/// through it. A name is a join key here, never evidence about a device.
///
/// Every interface in the snapshot gets an entry, including those the order
/// does not mention, so the caller needs no second fallback.
pub(crate) fn metrics_by_index(
    interfaces: &[Interface],
    order: &HashMap<String, u32>,
) -> HashMap<u32, u32> {
    interfaces
        .iter()
        .map(|interface| {
            (
                interface.index,
                metric_for_rank(order.get(&interface.name).copied()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use autonet_core::config::SelectionConfig;
    use autonet_core::model::{
        Address, Family, InterfaceKind, InterfaceState, IpNetwork, NetworkState, Route,
    };
    use autonet_core::select::select_address;
    use std::net::{IpAddr, Ipv4Addr};

    fn order(names: &[&str]) -> HashMap<String, u32> {
        names
            .iter()
            .enumerate()
            .map(|(rank, name)| {
                (
                    (*name).to_string(),
                    u32::try_from(rank).expect("test order is short"),
                )
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // The scale
    // -----------------------------------------------------------------------

    #[test]
    fn each_step_down_the_service_order_costs_one_hundred() {
        assert_eq!(metric_for_rank(Some(0)), 0);
        assert_eq!(metric_for_rank(Some(1)), 100);
        assert_eq!(metric_for_rank(Some(9)), 900);
    }

    #[test]
    fn an_interface_the_order_does_not_mention_gets_the_worst_metric() {
        assert_eq!(metric_for_rank(None), UNRANKED);
        assert_eq!(UNRANKED, weights::METRIC_CAP);
    }

    #[test]
    fn a_deep_service_order_saturates_rather_than_wrapping() {
        // The tenth service and beyond cannot be told apart from an absent one,
        // and neither can a rank large enough to overflow the multiply. What
        // matters is that no rank ever produces a *small* metric by wrapping,
        // which would turn the least-preferred link into the winner.
        assert_eq!(metric_for_rank(Some(10)), UNRANKED);
        assert_eq!(metric_for_rank(Some(500)), UNRANKED);
        assert_eq!(metric_for_rank(Some(u32::MAX)), UNRANKED);
    }

    #[test]
    fn the_scale_cannot_overturn_the_ethernet_wifi_gap() {
        // Guards the tradeoff this module documents: if either the weights or
        // METRIC_PER_RANK are ever changed so that one rank of separation beats
        // a category difference, that is a policy change and should be a
        // deliberate one, not a silent consequence of retuning a constant.
        let one_rank = metric_for_rank(Some(1)) / weights::METRIC_DIVISOR;
        let category = weights::KIND_ETHERNET - weights::KIND_WIRELESS;
        assert!(
            i32::try_from(one_rank).unwrap() < category,
            "one rank now costs {one_rank} points against a {category}-point category gap"
        );
    }

    // -----------------------------------------------------------------------
    // The join
    // -----------------------------------------------------------------------

    #[test]
    fn interfaces_are_keyed_by_index_through_their_names() {
        let interfaces = vec![
            iface("en0", 4, InterfaceKind::Ethernet, &[]),
            iface("en1", 5, InterfaceKind::Wireless, &[]),
        ];
        let metrics = metrics_by_index(&interfaces, &order(&["en1", "en0"]));

        assert_eq!(metrics.get(&5), Some(&0), "en1 is first in the order");
        assert_eq!(metrics.get(&4), Some(&100), "en0 is second");
    }

    #[test]
    fn every_interface_gets_an_entry_even_when_unlisted() {
        // A utun is the expected case: SystemConfiguration enumerates
        // configurable hardware, so a tunnel is absent from the order. The
        // caller must not have to distinguish "not in the map" from
        // "not in the order".
        let interfaces = vec![
            iface("en0", 4, InterfaceKind::Ethernet, &[]),
            iface("utun3", 17, InterfaceKind::Vpn, &[]),
        ];
        let metrics = metrics_by_index(&interfaces, &order(&["en0"]));

        assert_eq!(metrics.len(), interfaces.len());
        assert_eq!(metrics.get(&17), Some(&UNRANKED));
    }

    #[test]
    fn a_service_order_naming_an_interface_this_machine_does_not_have_is_ignored() {
        // Configured hardware that is currently unplugged stays in the service
        // order. It must not create a phantom entry, and must not shift the
        // interfaces that are actually present.
        let interfaces = vec![iface("en0", 4, InterfaceKind::Ethernet, &[])];
        let metrics = metrics_by_index(&interfaces, &order(&["en5", "en0"]));

        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics.get(&4), Some(&100));
    }

    #[test]
    fn an_empty_service_order_gives_every_interface_the_same_metric() {
        // SystemConfiguration unavailable, or no current network set. Every
        // interface ties, which is exactly the interface-index tie-break this
        // module replaces — a degradation back to the old behaviour rather
        // than a failure.
        let interfaces = vec![
            iface("en0", 4, InterfaceKind::Ethernet, &[]),
            iface("en1", 5, InterfaceKind::Ethernet, &[]),
        ];
        let metrics = metrics_by_index(&interfaces, &HashMap::new());

        assert_eq!(metrics.get(&4), metrics.get(&5));
    }

    // -----------------------------------------------------------------------
    // The regression this module exists to prevent
    // -----------------------------------------------------------------------

    #[test]
    fn the_service_order_beats_the_interface_index_between_same_kind_links() {
        // Built-in Ethernet at index 4 and a Thunderbolt dock at index 12, both
        // up, both carrying a default route, both classified Ethernet. Nothing
        // in the scoring policy separates them, so before this module the lower
        // index won — an accident. Here the dock is first in the service order
        // and must win despite the higher index.
        let interfaces = vec![
            iface("en0", 4, InterfaceKind::Ethernet, &[("10.0.0.4", 24)]),
            iface("en12", 12, InterfaceKind::Ethernet, &[("10.0.0.12", 24)]),
        ];
        let metrics = metrics_by_index(&interfaces, &order(&["en12", "en0"]));
        let state = NetworkState::new(
            interfaces,
            vec![
                default_route(4, "10.0.0.1", metrics[&4]),
                default_route(12, "10.0.0.1", metrics[&12]),
            ],
        );

        let selected = select_address(&state, &SelectionConfig::default()).unwrap();
        assert_eq!(
            selected.interface, "en12",
            "the service order was ignored in favour of the interface index"
        );
    }

    #[test]
    fn without_the_service_order_the_same_pair_falls_back_to_the_index() {
        // The other half of the test above: it is only meaningful if the
        // unranked case really does resolve by index, which is what makes the
        // ordering the deciding factor rather than something incidental.
        let interfaces = vec![
            iface("en0", 4, InterfaceKind::Ethernet, &[("10.0.0.4", 24)]),
            iface("en12", 12, InterfaceKind::Ethernet, &[("10.0.0.12", 24)]),
        ];
        let metrics = metrics_by_index(&interfaces, &HashMap::new());
        let state = NetworkState::new(
            interfaces,
            vec![
                default_route(4, "10.0.0.1", metrics[&4]),
                default_route(12, "10.0.0.1", metrics[&12]),
            ],
        );

        let selected = select_address(&state, &SelectionConfig::default()).unwrap();
        assert_eq!(selected.interface, "en0");
    }

    #[test]
    fn a_wifi_link_ranked_first_still_loses_to_ethernet() {
        // The documented limitation, asserted so it cannot change by accident.
        // If this test ever fails, the scale has started overriding the
        // selection policy and that needs to be a deliberate decision.
        let interfaces = vec![
            iface("en0", 4, InterfaceKind::Ethernet, &[("10.0.0.4", 24)]),
            iface("en1", 5, InterfaceKind::Wireless, &[("192.168.1.5", 24)]),
        ];
        let metrics = metrics_by_index(&interfaces, &order(&["en1", "en0"]));
        let state = NetworkState::new(
            interfaces,
            vec![
                default_route(4, "10.0.0.1", metrics[&4]),
                default_route(5, "192.168.1.1", metrics[&5]),
            ],
        );

        let selected = select_address(&state, &SelectionConfig::default()).unwrap();
        assert_eq!(selected.interface, "en0");
    }

    // -----------------------------------------------------------------------
    // Builders
    // -----------------------------------------------------------------------

    fn iface(name: &str, index: u32, kind: InterfaceKind, addrs: &[(&str, u8)]) -> Interface {
        let mut interface = Interface::new(name, index, kind, InterfaceState::Up);
        for (ip, prefix) in addrs {
            interface
                .addresses
                .push(Address::new(ip.parse().unwrap(), *prefix));
        }
        interface
    }

    fn default_route(index: u32, gateway: &str, metric: u32) -> Route {
        let gateway: IpAddr = gateway.parse().unwrap();
        Route {
            destination: Some(IpNetwork::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)),
            gateway: Some(gateway),
            interface_index: index,
            metric,
            family: Family::of(&gateway),
            preferred_source: None,
        }
    }
}
