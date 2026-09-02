//! The Windows backend.
//!
//! Windows exposes network configuration through the IP Helper API in
//! `iphlpapi.dll`, and the plan is for two calls to cover what Linux needed
//! netlink for and macOS needed three sources for:
//!
//! | Source | Expected to provide |
//! |---|---|
//! | `GetAdaptersAddresses` | interfaces, addresses, `IfType`, `OperStatus`, index, MTU, MAC |
//! | `GetIpForwardTable2` | routes, gateways, the default route, per-route metrics |
//!
//! # Why not names or descriptions
//!
//! The macOS backend refuses to read `en0` as "Wi-Fi", and the same rule holds
//! here against the two things Windows makes tempting: the adapter *description*
//! (`"Intel(R) Wi-Fi 6E AX211"`) and the adapter GUID. Both are vendor strings,
//! not kernel facts; a docking station, a virtual switch or a renamed connection
//! breaks either one. Classification will come from the `IfType` field —
//! `IF_TYPE_IEEE80211`, `IF_TYPE_ETHERNET_CSMACD`, `IF_TYPE_TUNNEL` and friends
//! — which is what Windows itself reports, playing the role `ifi_type` and
//! `SCNetworkInterfaceGetInterfaceType` play on macOS.
//!
//! # Two open questions, named rather than assumed
//!
//! **Is one call enough?** macOS needed SystemConfiguration alongside `AF_LINK`
//! because `ifi_type` reports Wi-Fi as plain Ethernet. `GetAdaptersAddresses`
//! looks like it answers everything in one pass, but "looks like" is not a
//! verification level. Whether a second source (WlanAPI, WMI) is needed is
//! settled in Task 3, by checking `IfType` first — the same ordering discipline
//! macOS used — not by reaching for the richer API up front.
//!
//! **Are the metrics real?** macOS turned out to have no per-route metric at
//! all, which is why [`crate::servicerank`] exists. `MIB_IPFORWARD_ROW2` has a
//! `Metric` field, but so did the BSD route entry that turned out to be an
//! unpopulated constant. Task 4 finds out whether Windows' metrics actually
//! differentiate real links, or whether Windows needs its own equivalent of the
//! service-order tie-break.
//!
//! # Status
//!
//! **Scaffold only, and unverified on hardware.** Nothing in this module calls
//! Windows yet. Written without access to a Windows machine: the CI job on
//! `windows-latest` can prove this crate compiles and links, and nothing more —
//! that runner is a VM with one virtual NIC, no Wi-Fi radio and no tunnel.

use autonet_core::model::NetworkState;

use crate::{NetworkProvider, PlatformError};

/// Reads network state from Windows.
///
/// Holds no state: every call re-queries the OS, because the whole point of
/// AutoNet is that this changes underneath you. Like the macOS backend and
/// unlike the Linux one it needs no runtime — the IP Helper calls are ordinary
/// blocking ones.
pub(crate) struct WindowsProvider;

impl WindowsProvider {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl NetworkProvider for WindowsProvider {
    fn snapshot(&self) -> Result<NetworkState, PlatformError> {
        // TASK 1 SCAFFOLD. Returns an empty machine so the dispatch and the
        // Windows CI job can be verified before any FFI is written. Task 2
        // replaces this with the real `GetAdaptersAddresses` walk.
        //
        // Deliberately `Ok` and not `Unsupported`: the point of this task is to
        // prove the Windows arm of `provider()` is the one selected, and an
        // error would be indistinguishable from no backend at all.
        Ok(NetworkState::new(Vec::new(), Vec::new()).captured_now())
    }

    fn platform_name(&self) -> &'static str {
        // Names the mechanism, not the OS, matching "linux-netlink" and
        // "macos-sysconfig". This string ships in every --json payload, so it
        // is part of the schema-1 surface from here on.
        "windows-iphlpapi"
    }
}
