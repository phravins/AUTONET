//! Which listening socket a bind would collide with.
//!
//! Shared by the Linux and Windows lookups, which reach the same question from
//! different tables. Kept out of both and pure, so the precedence rules — the
//! part that is easy to get subtly and silently wrong — are written once and
//! tested without a live socket.

use std::net::IpAddr;

/// A listening socket, reduced to what collision depends on.
pub(crate) trait Bound {
    /// The address it is bound to, unspecified for a wildcard bind.
    fn address(&self) -> IpAddr;

    /// The port it is listening on.
    fn port(&self) -> u16;
}

/// The listener that would collide with a bind to `(address, port)`.
///
/// An exact match is preferred because it names the most specific holder; a
/// broader one is remembered rather than returned on sight, so an exact row
/// wins wherever it appears in the table.
pub(crate) fn collides<T: Bound>(rows: &[T], address: IpAddr, port: u16) -> Option<&T> {
    let wanted = canonical(address);
    let mut broader = None;

    for row in rows.iter().filter(|row| row.port() == port) {
        let bound = canonical(row.address());
        if bound == wanted {
            return Some(row);
        }
        if contends(bound, wanted) {
            broader = broader.or(Some(row));
        }
    }

    broader
}

/// Whether a bind to `wanted` would contend with a listener already on `bound`.
///
/// The relation is **symmetric in the wildcard**, which is the part worth
/// spelling out: a listener on `0.0.0.0` blocks a bind to one address, and a
/// bind to `0.0.0.0` is equally blocked by a listener on one address. Asking
/// only the first question would leave the second case — the interesting one,
/// where the selected address is free and the wildcard is not — with a busy
/// port and nobody to name for it.
///
/// `::` covers IPv4 as well, because a dual-stack socket serves both families
/// through one binding. Windows defaults `IPV6_V6ONLY` on, where Linux defaults
/// it off, so that arm can over-report there; it decides only *whom to name*,
/// never whether the port is busy, which is settled by a real bind in the CLI.
fn contends(bound: IpAddr, wanted: IpAddr) -> bool {
    /// Whether `wide` being a wildcard subsumes `narrow`.
    fn covers(wide: IpAddr, narrow: IpAddr) -> bool {
        wide.is_unspecified() && (wide.is_ipv6() || narrow.is_ipv4())
    }

    bound == wanted || covers(bound, wanted) || covers(wanted, bound)
}

/// Collapse `::ffff:a.b.c.d` onto the IPv4 address it stands for.
///
/// A socket bound to a v4-mapped address is listed only in the IPv6 table, yet
/// it holds the IPv4 port. Comparing the two spellings as written would miss
/// it and report the port free.
fn canonical(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4 @ IpAddr::V4(_) => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listener with nothing behind it.
    #[derive(Debug, PartialEq, Eq)]
    struct Row {
        address: IpAddr,
        port: u16,
    }

    impl Bound for Row {
        fn address(&self) -> IpAddr {
            self.address
        }

        fn port(&self) -> u16 {
            self.port
        }
    }

    fn row(address: &str, port: u16) -> Row {
        Row {
            address: address.parse().expect("a test address"),
            port,
        }
    }

    fn wanted(address: &str) -> IpAddr {
        address.parse().expect("a test address")
    }

    #[test]
    fn a_wildcard_listener_collides_with_a_specific_address() {
        let rows = vec![row("0.0.0.0", 3000)];
        assert_eq!(
            collides(&rows, wanted("192.168.1.20"), 3000),
            Some(&rows[0])
        );
    }

    #[test]
    fn an_exact_match_wins_over_a_wildcard_whichever_comes_first() {
        let first = vec![row("0.0.0.0", 3000), row("192.168.1.20", 3000)];
        assert_eq!(
            collides(&first, wanted("192.168.1.20"), 3000),
            Some(&first[1])
        );

        let second = vec![row("192.168.1.20", 3000), row("0.0.0.0", 3000)];
        assert_eq!(
            collides(&second, wanted("192.168.1.20"), 3000),
            Some(&second[0])
        );
    }

    #[test]
    fn a_listener_on_another_address_is_not_a_collision() {
        // This is the asymmetry the CLI's second probe exists for: 127.0.0.1
        // being busy leaves the LAN address bindable.
        let rows = vec![row("127.0.0.1", 3000)];
        assert!(collides(&rows, wanted("192.168.1.20"), 3000).is_none());
    }

    #[test]
    fn a_listener_on_another_port_is_ignored() {
        let rows = vec![row("0.0.0.0", 3001)];
        assert!(collides(&rows, wanted("192.168.1.20"), 3000).is_none());
    }

    #[test]
    fn a_dual_stack_listener_holds_the_ipv4_port_too() {
        // `[::]` is listed only in the IPv6 table and still blocks 0.0.0.0.
        let rows = vec![row("::", 3000)];
        assert_eq!(
            collides(&rows, wanted("192.168.1.20"), 3000),
            Some(&rows[0])
        );
    }

    #[test]
    fn a_v4_mapped_row_matches_the_plain_v4_address() {
        let rows = vec![row("::ffff:192.168.1.20", 3000)];
        assert_eq!(
            collides(&rows, wanted("192.168.1.20"), 3000),
            Some(&rows[0])
        );
    }

    #[test]
    fn a_bind_to_the_wildcard_contends_with_a_listener_on_one_address() {
        // The reverse of the first case, and the one that names the holder
        // when the selected address is free but the wildcard is not.
        let rows = vec![row("127.0.0.1", 3000)];
        assert_eq!(collides(&rows, wanted("0.0.0.0"), 3000), Some(&rows[0]));
    }

    #[test]
    fn the_two_families_do_not_contend_through_an_ipv4_wildcard() {
        // 0.0.0.0 does not cover ::1, in either direction. Treating "wildcard"
        // as "everything" would report an IPv6 service as holding an IPv4 port.
        let v6 = vec![row("::1", 3000)];
        assert!(collides(&v6, wanted("0.0.0.0"), 3000).is_none());

        let v4 = vec![row("0.0.0.0", 3000)];
        assert!(collides(&v4, wanted("::1"), 3000).is_none());
    }

    #[test]
    fn a_bind_to_the_ipv6_wildcard_contends_with_an_ipv4_listener() {
        // The dual-stack direction: `::` and 0.0.0.0 are not independent.
        let rows = vec![row("127.0.0.1", 3000)];
        assert_eq!(collides(&rows, wanted("::"), 3000), Some(&rows[0]));
    }

    #[test]
    fn an_empty_table_yields_nothing_rather_than_a_guess() {
        let rows: Vec<Row> = Vec::new();
        assert!(collides(&rows, wanted("192.168.1.20"), 3000).is_none());
    }
}
