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

use std::collections::hash_map::{Entry, HashMap};

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use system_configuration::network_configuration::{
    self, SCNetworkInterface, SCNetworkInterfaceType as ScNative, SCNetworkService, SCNetworkSet,
};
use system_configuration::preferences::SCPreferences;
use system_configuration::sys::network_configuration::SCNetworkSetCopyCurrent;

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

/// The network service order, as ranks keyed by BSD interface name.
///
/// This is the list System Settings ▸ Network shows and lets the user drag to
/// reorder, and it is macOS's own answer to "which link is preferred". Rank 0
/// is the most preferred. [`crate::servicerank`] turns a rank into the route
/// metric the selector consumes, and documents why it is only a tie-breaker.
///
/// # The join
///
/// The service order is a list of *service* identifiers, not interfaces, so it
/// takes two hops: `SCNetworkSetGetServiceOrder` gives ordered service IDs,
/// `SCNetworkServiceCopyAll` gives services, and each service names the
/// interface behind it. The crate's own `test_service_order` already asserts
/// that the IDs in the order really do match `SCNetworkService::id()`, so this
/// join is confirmed upstream rather than assumed here.
///
/// Ranks are assigned densely over the interfaces actually resolved: a service
/// whose ID does not resolve to a BSD name — a VPN service that is configured
/// but not connected, for instance — is skipped without consuming a rank, so a
/// stale entry cannot push every real interface one step down.
///
/// Two services can name the same interface (a second configuration on `en0`).
/// The first wins, since that is the higher-priority one and the second says
/// nothing new about the link.
///
/// # Infallible, like [`interface_types`]
///
/// An empty map is a legitimate answer, not a failure: every interface then
/// gets the same metric and ranking degrades to what it was before this
/// existed. AutoNet reporting a slightly worse-ordered answer beats AutoNet
/// reporting no address at all.
///
/// Nothing here writes. `SCNetworkSetSetServiceOrder` is never called, and
/// reading the configuration needs no elevated privileges.
///
/// # Status
///
/// **Unverified on hardware.** The call chain type-checks and the ID join is
/// tested upstream, but that the resulting order matches what System Settings
/// displays is confirmed only by running on a Mac with two links up.
pub(crate) fn service_order() -> HashMap<String, u32> {
    let preferences = SCPreferences::default(&CFString::new("autonet"));
    let Some(set) = current_set(&preferences) else {
        return HashMap::new();
    };

    // Service ID -> BSD name, built once so the ordered walk below is a hash
    // probe per entry rather than a rescan of every service.
    let interface_of: HashMap<String, String> = SCNetworkService::get_services(&preferences)
        .iter()
        .filter_map(|service| {
            Some((
                service.id()?.to_string(),
                service.network_interface()?.bsd_name()?.to_string(),
            ))
        })
        .collect();

    // `SCNetworkService::enabled()` is deliberately not consulted: the vendored
    // crate implements it as `SCNetworkServiceGetEnabled(..) == 0`, which
    // reports a *disabled* service as enabled. Whether a link is usable already
    // comes from `IFF_UP` in `super::ifaddrs`, which is the kernel's answer
    // rather than the configuration's, so nothing is lost by leaving it alone.
    let order = set.service_order();
    let mut ranked: HashMap<String, u32> = HashMap::new();
    let mut rank = 0u32;

    for id in order.iter() {
        let Some(name) = interface_of.get(&id.to_string()) else {
            continue;
        };
        if let Entry::Vacant(slot) = ranked.entry(name.clone()) {
            slot.insert(rank);
            rank += 1;
        }
    }

    ranked
}

/// The machine's current network set, or `None` if it has none.
///
/// Not `SCNetworkSet::new`, which is the obvious call and is unsound:
/// `network_configuration.rs:281` wraps `SCNetworkSetCopyCurrent`'s result
/// without a null check, so a machine with no current set gets a `CFRelease`
/// of a null pointer when the wrapper drops — a hard crash rather than an
/// error. Calling through the re-exported `sys` bindings lets the pointer be
/// checked before it is wrapped, which is the entire reason for the raw FFI
/// here; everything else goes through the safe API.
fn current_set(preferences: &SCPreferences) -> Option<SCNetworkSet> {
    // SAFETY: `preferences` is a live `SCPreferences`, and its concrete ref is
    // exactly the `SCPreferencesRef` this function expects. The result follows
    // the Core Foundation *Copy* rule — we own a reference — so it is wrapped
    // under the create rule, and only after being checked for null.
    let set = unsafe { SCNetworkSetCopyCurrent(preferences.as_concrete_TypeRef()) };
    if set.is_null() {
        return None;
    }
    Some(unsafe { SCNetworkSet::wrap_under_create_rule(set) })
}
