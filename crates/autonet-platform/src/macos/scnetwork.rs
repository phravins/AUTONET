//! Asking SystemConfiguration what sort of device each interface is.
//!
//! This is the only source on macOS that distinguishes Wi-Fi from Ethernet —
//! the BSD link layer reports `IFT_ETHER` for both, because the AirPort driver
//! presents an Ethernet data link. See [`crate::linktype`] for why that makes
//! this source primary rather than a fallback.
//!
//! # One call, not one per interface
//!
//! `SCNetworkInterfaceCopyAll` enumerates every interface in a single call, so
//! the cost is paid once per snapshot and every lookup afterwards is a hash
//! probe. There is no per-interface framework call anywhere in this backend.
//!
//! # The join, and why it is safe to be lopsided
//!
//! The result is keyed by BSD name (`en0`, `bridge0`), which is what joins it
//! back to the `AF_LINK` walk in [`super::ifaddrs`]. The two enumerations are
//! *not* symmetric and are not assumed to be:
//!
//! - **In SystemConfiguration but not `getifaddrs`** — configurable hardware
//!   that is currently absent, such as a Thunderbolt adapter that is unplugged.
//!   Harmless: nothing ever looks the entry up.
//! - **In `getifaddrs` but not SystemConfiguration** — the interesting
//!   direction. `utun` devices created by WireGuard, Tailscale and every
//!   `NEPacketTunnelProvider` VPN are expected to land here, because
//!   SystemConfiguration enumerates configurable *hardware* rather than every
//!   kernel device. Those fall through to the link layer and then to
//!   `IFF_POINTOPOINT`, which is why the classifier has a step 4 at all.
//!
//! So the map is treated as *evidence that may be missing*, never as a census.
//!
//! # Status
//!
//! **Unverified.** That SystemConfiguration omits `utun` is inferred from what
//! it is documented to enumerate, not confirmed against a Mac with a VPN up. If
//! it turns out to list them, the classifier answers from this map instead and
//! is still correct — the uncertainty is one-directional.

use std::collections::HashMap;

use system_configuration::network_configuration::{
    self, SCNetworkInterface, SCNetworkInterfaceType as ScNative,
};

use crate::linktype::ScType;

/// Ask SystemConfiguration about every interface it knows, keyed by BSD name.
///
/// Interfaces without a BSD name are skipped: SystemConfiguration models some
/// entries that have no kernel device behind them (a VPN service that is
/// configured but not connected, for instance), and those cannot be joined to
/// anything the `AF_LINK` walk produced.
///
/// Infallible by design. `SCNetworkInterfaceCopyAll` reads the system's own
/// configuration and needs no privileges, but if it ever returns nothing, an
/// empty map is the right answer: classification degrades to the link layer
/// rather than the whole snapshot failing. AutoNet reporting slightly coarser
/// interface kinds beats AutoNet reporting no address at all.
pub(crate) fn interface_types() -> HashMap<String, ScType> {
    network_configuration::get_interfaces()
        .iter()
        .filter_map(|interface| {
            let name = interface.bsd_name()?.to_string();
            Some((name, translate(&interface)?))
        })
        .collect()
}

/// Translate the crate's macOS-only enum into AutoNet's own.
///
/// The one genuinely macOS-gated part of classification, and deliberately
/// nothing but a match: `SCNetworkInterfaceType` is neither `Copy` nor
/// comparable and exists only on Darwin, so mirroring it once here is what lets
/// every actual decision live in [`crate::linktype`] and be tested on Linux.
///
/// `None` when SystemConfiguration reports a type this build does not
/// recognise — a newer macOS naming something the vendored bindings predate.
/// Treated exactly like an interface SystemConfiguration never listed, so an
/// unfamiliar type falls through to the link layer instead of being guessed at.
fn translate(interface: &SCNetworkInterface) -> Option<ScType> {
    Some(match interface.interface_type()? {
        ScNative::Ethernet => ScType::Ethernet,
        ScNative::IEEE80211 => ScType::IEEE80211,
        ScNative::Bridge => ScType::Bridge,
        ScNative::Bond => ScType::Bond,
        ScNative::VLAN => ScType::Vlan,
        ScNative::FireWire => ScType::FireWire,
        ScNative::SixToFour => ScType::SixToFour,
        ScNative::IPSec => ScType::IpSec,
        ScNative::L2TP => ScType::L2tp,
        ScNative::PPP => ScType::Ppp,
        ScNative::PPTP => ScType::Pptp,
        ScNative::Modem => ScType::Modem,
        ScNative::Serial => ScType::Serial,
        ScNative::WWAN => ScType::Wwan,
        ScNative::Bluetooth => ScType::Bluetooth,
        ScNative::IrDA => ScType::IrDa,
        ScNative::IPv4 => ScType::Ipv4,
    })
}
