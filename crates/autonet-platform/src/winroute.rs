//! Shaping `MIB_IPFORWARD_ROW2` rows into the model's [`Route`].
//!
//! Kept outside `windows/` for the reason [`crate::winparse`] is: these are the
//! decisions that fail *plausibly* rather than loudly. An on-link route whose
//! `NextHop` is the unspecified address would otherwise be published as a
//! gateway of `0.0.0.0`, earning it a `has_gateway` bonus it has not got.
//!
//! [`Route`]: autonet_core::model::Route

use std::net::IpAddr;

use autonet_core::model::IpNetwork;

use crate::winparse;

/// The destination network, or `None` for a default route.
///
/// Normalised to `None` so `autonet routes --json` is byte-identical across
/// platforms; [`crate::rtparse`] does the same for macOS.
pub(crate) fn destination(prefix: IpAddr, prefix_len: u8) -> Option<IpNetwork> {
    let network = IpNetwork::new(prefix, winparse::prefix_len(prefix_len, &prefix));
    (!network.is_default()).then_some(network)
}

/// The next hop, or `None` when the route is on-link.
///
/// Windows fills `NextHop` with the unspecified address for a route that needs
/// no gateway, where Linux and macOS simply omit the attribute.
pub(crate) fn gateway(next_hop: IpAddr) -> Option<IpAddr> {
    (!next_hop.is_unspecified()).then_some(next_hop)
}

/// The index an adapter's [`Interface`] carries.
///
/// `IP_ADAPTER_ADDRESSES_LH` reports `IfIndex` and `Ipv6IfIndex` as two
/// *separate namespaces*, not two names for one number, and the model has room
/// for a single index. The IPv4 one is authoritative when the adapter has one;
/// an adapter with no IPv4 binding reports `IfIndex` as zero, and its IPv6 index
/// stands in.
///
/// [`Interface`]: autonet_core::model::Interface
pub(crate) fn adapter_index(ipv4_index: u32, ipv6_index: u32) -> u32 {
    if ipv4_index == 0 {
        ipv6_index
    } else {
        ipv4_index
    }
}

/// The index a route reports, resolved through the LUID join.
///
/// `joined` is what [`adapter_index`] gave the adapter whose LUID this row
/// names. It is preferred over the row's own `InterfaceIndex` because that field
/// holds whichever namespace the row's *family* uses: on a dual-stack adapter
/// whose two indices differ, an IPv6 row names an index no [`Interface`] carries,
/// and `default_route_for(V6)` then matches nothing at all.
///
/// A row whose adapter the walk did not return keeps its own index, and 0 when
/// it has none — emitted either way. An unjoined route is merely unscored; a
/// dropped one hides a working network. [`crate::rtparse`] does the same on
/// macOS.
///
/// [`Interface`]: autonet_core::model::Interface
pub(crate) fn route_index(row_index: u32, joined: Option<u32>) -> u32 {
    joined.unwrap_or(row_index)
}

/// The metric Windows itself ranks a route by.
///
/// `MIB_IPFORWARD_ROW2.Metric` is a route metric *offset*: Microsoft documents
/// the effective metric as that offset plus the owning interface's metric.
/// Default routes commonly carry an offset of 0, so reading the row alone would
/// tie every default route at zero and leave the selector's tie-break to
/// interface enumeration order.
pub(crate) fn effective_metric(route: u32, interface: u32) -> u32 {
    route.saturating_add(interface)
}

/// Whether a row belongs in the reported table.
///
/// Windows returns rows Linux's unicast-only dump and macOS's flag filter both
/// exclude. Deliberately minimal — no filtering on `Protocol`, because dropping
/// a real default route is far worse than reporting a route nothing scores.
pub(crate) fn is_reportable(loopback: bool, destination: IpAddr) -> bool {
    !loopback && !destination.is_multicast()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("a literal address")
    }

    #[test]
    fn a_default_destination_becomes_none_in_both_families() {
        assert_eq!(destination(ip("0.0.0.0"), 0), None);
        assert_eq!(destination(ip("::"), 0), None);
    }

    #[test]
    fn a_real_destination_is_kept() {
        assert_eq!(
            destination(ip("192.168.1.0"), 24),
            Some(IpNetwork::new(ip("192.168.1.0"), 24))
        );
        assert_eq!(
            destination(ip("2001:db8::"), 32),
            Some(IpNetwork::new(ip("2001:db8::"), 32))
        );
    }

    #[test]
    fn a_zero_prefix_on_a_named_network_is_not_a_default_route() {
        // Only the unspecified address at /0 is a default route. `10.0.0.0/0`
        // is malformed rather than default, and dropping its address would
        // turn a bad row into a convincing one.
        assert_eq!(
            destination(ip("10.0.0.0"), 0),
            Some(IpNetwork::new(ip("10.0.0.0"), 0))
        );
    }

    #[test]
    fn an_over_long_prefix_is_clamped_rather_than_published() {
        assert_eq!(
            destination(ip("192.168.1.1"), 255),
            Some(IpNetwork::new(ip("192.168.1.1"), 32))
        );
        assert_eq!(
            destination(ip("2001:db8::1"), 255),
            Some(IpNetwork::new(ip("2001:db8::1"), 128))
        );
    }

    #[test]
    fn an_on_link_next_hop_is_not_a_gateway() {
        // The whole point of the function: `Some(0.0.0.0)` would print a
        // gateway that does not exist and score a `has_gateway` bonus.
        assert_eq!(gateway(ip("0.0.0.0")), None);
        assert_eq!(gateway(ip("::")), None);
    }

    #[test]
    fn a_real_next_hop_is_a_gateway() {
        assert_eq!(gateway(ip("192.168.1.1")), Some(ip("192.168.1.1")));
        assert_eq!(gateway(ip("fe80::1")), Some(ip("fe80::1")));
    }

    #[test]
    fn an_adapter_with_no_ipv4_binding_falls_back_to_its_ipv6_index() {
        assert_eq!(
            adapter_index(12, 24),
            12,
            "IPv4 is authoritative when present"
        );
        assert_eq!(adapter_index(0, 24), 24, "an IPv6-only adapter");
        assert_eq!(adapter_index(12, 0), 12);
        assert_eq!(adapter_index(0, 0), 0, "nothing to report");
    }

    #[test]
    fn a_dual_stack_adapters_two_route_families_resolve_to_one_interface() {
        // The failure Task 4's LUID join exists to prevent, at the level where
        // it is decided. `IfIndex` and `Ipv6IfIndex` differ, as they do on any
        // adapter Windows bound to IPv6 at a different time than IPv4.
        const IF_INDEX: u32 = 12;
        const IPV6_IF_INDEX: u32 = 24;

        // The one index the adapter's `Interface` will carry.
        let interface = adapter_index(IF_INDEX, IPV6_IF_INDEX);
        assert_eq!(interface, IF_INDEX);

        // Each `MIB_IPFORWARD_ROW2` reports the namespace of its own family, so
        // the two rows on this adapter disagree with each other. Both must
        // still land on the interface, because the join key is the LUID.
        let joined = Some(interface);
        assert_eq!(route_index(IF_INDEX, joined), interface, "the IPv4 route");
        assert_eq!(
            route_index(IPV6_IF_INDEX, joined),
            interface,
            "the IPv6 route"
        );

        // What made it a silent failure rather than a loud one: trusting the
        // row produces a plausible index that simply belongs to no interface.
        assert_ne!(
            IPV6_IF_INDEX, interface,
            "the fixture must actually exercise differing indices"
        );
    }

    #[test]
    fn an_unjoined_row_keeps_its_own_index_rather_than_being_dropped() {
        assert_eq!(route_index(7, None), 7);
        assert_eq!(route_index(0, None), 0);
    }

    #[test]
    fn the_effective_metric_is_the_documented_sum() {
        assert_eq!(effective_metric(0, 25), 25, "the common default-route case");
        assert_eq!(effective_metric(256, 25), 281);
    }

    #[test]
    fn the_effective_metric_saturates_rather_than_wrapping() {
        // Wrapping would turn the worst possible route into the best one.
        assert_eq!(effective_metric(u32::MAX, 25), u32::MAX);
        assert_eq!(effective_metric(u32::MAX, u32::MAX), u32::MAX);
    }

    #[test]
    fn loopback_and_multicast_rows_are_not_reported() {
        assert!(!is_reportable(true, ip("0.0.0.0")));
        assert!(!is_reportable(false, ip("224.0.0.251")));
        assert!(!is_reportable(false, ip("ff02::1")));
    }

    #[test]
    fn ordinary_rows_are_reported() {
        assert!(is_reportable(false, ip("0.0.0.0")), "a default route");
        assert!(is_reportable(false, ip("::")), "a v6 default route");
        assert!(is_reportable(false, ip("192.168.1.0")));
        // Link-local stays: Linux and macOS both report it.
        assert!(is_reportable(false, ip("169.254.0.0")));
        assert!(is_reportable(false, ip("fe80::")));
    }
}
