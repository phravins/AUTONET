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
//! # One question answered, one still open
//!
//! **Is one call enough for interfaces and addresses? Yes — and `if-addrs` is
//! not needed on Windows at all.** Task 1 raised this without settling it,
//! because macOS needing two sources made it tempting to assume Windows would
//! too. It does not: a single `GetAdaptersAddresses` call returns strictly more
//! than macOS's `getifaddrs` + `if-addrs` + `SIOCGIFAFLAG_IN6` combined. The
//! defect that disqualified `if-addrs` for the macOS link pass — one record per
//! *address*, so addressless interfaces vanish — cannot arise when the adapter
//! is the outer list; `OnLinkPrefixLength` supplies the prefix directly, which
//! is the one thing `if-addrs` was genuinely kept for on macOS; and the two IPv6
//! address flags that cost macOS a hand-computed ioctl arrive in the same struct
//! as the address. [`adapters`] holds the full comparison. The practical
//! consequence is that this backend adds no dependency.
//!
//! Classification is a separate question and stays open: whether `IfType` alone
//! can tell Wi-Fi from Ethernet, or whether WlanAPI or WMI is needed the way
//! SystemConfiguration was on macOS, is settled in Task 3 by checking `IfType`
//! first — the same ordering discipline macOS used — not by reaching for the
//! richer API up front.
//!
//! **Are the metrics real?** macOS turned out to have no per-route metric at
//! all, which is why [`crate::servicerank`] exists. `MIB_IPFORWARD_ROW2` has a
//! `Metric` field, but so did the BSD route entry that turned out to be an
//! unpopulated constant. Task 4 finds out whether Windows' metrics actually
//! differentiate real links, or whether Windows needs its own equivalent of the
//! service-order tie-break.
//!
//! # A known limit of naming interfaces by `FriendlyName`
//!
//! `Interface.name` is the adapter's friendly name — the RFC 2863 ifAlias, what
//! `ipconfig` and the Connection folder show, and therefore what someone writing
//! `--interface` or an `exclude_interfaces` rule will type. The alternative,
//! `AdapterName`, is a GUID: unique and stable, but unusable by a human.
//!
//! The cost of that choice, recorded rather than papered over: a friendly name
//! is renameable and **not guaranteed unique**, while `autonet-core`'s `diff()`
//! matches interfaces by name. Two adapters sharing a friendly name would be
//! indistinguishable to M4's change detection. This backend does not silently
//! de-duplicate them — both are reported — so the limit stays visible for M4 to
//! decide on knowingly. The `Luid`, which *is* unique and persistent, is the
//! join key Task 4 will use for routes.
//!
//! # Status
//!
//! **Unverified on hardware.** Written without access to a Windows machine.
//! Interfaces and addresses are real from Task 2 onward; routes are still empty
//! until Task 4. The `windows-latest` CI job can prove this crate compiles,
//! links against the real `iphlpapi.dll` and returns something coherent for a VM
//! with one virtual NIC — no Wi-Fi radio, no VPN, no tunnel, no IPv6 temporary
//! address. Every path that depends on those is type-checked and never executed
//! until the Task 7 hardware run.

mod adapters;

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
        // Routes stay empty until Task 4 adds `GetIpForwardTable2`. That is a
        // partial answer, not a broken one: `select` treats a machine with no
        // default route as offline and says so, which is the same thing
        // `disconnected.json` asserts. It is honest about what has been
        // gathered rather than inventing a route to look complete.
        Ok(NetworkState::new(adapters::interfaces()?, Vec::new()).captured_now())
    }

    fn platform_name(&self) -> &'static str {
        // Names the mechanism, not the OS, matching "linux-netlink" and
        // "macos-sysconfig". This string ships in every --json payload, so it
        // is part of the schema-1 surface from here on.
        "windows-iphlpapi"
    }
}
