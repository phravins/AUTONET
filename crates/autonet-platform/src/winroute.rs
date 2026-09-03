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
