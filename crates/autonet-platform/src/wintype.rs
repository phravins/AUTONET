//! What *sort* of device an interface is, on Windows.
//!
//! # Why this module is not inside `windows/`
//!
//! The same reason [`crate::linktype`] is not inside `macos/`: everything here
//! is a pure decision over already-gathered evidence, so keeping it outside the
//! `#[cfg(target_os = "windows")]` backend means the table below is compiled and
//! its tests run on the Linux CI job too. A misclassification does not crash —
//! it quietly moves an address up or down by hundreds of points — so it needs to
//! be exercised somewhere a failure can be read and debugged, not only on the one
//! runner nobody in this project can attach to.
//!
//! # Does `IfType` alone settle it? No — and not for the reason macOS failed
//!
//! Task 1 left this open, and the honest answer needs the two halves separated,
//! because Windows and macOS fail in opposite places.
//!
//! **Wi-Fi versus Ethernet: `IfType` genuinely answers this, and macOS's problem
//! does not recur.** `linktype`'s whole design — SystemConfiguration first,
//! `ifi_type` second — exists because Darwin reports `IFT_ETHER` for a Wi-Fi
//! card: the BSD layer cannot see the radio at all. Windows does not have that
//! defect. `IF_TYPE_IEEE80211` (71) is what NDIS reports for an 802.11 miniport,
//! and it is the kernel's own answer, not a vendor string. So the Windows
//! equivalent of SystemConfiguration — **WlanAPI** — is *not* needed here, and it
//! is deliberately not enabled: it would add a feature flag and a second DLL to
//! answer a question `IfType` already answers, and it answers only the Wi-Fi
//! half while saying nothing about the half below that actually is broken.
//!
//! **Ethernet versus a virtual adapter pretending to be Ethernet: `IfType`
//! cannot answer this, and it is the dangerous half.** A TAP-mode VPN adapter
//! presents as `IF_TYPE_ETHERNET_CSMACD` — it *is* an Ethernet device, as far as
//! NDIS is concerned. Hyper-V and WSL switches do the same. On `IfType` alone
//! every one of them collects `KIND_ETHERNET`'s +250, which is precisely the
//! failure that let a VPN address outrank a real LAN address on macOS before the
//! `IFF_POINTOPOINT` fix.
//!
//! # The second source, chosen by comparison rather than by reflex
//!
//! | Candidate | Cost | What it adds | Verdict |
//! |---|---|---|---|
//! | `IfType` alone (free with Task 2) | none | Wi-Fi, loopback, declared tunnels | **necessary, not sufficient** |
//! | **`GetIfTable2`** | **one call for the whole machine** | `AccessType`, `PhysicalMediumType`, `MediaType`, the `HardwareInterface` bit, `AdminStatus` | **chosen** |
//! | `GetIfEntry2` per adapter | one call *per adapter* | the same fields | rejected: same data, N times the syscalls |
//! | WlanAPI (`Win32_NetworkManagement_WiFi`) | new feature, new DLL, handle lifecycle | Wi-Fi confirmation only | rejected: answers the half that already works |
//! | WMI (`Win32_NetworkAdapter`) | COM, a service dependency, slow | descriptions, `NetEnabled` | rejected: string-shaped, and this backend does not read strings as evidence |
//!
//! `GetIfTable2` wins on a fact worth stating plainly, because an earlier note in
//! [`crate::windows::adapters`] guessed the opposite: it is **one call for every
//! interface on the machine**, joined to the adapter list by LUID — not one call
//! per adapter. That is what makes the `broadcast` and `point_to_point` flags
//! Task 2 had to leave `false`, and the `up`/`running` distinction it had to
//! collapse, cheap enough to close here rather than defer.
//!
//! `AccessType` is the load-bearing addition: `NET_IF_ACCESS_POINT_TO_POINT` is
//! the exact analogue of the `IFF_POINTOPOINT` flag that fixed the macOS VPN bug,
//! and it is a kernel-reported fact rather than an inference from a name.
//!
//! # What is deliberately not consulted
//!
//! `Description` (`"TAP-Windows Adapter V9"`), `Alias`, and the adapter GUID.
//! Matching `"TAP-"` or `"WireGuard"` would classify today's VPNs and miss
//! tomorrow's, and it is the Windows spelling of guessing that `en0` means Wi-Fi.
//! Every input below is a numeric field NDIS or the TCP/IP stack filled in.
//!
//! # Status
//!
//! **Table-checked, not hardware-verified.** Every constant is pinned to
//! windows-sys by a `const` assertion in the backend, and every row of the
//! decision table is exercised by the tests below. What no test here can
//! establish is which of these values a *real* VPN, dock or Hyper-V switch
//! actually reports; that is the Task 7 hardware run, and until then the
//! mappings from evidence to kind are reasoned, not observed.

use autonet_core::model::InterfaceKind;

use crate::linktype::UNCLASSIFIED;

/// `IFTYPE` values from `ipifcons.h`.
///
/// Restated rather than imported so this module builds on Linux; the `const`
/// block in [`crate::windows::iftable`] proves each one against windows-sys.
/// Nearly all are IANA `ifType` registry assignments, which is why they can be
/// trusted as stable numbers rather than Microsoft's to renumber.
pub(crate) mod if_type {
    /// Assigned when nothing more specific fits.
    pub(crate) const OTHER: u32 = 1;
    /// Ethernet — *and every virtual adapter that emulates it*, which is the
    /// problem this module exists to solve.
    pub(crate) const ETHERNET_CSMACD: u32 = 6;
    /// Point-to-point protocol.
    pub(crate) const PPP: u32 = 23;
    /// The loopback device.
    pub(crate) const SOFTWARE_LOOPBACK: u32 = 24;
    /// 802.11. Unlike Darwin's `ifi_type`, this really is reported for Wi-Fi.
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
/// Only the values Microsoft enumerates are treated as evidence. The tempting
/// shortcut — "anything other than `NONE` is a tunnel" — is rejected on purpose:
/// it turns an uninitialised or future field value into a −300 penalty on what
/// might be a real wired NIC, and a false *tunnel* is the more damaging direction
/// of error, since it demotes the very address AutoNet is meant to find.
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
/// Not [`InterfaceKind::Virtual`], which would be the obvious-looking choice and
/// is wrong: `Virtual` is synthetic, worth **−800**, and on a Hyper-V host the
/// machine's genuine connectivity runs through exactly such a vEthernet adapter.
/// Demoting it by 800 points would make AutoNet unusable on the machines where
/// it is hardest to work out the right address by hand — the opposite of the
/// point. Not [`InterfaceKind::Ethernet`] either, since collecting +250 is the
/// bug being fixed.
///
/// [`InterfaceKind::Other`] scores **zero**: neither preferred nor penalised.
/// That is the honest position for "Windows says Ethernet, but nothing is
/// plugged into it", and it leaves a default route or an `exclude_interfaces`
/// rule to decide, which is what those exist for.
pub(crate) const VIRTUAL_ETHERNET: &str = "virtual-ethernet";

/// Everything the second source adds, for one interface.
///
/// Separate from [`Evidence`] and optional inside it because the join can
/// genuinely miss: an adapter appearing between the `GetAdaptersAddresses` call
/// and the `GetIfTable2` call is in one list and not the other. When that
/// happens the table falls back to `IfType` alone rather than inventing values,
/// and the tests below pin what that degraded answer looks like.
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
    /// `None` when the backend's own consistency check found the bit could not
    /// be trusted — see [`crate::windows::iftable`]. Distinguishing "the OS says
    /// this is software" from "we could not read the bit" is what keeps a wrong
    /// ABI assumption from silently reclassifying every adapter on the machine.
    pub hardware_interface: Option<bool>,
}

/// Everything known about an interface that bears on what kind it is.
///
/// As with [`crate::linktype::Evidence`], there is no `name` field and no
/// `description` field, and that is the entire point.
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
/// Sources are consulted most-trustworthy first, and within each step the
/// cheaper source is checked before the corroborating one:
///
/// 1. **Loopback** — a fact the stack states about itself, from any of three
///    fields; nothing may override it.
/// 2. **Wi-Fi** — `IF_TYPE_IEEE80211`, corroborated by the NDIS medium for a
///    driver that presents 802.3 framing over a radio.
/// 3. **Declared tunnels** — the adapter says it encapsulates.
/// 4. **Mobile broadband and WiMAX** — real radios AutoNet has no opinion about,
///    named rather than guessed, and checked before the point-to-point rule so a
///    cellular modem is not filed as a VPN.
/// 5. **The Ethernet family** — where `IfType` stops being enough and
///    `AccessType` and `HardwareInterface` decide.
/// 6. **Point-to-point, last** — the catch-all for a tunnel that declared
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

    // Named, not guessed — the same treatment `linktype` gives cellular and
    // Bluetooth. Both score zero, so the effect is purely on what the user reads
    // in `autonet interfaces`.
    match evidence.if_type {
        if_type::WWANPP | if_type::WWANPP2 => return InterfaceKind::Other("wwan".to_owned()),
        if_type::IEEE80216_WMAN => return InterfaceKind::Other("wimax".to_owned()),
        _ => {}
    }

    if is_ethernet_family(evidence.if_type) {
        return match ndis {
            // A point-to-point link that calls itself Ethernet is a tunnel
            // wearing an Ethernet costume: WireGuard, OpenVPN in TUN mode, and
            // every L3 VPN that binds a virtual miniport. This is the rule that
            // stops a VPN address outranking the real LAN address, and it is the
            // direct analogue of the macOS `IFF_POINTOPOINT` fix.
            Some(n) if n.access_type == access::POINT_TO_POINT => InterfaceKind::Vpn,
            // Windows says Ethernet and NDIS says no hardware sits behind it:
            // a Hyper-V or WSL switch, a TAP-mode VPN, a loopback-style test
            // adapter. See `VIRTUAL_ETHERNET` for why this is neither Ethernet
            // nor Virtual.
            Some(n) if n.hardware_interface == Some(false) => {
                InterfaceKind::Other(VIRTUAL_ETHERNET.to_owned())
            }
            // Either NDIS confirmed real hardware, or the join missed and
            // `IfType` is all there is. The second case is a weaker answer than
            // the first and is recorded as such in the module docs, but Ethernet
            // remains the best available reading of `IF_TYPE_ETHERNET_CSMACD`.
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
/// Two fields rather than one because they answer at different layers:
/// `PhysicalMediumType` describes the hardware, `MediaType` the framing the
/// driver presents. A Wi-Fi card that reports 802.3 framing — the common case,
/// and the one that fooled macOS — is caught by the first.
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
/// Aggregates resolve here rather than to a variant of their own, matching both
/// siblings: Linux maps `bond`/`team` to Ethernet and `linktype` maps
/// `IFT_IEEE8023ADLAG` the same way, on the grounds that an aggregated link is
/// still the real network path. FireWire joins them for the same reason
/// `linktype` maps `ScType::FireWire` to Ethernet.
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
        // The half macOS could not do. No second source, no WlanAPI: Windows
        // reports the radio itself. If this ever regresses, the argument for
        // leaving `Win32_NetworkManagement_WiFi` disabled goes with it.
        let kind = classify(Evidence {
            if_type: if_type::IEEE80211,
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Wireless);
    }

    #[test]
    fn a_radio_presenting_ethernet_framing_is_still_wireless() {
        // A miniport reporting IF_TYPE_ETHERNET_CSMACD over an 802.11 card.
        // Without PhysicalMediumType this scores as Ethernet: +250 instead of
        // +200, and a docked laptop would prefer the wrong link.
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
        // The headline case. OpenVPN in TAP mode is IF_TYPE_ETHERNET_CSMACD with
        // broadcast access — indistinguishable from a real NIC on IfType alone,
        // and worth +250 if believed.
        let kind = classify(Evidence {
            if_type: if_type::ETHERNET_CSMACD,
            ndis: Some(software()),
            ..evidence()
        });
        assert_eq!(kind, InterfaceKind::Other(VIRTUAL_ETHERNET.to_owned()));
    }

    #[test]
    fn a_tun_mode_vpn_is_penalised_as_a_tunnel() {
        // WireGuard and OpenVPN in TUN mode: Ethernet-typed, but point-to-point.
        // AccessType is the IFF_POINTOPOINT analogue, and it outranks the
        // hardware bit precisely so this lands on Vpn rather than Other.
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
        // The other direction of the same rule: none of the VPN detection above
        // may cost a plain wired port its +250.
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
        // Three independent statements of the same fact; any one settles it, and
        // no other evidence may override it.
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
        // The reason `is_declared_tunnel` is an allow-list. A driver that leaves
        // TunnelType uninitialised, or a value Microsoft adds later, must not
        // cost a real wired NIC 300 points and 550 relative to Ethernet.
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
        // Mobile broadband is point-to-point, so the last-resort rule would call
        // it a VPN and dock it 300 points. It is a real uplink; checking the
        // WWAN types first is what keeps that from happening.
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
        // The `linktype` fallback, restated: a device no source named, with one
        // peer at the far end. Wintun presents this way if it does not report
        // IF_TYPE_TUNNEL.
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
        // Not Ethernet. `Other` scores zero, which is the honest position when
        // AutoNet genuinely does not know — the same rule `linktype` follows.
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
        // An adapter added between the two calls is in one list and not the
        // other. `IfType` still answers the questions it can answer; only the
        // virtual-Ethernet distinction is lost, and it is lost in the direction
        // Task 2 already shipped rather than in a new one.
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
        // `hardware_interface: None` is the backend saying its consistency check
        // failed. That must fall back to Ethernet — the Task 2 behaviour —
        // rather than to `virtual-ethernet`, which would strip +250 from every
        // real NIC on the machine at once.
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
