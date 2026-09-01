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
//! `SCNetworkInterfaceGetInterfaceType`, which is the OS's own answer.
//!
//! Names are used for exactly one thing — interfaces SystemConfiguration does
//! not enumerate at all, chiefly the `utun` devices that WireGuard, Tailscale
//! and every `NEPacketTunnelProvider` VPN create. See `scnetwork.rs`.
//!
//! # Status
//!
//! **Unverified.** Written without access to a Mac. Every claim about struct
//! layout, sysctl behaviour and SystemConfiguration coverage here is derived
//! from documentation, not from a running system, and must be confirmed on
//! hardware before this backend is trusted.

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
    fn snapshot(&self) -> Result<NetworkState, PlatformError> {
        // TASK 1 SCAFFOLD. Returns an empty machine so the dispatch and the
        // macOS CI job can be verified before any FFI is written. Task 2
        // replaces this with the real `getifaddrs` walk.
        Ok(NetworkState::new(Vec::new(), Vec::new()).captured_now())
    }

    fn platform_name(&self) -> &'static str {
        "macos-sysconfig"
    }
}
