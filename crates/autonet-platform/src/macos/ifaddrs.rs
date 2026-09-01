//! Interfaces and addresses, from `getifaddrs(3)`.
//!
//! This mirrors the Linux backend's structure deliberately: a link pass that
//! establishes which devices exist, then an address pass that attaches IPs to
//! them. `getifaddrs` returns both kinds of record interleaved in one list —
//! an `AF_LINK` node per device followed by an `AF_INET`/`AF_INET6` node per
//! address — so the split is ours, not the API's.
//!
//! # Why the link pass is not simply `if-addrs`
//!
//! `if_addrs::get_if_addrs()` yields one record per *address* and silently
//! drops any interface that has none: its conversion returns `None` for
//! `AF_LINK` nodes, and the loop skips them. An Ethernet port with a cable but
//! no DHCP lease, or a `bridge0` with no address, would vanish from the
//! snapshot entirely — while Linux reports both, because netlink's link dump is
//! a separate query from its address dump.
//!
//! So the device list is built here from the `AF_LINK` records, which also
//! carry the three things `if-addrs` does not expose at all: the interface
//! flags, the hardware address, and the MTU. `if-addrs` is then used for what
//! it is good at — turning a `sockaddr` into an `IpAddr` and a netmask into a
//! prefix length.
//!
//! # Status
//!
//! **Unverified on hardware.** Compiled and type-checked for both
//! `aarch64-apple-darwin` and `x86_64-apple-darwin`, but every claim about
//! struct layout here is read from headers, not observed on a Mac.

use std::collections::{BTreeMap, HashMap};
use std::ffi::CStr;
use std::net::{IpAddr, Ipv6Addr};

use autonet_core::model::{
    Address, Interface, InterfaceFlags, InterfaceKind, InterfaceState, NetworkState,
};
use if_addrs::IfAddr;
use libc::{c_int, c_uint};

use crate::hwaddr::format_mac;
use crate::linktype::{self, Evidence, ScType, UNCLASSIFIED};
use crate::PlatformError;

/// What SystemConfiguration calls each interface, keyed by BSD name.
///
/// Gathered once per snapshot and threaded through the walk, because
/// `SCNetworkInterfaceCopyAll` enumerates everything in a single call — there
/// is no per-interface framework call anywhere here.
type ScTypes = HashMap<String, ScType>;

/// Capture the machine's interfaces and their addresses.
///
/// Routes are not collected yet, so the returned state has none. Until they
/// are, selection has no default-route evidence to work with on macOS and its
/// answer should not be trusted.
pub(crate) fn snapshot(sc_types: &ScTypes) -> Result<NetworkState, PlatformError> {
    let mut interfaces = links(sc_types)?;
    attach_addresses(&mut interfaces)?;

    Ok(NetworkState::new(interfaces.into_values().collect(), Vec::new()).captured_now())
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

/// Every device the kernel reports, keyed by name.
///
/// Keyed by name rather than index because that is the only join key the
/// address pass has. A `BTreeMap` for the same reason as the Linux backend's:
/// it makes `autonet interfaces` list devices in a stable order between runs.
fn links(sc_types: &ScTypes) -> Result<BTreeMap<String, Interface>, PlatformError> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();

    // SAFETY: `getifaddrs` either writes an owned linked list into `head` and
    // returns 0, or returns -1 and leaves it alone.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return Err(PlatformError::query(
            "list network interfaces",
            std::io::Error::last_os_error(),
        ));
    }

    let mut interfaces = BTreeMap::new();
    let mut node = head;
    while !node.is_null() {
        // SAFETY: `node` is non-null and points into the list `getifaddrs`
        // built, which is not freed until the walk is over.
        let entry = unsafe { &*node };
        if let Some(interface) = link_from(entry, sc_types) {
            interfaces.insert(interface.name.clone(), interface);
        }
        node = entry.ifa_next;
    }

    // SAFETY: `head` came from `getifaddrs` and has not been freed. Everything
    // above was copied out of the list, so nothing borrows it any more.
    unsafe { libc::freeifaddrs(head) };

    Ok(interfaces)
}

/// Build an interface from one `AF_LINK` record, or skip anything else.
fn link_from(entry: &libc::ifaddrs, sc_types: &ScTypes) -> Option<Interface> {
    if entry.ifa_addr.is_null() {
        return None;
    }
    // SAFETY: non-null, and every `sockaddr` begins with `sa_len`/`sa_family`
    // whatever its concrete type turns out to be.
    let family = unsafe { (*entry.ifa_addr).sa_family };
    if c_int::from(family) != libc::AF_LINK {
        return None;
    }

    let name = name_of(entry)?;
    let link = entry.ifa_addr.cast::<libc::sockaddr_dl>();

    // SAFETY: `sa_family` says `AF_LINK`, so this really is a `sockaddr_dl`.
    let index = u32::from(unsafe { (*link).sdl_index });

    let flags = entry.ifa_flags;
    let is_loopback = has(flags, libc::IFF_LOOPBACK);
    let point_to_point = has(flags, libc::IFF_POINTOPOINT);

    // Three independent sources, weighed in `linktype`: the two flags the
    // kernel set, what SystemConfiguration calls this BSD name, and the link
    // type the driver reported. The name itself is passed only as a map key —
    // it is never evidence.
    let kind = linktype::classify(Evidence {
        loopback: is_loopback,
        point_to_point,
        sc: sc_types.get(&name).copied(),
        ifi_type: ifi_type_of(entry),
    });

    Some(Interface {
        name,
        index,
        kind,
        state: interface_state(flags),
        flags: InterfaceFlags {
            up: has(flags, libc::IFF_UP),
            running: has(flags, libc::IFF_RUNNING),
            loopback: is_loopback,
            broadcast: has(flags, libc::IFF_BROADCAST),
            point_to_point,
            multicast: has(flags, libc::IFF_MULTICAST),
        },
        // SAFETY: as above — an `AF_LINK` sockaddr.
        mac: unsafe { hardware_address(link) },
        mtu: mtu_of(entry),
        addresses: Vec::new(),
    })
}

/// The interface name, or `None` if it is absent or not UTF-8.
///
/// A device we cannot name is one no config rule could match and no user could
/// act on, so it is dropped rather than given a synthetic name — the same
/// choice the Linux backend makes for a link with no `IFLA_IFNAME`.
fn name_of(entry: &libc::ifaddrs) -> Option<String> {
    if entry.ifa_name.is_null() {
        return None;
    }
    // SAFETY: non-null, and `getifaddrs` guarantees a NUL-terminated name.
    let name = unsafe { CStr::from_ptr(entry.ifa_name) };
    Some(name.to_str().ok()?.to_owned())
}

/// The hardware address carried by an `AF_LINK` record.
///
/// `libc` declares `sdl_data` as `[c_char; 12]`, but the kernel's structure is
/// variable-length: `sdl_nlen` bytes of interface name followed by `sdl_alen`
/// bytes of hardware address. On `bridge100` the address starts at offset 17
/// and runs past the end of the 20-byte `sockaddr_dl` Rust believes in, so
/// **copying the struct by value would truncate the MAC**. The bytes are read
/// through the pointer `getifaddrs` gave us, which addresses the real, longer
/// allocation, and bounds-checked against the `sdl_len` the kernel wrote.
///
/// # Safety
///
/// `link` must point at a `sockaddr_dl` from a `getifaddrs` list.
unsafe fn hardware_address(link: *const libc::sockaddr_dl) -> Option<String> {
    // SAFETY: the caller guarantees this points at an `AF_LINK` sockaddr.
    let (total, name_len, addr_len) = unsafe {
        (
            usize::from((*link).sdl_len),
            usize::from((*link).sdl_nlen),
            usize::from((*link).sdl_alen),
        )
    };

    // Anything claiming to reach past `sdl_len` is malformed, and a `sdl_alen`
    // that is not six is not a MAC — `format_mac` would reject it anyway, but
    // the check has to happen before the read, not after.
    let offset = std::mem::offset_of!(libc::sockaddr_dl, sdl_data) + name_len;
    if addr_len != 6 || offset + addr_len > total {
        return None;
    }

    let start = link.cast::<u8>().wrapping_add(offset);
    // SAFETY: bounds-checked above against the length the kernel itself wrote,
    // and `getifaddrs` allocated the whole `sdl_len` bytes contiguously.
    let bytes = unsafe { std::slice::from_raw_parts(start, addr_len) };

    format_mac(bytes)
}

/// The link type the driver reported, from the same `if_data` block as the MTU.
///
/// `ifi_type` is the first byte of `if_data`, so unlike `ifi_mtu` there is no
/// offset arithmetic and no alignment question — a `u8` read is aligned
/// everywhere. It is still read through the pointer rather than by copying the
/// struct, for the reason spelled out on [`mtu_of`].
///
/// A null `ifa_data` yields [`linktype::IFI_TYPE_UNSPECIFIED`], which is the
/// value the header itself uses for "unspecified", so an absent `if_data` block
/// and an uninformative one take the same path through the classifier.
fn ifi_type_of(entry: &libc::ifaddrs) -> u8 {
    if entry.ifa_data.is_null() {
        return linktype::IFI_TYPE_UNSPECIFIED;
    }

    let field = entry
        .ifa_data
        .cast::<u8>()
        .wrapping_add(std::mem::offset_of!(libc::if_data, ifi_type));

    // SAFETY: for an `AF_LINK` record `ifa_data` points at a `struct if_data`,
    // whose first byte is `ifi_type`. Any `if_data` at all is at least one byte
    // long, so this cannot read past a shorter kernel struct.
    unsafe { field.read() }
}

/// The MTU, from the `if_data` block `getifaddrs` hangs off an `AF_LINK` entry.
///
/// Only the four bytes of `ifi_mtu` are read, at their offset, rather than the
/// whole `if_data` by value. Nothing tells us how many bytes `ifa_data` really
/// points at, so if the running kernel's `if_data` were ever shorter than the
/// one `libc` describes, a by-value read would run off the end of it — the same
/// class of mistake as trusting `sockaddr_dl`'s declared size. `ifi_mtu` sits
/// eight bytes in, so this reads a small, stable prefix instead.
///
/// A zero MTU is reported as absent rather than as zero: it means the driver
/// did not fill the field in, and "0" would read as a real, absurd value.
fn mtu_of(entry: &libc::ifaddrs) -> Option<u32> {
    if entry.ifa_data.is_null() {
        return None;
    }

    let field = entry
        .ifa_data
        .cast::<u8>()
        .wrapping_add(std::mem::offset_of!(libc::if_data, ifi_mtu));

    // The bytes are copied out and reassembled rather than read through a
    // `*const u32`: `ifa_data` is an untyped pointer, so nothing promises the
    // alignment that cast would claim, and native byte order is right because
    // the kernel wrote the field on this machine.
    let mut bytes = [0u8; 4];
    // SAFETY: for an `AF_LINK` record `ifa_data` points at a `struct if_data`,
    // whose `ifi_mtu` lies wholly within the first sixteen bytes.
    unsafe { std::ptr::copy_nonoverlapping(field, bytes.as_mut_ptr(), bytes.len()) };

    match u32::from_ne_bytes(bytes) {
        0 => None,
        mtu => Some(mtu),
    }
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

fn attach_addresses(interfaces: &mut BTreeMap<String, Interface>) -> Result<(), PlatformError> {
    let reported =
        if_addrs::get_if_addrs().map_err(|e| PlatformError::query("list IP addresses", e))?;

    for record in reported {
        let (ip, prefix_len) = match record.addr {
            IfAddr::V4(v4) => (IpAddr::V4(v4.ip), v4.prefixlen),
            IfAddr::V6(v6) => (IpAddr::V6(strip_embedded_scope_id(v6.ip)), v6.prefixlen),
        };

        let (name, index) = (record.name, record.index);
        interfaces
            .entry(name.clone())
            .or_insert_with(|| orphan(&name, index))
            // `Address::new` derives family and scope from the IP itself, which
            // is why no scope logic appears in this crate.
            .addresses
            .push(Address::new(ip, prefix_len));
    }

    Ok(())
}

/// Undo the KAME scope-id embedding in a link-local IPv6 address.
///
/// BSD kernels, macOS among them, store the interface index *inside* the
/// address — octets 2 and 3 of an `fe80::/10` address — as well as in
/// `sin6_scope_id`. `getifaddrs` returns that raw form, so the link-local
/// address of interface 5 arrives looking like `fe80:5::…`. RFC 4291 requires
/// those octets to be zero in a real link-local address, so clearing them
/// repairs a known kernel quirk rather than guessing, and it is a no-op on any
/// address that does not carry it.
fn strip_embedded_scope_id(ip: Ipv6Addr) -> Ipv6Addr {
    let mut octets = ip.octets();
    if octets[0] == 0xfe && octets[1] & 0xc0 == 0x80 {
        octets[2] = 0;
        octets[3] = 0;
    }
    Ipv6Addr::from(octets)
}

/// An interface that appeared in the address list but not in the link walk.
///
/// `getifaddrs` reports an `AF_LINK` record for every device, so this should
/// never fire. It exists because the alternative — dropping the address —
/// would lose a real, usable IP if that assumption is ever wrong, and an
/// interface missing its MTU is a far smaller lie than a missing address.
fn orphan(name: &str, index: Option<u32>) -> Interface {
    Interface {
        name: name.to_owned(),
        index: index.unwrap_or_default(),
        // No `AF_LINK` record means no flags and no `ifi_type`, so there is no
        // evidence to classify from. Consulting SystemConfiguration alone would
        // work, but an interface that reached this branch already contradicts
        // what `getifaddrs` promises; saying so is better than half an answer.
        kind: InterfaceKind::Other(UNCLASSIFIED.to_owned()),
        state: InterfaceState::Unknown,
        flags: InterfaceFlags::default(),
        mac: None,
        mtu: None,
        addresses: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// Whether one of the `IFF_*` flags is set.
///
/// `ifa_flags` is unsigned and the `IFF_*` constants are signed, so both widen
/// into `i64` rather than one of them being cast into the other's type.
fn has(flags: c_uint, flag: c_int) -> bool {
    i64::from(flags) & i64::from(flag) != 0
}

/// Map `ifa_flags` onto AutoNet's state, matching the Linux backend's rules.
///
/// The interesting case is administratively up but not `IFF_RUNNING`, which
/// resolves to `Unknown` rather than `Down`. `utun` devices report exactly that
/// while carrying traffic, and the selection engine treats only `Down` as
/// disqualifying — so guessing "down" here would make `autonet ip --interface
/// utun3` fail on a healthy tunnel.
///
/// macOS offers no equivalent of Linux's `IF_OPER_DORMANT`, so `Dormant` is
/// never produced here. That is a real difference between the platforms rather
/// than an omission: nothing in the flags distinguishes "associating" from
/// "associated".
fn interface_state(flags: c_uint) -> InterfaceState {
    if !has(flags, libc::IFF_UP) {
        InterfaceState::Down
    } else if has(flags, libc::IFF_RUNNING) {
        InterfaceState::Up
    } else {
        InterfaceState::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kame_scope_id_is_removed_from_link_local_addresses() {
        // What macOS actually returns for interface 5's link-local address.
        let embedded: Ipv6Addr = "fe80:5::1c4a:b2ff:fe3d:9e10".parse().unwrap();
        assert_eq!(
            strip_embedded_scope_id(embedded),
            "fe80::1c4a:b2ff:fe3d:9e10".parse::<Ipv6Addr>().unwrap()
        );
    }

    #[test]
    fn an_address_without_the_hack_is_left_alone() {
        for address in [
            "fe80::1c4a:b2ff:fe3d:9e10", // already clean
            "2401:db8:1234::1",          // global
            "fd00:1234::1",              // unique local
            "::1",                       // loopback
        ] {
            let ip: Ipv6Addr = address.parse().unwrap();
            assert_eq!(strip_embedded_scope_id(ip), ip, "{address}");
        }
    }

    #[test]
    fn the_strip_covers_the_whole_of_fe80_slash_10() {
        // `febf::/16` is still link-local under RFC 4291's fe80::/10, and BSD
        // embeds the scope id there too. Matching only the literal byte 0x80
        // would miss it.
        let embedded: Ipv6Addr = "feb0:7::1".parse().unwrap();
        assert_eq!(
            strip_embedded_scope_id(embedded),
            "feb0::1".parse::<Ipv6Addr>().unwrap()
        );
    }

    #[test]
    fn a_tunnel_that_is_up_but_not_running_stays_usable() {
        // utun devices carry traffic while reporting exactly this.
        let flags = u32::try_from(libc::IFF_UP).unwrap();
        assert_eq!(interface_state(flags), InterfaceState::Unknown);
    }

    #[test]
    fn an_administratively_down_interface_is_down() {
        assert_eq!(interface_state(0), InterfaceState::Down);
    }

    #[test]
    fn a_running_interface_is_up() {
        let flags = u32::try_from(libc::IFF_UP | libc::IFF_RUNNING).unwrap();
        assert_eq!(interface_state(flags), InterfaceState::Up);
    }

    #[test]
    fn an_interface_the_link_walk_missed_is_not_guessed_at() {
        // Notably *not* Ethernet, which is what name-based classification
        // would return for en0. The decision table itself is exercised in
        // `linktype`, whose tests run on every platform rather than only here.
        assert_eq!(
            orphan("en0", Some(4)).kind,
            InterfaceKind::Other(UNCLASSIFIED.to_owned())
        );
    }
}
