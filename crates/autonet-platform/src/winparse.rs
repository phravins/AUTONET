//! Decoding the scalar values the Windows IP Helper API returns.
//!
//! Kept outside `windows/` for the reason [`crate::rtparse`] is kept outside
//! `macos/`: it is pure, so its tests run on the Linux job. That matters most
//! for [`sockaddr_ip`], where a wrong offset yields a *plausible* address rather
//! than an error.
//!
//! Bytes rather than windows-sys structs, because `SOCKET_ADDRESS` hands over a
//! `*mut SOCKADDR` whose concrete type is decided by the two bytes at its front,
//! and windows-sys declares no `sockaddr_in` to cast to. A Windows `sockaddr`
//! begins directly with a two-byte `sa_family` where the BSD one begins with
//! `sa_len`, and `AF_INET6` is 23 here against 30 on Darwin and 10 on Linux, so
//! nothing may be assumed from either sibling. Every constant below is pinned to
//! windows-sys's own definition by a `const` assertion in the backend.
//!
//! **Verified against headers and the compiler, not against a live machine.**

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use autonet_core::model::InterfaceState;

/// Address families, as Windows numbers them.
///
/// Restated rather than imported so that the parser and its tests build on
/// Linux. The `const` block in the backend proves they match windows-sys.
pub(crate) mod af {
    /// IPv4. The same number everywhere, as it happens.
    pub(crate) const INET: u16 = 2;
    /// IPv6. Note this is neither Linux's 10 nor Darwin's 30.
    pub(crate) const INET6: u16 = 23;
}

/// `IF_OPER_STATUS`, the RFC 2863 operational states.
///
/// This is the same enumeration Linux's `IF_OPER_*` implements, which is why
/// [`interface_state`] can mirror the netlink backend's mapping rather than
/// inventing one.
pub(crate) mod oper {
    /// Up and able to pass packets.
    pub(crate) const UP: i32 = 1;
    /// Down and not in a condition to pass packets.
    pub(crate) const DOWN: i32 = 2;
    /// In testing mode.
    pub(crate) const TESTING: i32 = 3;
    /// The operational status is not known.
    pub(crate) const UNKNOWN: i32 = 4;
    /// Not up, but waiting on an external event.
    pub(crate) const DORMANT: i32 = 5;
    /// Down because a component — typically hardware — is absent.
    pub(crate) const NOT_PRESENT: i32 = 6;
    /// Down because an interface underneath it is down.
    pub(crate) const LOWER_LAYER_DOWN: i32 = 7;
}

/// Byte offsets and sizes within the two `sockaddr` variants we decode.
mod layout {
    /// `sa_family`, a `u16` at the very front of every `sockaddr`.
    pub(super) const FAMILY: usize = 0;
    /// `sin_addr`, after `sin_family` and `sin_port`.
    pub(super) const INET_ADDR: usize = 4;
    /// `sin6_addr`, after `sin6_family`, `sin6_port` and `sin6_flowinfo`.
    pub(super) const INET6_ADDR: usize = 8;
}

/// The IP address inside a `sockaddr`, or `None` if the bytes cannot hold one.
///
/// The bounds checks are the point: a buffer shorter than the family it claims
/// must produce nothing, not an address built from whatever followed in memory.
pub(crate) fn sockaddr_ip(bytes: &[u8]) -> Option<IpAddr> {
    let family: [u8; 2] = bytes
        .get(layout::FAMILY..layout::FAMILY + 2)?
        .try_into()
        .ok()?;

    match u16::from_ne_bytes(family) {
        af::INET => {
            let octets: [u8; 4] = bytes
                .get(layout::INET_ADDR..layout::INET_ADDR + 4)?
                .try_into()
                .ok()?;
            Some(IpAddr::V4(Ipv4Addr::from(octets)))
        }
        af::INET6 => {
            let octets: [u8; 16] = bytes
                .get(layout::INET6_ADDR..layout::INET6_ADDR + 16)?
                .try_into()
                .ok()?;
            Some(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        _ => None,
    }
}

/// What `OperStatus` means in the model's vocabulary.
///
/// Identical to the Linux backend's mapping of `IF_OPER_*` — the same RFC 2863
/// enumeration — so the two cannot disagree silently.
///
/// Anything unrecognised becomes [`InterfaceState::Unknown`] rather than `Down`:
/// `Unknown` still lets the selector consider an interface, where guessing
/// `Down` would hide a working link if Microsoft adds a state.
pub(crate) fn interface_state(status: i32) -> InterfaceState {
    match status {
        oper::UP => InterfaceState::Up,
        oper::DOWN | oper::NOT_PRESENT | oper::LOWER_LAYER_DOWN => InterfaceState::Down,
        oper::DORMANT | oper::TESTING => InterfaceState::Dormant,
        _ => InterfaceState::Unknown,
    }
}

/// A UTF-16 string from Windows, rendered lossily and truncated at its NUL.
///
/// Lossy rather than fallible: an adapter whose name contains an unpaired
/// surrogate is still an adapter. The NUL scan is repeated here, though the
/// caller passes only the units before the terminator, so it is testable.
pub(crate) fn wide_to_string(units: &[u16]) -> String {
    let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// A prefix length clamped to what its family can actually have.
///
/// `OnLinkPrefixLength` is a `u8` and tunnel adapters have been observed
/// reporting `255`, which would otherwise flow into the JSON contract. Clamping
/// to the host length is the conservative reading: a `/32` claims nothing about
/// the surrounding network, where a `/255` claims something impossible.
pub(crate) fn prefix_len(raw: u8, ip: &IpAddr) -> u8 {
    let max = if ip.is_ipv4() { 32 } else { 128 };
    raw.min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Windows `sockaddr_in`: family, port, address, then eight zero bytes.
    fn sockaddr_in(octets: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&af::INET.to_ne_bytes());
        bytes.extend_from_slice(&3000u16.to_be_bytes());
        bytes.extend_from_slice(&octets);
        bytes.extend_from_slice(&[0; 8]);
        bytes
    }

    /// A Windows `sockaddr_in6`: family, port, flowinfo, address, scope id.
    fn sockaddr_in6(octets: [u8; 16], scope: u32) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(28);
        bytes.extend_from_slice(&af::INET6.to_ne_bytes());
        bytes.extend_from_slice(&3000u16.to_be_bytes());
        bytes.extend_from_slice(&7u32.to_be_bytes());
        bytes.extend_from_slice(&octets);
        bytes.extend_from_slice(&scope.to_ne_bytes());
        bytes
    }

    #[test]
    fn an_ipv4_sockaddr_decodes_past_the_port() {
        // The port sits between the family and the address. Reading the address
        // at offset 2 rather than 4 would yield 0.11.192.168 here, which is a
        // perfectly plausible-looking wrong answer.
        assert_eq!(
            sockaddr_ip(&sockaddr_in([192, 168, 1, 101])),
            Some("192.168.1.101".parse().unwrap())
        );
    }

    #[test]
    fn an_ipv6_sockaddr_decodes_past_the_flow_label() {
        let octets = [
            0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
        ];
        assert_eq!(
            sockaddr_ip(&sockaddr_in6(octets, 0)),
            Some("2001:db8::1".parse().unwrap())
        );
    }

    #[test]
    fn a_link_local_address_ignores_its_scope_id() {
        // The model has nowhere to put a scope id and `IpAddr` cannot hold
        // `fe80::1%12`. Dropping it is a real limitation, recorded as such.
        let octets = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01];
        assert_eq!(
            sockaddr_ip(&sockaddr_in6(octets, 12)),
            Some("fe80::1".parse().unwrap())
        );
    }

    #[test]
    fn a_truncated_sockaddr_yields_nothing_rather_than_a_short_address() {
        let full = sockaddr_in([10, 0, 0, 1]);
        for len in 0..full.len() {
            let truncated = &full[..len];
            if len < 8 {
                assert_eq!(sockaddr_ip(truncated), None, "{len} bytes should not parse");
            } else {
                assert!(sockaddr_ip(truncated).is_some(), "{len} bytes is enough");
            }
        }

        let full = sockaddr_in6([0; 16], 0);
        assert_eq!(sockaddr_ip(&full[..23]), None, "one byte short of an IPv6");
    }

    #[test]
    fn an_unknown_family_is_not_guessed_at() {
        // AF_LINK-alikes, AF_UNSPEC and anything Microsoft adds later.
        for family in [0u16, 1, 10, 17, 30, 32, 0xffff] {
            let mut bytes = family.to_ne_bytes().to_vec();
            bytes.extend_from_slice(&[0xab; 26]);
            assert_eq!(sockaddr_ip(&bytes), None, "family {family}");
        }
    }

    #[test]
    fn operational_states_match_the_linux_mapping() {
        assert_eq!(interface_state(oper::UP), InterfaceState::Up);
        assert_eq!(interface_state(oper::DOWN), InterfaceState::Down);
        assert_eq!(interface_state(oper::NOT_PRESENT), InterfaceState::Down);
        assert_eq!(
            interface_state(oper::LOWER_LAYER_DOWN),
            InterfaceState::Down
        );
        assert_eq!(interface_state(oper::DORMANT), InterfaceState::Dormant);
        assert_eq!(interface_state(oper::TESTING), InterfaceState::Dormant);
        assert_eq!(interface_state(oper::UNKNOWN), InterfaceState::Unknown);
    }

    #[test]
    fn an_unrecognised_state_stays_selectable() {
        // Not `Down`: `is_down()` disqualifies an interface outright, so
        // guessing wrong there would hide a working link entirely.
        assert_eq!(interface_state(99), InterfaceState::Unknown);
        assert_eq!(interface_state(-1), InterfaceState::Unknown);
        assert!(!interface_state(99).is_down());
    }

    #[test]
    fn wide_strings_stop_at_the_terminator() {
        let units: Vec<u16> = "Wi-Fi\0Ethernet".encode_utf16().collect();
        assert_eq!(wide_to_string(&units), "Wi-Fi");
        assert_eq!(wide_to_string(&[]), "");
        assert_eq!(wide_to_string(&[0]), "");
    }

    #[test]
    fn wide_strings_survive_non_ascii_and_bad_surrogates() {
        let units: Vec<u16> = "Ethernet 2 — Büro".encode_utf16().collect();
        assert_eq!(wide_to_string(&units), "Ethernet 2 — Büro");

        // A lone high surrogate. An adapter is not worth losing over one.
        assert_eq!(wide_to_string(&[0x0041, 0xd800, 0x0042]), "A\u{fffd}B");
    }

    #[test]
    fn prefix_lengths_are_clamped_to_their_family() {
        let v4: IpAddr = "192.168.1.1".parse().unwrap();
        let v6: IpAddr = "2001:db8::1".parse().unwrap();

        assert_eq!(prefix_len(24, &v4), 24);
        assert_eq!(prefix_len(64, &v6), 64);
        assert_eq!(prefix_len(255, &v4), 32, "observed on tunnel adapters");
        assert_eq!(prefix_len(255, &v6), 128);
        assert_eq!(prefix_len(0, &v4), 0, "a default route's prefix is legal");
    }
}
