//! What *sort* of device an interface is, on Windows.
//!
//! Outside `windows/` for the same reason [`crate::linktype`] is outside
//! `macos/`: this is a pure decision over already-gathered evidence, so it is
//! compiled and tested on Linux too. A misclassification does not crash — it
//! quietly moves an address hundreds of points up or down — so it needs
//! exercising somewhere a failure can be debugged.
//!
//! `IfType` settles half the question. Wi-Fi versus Ethernet it genuinely
//! answers, since `IF_TYPE_IEEE80211` is what NDIS reports for an 802.11
//! miniport — macOS's problem, where Darwin reports `IFT_ETHER` for a radio,
//! does not recur, so **WlanAPI is not needed and stays disabled**. What
//! `IfType` cannot answer is Ethernet versus a virtual adapter *presenting* as
//! Ethernet: a TAP-mode VPN, a Hyper-V switch and a WSL switch all report
//! `IF_TYPE_ETHERNET_CSMACD` and would collect `KIND_ETHERNET`'s +250 — the
//! failure that let a VPN outrank a real LAN address on macOS before the
//! `IFF_POINTOPOINT` fix.
//!
//! `GetIfTable2` supplies the rest, in one call for the whole machine, joined by
//! LUID. `GetIfEntry2` was rejected as the same data N times over, WlanAPI as
//! answering only the half that already works, and WMI as COM-bound and
//! string-shaped. `AccessType` is the load-bearing addition:
//! `NET_IF_ACCESS_POINT_TO_POINT` is the exact `IFF_POINTOPOINT` analogue.
//!
//! `Description`, `Alias` and the adapter GUID are deliberately not consulted —
//! matching `"TAP-"` or `"WireGuard"` classifies today's VPNs and misses
//! tomorrow's. Every input below is a numeric field NDIS filled in.
//!
//! **Table-checked, not hardware-verified.** Which values a real VPN, dock or
//! Hyper-V switch actually reports is unobserved until the hardware run.

use autonet_core::model::InterfaceKind;

use crate::linktype::UNCLASSIFIED;

/// `IFTYPE` values from `ipifcons.h`, mostly IANA registry assignments.
///
/// Restated rather than imported so this module builds on Linux; the `const`
/// block in [`crate::windows::iftable`] pins each one to windows-sys.
pub(crate) mod if_type {
    /// Assigned when nothing more specific fits.
    pub(crate) const OTHER: u32 = 1;
    /// Ethernet — and every virtual adapter that emulates it.
    pub(crate) const ETHERNET_CSMACD: u32 = 6;
    /// Point-to-point protocol.
    pub(crate) const PPP: u32 = 23;
    /// The loopback device.
    pub(crate) const SOFTWARE_LOOPBACK: u32 = 24;
    /// 802.11. Unlike Darwin's `ifi_type`, really is reported for Wi-Fi.
    pub(crate) const IEEE80211: u32 = 71;
    /// An encapsulation tunnel.
    pub(crate) const TUNNEL: u32 = 131;
    /// FireWire networking.
    pub(crate) const IEEE1394: u32 = 144;
    /// An 802.3ad link aggregate — a team, in Windows' vocabulary.
    pub(crate) const IEEE8023AD_LAG: u32 = 161;
    /// 802.16 wireless MAN, better known as WiMAX.
    pub(crate) const IEEE80216_WMAN: u32 = 237;
    /// Mobile broadband, packet-switched.
    pub(crate) const WWANPP: u32 = 243;
    /// Mobile broadband, the LTE-era variant.
    pub(crate) const WWANPP2: u32 = 244;
}

/// `NET_IF_ACCESS_TYPE`, how the link delivers a frame.
pub(crate) mod access {
    /// The interface loops back to itself.
    pub(crate) const LOOPBACK: i32 = 1;
    /// An ordinary shared medium: Ethernet, Wi-Fi.
    pub(crate) const BROADCAST: i32 = 2;
    /// Exactly one peer at the far end. The `IFF_POINTOPOINT` analogue.
    pub(crate) const POINT_TO_POINT: i32 = 3;
    /// One sender, many receivers, no broadcast.
    pub(crate) const POINT_TO_MULTI_POINT: i32 = 4;
}

/// `NDIS_PHYSICAL_MEDIUM`, what the miniport says it is made of.
pub(crate) mod medium {
    /// The driver declined to say.
    pub(crate) const UNSPECIFIED: i32 = 0;
    /// A wireless LAN presenting an 802.3 interface to the stack.
    pub(crate) const WIRELESS_LAN: i32 = 1;
    /// A native 802.11 miniport.
    pub(crate) const NATIVE_802_11: i32 = 9;
    /// Ordinary wired Ethernet.
    pub(crate) const ETHERNET_802_3: i32 = 14;
}

/// `NDIS_MEDIUM`, the frame format the miniport presents to the stack.
pub(crate) mod media {
    /// 802.3 framing — what almost everything, real or virtual, reports.
    pub(crate) const ETHERNET_802_3: i32 = 0;
    /// A tunnel that does not present a link layer.
    pub(crate) const TUNNEL: i32 = 15;
    /// Native 802.11 framing.
    pub(crate) const NATIVE_802_11: i32 = 16;
    /// The loopback device.
    pub(crate) const LOOPBACK: i32 = 17;
}

/// `TUNNEL_TYPE`, the encapsulation a tunnel adapter uses.
///
/// Only enumerated values count as evidence. "Anything other than `NONE` is a
/// tunnel" is rejected on purpose: it turns an uninitialised or future value
/// into a −300 penalty on a possibly-real NIC, and a false *tunnel* is the
/// damaging direction — it demotes the address AutoNet exists to find.
pub(crate) mod tunnel {
    /// Not a tunnel.
    pub(crate) const NONE: i32 = 0;
    /// A tunnel of a kind Windows has no specific name for.
    pub(crate) const OTHER: i32 = 1;
    /// A DirectAccess-style direct tunnel.
    pub(crate) const DIRECT: i32 = 2;
    /// 6to4.
    pub(crate) const SIX_TO_FOUR: i32 = 11;
    /// ISATAP.
    pub(crate) const ISATAP: i32 = 13;
    /// Teredo.
    pub(crate) const TEREDO: i32 = 14;
    /// IP-HTTPS.
    pub(crate) const IPHTTPS: i32 = 15;
}

/// What `kind` says for an Ethernet-typed adapter that is not real hardware.
///
/// Deliberately [`InterfaceKind::Other`], which scores zero. Not `Virtual`
/// (−800): on a Hyper-V host the machine's genuine connectivity runs through
/// exactly such a vEthernet adapter. Not `Ethernet` (+250) either, since that is
/// the bug being fixed. Zero leaves the default route or an `exclude_interfaces`
/// rule to decide.
pub(crate) const VIRTUAL_ETHERNET: &str = "virtual-ethernet";

/// Everything the second source adds, for one interface.
///
/// Optional inside [`Evidence`] because the join can miss: an adapter appearing
/// between the two calls is in one list and not the other. The table then falls
/// back to `IfType` alone rather than inventing values.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ndis {
    /// `MIB_IF_ROW2::AccessType`.
    pub access_type: i32,
    /// `MIB_IF_ROW2::PhysicalMediumType`.
    pub physical_medium: i32,
    /// `MIB_IF_ROW2::MediaType`.
    pub media_type: i32,
    /// The `HardwareInterface` bit of `InterfaceAndOperStatusFlags`.
    ///
    /// `None` when the backend's consistency check found it untrustworthy — see
    /// [`crate::windows::iftable`]. Distinguishing "the OS says software" from
    /// "the bit could not be read" is what stops a wrong ABI assumption
    /// reclassifying every adapter at once.
    pub hardware_interface: Option<bool>,
}

/// Everything known about an interface that bears on what kind it is.
///
/// As with [`crate::linktype::Evidence`], there is no `name` or `description`
/// field, and that is the point.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Evidence {
    /// `IP_ADAPTER_ADDRESSES_LH::IfType`, an `IFTYPE` constant.
    pub if_type: u32,
    /// `IP_ADAPTER_ADDRESSES_LH::TunnelType`. Free with Task 2's call.
    pub tunnel_type: i32,
    /// What `GetIfTable2` added, when the LUID join found a row.
    pub ndis: Option<Ndis>,
}

/// Decide what kind of device an interface is.
///
/// Sources are consulted most-trustworthy first:
///
/// 1. **Loopback**, from any of three fields; nothing may override it.
/// 2. **Wi-Fi**, corroborated by the NDIS medium for a radio presenting 802.3.
/// 3. **Declared tunnels**.
/// 4. **Mobile broadband and WiMAX**, before the point-to-point rule so a
///    cellular modem is not filed as a VPN.
/// 5. **The Ethernet family**, where `AccessType` and `HardwareInterface`
///    decide.
/// 6. **Point-to-point**, last: the catch-all for a tunnel that declared
///    nothing, mirroring `linktype`'s `IFF_POINTOPOINT` fallback.
pub(crate) fn classify(evidence: Evidence) -> InterfaceKind {
    let ndis = evidence.ndis;

    if evidence.if_type == if_type::SOFTWARE_LOOPBACK
        || matches!(ndis, Some(n) if n.media_type == media::LOOPBACK
            || n.access_type == access::LOOPBACK)
    {
        return InterfaceKind::Loopback;
    }

    if evidence.if_type == if_type::IEEE80211 || matches!(ndis, Some(n) if is_wireless(n)) {
        return InterfaceKind::Wireless;
    }

    if evidence.if_type == if_type::TUNNEL
        || evidence.if_type == if_type::PPP
        || is_declared_tunnel(evidence.tunnel_type)
        || matches!(ndis, Some(n) if n.media_type == media::TUNNEL)
    {
        return InterfaceKind::Vpn;
    }

    // Named, not guessed. Both score zero, so the effect is only on what the
    // user reads in `autonet interfaces`.
    match evidence.if_type {
        if_type::WWANPP | if_type::WWANPP2 => return InterfaceKind::Other("wwan".to_owned()),
        if_type::IEEE80216_WMAN => return InterfaceKind::Other("wimax".to_owned()),
        _ => {}
    }

    if is_ethernet_family(evidence.if_type) {
        return match ndis {
            // A point-to-point link calling itself Ethernet is a tunnel in an
            // Ethernet costume: WireGuard, OpenVPN in TUN mode, any L3 VPN on a
            // virtual miniport. The direct analogue of the macOS
            // `IFF_POINTOPOINT` fix.
            Some(n) if n.access_type == access::POINT_TO_POINT => InterfaceKind::Vpn,
            // Ethernet by type, no hardware behind it: a Hyper-V or WSL switch,
            // a TAP-mode VPN. See `VIRTUAL_ETHERNET`.
            Some(n) if n.hardware_interface == Some(false) => {
                InterfaceKind::Other(VIRTUAL_ETHERNET.to_owned())
            }
            // Real hardware, or the join missed and `IfType` is all there is.
            _ => InterfaceKind::Ethernet,
        };
    }

    if matches!(ndis, Some(n) if n.access_type == access::POINT_TO_POINT) {
        return InterfaceKind::Vpn;
    }

    InterfaceKind::Other(UNCLASSIFIED.to_owned())
}

/// Whether NDIS describes a radio.
///
/// Two fields because they answer at different layers: `PhysicalMediumType`
/// describes the hardware, `MediaType` the framing the driver presents. A Wi-Fi
/// card reporting 802.3 framing — the common case — is caught by the first.
fn is_wireless(ndis: Ndis) -> bool {
    ndis.physical_medium == medium::NATIVE_802_11
        || ndis.physical_medium == medium::WIRELESS_LAN
        || ndis.media_type == media::NATIVE_802_11
}

/// Whether `TunnelType` names an encapsulation Microsoft actually enumerates.
///
/// An exhaustive allow-list, not `!= NONE`; see the [`tunnel`] module for why
/// the difference matters.
fn is_declared_tunnel(tunnel_type: i32) -> bool {
    matches!(
        tunnel_type,
        tunnel::OTHER
            | tunnel::DIRECT
            | tunnel::SIX_TO_FOUR
            | tunnel::ISATAP
            | tunnel::TEREDO
            | tunnel::IPHTTPS
    )
}

/// Whether this `IfType` describes a device that carries Ethernet frames.
///
/// Aggregates resolve here rather than to a variant of their own, matching
/// Linux's `bond`/`team` and `linktype`'s `IFT_IEEE8023ADLAG`: an aggregated
/// link is still the real network path. FireWire joins them likewise.
fn is_ethernet_family(if_type: u32) -> bool {
    matches!(
        if_type,
        if_type::ETHERNET_CSMACD | if_type::IEEE8023AD_LAG | if_type::IEEE1394
    )
}

/// Whether the link is a shared broadcast medium, for `InterfaceFlags`.
pub(crate) fn is_broadcast(access_type: i32) -> bool {
    access_type == access::BROADCAST
}

/// Whether the link has exactly one peer, for `InterfaceFlags`.
pub(crate) fn is_point_to_point(access_type: i32) -> bool {
    access_type == access::POINT_TO_POINT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An adapter about which nothing has been established.
    fn evidence() -> Evidence {
        Evidence {
            if_type: if_type::OTHER,
            tunnel_type: tunnel::NONE,
            ndis: None,
        }
    }

    /// A physical NIC's NDIS row: broadcast medium, real hardware behind it.
    fn hardware() -> Ndis {
        Ndis {
            access_type: access::BROADCAST,
            physical_medium: medium::ETHERNET_802_3,
            media_type: media::ETHERNET_802_3,
            hardware_interface: Some(true),
        }
    }

    /// A virtual adapter's NDIS row: Ethernet framing, nothing underneath.
    fn software() -> Ndis {
        Ndis {
            hardware_interface: Some(false),
            physical_medium: medium::UNSPECIFIED,
            ..hardware()
        }
    }

    #[test]
    fn wifi_is_wireless_from_if_type_alone() {
        // The half macOS could not do, from `IfType` alone. If this
        // regresses, the case for leaving WlanAPI disabled goes with it.
        let kind = classify(Evidence {
            if_type: if_type::IEEE80211,
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Wireless);
    }

    #[test]
    fn a_radio_presenting_ethernet_framing_is_still_wireless() {
        // A miniport reporting Ethernet over an 802.11 card. Without
        // PhysicalMediumType it scores +250 instead of +200.
        for physical_medium in [medium::NATIVE_802_11, medium::WIRELESS_LAN] {
            let kind = classify(Evidence {
                if_type: if_type::ETHERNET_CSMACD,
                ndis: Some(Ndis {
                    physical_medium,
                    ..hardware()
                }),
                ..evidence()
            });
            assert_eq!(kind, InterfaceKind::Wireless, "medium {physical_medium}");
        }
    }

    #[test]
    fn a_tap_mode_vpn_does_not_collect_the_ethernet_bonus() {
        // OpenVPN in TAP mode: Ethernet-typed with broadcast access,
        // indistinguishable from a real NIC on `IfType` alone.
        let kind = classify(Evidence {
            if_type: if_type::ETHERNET_CSMACD,
            ndis: Some(software()),
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Other(VIRTUAL_ETHERNET.to_owned()));
    }

    #[test]
    fn a_tun_mode_vpn_is_penalised_as_a_tunnel() {
        // WireGuard and OpenVPN in TUN mode. `AccessType` outranks the
        // hardware bit so this lands on Vpn rather than Other.
        let kind = classify(Evidence {
            if_type: if_type::ETHERNET_CSMACD,
            ndis: Some(Ndis {
                access_type: access::POINT_TO_POINT,
                ..software()
            }),
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Vpn);
    }

    #[test]
    fn a_real_nic_keeps_its_ethernet_bonus() {
        // No VPN rule above may cost a plain wired port its +250.
        for if_type in [
            if_type::ETHERNET_CSMACD,
            if_type::IEEE8023AD_LAG,
            if_type::IEEE1394,
        ] {
            let kind = classify(Evidence {
                if_type,
                ndis: Some(hardware()),
                ..evidence()
            });
            assert_eq!(kind, InterfaceKind::Ethernet, "if_type {if_type}");
        }
    }

    #[test]
    fn loopback_is_decided_before_anything_else_is_consulted() {
        // Any one of the three settles it; nothing may override it.
        for evidence in [
            Evidence {
                if_type: if_type::SOFTWARE_LOOPBACK,
                ndis: Some(hardware()),
                ..evidence()
            },
            Evidence {
                if_type: if_type::ETHERNET_CSMACD,
                ndis: Some(Ndis {
                    media_type: media::LOOPBACK,
                    ..hardware()
                }),
                ..evidence()
            },
            Evidence {
                if_type: if_type::ETHERNET_CSMACD,
                ndis: Some(Ndis {
                    access_type: access::LOOPBACK,
                    ..hardware()
                }),
                ..evidence()
            },
        ] {
            assert_eq!(classify(evidence), InterfaceKind::Loopback, "{evidence:?}");
        }
    }

    #[test]
    fn declared_tunnels_are_vpns() {
        for tunnel_type in [
            tunnel::OTHER,
            tunnel::DIRECT,
            tunnel::SIX_TO_FOUR,
            tunnel::ISATAP,
            tunnel::TEREDO,
            tunnel::IPHTTPS,
        ] {
            let kind = classify(Evidence {
                tunnel_type,
                ..evidence()
            });
            assert_eq!(kind, InterfaceKind::Vpn, "tunnel type {tunnel_type}");
        }

        for if_type in [if_type::TUNNEL, if_type::PPP] {
            let kind = classify(Evidence {
                if_type,
                ..evidence()
            });
            assert_eq!(kind, InterfaceKind::Vpn, "if_type {if_type}");
        }
    }

    #[test]
    fn an_unenumerated_tunnel_type_is_not_treated_as_a_tunnel() {
        // Why `is_declared_tunnel` is an allow-list: an uninitialised or
        // future value must not cost a real NIC 550 points against Ethernet.
        for tunnel_type in [3, 7, 99, -1, i32::MAX] {
            let kind = classify(Evidence {
                if_type: if_type::ETHERNET_CSMACD,
                tunnel_type,
                ndis: Some(hardware()),
            });
            assert_eq!(kind, InterfaceKind::Ethernet, "tunnel type {tunnel_type}");
        }
    }

    #[test]
    fn a_cellular_modem_is_named_rather_than_filed_as_a_vpn() {
        // Mobile broadband is point-to-point, so the last-resort rule would
        // dock a real uplink 300 points. The WWAN check must come first.
        for if_type in [if_type::WWANPP, if_type::WWANPP2] {
            let kind = classify(Evidence {
                if_type,
                ndis: Some(Ndis {
                    access_type: access::POINT_TO_POINT,
                    ..hardware()
                }),
                ..evidence()
            });
            assert_eq!(kind, InterfaceKind::Other("wwan".to_owned()), "{if_type}");
        }

        let kind = classify(Evidence {
            if_type: if_type::IEEE80216_WMAN,
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Other("wimax".to_owned()));
    }

    #[test]
    fn an_undeclared_point_to_point_link_is_a_tunnel() {
        // A device no source named, with one peer at the far end — how
        // Wintun presents when it does not report IF_TYPE_TUNNEL.
        let kind = classify(Evidence {
            ndis: Some(Ndis {
                access_type: access::POINT_TO_POINT,
                ..software()
            }),
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Vpn);
    }

    #[test]
    fn a_device_no_source_recognises_is_reported_as_unknown() {
        // Not Ethernet: `Other` scores zero, the honest position for a
        // device AutoNet does not recognise.
        assert_eq!(
            classify(evidence()),
            InterfaceKind::Other(UNCLASSIFIED.to_owned())
        );

        for if_type in [0, 53, 200, u32::MAX] {
            let kind = classify(Evidence {
                if_type,
                ndis: Some(hardware()),
                ..evidence()
            });
            assert_eq!(
                kind,
                InterfaceKind::Other(UNCLASSIFIED.to_owned()),
                "if_type {if_type}"
            );
        }
    }

    #[test]
    fn a_missed_join_degrades_to_if_type_rather_than_guessing() {
        // An adapter added between the two calls is in one list only. Just
        // the virtual-Ethernet distinction is lost.
        assert_eq!(
            classify(Evidence {
                if_type: if_type::ETHERNET_CSMACD,
                ..evidence()
            }),
            InterfaceKind::Ethernet
        );
        assert_eq!(
            classify(Evidence {
                if_type: if_type::IEEE80211,
                ..evidence()
            }),
            InterfaceKind::Wireless
        );
        assert_eq!(
            classify(Evidence {
                if_type: if_type::SOFTWARE_LOOPBACK,
                ..evidence()
            }),
            InterfaceKind::Loopback
        );
    }

    #[test]
    fn an_untrustworthy_hardware_bit_does_not_reclassify_the_machine() {
        // `None` means the consistency check failed. Falling back to
        // `virtual-ethernet` would strip +250 from every real NIC at once.
        let kind = classify(Evidence {
            if_type: if_type::ETHERNET_CSMACD,
            ndis: Some(Ndis {
                hardware_interface: None,
                ..hardware()
            }),
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Ethernet);
    }

    #[test]
    fn access_types_map_onto_the_two_interface_flags() {
        assert!(is_broadcast(access::BROADCAST));
        assert!(!is_broadcast(access::POINT_TO_POINT));
        assert!(!is_broadcast(access::LOOPBACK));

        assert!(is_point_to_point(access::POINT_TO_POINT));
        assert!(!is_point_to_point(access::BROADCAST));
        // Point-to-multipoint is neither, and must not be rounded to either.
        assert!(!is_broadcast(access::POINT_TO_MULTI_POINT));
        assert!(!is_point_to_point(access::POINT_TO_MULTI_POINT));
    }
}
