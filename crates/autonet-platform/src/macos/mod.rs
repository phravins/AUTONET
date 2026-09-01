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
//! # Why not just names
//!
//! Interface *names* are a poor classifier on macOS. `en0` is Wi-Fi on a
//! laptop and Ethernet on a Mac Pro, `en1` may be either, and Thunderbolt
//! docks renumber them. So the type comes from
//! `SCNetworkInterfaceGetInterfaceType`, which is the OS's own answer, falling
//! back to the driver's own `ifi_type` and then to the kernel's flags. A name
//! is used only as the key that joins the sources to each other, never as
//! evidence about what a device is. See [`crate::linktype`].
//!
//! # Status
//!
//! **Unverified on hardware.** Written without access to a Mac. Struct sizes
//! and field offsets are checked against `libc` at compile time, but every
//! claim about sysctl behaviour, ioctl semantics and SystemConfiguration
//! coverage is derived from documentation, not from a running system, and must
//! be confirmed on hardware before this backend is trusted.
//!
//! All three sources are implemented. Note that `Route::metric` means something
//! different here than on Linux: macOS has no per-route metric, so it is
//! synthesized from the network service order rather than read from the kernel
//! — see [`crate::servicerank`] for the scale and its limits.

mod ifaddrs;
mod route;
mod scnetwork;

use autonet_core::model::NetworkState;

use crate::{servicerank, NetworkProvider, PlatformError};

/// Reads network state from macOS.
///
/// Holds no state: every call re-queries the OS, because the whole point of
/// AutoNet is that this changes underneath you. Unlike the Linux backend it
/// needs no runtime — `getifaddrs` and `sysctl` are ordinary blocking calls.
pub(crate) struct MacosProvider;

impl MacosProvider {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl NetworkProvider for MacosProvider {
    /// Capture interfaces, their addresses, what sort of device each is, and
    /// the routing table.
    ///
    /// SystemConfiguration is queried first, once, and the resulting map is
    /// handed to the `getifaddrs` walk — so the two sources are joined while
    /// both are fresh, rather than the walk re-querying per interface. Routes
    /// come last, in the same order as the Linux backend's `collect`, and join
    /// back to the interfaces on the kernel's own interface index.
    ///
    /// The service order is resolved in between, because it is the one join
    /// that needs both sides: SystemConfiguration knows interfaces by BSD name
    /// and routing messages know them only by index, so the interface list is
    /// what bridges them. Routes are therefore built with their metric already
    /// in place rather than being walked a second time to patch it in.
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
