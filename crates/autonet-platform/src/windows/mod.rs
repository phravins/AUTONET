//! The Windows backend.
//!
//! Windows exposes network configuration through the IP Helper API in
//! `iphlpapi.dll`, and the plan is for two calls to cover what Linux needed
//! netlink for and macOS needed three sources for:
//!
//! | Source | Provides |
//! |---|---|
//! | `GetAdaptersAddresses` | interfaces, addresses, `IfType`, `TunnelType`, `OperStatus`, index, MTU, MAC, LUID |
//! | `GetIfTable2` | `AccessType`, `PhysicalMediumType`, `MediaType`, the `HardwareInterface` bit, `AdminStatus` |
//! | `GetIpForwardTable2` | routes, gateways, the default route, per-route metrics — Task 4 |
//!
//! Two whole-machine calls, joined by LUID, rather than anything per adapter.
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
//! **Does `IfType` alone classify? Half of it does, and it is the other half
//! that bites.** Task 3 settled this by checking `IfType` first rather than
//! reaching for the richer API. Wi-Fi versus Ethernet — the question that forced
//! macOS to lead with SystemConfiguration — Windows answers itself, with
//! `IF_TYPE_IEEE80211`, so **WlanAPI is not needed and stays disabled**. What
//! `IfType` cannot answer is Ethernet versus a virtual adapter *presenting* as
//! Ethernet, which is what a TAP-mode VPN, a Hyper-V switch and a WSL switch all
//! do. `GetIfTable2` settles that, for one call. [`crate::wintype`] holds the
//! full comparison against `GetIfEntry2`, WlanAPI and WMI.
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
//! # Kinds this backend cannot identify, stated rather than approximated
//!
//! Two gaps have no source in the IP Helper API, and neither is worked around:
//!
//! - **Bridges.** There is no `IF_TYPE_BRIDGE`; windows-sys has no such constant
//!   because `ipifcons.h` has none. A Windows network bridge presents as an
//!   Ethernet device, so it is reported as Ethernet (+250) rather than Bridge
//!   (+150). The 100-point difference only ever matters between a bridge and a
//!   real port on the same machine, and calling a bridge Ethernet is closer to
//!   the truth than calling a real port a bridge would be.
//! - **Container devices.** On Linux `docker0` is `Container` and loses 800
//!   points. Docker Desktop on Windows runs behind a Hyper-V vEthernet adapter
//!   that is indistinguishable, by any numeric field, from the vEthernet adapter
//!   carrying the host's real connectivity. So both land on
//!   `Other("virtual-ethernet")` and score zero. Distinguishing them would
//!   require matching the adapter name against `"vEthernet (WSL)"` or similar,
//!   which is the guess this milestone forbids; `exclude_interfaces` is the
//!   supported way to express a preference AutoNet cannot derive.
//!
//! `IF_TYPE_PROP_VIRTUAL` is likewise left unmapped. IANA calls it "proprietary
//! virtual/internal" and this project has no observation of what Windows
//! actually uses it for, so it falls through to `unclassified` (zero) rather
//! than to `InterfaceKind::Virtual`, which would cost it 800 points on a guess.
//!
//! # Status
//!
//! **Unverified on hardware.** Written without access to a Windows machine.
//! Interfaces, addresses and kinds are real from Tasks 2 and 3; routes are still
//! empty until Task 4. The `windows-latest` CI job proves this crate compiles,
//! links against the real `iphlpapi.dll` and returns something coherent for a VM
//! with one virtual NIC — no Wi-Fi radio, no VPN, no tunnel, no IPv6 temporary
//! address. Every path that depends on those is type-checked and never executed
//! until the Task 7 hardware run.
//!
//! The weakest claim in the backend is the bit position of `HardwareInterface`
//! in `MIB_IF_ROW2`, which windows-sys exposes as an unnamed `u8`. It is
//! reasoned from the MSVC bitfield ABI, guarded at runtime by a loopback
//! consistency check, and **not observed** — see [`iftable`].

mod adapters;
mod iftable;

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
