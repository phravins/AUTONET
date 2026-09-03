//! What *sort* of device an interface is, on macOS.
//!
//! A pure decision over already-gathered evidence, kept out of the `cfg`-gated
//! backend so the table below and its tests run on the Linux job too.
//!
//! SystemConfiguration leads and `ifi_type` follows, despite `ifi_type` arriving
//! free with the `AF_LINK` walk, for one decisive reason: **`ifi_type` cannot
//! tell Wi-Fi from Ethernet.** A Mac's Wi-Fi card reports `IFT_ETHER`;
//! `IFT_IEEE80211` never surfaces through the BSD layer. Apple's guidance is to
//! use `SCNetworkInterfaceCopyAll`. `ifi_type` still earns its place: it is
//! free, and it describes devices SystemConfiguration does not enumerate at all,
//! which is where the interesting tunnels live.
//!
//! **The premises here are documented, not observed.**

use autonet_core::model::InterfaceKind;

/// Link types from Apple's `<net/if_types.h>`.
///
/// Hardcoded because `libc` defines no `IFT_*` constants for any BSD. Most are
/// IANA `ifType` registry assignments rather than Apple inventions, so the
/// numbers are stable: `IFT_ETHER` 6, `IFT_PPP` 23, `IFT_LOOP` 24, `IFT_L2VLAN`
/// 135, `IFT_IEEE8023ADLAG` 136, `IFT_BRIDGE` 209. `IFT_CELLULAR` is Apple's.
mod ift {
    /// Assigned when nothing more specific fits. `utun` devices report this.
    pub const OTHER: u8 = 0x01;
    /// Ethernet — *and Wi-Fi*, which is the whole problem.
    pub const ETHER: u8 = 0x06;
    /// Point-to-point protocol, as used by older dial-up style VPNs.
    pub const PPP: u8 = 0x17;
    /// The loopback device.
    pub const LOOP: u8 = 0x18;
    /// Generic tunnel interface (`gif`).
    pub const GIF: u8 = 0x37;
    /// 6to4 tunnel interface (`stf`).
    pub const STF: u8 = 0x39;
    /// A VLAN-tagged pseudo-interface.
    pub const L2VLAN: u8 = 0x87;
    /// An 802.3ad link aggregate — a bond.
    pub const IEEE8023ADLAG: u8 = 0x88;
    /// A bridge pseudo-interface.
    pub const BRIDGE: u8 = 0xd1;
    /// Apple-specific: the cellular data interface.
    pub const CELLULAR: u8 = 0xff;
}

/// What to pass as `ifi_type` when the link layer said nothing.
///
/// `IFT_OTHER` is the header's own "unspecified", so a missing `if_data` block
/// and an uninformative one take the same path through [`classify`] rather than
/// needing an `Option` that every caller would have to unwrap the same way.
pub(crate) const IFI_TYPE_UNSPECIFIED: u8 = ift::OTHER;

/// The interface types SystemConfiguration reports.
///
/// A mirror of `system_configuration::SCNetworkInterfaceType`, which is
/// macOS-only and neither `Copy` nor comparable. Restating it lets the decision
/// table live outside the `cfg` gate; `macos/scnetwork.rs` translates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScType {
    /// Wired Ethernet.
    Ethernet,
    /// Wi-Fi.
    IEEE80211,
    /// A bridge.
    Bridge,
    /// An Ethernet bond.
    Bond,
    /// A VLAN-tagged interface.
    Vlan,
    /// FireWire networking.
    FireWire,
    /// A 6to4 tunnel.
    SixToFour,
    /// An IPSec VPN configured in Network preferences.
    IpSec,
    /// An L2TP VPN.
    L2tp,
    /// A PPP link.
    Ppp,
    /// A PPTP VPN. Deprecated by Apple in favour of PPP.
    Pptp,
    /// A dial-up modem.
    Modem,
    /// A serial link.
    Serial,
    /// Cellular / mobile broadband.
    Wwan,
    /// Bluetooth PAN.
    Bluetooth,
    /// Infrared. Deprecated since macOS 12.
    IrDa,
    /// A pseudo-type SystemConfiguration uses for IPv4 configuration; it says
    /// nothing about the hardware, so the table falls through past it.
    Ipv4,
}

/// Everything known about an interface that bears on what kind it is.
///
/// Only facts the kernel or a framework stated. There is no `name` field, and
/// that is the point: `en0` is Wi-Fi on a laptop and Ethernet on a Mac Pro, so a
/// name-based guess fails differently on different machines.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Evidence {
    /// The `IFF_LOOPBACK` flag.
    pub loopback: bool,
    /// The `IFF_POINTOPOINT` flag.
    pub point_to_point: bool,
    /// What SystemConfiguration calls this device, if it enumerates it at all.
    pub sc: Option<ScType>,
    /// `if_data.ifi_type` from the `AF_LINK` record.
    pub ifi_type: u8,
}

/// Decide what kind of device an interface is.
///
/// Sources are consulted most-trustworthy first:
///
/// 1. `IFF_LOOPBACK` — the kernel stating a fact about itself.
/// 2. SystemConfiguration — the OS's own answer, and the only source that
///    distinguishes Wi-Fi from Ethernet.
/// 3. `ifi_type` — coarser, but covers devices SystemConfiguration omits.
/// 4. `IFF_POINTOPOINT` — the last resort that catches modern VPN tunnels.
pub(crate) fn classify(evidence: Evidence) -> InterfaceKind {
    if evidence.loopback {
        return InterfaceKind::Loopback;
    }

    if let Some(kind) = evidence.sc.and_then(from_sc) {
        return kind;
    }

    if let Some(kind) = from_ifi_type(evidence.ifi_type) {
        return kind;
    }

    // Nothing named this device, but it is point-to-point — on macOS that is
    // overwhelmingly a `utun`, which WireGuard, Tailscale and every
    // NEPacketTunnelProvider VPN create and none of which SystemConfiguration
    // enumerates. This is what stops a VPN address outranking the real LAN
    // address, and it rests on a kernel flag rather than the name `utun`.
    if evidence.point_to_point {
        return InterfaceKind::Vpn;
    }

    InterfaceKind::Other(UNCLASSIFIED.to_owned())
}

/// What `kind` says when no source could identify the device.
///
/// Reported rather than guessed. `InterfaceKind::Other` scores zero, so an
/// unidentified device is neither preferred nor penalised — which is the honest
/// position when AutoNet genuinely does not know.
pub(crate) const UNCLASSIFIED: &str = "unclassified";

/// Map SystemConfiguration's answer onto AutoNet's vocabulary.
///
/// Returns `None` for [`ScType::Ipv4`], which describes a configuration method
/// rather than a device, so the caller falls through to the next source.
fn from_sc(sc: ScType) -> Option<InterfaceKind> {
    // Aggregates and tagged links resolve to Ethernet, matching the Linux
    // backend's bond/vlan/macvlan/team mapping: a bonded uplink should not
    // score differently depending on which OS observed it.
    Some(match sc {
        ScType::Ethernet | ScType::Bond | ScType::Vlan | ScType::FireWire => {
            InterfaceKind::Ethernet
        }
        ScType::IEEE80211 => InterfaceKind::Wireless,
        ScType::Bridge => InterfaceKind::Bridge,
        // The old-style VPN service types, plus 6to4 — which Linux also treats
        // as a tunnel via its "sit" link kind.
        ScType::IpSec | ScType::L2tp | ScType::Ppp | ScType::Pptp | ScType::SixToFour => {
            InterfaceKind::Vpn
        }
        // Real devices AutoNet has no considered opinion about yet. `Other`
        // carries the source's own word for it rather than inventing one.
        ScType::Wwan => InterfaceKind::Other("wwan".to_owned()),
        ScType::Bluetooth => InterfaceKind::Other("bluetooth".to_owned()),
        ScType::Modem => InterfaceKind::Other("modem".to_owned()),
        ScType::Serial => InterfaceKind::Other("serial".to_owned()),
        ScType::IrDa => InterfaceKind::Other("irda".to_owned()),
        ScType::Ipv4 => return None,
    })
}

/// Map `if_data.ifi_type` onto AutoNet's vocabulary.
///
/// Returns `None` for [`ift::OTHER`] and anything unrecognised, both of which
/// mean "the link layer declined to say".
fn from_ifi_type(ifi_type: u8) -> Option<InterfaceKind> {
    Some(match ifi_type {
        ift::LOOP => InterfaceKind::Loopback,
        ift::BRIDGE => InterfaceKind::Bridge,
        // L2VLAN and IEEE8023ADLAG for the same reasoning as `from_sc`.
        // IFT_ETHER is the weakest of the three: it is reached only when SC did
        // not enumerate the device, so Ethernet is the best available reading
        // rather than a certain one.
        ift::L2VLAN | ift::IEEE8023ADLAG | ift::ETHER => InterfaceKind::Ethernet,
        ift::PPP | ift::GIF | ift::STF => InterfaceKind::Vpn,
        ift::CELLULAR => InterfaceKind::Other("cellular".to_owned()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An interface with nothing remarkable about it.
    fn evidence() -> Evidence {
        Evidence {
            loopback: false,
            point_to_point: false,
            sc: None,
            ifi_type: ift::OTHER,
        }
    }

    #[test]
    fn wifi_is_wireless_even_though_the_link_layer_calls_it_ethernet() {
        // The case the whole SC-first design exists for: macOS reports
        // IFT_ETHER for a Wi-Fi card, so ifi_type alone would say Ethernet.
        let kind = classify(Evidence {
            sc: Some(ScType::IEEE80211),
            ifi_type: ift::ETHER,
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Wireless);
    }

    #[test]
    fn system_configuration_outranks_the_link_layer() {
        // Same evidence as above stated the other way round: whenever the two
        // sources disagree, SC wins.
        let kind = classify(Evidence {
            sc: Some(ScType::Bridge),
            ifi_type: ift::ETHER,
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Bridge);
    }

    #[test]
    fn a_tunnel_absent_from_system_configuration_is_a_vpn() {
        // utun as WireGuard, Tailscale and NEPacketTunnelProvider leave it:
        // unknown to SC, IFT_OTHER, point-to-point.
        let kind = classify(Evidence {
            point_to_point: true,
            sc: None,
            ifi_type: ift::OTHER,
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Vpn);
    }

    #[test]
    fn loopback_is_decided_before_anything_else_is_consulted() {
        // IFF_LOOPBACK is a kernel fact; no other source can override it.
        let kind = classify(Evidence {
            loopback: true,
            sc: Some(ScType::Ethernet),
            ifi_type: ift::ETHER,
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Loopback);
    }

    #[test]
    fn aggregated_and_tagged_links_are_still_the_real_network_path() {
        // Matches the Linux backend, which maps bond/vlan/macvlan/team to
        // Ethernet. Scoring them as Other would cost them 250 points against a
        // plain port, which is backwards for a bonded uplink.
        for sc in [ScType::Bond, ScType::Vlan] {
            let kind = classify(Evidence {
                sc: Some(sc),
                ..evidence()
            });
            assert_eq!(kind, InterfaceKind::Ethernet, "{sc:?}");
        }
        for ifi_type in [ift::IEEE8023ADLAG, ift::L2VLAN] {
            let kind = classify(Evidence {
                ifi_type,
                ..evidence()
            });
            assert_eq!(kind, InterfaceKind::Ethernet, "ifi_type {ifi_type:#x}");
        }
    }

    #[test]
    fn the_link_layer_answers_for_devices_system_configuration_omits() {
        for (ifi_type, expected) in [
            (ift::BRIDGE, InterfaceKind::Bridge),
            (ift::LOOP, InterfaceKind::Loopback),
            (ift::PPP, InterfaceKind::Vpn),
            (ift::GIF, InterfaceKind::Vpn),
            (ift::STF, InterfaceKind::Vpn),
            (ift::ETHER, InterfaceKind::Ethernet),
            (ift::CELLULAR, InterfaceKind::Other("cellular".to_owned())),
        ] {
            let kind = classify(Evidence {
                ifi_type,
                ..evidence()
            });
            assert_eq!(kind, expected, "ifi_type {ifi_type:#x}");
        }
    }

    #[test]
    fn old_style_vpn_service_types_are_penalised_as_tunnels() {
        for sc in [
            ScType::IpSec,
            ScType::L2tp,
            ScType::Ppp,
            ScType::Pptp,
            ScType::SixToFour,
        ] {
            let kind = classify(Evidence {
                sc: Some(sc),
                ..evidence()
            });
            assert_eq!(kind, InterfaceKind::Vpn, "{sc:?}");
        }
    }

    #[test]
    fn devices_autonet_has_no_opinion_about_are_named_not_guessed() {
        for (sc, expected) in [
            (ScType::Wwan, "wwan"),
            (ScType::Bluetooth, "bluetooth"),
            (ScType::Modem, "modem"),
            (ScType::Serial, "serial"),
            (ScType::IrDa, "irda"),
        ] {
            let kind = classify(Evidence {
                sc: Some(sc),
                ..evidence()
            });
            assert_eq!(kind, InterfaceKind::Other(expected.to_owned()), "{sc:?}");
        }
    }

    #[test]
    fn the_ipv4_pseudo_type_falls_through_to_the_link_layer() {
        // kSCNetworkInterfaceTypeIPv4 describes a configuration method, not a
        // device, so it must not shadow the real answer underneath.
        let kind = classify(Evidence {
            sc: Some(ScType::Ipv4),
            ifi_type: ift::BRIDGE,
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Bridge);
    }

    #[test]
    fn a_device_no_source_recognises_is_reported_as_unknown() {
        // Not Ethernet. Defaulting to a real kind here is exactly the
        // wrong-but-plausible guess this module exists to avoid.
        let kind = classify(evidence());
        assert_eq!(kind, InterfaceKind::Other(UNCLASSIFIED.to_owned()));
    }

    #[test]
    fn a_point_to_point_link_the_link_layer_named_keeps_that_name() {
        // IFF_POINTOPOINT is the last resort, not an override: a PPP link is
        // already Vpn, and a point-to-point Ethernet stays Ethernet.
        let kind = classify(Evidence {
            point_to_point: true,
            ifi_type: ift::ETHER,
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Ethernet);
    }
}
