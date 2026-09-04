//! The macOS backend.
//!
//! macOS has no single source for the information AutoNet needs, so this
//! backend joins three of them on the BSD interface name:
//!
//! | Source | Provides |
//! |---|---|
//! | `getifaddrs(3)` | interfaces, addresses, link flags, MAC, MTU |
//! | SystemConfiguration | what *sort* of device each interface is, and which link is preferred |
//! | `PF_ROUTE` / `NET_RT_DUMP` | routes, gateways, the default route |
//!
//! Interface *names* are a poor classifier here: `en0` is Wi-Fi on a laptop and
//! Ethernet on a Mac Pro, and Thunderbolt docks renumber them. The type comes
//! from `SCNetworkInterfaceGetInterfaceType`, falling back to `ifi_type` and
//! then the kernel's flags; a name only ever joins the three sources to each
//! other. See [`crate::linktype`].
//!
//! `Route::metric` means something different here than on Linux: macOS has no
//! per-route metric, so it is synthesized from the network service order — see
//! [`crate::servicerank`].
//!
//! **Unverified on hardware.** Struct sizes and field offsets are checked
//! against `libc` at compile time, but every claim about sysctl behaviour, ioctl
//! semantics and SystemConfiguration coverage comes from documentation rather
//! than a running system.

mod ifaddrs;
pub(crate) mod portowner;
mod route;
mod scnetwork;

use autonet_core::model::NetworkState;

use crate::{servicerank, NetworkProvider, PlatformError};

/// Reads network state from macOS.
///
/// Holds no state: every call re-queries the OS. Unlike the Linux backend it
/// needs no runtime — `getifaddrs` and `sysctl` are ordinary blocking calls.
pub(crate) struct MacosProvider;

impl MacosProvider {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl NetworkProvider for MacosProvider {
    /// SystemConfiguration is queried once up front and handed to the
    /// `getifaddrs` walk, so both sources are joined while fresh. The service
    /// order is resolved in between because it needs both sides —
    /// SystemConfiguration knows interfaces by BSD name, routing messages only
    /// by index — which lets routes be built with their metric already in place.
    fn snapshot(&self) -> Result<NetworkState, PlatformError> {
        let interfaces = ifaddrs::interfaces(&scnetwork::interface_types())?;
        let metrics = servicerank::metrics_by_index(&interfaces, &scnetwork::service_order());
        let routes = route::dump_routes(&metrics)?;

        Ok(NetworkState::new(interfaces, routes).captured_now())
    }

    fn platform_name(&self) -> &'static str {
        "macos-sysconfig"
    }
}
