//! Pure address and interface classification.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::model::{AddressScope, InterfaceKind};

/// Classify an IP address into the scope the selection engine reasons about.
#[must_use]
pub fn classify_address(ip: &IpAddr) -> AddressScope {
    match ip {
        IpAddr::V4(v4) => classify_ipv4(v4),
        IpAddr::V6(v6) => classify_ipv6(v6),
    }
}

/// Classify an IPv4 address.
#[must_use]
pub fn classify_ipv4(ip: &Ipv4Addr) -> AddressScope {
    let o = ip.octets();

    if ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() {
        return AddressScope::Special;
    }
    if ip.is_loopback() {
        return AddressScope::Loopback;
    }
    if ip.is_link_local() {
        return AddressScope::LinkLocal;
    }
    // RFC 6598 carrier-grade NAT, 100.64.0.0/10.
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return AddressScope::Cgnat;
    }
    if ip.is_private() {
        return AddressScope::Private;
    }
    // Documentation, benchmarking, and reserved ranges.
    let is_documentation = matches!(
        (o[0], o[1], o[2]),
        (192, 0, 2) | (198, 51, 100) | (203, 0, 113)
    );
    if is_documentation || (o[0] == 198 && (o[1] & 0xfe) == 18) || o[0] >= 240 {
        return AddressScope::Special;
    }
    // 192.0.0.0/24 — IETF protocol assignments.
    if o[0] == 192 && o[1] == 0 && o[2] == 0 {
        return AddressScope::Special;
    }
    AddressScope::Global
}

/// Classify an IPv6 address.
#[must_use]
pub fn classify_ipv6(ip: &Ipv6Addr) -> AddressScope {
    let segments = ip.segments();

    if ip.is_unspecified() || ip.is_multicast() {
        return AddressScope::Special;
    }
    if ip.is_loopback() {
        return AddressScope::Loopback;
    }
    // fe80::/10 — link-local unicast. `is_unicast_link_local` is unstable, so
    // the prefix is masked directly.
    if (segments[0] & 0xffc0) == 0xfe80 {
        return AddressScope::LinkLocal;
    }
    // fc00::/7 — unique local addresses.
    if (segments[0] & 0xfe00) == 0xfc00 {
        return AddressScope::UniqueLocal;
    }
    // 2001:db8::/32 — documentation.
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return AddressScope::Special;
    }
    // ::ffff:0:0/96 — IPv4-mapped. These do not appear on real interfaces; if
    // one turns up, judge it by the IPv4 address it carries.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return classify_ipv4(&v4);
    }
    // fec0::/10 — deprecated site-local (RFC 3879).
    if (segments[0] & 0xffc0) == 0xfec0 {
        return AddressScope::Special;
    }
    AddressScope::Global
}

/// Decide what kind of device an interface is.
///
/// `link_kind` is the kernel's own link type (netlink's `IFLA_INFO_KIND`:
/// `"bridge"`, `"veth"`, `"wireguard"`, …). It is far more trustworthy than a
/// name, so it is consulted first; names only disambiguate cases the kernel
/// reports identically — most importantly telling a container runtime's bridge
/// apart from a user's own `br0`, both of which the kernel just calls `bridge`.
///
/// `is_wireless` comes from the platform backend (on Linux, the presence of
/// `/sys/class/net/<name>/phy80211`).
#[must_use]
pub fn classify_interface(
    name: &str,
    link_kind: Option<&str>,
    is_loopback: bool,
    is_wireless: bool,
) -> InterfaceKind {
    if is_loopback || name == "lo" {
        return InterfaceKind::Loopback;
    }
    if is_wireless {
        return InterfaceKind::Wireless;
    }

    if let Some(kind) = link_kind {
        match kind {
            "veth" => return InterfaceKind::Container,
            "bridge" => {
                return if is_container_name(name) {
                    InterfaceKind::Container
                } else if is_virtual_name(name) {
                    InterfaceKind::Virtual
                } else {
                    InterfaceKind::Bridge
                };
            }
            "tun" | "tap" | "wireguard" | "ppp" | "ipip" | "sit" | "gre" | "gretap" | "vti"
            | "vti6" | "xfrm" | "ip6tnl" | "wg" => return InterfaceKind::Vpn,
            // Aggregated and tagged links are still the real network path.
            "bond" | "vlan" | "macvlan" | "team" => return InterfaceKind::Ethernet,
            "dummy" | "ifb" | "nlmon" => return InterfaceKind::Virtual,
            _ => {
                // Unrecognised kernel kind: fall through to name heuristics, and
                // keep the kernel's own word for it if those find nothing.
                if let Some(by_name) = classify_by_name(name) {
                    return by_name;
                }
                return InterfaceKind::Other(kind.to_string());
            }
        }
    }

    // No kernel link type at all — the usual case for a physical NIC.
    classify_by_name(name).unwrap_or(InterfaceKind::Ethernet)
}

/// Name-only classification, used when the kernel offers no link type.
///
/// Returns `None` when the name says nothing useful, so callers can pick their
/// own fallback rather than being handed a wrong guess.
#[must_use]
pub fn classify_by_name(name: &str) -> Option<InterfaceKind> {
    if name == "lo" {
        return Some(InterfaceKind::Loopback);
    }
    if is_container_name(name) {
        return Some(InterfaceKind::Container);
    }
    if is_virtual_name(name) {
        return Some(InterfaceKind::Virtual);
    }
    if is_vpn_name(name) {
        return Some(InterfaceKind::Vpn);
    }
    if is_wireless_name(name) {
        return Some(InterfaceKind::Wireless);
    }
    if is_ethernet_name(name) {
        return Some(InterfaceKind::Ethernet);
    }
    None
}

/// Whether a name belongs to a container runtime's networking.
///
/// Note `br-<12 hex digits>`: Docker names every user-defined network's bridge
/// that way, and it is the only thing distinguishing those from a hand-made
/// `br0` that may well be the machine's real path to the LAN.
#[must_use]
pub fn is_container_name(name: &str) -> bool {
    const PREFIXES: [&str; 8] = [
        "docker", "veth", "cni", "flannel", "podman", "cbr", "kube", "cali",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p)) || is_docker_bridge_name(name)
}

/// Whether a name matches Docker's `br-<12 hex>` user-defined-network bridges.
#[must_use]
pub fn is_docker_bridge_name(name: &str) -> bool {
    name.strip_prefix("br-")
        .is_some_and(|id| id.len() == 12 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Whether a name belongs to a hypervisor or virtualisation device.
#[must_use]
pub fn is_virtual_name(name: &str) -> bool {
    const PREFIXES: [&str; 6] = ["virbr", "vboxnet", "vmnet", "vnet", "dummy", "ifb"];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Whether a name belongs to a tunnel or VPN device.
#[must_use]
pub fn is_vpn_name(name: &str) -> bool {
    const PREFIXES: [&str; 12] = [
        "tun",
        "tap",
        "wg",
        "ppp",
        "utun",
        "ipsec",
        "nordlynx",
        "proton",
        "tailscale",
        "zt",
        "mullvad",
        "gpd",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Whether a name looks like a Wi-Fi device (`wlan0`, `wlo1`, `wlp3s0`).
#[must_use]
pub fn is_wireless_name(name: &str) -> bool {
    name.starts_with("wl") || name.starts_with("wifi") || name.starts_with("ath")
}

/// Whether a name looks like a wired NIC, under either the classic or the
/// predictable naming scheme.
#[must_use]
pub fn is_ethernet_name(name: &str) -> bool {
    const PREFIXES: [&str; 6] = ["eth", "eno", "ens", "enp", "enx", "em"];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }
    fn v6(s: &str) -> Ipv6Addr {
        s.parse().unwrap()
    }

    #[test]
    fn ipv4_loopback() {
        assert_eq!(classify_ipv4(&v4("127.0.0.1")), AddressScope::Loopback);
        assert_eq!(
            classify_ipv4(&v4("127.255.255.254")),
            AddressScope::Loopback
        );
    }

    #[test]
    fn ipv4_link_local() {
        assert_eq!(classify_ipv4(&v4("169.254.0.1")), AddressScope::LinkLocal);
        assert_eq!(
            classify_ipv4(&v4("169.254.255.255")),
            AddressScope::LinkLocal
        );
        // Just outside the /16.
        assert_eq!(classify_ipv4(&v4("169.253.255.255")), AddressScope::Global);
    }

    #[test]
    fn ipv4_private_ranges() {
        for ip in [
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.101",
        ] {
            assert_eq!(classify_ipv4(&v4(ip)), AddressScope::Private, "{ip}");
        }
        // Docker's default bridge, and the user-defined-network bridges on this host.
        for ip in ["172.17.0.1", "172.20.0.1", "172.24.0.1"] {
            assert_eq!(classify_ipv4(&v4(ip)), AddressScope::Private, "{ip}");
        }
    }

    #[test]
    fn ipv4_private_boundaries_are_exact() {
        // 172.16/12 spans 172.16 through 172.31 only.
        assert_eq!(classify_ipv4(&v4("172.15.255.255")), AddressScope::Global);
        assert_eq!(classify_ipv4(&v4("172.32.0.0")), AddressScope::Global);
        assert_eq!(classify_ipv4(&v4("192.167.255.255")), AddressScope::Global);
        assert_eq!(classify_ipv4(&v4("192.169.0.0")), AddressScope::Global);
    }

    #[test]
    fn ipv4_cgnat() {
        assert_eq!(classify_ipv4(&v4("100.64.0.1")), AddressScope::Cgnat);
        assert_eq!(classify_ipv4(&v4("100.127.255.255")), AddressScope::Cgnat);
        // Either side of 100.64.0.0/10.
        assert_eq!(classify_ipv4(&v4("100.63.255.255")), AddressScope::Global);
        assert_eq!(classify_ipv4(&v4("100.128.0.0")), AddressScope::Global);
    }

    #[test]
    fn ipv4_special() {
        assert_eq!(classify_ipv4(&v4("0.0.0.0")), AddressScope::Special);
        assert_eq!(classify_ipv4(&v4("255.255.255.255")), AddressScope::Special);
        assert_eq!(classify_ipv4(&v4("224.0.0.1")), AddressScope::Special);
        assert_eq!(classify_ipv4(&v4("192.0.2.1")), AddressScope::Special);
        assert_eq!(classify_ipv4(&v4("198.51.100.1")), AddressScope::Special);
        assert_eq!(classify_ipv4(&v4("203.0.113.1")), AddressScope::Special);
        assert_eq!(classify_ipv4(&v4("198.18.0.1")), AddressScope::Special);
        assert_eq!(classify_ipv4(&v4("240.0.0.1")), AddressScope::Special);
    }

    #[test]
    fn ipv4_global() {
        assert_eq!(classify_ipv4(&v4("1.1.1.1")), AddressScope::Global);
        assert_eq!(classify_ipv4(&v4("8.8.8.8")), AddressScope::Global);
    }

    #[test]
    fn ipv6_scopes() {
        assert_eq!(classify_ipv6(&v6("::1")), AddressScope::Loopback);
        assert_eq!(classify_ipv6(&v6("::")), AddressScope::Special);
        assert_eq!(classify_ipv6(&v6("ff02::1")), AddressScope::Special);
        assert_eq!(classify_ipv6(&v6("2001:db8::1")), AddressScope::Special);
        assert_eq!(classify_ipv6(&v6("fec0::1")), AddressScope::Special);
    }

    #[test]
    fn ipv6_link_local_covers_the_whole_ten_bit_prefix() {
        // fe80::/10 is fe80 through febf — not just fe80.
        for ip in ["fe80::1", "fe80::a00:27ff:fe4e:66a1", "feaf::1", "febf::1"] {
            assert_eq!(classify_ipv6(&v6(ip)), AddressScope::LinkLocal, "{ip}");
        }
        assert_ne!(classify_ipv6(&v6("fe7f::1")), AddressScope::LinkLocal);
        assert_ne!(classify_ipv6(&v6("fec0::1")), AddressScope::LinkLocal);
    }

    #[test]
    fn ipv6_unique_local_covers_fc_and_fd() {
        assert_eq!(classify_ipv6(&v6("fc00::1")), AddressScope::UniqueLocal);
        assert_eq!(
            classify_ipv6(&v6("fd12:3456::1")),
            AddressScope::UniqueLocal
        );
        assert_eq!(classify_ipv6(&v6("fdff::1")), AddressScope::UniqueLocal);
    }

    #[test]
    fn ipv6_global() {
        // A well-known public address rather than a real host's, so that
        // fixtures and tests carry nobody's identifying prefix.
        assert_eq!(
            classify_ipv6(&v6("2606:4700:4700::1111")),
            AddressScope::Global
        );
        assert_eq!(
            classify_ipv6(&v6("2a00:1450:4009:81f::200e")),
            AddressScope::Global
        );
    }

    #[test]
    fn ipv4_mapped_is_judged_by_its_ipv4() {
        assert_eq!(
            classify_ipv6(&v6("::ffff:192.168.1.1")),
            AddressScope::Private
        );
        assert_eq!(classify_ipv6(&v6("::ffff:8.8.8.8")), AddressScope::Global);
    }

    #[test]
    fn address_scope_reachability() {
        assert!(AddressScope::Private.is_reachable_by_peers());
        assert!(AddressScope::Global.is_reachable_by_peers());
        assert!(!AddressScope::Loopback.is_reachable_by_peers());
        assert!(!AddressScope::LinkLocal.is_reachable_by_peers());
        assert!(!AddressScope::Special.is_reachable_by_peers());
    }

    #[test]
    fn kernel_link_kind_beats_the_name() {
        // A bridge called something entirely ordinary is still a bridge.
        assert_eq!(
            classify_interface("lan0", Some("bridge"), false, false),
            InterfaceKind::Bridge
        );
        assert_eq!(
            classify_interface("enp0s1", Some("veth"), false, false),
            InterfaceKind::Container
        );
    }

    #[test]
    fn docker_bridges_are_told_apart_from_user_bridges() {
        // Both are `bridge` to the kernel; only the name separates them.
        assert_eq!(
            classify_interface("br-18642d3532b2", Some("bridge"), false, false),
            InterfaceKind::Container
        );
        assert_eq!(
            classify_interface("br0", Some("bridge"), false, false),
            InterfaceKind::Bridge
        );
        assert_eq!(
            classify_interface("virbr0", Some("bridge"), false, false),
            InterfaceKind::Virtual
        );
    }

    #[test]
    fn docker_bridge_name_pattern_is_strict() {
        assert!(is_docker_bridge_name("br-18642d3532b2"));
        assert!(is_docker_bridge_name("br-1c8141f6b1d9"));
        assert!(!is_docker_bridge_name("br0"));
        assert!(!is_docker_bridge_name("br-lan"));
        // Right shape, wrong length.
        assert!(!is_docker_bridge_name("br-18642d3532"));
        assert!(!is_docker_bridge_name("br-18642d3532b2c"));
        // Right length, not hex.
        assert!(!is_docker_bridge_name("br-zzzzzzzzzzzz"));
    }

    #[test]
    fn wireless_wins_over_an_ethernet_looking_name() {
        assert_eq!(
            classify_interface("wlo1", None, false, true),
            InterfaceKind::Wireless
        );
        // Even with no sysfs evidence, the name is enough.
        assert_eq!(
            classify_interface("wlp3s0", None, false, false),
            InterfaceKind::Wireless
        );
    }

    #[test]
    fn physical_nics_without_a_link_kind_are_ethernet() {
        for name in ["eno2", "eth0", "enp0s31f6", "ens33", "em1"] {
            assert_eq!(
                classify_interface(name, None, false, false),
                InterfaceKind::Ethernet,
                "{name}"
            );
        }
    }

    #[test]
    fn vpn_devices() {
        assert_eq!(
            classify_interface("wg0", Some("wireguard"), false, false),
            InterfaceKind::Vpn
        );
        assert_eq!(
            classify_interface("tun0", Some("tun"), false, false),
            InterfaceKind::Vpn
        );
        assert_eq!(
            classify_interface("tailscale0", None, false, false),
            InterfaceKind::Vpn
        );
    }

    #[test]
    fn loopback_flag_wins_regardless_of_name() {
        assert_eq!(
            classify_interface("anything", None, true, false),
            InterfaceKind::Loopback
        );
        assert_eq!(
            classify_interface("lo", None, false, false),
            InterfaceKind::Loopback
        );
    }

    #[test]
    fn bonds_and_vlans_count_as_the_real_network_path() {
        assert_eq!(
            classify_interface("bond0", Some("bond"), false, false),
            InterfaceKind::Ethernet
        );
        assert_eq!(
            classify_interface("eth0.100", Some("vlan"), false, false),
            InterfaceKind::Ethernet
        );
    }

    #[test]
    fn unknown_kernel_kind_is_preserved() {
        assert_eq!(
            classify_interface("weird0", Some("batadv"), false, false),
            InterfaceKind::Other("batadv".to_string())
        );
    }

    #[test]
    fn synthetic_kinds() {
        assert!(InterfaceKind::Container.is_synthetic());
        assert!(InterfaceKind::Virtual.is_synthetic());
        assert!(!InterfaceKind::Ethernet.is_synthetic());
        assert!(!InterfaceKind::Bridge.is_synthetic());
        assert!(!InterfaceKind::Vpn.is_synthetic());
    }
}
