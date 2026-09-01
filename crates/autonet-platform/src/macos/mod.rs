//! The macOS backend.
//!
//! macOS has no single source for the information AutoNet needs, so this
//! backend joins three of them on the BSD interface name:
//!
//! | Source | Provides |
//! |---|---|
//! | `getifaddrs(3)` | interfaces, addresses, link flags, MAC, MTU |
//! | SystemConfiguration | what *sort* of device each interface is |
//! | `PF_ROUTE` / `NET_RT_DUMP2` | routes, gateways, the default route |
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
//! **Unverified.** Written without access to a Mac. Every claim about struct
//! layout, sysctl behaviour and SystemConfiguration coverage here is derived
//! from documentation, not from a running system, and must be confirmed on
//! hardware before this backend is trusted.
//!
//! Only the first two of the three sources above are implemented. Until routes
//! exist, the selection engine has no default-route evidence on macOS and its
//! choice of address should not be relied on — see [`snapshot`].
//!
//! [`snapshot`]: MacosProvider::snapshot

mod ifaddrs;
mod scnetwork;

use autonet_core::model::NetworkState;

use crate::{NetworkProvider, PlatformError};

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
    /// Capture interfaces, their addresses, and what sort of device each is.
    ///
    /// SystemConfiguration is queried first, once, and the resulting map is
    /// handed to the `getifaddrs` walk — so the two sources are joined while
    /// both are fresh, rather than the walk re-querying per interface.
    ///
    /// **Incomplete.** The returned state still carries no routes, which is
    /// where the strongest evidence for selection lives. The snapshot is honest
    /// about what it knows; it is not yet enough for selection to be trusted.
    fn snapshot(&self) -> Result<NetworkState, PlatformError> {
        ifaddrs::snapshot(&scnetwork::interface_types())
    }

    fn platform_name(&self) -> &'static str {
        "macos-sysconfig"
    }
}
