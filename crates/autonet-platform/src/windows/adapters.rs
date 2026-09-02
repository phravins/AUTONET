//! Enumerating adapters and their addresses through `GetAdaptersAddresses`.
//!
//! # Why this is the only source
//!
//! macOS needed `getifaddrs` for links, `if-addrs` for tidy per-address data and
//! a hand-rolled `SIOCGIFAFLAG_IN6` ioctl for the two IPv6 address flags. It
//! would be easy to assume Windows needs a comparable spread. It does not: one
//! `GetAdaptersAddresses` call returns strictly more than those three sources
//! combined.
//!
//! | Fact | macOS source | Here |
//! |---|---|---|
//! | prefix length | `if-addrs`, from a netmask | `OnLinkPrefixLength`, directly |
//! | MTU | a `SIOCGIFMTU` ioctl | `Mtu` |
//! | MAC | an `AF_LINK` `sockaddr_dl` walk | `PhysicalAddress` |
//! | IPv6 temporary | `IN6_IFF_TEMPORARY`, via ioctl | `SuffixOrigin` |
//! | duplicate address | `IN6_IFF_DUPLICATED`, via ioctl | `DadState` |
//!
//! The defect that disqualified `if-addrs` for the macOS link pass — it yields
//! one record *per address* and silently drops interfaces that have none —
//! cannot arise here, because the adapter is the outer list and addresses hang
//! off it. An adapter with no address is still an adapter. So the Windows
//! backend adds no dependency at all.
//!
//! # What is deliberately not read
//!
//! `Description` — `"Intel(R) Wi-Fi 6E AX211"` — and the adapter GUID. Both are
//! vendor strings rather than kernel facts, and reading either as evidence about
//! what a device *is* would be the Windows spelling of guessing that `en0` means
//! Wi-Fi. Classification is Task 3's, from `IfType`.
//!
//! # Status
//!
//! **Unverified on hardware.** Written without access to a Windows machine.
//! Constants are pinned to windows-sys at compile time and the parsing is
//! exercised by hand-built buffers in [`crate::winparse`], but no claim here
//! about what a real machine reports has been observed. In particular a CI
//! runner has one virtual NIC, so the IPv6, temporary-address,
//! duplicate-address and index-fallback paths compile and never execute.

use autonet_core::model::{Address, Interface, InterfaceFlags, InterfaceKind};
use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, ERROR_SUCCESS};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_NO_MULTICAST,
    IP_ADAPTER_UNICAST_ADDRESS_LH,
};
use windows_sys::Win32::NetworkManagement::Ndis::{
    IfOperStatusDormant, IfOperStatusDown, IfOperStatusLowerLayerDown, IfOperStatusNotPresent,
    IfOperStatusTesting, IfOperStatusUnknown, IfOperStatusUp,
};
use windows_sys::Win32::Networking::WinSock::{
    IpDadStateDuplicate, IpSuffixOriginRandom, AF_INET, AF_INET6, AF_UNSPEC,
};

use crate::hwaddr::format_mac;
use crate::linktype::UNCLASSIFIED;
use crate::winparse::{self, af, oper};
use crate::PlatformError;

// The constants `winparse` restates so that it can be built and tested off
// Windows, pinned here to windows-sys's own definitions. This is the whole
// safety net for keeping a Linux-testable parser honest: if Microsoft ever
// renumbers one, the Windows build fails rather than the parser quietly
// misreading a status or an address family.
const _: () = {
    assert!(af::INET == AF_INET);
    assert!(af::INET6 == AF_INET6);
    assert!(oper::UP == IfOperStatusUp);
    assert!(oper::DOWN == IfOperStatusDown);
    assert!(oper::TESTING == IfOperStatusTesting);
    assert!(oper::UNKNOWN == IfOperStatusUnknown);
    assert!(oper::DORMANT == IfOperStatusDormant);
    assert!(oper::NOT_PRESENT == IfOperStatusNotPresent);
    assert!(oper::LOWER_LAYER_DOWN == IfOperStatusLowerLayerDown);
};

/// What is asked for, and what is skipped.
///
/// Unicast addresses are the only ones that answer "what can another device
/// reach", so anycast, multicast and DNS servers are skipped — which shrinks
/// both the buffer and the walk. `GAA_FLAG_INCLUDE_PREFIX` is deliberately not
/// set: `OnLinkPrefixLength` already carries the prefix, and Microsoft warns
/// that the `FirstPrefix` list has no positional relationship to the unicast
/// list, so it could not be zipped with it anyway.
///
/// `GAA_FLAG_INCLUDE_ALL_INTERFACES` is also not set. Without it Windows returns
/// only adapters bound to IPv4 or IPv6, which is what `ipconfig` shows; with it
/// the list fills with WAN Miniport, Teredo and filter-driver entries that have
/// no IP stack and can never hold a reachable address.
const FLAGS: u32 = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;

/// The working-buffer size Microsoft's own reference implementation starts at,
/// chosen there because on typical machines it avoids a second call entirely.
const INITIAL_BYTES: usize = 15_000;

/// How many times to grow the buffer before giving up.
///
/// The list can change size *between* calls — an adapter appearing mid-call is
/// exactly what AutoNet exists to cope with — so a single retry is not enough.
/// Three matches the reference implementation's `MAX_TRIES`. It is bounded
/// rather than open-ended because a machine churning adapters faster than three
/// allocations will not settle on the fourth either, and `autonet ip` hanging
/// would be worse than `autonet ip` failing.
const MAX_ATTEMPTS: usize = 3;

/// The longest friendly name we will scan for a NUL terminator.
///
/// Windows caps the ifAlias well below this. The bound exists so that a
/// corrupted pointer produces a truncated name rather than an unbounded read.
const MAX_NAME_UNITS: usize = 512;

/// Every adapter the machine has bound to IPv4 or IPv6, with its addresses.
///
/// # Errors
///
/// Returns [`PlatformError::Query`] if `GetAdaptersAddresses` fails for any
/// reason other than having nothing to report.
pub(crate) fn interfaces() -> Result<Vec<Interface>, PlatformError> {
    let Some(buffer) = adapter_buffer()? else {
        return Ok(Vec::new());
    };

    Ok(walk(buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()))
}

/// Call `GetAdaptersAddresses`, growing the buffer until it fits.
///
/// `Ok(None)` means `ERROR_NO_DATA`: Windows found no addresses for the
/// requested parameters. That is a legitimate description of a machine with
/// nothing configured — the same answer the `disconnected.json` fixture
/// encodes — and not a failure to query the OS, so it must not become an error
/// the CLI reports as exit 2.
///
/// The buffer is a `Vec<u64>` rather than a `Vec<u8>` because
/// `IP_ADAPTER_ADDRESSES_LH` leads with a union whose first member is a `u64`,
/// so the allocation has to be eight-byte aligned. `Vec<u8>` promises only one.
fn adapter_buffer() -> Result<Option<Vec<u64>>, PlatformError> {
    let mut bytes = INITIAL_BYTES;

    for _ in 0..MAX_ATTEMPTS {
        let words = bytes.div_ceil(size_of::<u64>());
        let mut buffer: Vec<u64> = vec![0; words];
        let mut size = u32::try_from(words * size_of::<u64>()).unwrap_or(u32::MAX);

        // SAFETY: `buffer` is `size` bytes of writable, `u64`-aligned memory and
        // `size` describes it truthfully, so the call either fills the buffer or
        // reports that it is too small without writing past it. `reserved` is
        // documented as unused and required to be null.
        let result = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                FLAGS,
                std::ptr::null(),
                buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                &raw mut size,
            )
        };

        match result {
            ERROR_SUCCESS => return Ok(Some(buffer)),
            ERROR_NO_DATA => return Ok(None),
            // `size` now holds the size Windows says it needs. `max` keeps the
            // next attempt from shrinking, so the loop cannot repeat itself.
            ERROR_BUFFER_OVERFLOW => {
                bytes = bytes.max(usize::try_from(size).unwrap_or(usize::MAX));
            }
            other => {
                return Err(PlatformError::query(
                    "enumerate network adapters",
                    format!("Windows error {other}"),
                ))
            }
        }
    }

    Err(PlatformError::query(
        "enumerate network adapters",
        format!("the adapter list kept growing across {MAX_ATTEMPTS} attempts"),
    ))
}

/// Walk the adapter linked list, in the order Windows wrote it.
///
/// Sorted by name afterwards rather than collected into a map, for two reasons.
/// Windows' own order is not stable — since Windows 10 it is derived from the
/// route metric, so it changes when the network changes — and a `BTreeMap` keyed
/// by name would silently *drop* one of two adapters sharing a friendly name.
/// Sorting keeps both. The sort is stable, so adapters that do collide stay in
/// the order Windows listed them.
fn walk(head: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<Interface> {
    let mut interfaces = Vec::new();
    let mut current = head;

    while !current.is_null() {
        // SAFETY: `current` is either the start of the buffer
        // `GetAdaptersAddresses` filled or a `Next` pointer it wrote there, so
        // it addresses a fully initialised structure inside that buffer, which
        // outlives this walk.
        let adapter = unsafe { &*current };

        if let Some(interface) = interface_from(adapter) {
            interfaces.push(interface);
        }

        current = adapter.Next;
    }

    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    interfaces
}

/// Translate one adapter, or skip it if it has no usable name.
///
/// A nameless adapter is dropped rather than given a synthetic name, for the
/// same reason `macos/ifaddrs.rs` drops one: no config rule could match it and
/// no user could act on it. It should not happen — `GAA_FLAG_SKIP_FRIENDLY_NAME`
/// is not set, so Windows always fills `FriendlyName` in — which is precisely
/// why the case is handled explicitly rather than assumed away.
fn interface_from(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Option<Interface> {
    let name = friendly_name(adapter.FriendlyName);
    if name.is_empty() {
        return None;
    }

    // Loopback is identified by the type Windows reports, never by name. The
    // built-in adapter is called "Loopback Pseudo-Interface 1" in English and
    // something else on a localised install, and a user can add further
    // KM-TEST loopback adapters under arbitrary names — two independent ways a
    // name check would be wrong.
    let is_loopback = adapter.IfType == IF_TYPE_SOFTWARE_LOOPBACK;

    // SAFETY: `Anonymous1` unions a `u64` with a `{ Length, IfIndex }` pair of
    // the same total width. Every bit pattern is a valid `u32`, so reading
    // `IfIndex` is sound whichever member Windows wrote.
    let ipv4_index = unsafe { adapter.Anonymous1.Anonymous.IfIndex };
    // SAFETY: as above — `Anonymous2` unions `Flags` with a bitfield of `u32`.
    let flags = unsafe { adapter.Anonymous2.Flags };

    Some(Interface {
        name,
        // Windows numbers an adapter once per family, and either index is zero
        // when that family is unavailable. The model holds one, and IPv4 is the
        // default family, so IPv4 wins where it exists. Zero — both families
        // absent — is the same "index unknown" value the macOS orphan path uses.
        index: if ipv4_index == 0 {
            adapter.Ipv6IfIndex
        } else {
            ipv4_index
        },
        // Task 3's job, not this one's. Loopback is the single exception, and
        // only because the model needs it as a *flag*: `flags.loopback` and
        // `InterfaceKind::Loopback` describe the same fact, and setting one
        // without the other would be internally inconsistent.
        kind: if is_loopback {
            InterfaceKind::Loopback
        } else {
            InterfaceKind::Other(UNCLASSIFIED.to_owned())
        },
        state: winparse::interface_state(adapter.OperStatus),
        flags: InterfaceFlags {
            // `up` and `running` carry the same fact here, and that is honest
            // rather than lazy. Linux distinguishes administrative `IFF_UP` from
            // carrier `IFF_RUNNING`; `GetAdaptersAddresses` reports only
            // operational status. Administrative status lives in
            // `MIB_IF_ROW2.AdminStatus`, behind a separate `GetIfEntry2` call
            // per adapter. Inventing a difference between the two fields would
            // be worse than reporting the one fact twice.
            up: adapter.OperStatus == oper::UP,
            running: adapter.OperStatus == oper::UP,
            loopback: is_loopback,
            // Neither has any source in this API. `MIB_IF_ROW2.AccessType`
            // supplies both, at the cost of one more call per adapter — and
            // nothing in autonet-core reads these two fields, so the cost buys
            // only a more complete `interfaces --json`. Deriving them from
            // `IfType` instead would be exactly the inference this backend
            // refuses to make.
            broadcast: false,
            point_to_point: false,
            multicast: flags & IP_ADAPTER_NO_MULTICAST == 0,
        },
        mac: hardware_address(adapter),
        mtu: mtu_of(adapter),
        addresses: addresses_of(adapter),
    })
}

/// The adapter's user-visible name.
///
/// `FriendlyName` rather than `AdapterName`: it is the RFC 2863 ifAlias, it is
/// what `ipconfig` and the Connection folder show, and so it is what a person
/// writing `--interface` or an `exclude_interfaces` rule will reach for.
/// `AdapterName` is a GUID — stable and unique, but not something anyone can
/// type. The trade is that a friendly name can be renamed and is not guaranteed
/// unique; see the note on [`walk`].
fn friendly_name(name: PWSTR) -> String {
    if name.is_null() {
        return String::new();
    }

    let mut len = 0;
    // SAFETY: Windows NUL-terminates `FriendlyName`, and the scan stops at
    // `MAX_NAME_UNITS` regardless, so no read runs past a plausible allocation
    // even if the terminator were missing.
    while len < MAX_NAME_UNITS && unsafe { *name.add(len) } != 0 {
        len += 1;
    }

    // SAFETY: `len` units were just read successfully through this pointer.
    let units = unsafe { std::slice::from_raw_parts(name, len) };
    winparse::wide_to_string(units)
}

/// The adapter's hardware address, when it has a reportable one.
///
/// `PhysicalAddress` is a fixed eight-byte array with `PhysicalAddressLength`
/// meaningful bytes — zero "for interfaces that do not have a data-link layer".
/// The length is clamped to the array before slicing, so a wrong length from the
/// OS cannot become an out-of-bounds read. `format_mac` then rejects anything
/// that is not six non-zero bytes, which is what drops loopback and tunnel
/// adapters and keeps this backend agreeing with the other two about what counts
/// as a MAC.
fn hardware_address(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Option<String> {
    let len = usize::try_from(adapter.PhysicalAddressLength)
        .unwrap_or(0)
        .min(adapter.PhysicalAddress.len());

    format_mac(&adapter.PhysicalAddress[..len])
}

/// The link MTU, or `None` when the adapter reports a placeholder.
///
/// Zero means unreported. `u32::MAX` is the sentinel some tunnel adapters use;
/// it is not an MTU any link has, and passing it through would put a nonsense
/// number in the JSON contract.
fn mtu_of(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Option<u32> {
    match adapter.Mtu {
        0 | u32::MAX => None,
        mtu => Some(mtu),
    }
}

/// Walk the unicast addresses hanging off one adapter.
fn addresses_of(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Vec<Address> {
    let mut addresses = Vec::new();
    let mut current: *const IP_ADAPTER_UNICAST_ADDRESS_LH = adapter.FirstUnicastAddress;

    while !current.is_null() {
        // SAFETY: `current` is a pointer `GetAdaptersAddresses` wrote into the
        // buffer, addressing an initialised structure that outlives this walk.
        let unicast = unsafe { &*current };

        if let Some(address) = address_from(unicast) {
            addresses.push(address);
        }

        current = unicast.Next;
    }

    addresses
}

/// Translate one unicast address, or skip it.
///
/// `family` and `scope` come from `Address::new`, which calls
/// `autonet_core::classify`. Reimplementing "is this private, link-local,
/// loopback or global" in a platform backend is how three backends end up
/// disagreeing about RFC 1918 — so the derivation happens once, in the core,
/// and `is_temporary` is the only field this backend fills in itself.
fn address_from(unicast: &IP_ADAPTER_UNICAST_ADDRESS_LH) -> Option<Address> {
    // A duplicate address is one the machine has already been told it may not
    // use, so publishing it as reachable would be a lie. This is the Windows
    // spelling of the macOS `IN6_IFF_DUPLICATED` check. Tentative and deprecated
    // addresses are deliberately kept: the first is on its way to working, and
    // the second still works.
    if unicast.DadState == IpDadStateDuplicate {
        return None;
    }

    let socket = &unicast.Address;
    if socket.lpSockaddr.is_null() {
        return None;
    }

    let len = usize::try_from(socket.iSockaddrLength).ok()?;
    // SAFETY: `iSockaddrLength` is the length Windows itself wrote for this
    // sockaddr, and the allocation it points into outlives this call.
    let bytes = unsafe { std::slice::from_raw_parts(socket.lpSockaddr.cast::<u8>(), len) };
    let ip = winparse::sockaddr_ip(bytes)?;

    Some(Address {
        // Windows' name for an RFC 4941 privacy-extension address: the suffix
        // was generated randomly rather than from the interface identifier.
        // These change every few hours, so the selector prefers stable ones.
        is_temporary: unicast.SuffixOrigin == IpSuffixOriginRandom,
        ..Address::new(ip, winparse::prefix_len(unicast.OnLinkPrefixLength, &ip))
    })
}

// Nothing in this module is unit-testable: every function reads a structure only
// Windows can produce. The logic that *can* be tested off a live machine — the
// sockaddr decode, the status mapping, the name conversion and the prefix
// clamp — lives in `crate::winparse` for exactly that reason, and its tests run
// on the Linux job where a failure can be debugged.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_address_families_are_the_ones_we_never_read() {
        // A guard against a future edit adding a flag that changes what the walk
        // above is looking at. Unicast must not be skipped; the prefix list must
        // not be requested, since `OnLinkPrefixLength` is used instead.
        assert_eq!(FLAGS & 1, 0, "GAA_FLAG_SKIP_UNICAST must not be set");
        assert_eq!(FLAGS & 16, 0, "GAA_FLAG_INCLUDE_PREFIX must not be set");
        assert_eq!(FLAGS & 32, 0, "GAA_FLAG_SKIP_FRIENDLY_NAME must not be set");
        assert_eq!(FLAGS & 256, 0, "GAA_FLAG_INCLUDE_ALL_INTERFACES: see FLAGS");
    }

    #[test]
    fn the_buffer_starts_at_the_documented_working_size() {
        assert_eq!(INITIAL_BYTES, 15_000);
        assert!(INITIAL_BYTES.div_ceil(size_of::<u64>()) * size_of::<u64>() >= INITIAL_BYTES);
    }
}
