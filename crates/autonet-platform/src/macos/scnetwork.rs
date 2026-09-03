//! Asking SystemConfiguration what sort of device each interface is.
//!
//! This is the only source on macOS that distinguishes Wi-Fi from Ethernet —
//! the BSD link layer reports `IFT_ETHER` for both, because the AirPort driver
//! presents an Ethernet data link. See [`crate::linktype`] for why that makes
//! this source primary rather than a fallback.
//!
//! `SCNetworkInterfaceCopyAll` enumerates everything in one call, keyed by BSD
//! name, which is what joins it back to the `AF_LINK` walk in
//! [`super::ifaddrs`]. The two enumerations are not symmetric and are not
//! assumed to be: hardware that is configurable but absent appears only here
//! (harmless, nothing looks it up), while `utun` devices from WireGuard,
//! Tailscale and every `NEPacketTunnelProvider` VPN are expected to appear only
//! in `getifaddrs`, since SystemConfiguration enumerates configurable
//! *hardware*. Those fall through to `IFF_POINTOPOINT`, which is why the
//! classifier has a step 4. The map is evidence that may be missing, not a
//! census.
//!
//! **Unverified.** That SystemConfiguration omits `utun` is inferred from what
//! it documents itself as enumerating. If it does list them the classifier
//! answers from this map and is still correct — the uncertainty is
//! one-directional.

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
/// Interfaces without a BSD name are skipped — SystemConfiguration models
/// entries with no kernel device behind them, which cannot be joined to the
/// `AF_LINK` walk.
///
/// Infallible by design: an empty map degrades classification to the link layer
/// rather than failing the whole snapshot.
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
/// Deliberately nothing but a match: `SCNetworkInterfaceType` exists only on
/// Darwin, so mirroring it once here lets every actual decision live in
/// [`crate::linktype`] and be tested on Linux.
///
/// `None` for a type this build does not recognise, which is treated like an
/// interface SystemConfiguration never listed — it falls through to the link
/// layer rather than being guessed at.
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
/// The list System Settings ▸ Network shows, and macOS's own answer to "which
/// link is preferred". Rank 0 is most preferred; [`crate::servicerank`] turns a
/// rank into the route metric the selector consumes.
///
/// The order lists *services*, not interfaces, so it takes two hops: ordered
/// service IDs, then services, each naming the interface behind it. Ranks are
/// assigned densely over the interfaces actually resolved, so a service whose ID
/// does not resolve cannot push every real interface a step down. Where two
/// services name one interface the first wins, being the higher-priority one.
///
/// Infallible, like [`interface_types`]: an empty map means every interface gets
/// the same metric. Nothing here writes, and reading needs no privileges.
///
/// **Unverified on hardware.** The call chain type-checks and the ID join is
/// tested upstream, but matching what System Settings displays needs a Mac with
/// two links up.
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

    // `SCNetworkService::enabled()` is not consulted: the vendored crate
    // implements it as `SCNetworkServiceGetEnabled(..) == 0`, which reports a
    // disabled service as enabled. `IFF_UP` in `super::ifaddrs` already answers
    // this, and from the kernel rather than the configuration.
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
/// Not `SCNetworkSet::new`, which is unsound: `network_configuration.rs:281`
/// wraps `SCNetworkSetCopyCurrent`'s result without a null check, so a machine
/// with no current set gets a `CFRelease` of null when the wrapper drops. Going
/// through the `sys` bindings lets the pointer be checked first, which is the
/// only reason for raw FFI here.
fn current_set(preferences: &SCPreferences) -> Option<SCNetworkSet> {
    // SAFETY: `preferences` is a live `SCPreferences` whose concrete ref is the
    // `SCPreferencesRef` expected here. The result follows the Core Foundation
    // Copy rule, so it is wrapped under the create rule after a null check.
    let set = unsafe { SCNetworkSetCopyCurrent(preferences.as_concrete_TypeRef()) };
    if set.is_null() {
        return None;
    }
    Some(unsafe { SCNetworkSet::wrap_under_create_rule(set) })
}
