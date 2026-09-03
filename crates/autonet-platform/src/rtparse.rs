//! Reading BSD routing-socket messages, byte by byte.
//!
//! Kept outside `macos/` so its tests run on Linux too: sockaddr alignment and
//! netmask truncation are the classic ways a routing-socket parser goes
//! *subtly* wrong, producing plausible addresses rather than an error.
//!
//! Bytes rather than `libc` structs, because a BSD `sockaddr` begins with a
//! `sa_len` byte that Linux's does not have and `AF_INET6` is 30 on Darwin
//! against 10 on Linux — the types could not be shared even in principle. Every
//! constant and offset below is pinned to `libc`'s Darwin definitions by a
//! `const` assertion, so a moved field fails the build rather than quietly
//! returning wrong addresses.
//!
//! **Verified against headers and the compiler, not a live routing table.**

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use autonet_core::model::{Family, IpNetwork, Route};

use crate::servicerank;

/// Address families, as Darwin numbers them.
///
/// Restated rather than imported so that the parser and its tests build on
/// Linux. The `const` block below proves they match `libc` on macOS.
mod af {
    /// IPv4.
    pub const INET: i32 = 2;
    /// A link-layer address — `sockaddr_dl`.
    pub const LINK: i32 = 18;
    /// IPv6. Note this is *not* Linux's 10.
    pub const INET6: i32 = 30;
}

/// Which sockaddrs a routing message carries, as a bitmask in `rtm_addrs`.
///
/// The order of the bits is the order the sockaddrs appear in the buffer, which
/// is the only thing that makes the walk in [`slots`] possible: there are no
/// tags, just a run of variable-length structures whose identity is positional.
mod rta {
    /// The destination network.
    pub const DST: i32 = 0x1;
    /// The next hop.
    pub const GATEWAY: i32 = 0x2;
    /// The netmask, very often truncated. See [`super::prefix_len`].
    pub const NETMASK: i32 = 0x4;
    /// A cloning mask. Unused by AutoNet, but it occupies a slot.
    pub const GENMASK: i32 = 0x8;
    /// The interface, as a `sockaddr_dl`.
    pub const IFP: i32 = 0x10;
    /// The interface address — the source address for this route.
    pub const IFA: i32 = 0x20;
    /// The author of a redirect. Occupies a slot.
    pub const AUTHOR: i32 = 0x40;
    /// The broadcast or point-to-point destination. Occupies a slot.
    pub const BRD: i32 = 0x80;

    /// How many slots the bitmask can describe, and therefore how far the walk
    /// has to count. Bits above this are not defined by the kernel.
    pub const SLOTS: u32 = 8;
}

/// Route flags, from `<net/route.h>`.
mod rtf {
    /// The route is usable.
    pub const UP: i32 = 0x1;
    /// The route has a real next hop rather than being on-link.
    pub const GATEWAY: i32 = 0x2;
    /// A host route: the destination is a single address, not a network.
    pub const HOST: i32 = 0x4;
    /// Traffic is silently discarded.
    pub const BLACKHOLE: i32 = 0x1000;
    /// Traffic is rejected with an error.
    pub const REJECT: i32 = 0x8;
    /// An ARP or neighbour-discovery cache entry.
    pub const LLINFO: i32 = 0x400;
    /// A multicast route.
    pub const MULTICAST: i32 = 0x0080_0000;
    /// Generated from a parent route by cloning — the other half of the ARP and
    /// ND cache.
    pub const WASCLONED: i32 = 0x20000;
}

/// Byte offsets into `struct rt_msghdr`, and its total size.
///
/// The sockaddrs begin immediately after the header, so [`LEN`] being right is
/// what puts every address at the right offset. It is 92 on both Darwin targets:
/// `rt_metrics` is fourteen 32-bit fields, so nothing depends on pointer width.
///
/// [`LEN`]: header::LEN
mod header {
    /// `rtm_msglen` — the total length of this message, header included.
    pub const MSGLEN: usize = 0;
    /// `rtm_version`.
    pub const VERSION: usize = 2;
    /// `rtm_type`.
    pub const KIND: usize = 3;
    /// `rtm_index` — the interface this route exits through.
    pub const INDEX: usize = 4;
    /// `rtm_flags`.
    pub const FLAGS: usize = 8;
    /// `rtm_addrs` — the [`super::rta`] bitmask.
    pub const ADDRS: usize = 12;
    /// `size_of::<rt_msghdr>()`, and so the offset of the first sockaddr.
    pub const LEN: usize = 92;
}

/// `RTM_VERSION` — the routing-message ABI this parser understands.
pub(crate) const RTM_VERSION: u8 = 5;

// Everything above is restated from Apple's headers so this module builds on
// Linux. On macOS it is checked against `libc` at compile time, so a wrong
// number fails the build instead of producing wrong routes.
#[cfg(target_os = "macos")]
const _: () = {
    assert!(af::INET == libc::AF_INET);
    assert!(af::INET6 == libc::AF_INET6);
    assert!(af::LINK == libc::AF_LINK);

    assert!(rta::DST == libc::RTA_DST);
    assert!(rta::GATEWAY == libc::RTA_GATEWAY);
    assert!(rta::NETMASK == libc::RTA_NETMASK);
    assert!(rta::GENMASK == libc::RTA_GENMASK);
    assert!(rta::IFP == libc::RTA_IFP);
    assert!(rta::IFA == libc::RTA_IFA);
    assert!(rta::AUTHOR == libc::RTA_AUTHOR);
    assert!(rta::BRD == libc::RTA_BRD);

    assert!(rtf::UP == libc::RTF_UP);
    assert!(rtf::GATEWAY == libc::RTF_GATEWAY);
    assert!(rtf::HOST == libc::RTF_HOST);
    assert!(rtf::BLACKHOLE == libc::RTF_BLACKHOLE);
    assert!(rtf::REJECT == libc::RTF_REJECT);
    assert!(rtf::LLINFO == libc::RTF_LLINFO);
    assert!(rtf::MULTICAST == libc::RTF_MULTICAST);
    assert!(rtf::WASCLONED == libc::RTF_WASCLONED);

    assert!(header::LEN == std::mem::size_of::<libc::rt_msghdr>());
    assert!(header::INDEX == std::mem::offset_of!(libc::rt_msghdr, rtm_index));
    assert!(header::FLAGS == std::mem::offset_of!(libc::rt_msghdr, rtm_flags));
    assert!(header::ADDRS == std::mem::offset_of!(libc::rt_msghdr, rtm_addrs));

    // The sockaddr field offsets `ip_of` and `sdl_index` read.
    assert!(sockaddr::V4_ADDR == std::mem::offset_of!(libc::sockaddr_in, sin_addr));
    assert!(sockaddr::V6_ADDR == std::mem::offset_of!(libc::sockaddr_in6, sin6_addr));
    assert!(sockaddr::DL_INDEX == std::mem::offset_of!(libc::sockaddr_dl, sdl_index));

    assert!(RTM_VERSION as i32 == libc::RTM_VERSION);
};

/// Field offsets shared by every BSD `sockaddr` variant.
mod sockaddr {
    /// `sa_len`. Always the first byte, whatever the concrete type.
    pub const LEN: usize = 0;
    /// `sa_family`.
    pub const FAMILY: usize = 1;
    /// `sin_addr` within a `sockaddr_in`.
    pub const V4_ADDR: usize = 4;
    /// `sin6_addr` within a `sockaddr_in6`.
    pub const V6_ADDR: usize = 8;
    /// `sdl_index` within a `sockaddr_dl`.
    pub const DL_INDEX: usize = 2;
}

/// Round a sockaddr's length up to the next message boundary.
///
/// **Four bytes on Darwin, not eight.** `<net/route.h>` rounds to
/// `sizeof(uint32_t)`; FreeBSD rounds to `sizeof(long)`, and copying that
/// version of the macro is a well-worn way to misparse every sockaddr after the
/// first on a 64-bit Mac.
///
/// A zero length still advances by four — the kernel writes a zero-length
/// sockaddr for a default route's netmask, and treating that as zero bytes would
/// leave the walk stuck on it forever.
const fn roundup(len: usize) -> usize {
    if len == 0 {
        4
    } else {
        (len + 3) & !3
    }
}

/// The sockaddrs of one routing message, as raw byte slices.
///
/// Only the slots AutoNet reads are kept, but [`slots`] still walks past the
/// others: their lengths are what put the interesting ones at the right offset.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Slots<'a> {
    destination: Option<&'a [u8]>,
    gateway: Option<&'a [u8]>,
    netmask: Option<&'a [u8]>,
    interface: Option<&'a [u8]>,
    source: Option<&'a [u8]>,
}

/// Split a message's address block into its individual sockaddrs.
///
/// Returns `None` if the buffer ends in the middle of one, rather than guessing
/// at the remainder — a truncated dump is a bug worth surfacing, not something
/// to paper over with a half-read address.
fn slots(block: &[u8], addrs: i32) -> Option<Slots<'_>> {
    let mut found = Slots::default();
    let mut rest = block;

    for bit in 0..rta::SLOTS {
        let slot = 1_i32 << bit;
        if addrs & slot == 0 {
            continue;
        }

        // The bitmask claims another sockaddr but the buffer is spent.
        let &len = rest.get(sockaddr::LEN)?;
        let len = usize::from(len);
        if len > rest.len() {
            return None;
        }

        let (sa, tail) = (&rest[..len], &rest[roundup(len).min(rest.len())..]);
        rest = tail;

        match slot {
            rta::DST => found.destination = Some(sa),
            rta::GATEWAY => found.gateway = Some(sa),
            rta::NETMASK => found.netmask = Some(sa),
            rta::IFP => found.interface = Some(sa),
            rta::IFA => found.source = Some(sa),
            // GENMASK, AUTHOR and BRD: skipped, but only after their length has
            // moved the cursor along.
            _ => {}
        }
    }

    Some(found)
}

/// The address family a sockaddr declares, if it is long enough to declare one.
fn family_of(sa: &[u8]) -> Option<i32> {
    sa.get(sockaddr::FAMILY).map(|&family| i32::from(family))
}

/// Read an IP address out of a sockaddr.
///
/// `None` for anything that is not an `AF_INET` or `AF_INET6` sockaddr, which
/// notably includes the `sockaddr_dl` macOS puts in the gateway slot of an
/// on-link route. That case must not become a fabricated IP: the correct answer
/// is that the route has no next hop, which is also how Linux reports it.
fn ip_of(sa: &[u8]) -> Option<IpAddr> {
    match family_of(sa)? {
        af::INET => {
            let octets: [u8; 4] = sa
                .get(sockaddr::V4_ADDR..sockaddr::V4_ADDR + 4)?
                .try_into()
                .ok()?;
            Some(IpAddr::V4(Ipv4Addr::from(octets)))
        }
        af::INET6 => {
            let octets: [u8; 16] = sa
                .get(sockaddr::V6_ADDR..sockaddr::V6_ADDR + 16)?
                .try_into()
                .ok()?;
            Some(IpAddr::V6(strip_embedded_scope_id(Ipv6Addr::from(octets))))
        }
        _ => None,
    }
}

/// The interface index carried by a `sockaddr_dl`.
fn sdl_index(sa: &[u8]) -> Option<u32> {
    if family_of(sa)? != af::LINK {
        return None;
    }
    let bytes: [u8; 2] = sa
        .get(sockaddr::DL_INDEX..sockaddr::DL_INDEX + 2)?
        .try_into()
        .ok()?;
    Some(u32::from(u16::from_ne_bytes(bytes)))
}

/// Undo the KAME scope-id embedding in a link-local IPv6 address.
///
/// BSD kernels store the interface index *inside* the address — octets 2 and 3
/// of an `fe80::/10` address — so the link-local gateway on interface 5 arrives
/// as `fe80:5::1`. RFC 4291 requires those octets to be zero, so clearing them
/// repairs a known quirk rather than guessing, and is a no-op otherwise.
///
/// Applied to routes as well as addresses: a v6 default gateway is almost always
/// link-local, and netlink reports the clean form, so leaving the embedding in
/// would make the two platforms disagree about the same gateway.
pub(crate) fn strip_embedded_scope_id(ip: Ipv6Addr) -> Ipv6Addr {
    let mut octets = ip.octets();
    if octets[0] == 0xfe && octets[1] & 0xc0 == 0x80 {
        octets[2] = 0;
        octets[3] = 0;
    }
    Ipv6Addr::from(octets)
}

/// How many leading one-bits a netmask sockaddr carries.
///
/// **The netmask is the sockaddr most likely to be truncated.** The kernel
/// writes only as many bytes as it needs, so `sa_len` is 0 for a default route
/// and can fall well short of `sizeof(sockaddr_in)` otherwise. Absent bytes are
/// simply zero and are treated as zero here.
///
/// The family is taken from the *destination*, never from the mask. A netmask
/// sockaddr's `sa_family` field is frequently left as zero, so trusting it would
/// mean reading a v6 mask at a v4 offset.
///
/// Counting stops at the first byte that is not `0xff`, so a non-contiguous mask
/// yields the length of its contiguous prefix rather than a bit population that
/// no prefix could represent.
fn prefix_len(mask: &[u8], family: Family) -> u8 {
    let (offset, width) = match family {
        Family::V4 => (sockaddr::V4_ADDR, 4),
        Family::V6 => (sockaddr::V6_ADDR, 16),
    };

    let mut bits: u8 = 0;
    for byte in (0..width).map(|i| mask.get(offset + i).copied().unwrap_or(0)) {
        // `leading_ones` on a `u8` is 0..=8, so the conversion cannot fail.
        bits += u8::try_from(byte.leading_ones()).unwrap_or(8);
        if byte != 0xff {
            break;
        }
    }

    bits
}

/// The full prefix length for a family — what a host route's destination has.
const fn host_prefix(family: Family) -> u8 {
    match family {
        Family::V4 => 32,
        Family::V6 => 128,
    }
}

/// The fixed-size head of one routing-socket message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Message {
    /// Total length of this message, so the caller can find the next one.
    pub msglen: usize,
    /// `rtm_version`; anything but [`RTM_VERSION`] is an ABI we do not know.
    pub version: u8,
    /// `rtm_type`.
    pub kind: u8,
    /// The interface index this route exits through.
    pub index: u32,
    /// `RTF_*` flags.
    pub flags: i32,
    /// Which sockaddrs follow the header.
    pub addrs: i32,
}

/// Read the header of the message at the front of `bytes`.
///
/// `None` when the buffer is too short for a header, or when `rtm_msglen` is
/// inconsistent with it: too small would make the caller's walk loop forever,
/// too large would read another message's bytes as sockaddrs.
pub(crate) fn message(bytes: &[u8]) -> Option<Message> {
    let field = |at: usize, len: usize| bytes.get(at..at + len);
    let two =
        |at: usize| -> Option<u16> { Some(u16::from_ne_bytes(field(at, 2)?.try_into().ok()?)) };
    let four =
        |at: usize| -> Option<i32> { Some(i32::from_ne_bytes(field(at, 4)?.try_into().ok()?)) };

    if bytes.len() < header::LEN {
        return None;
    }

    let msglen = usize::from(two(header::MSGLEN)?);
    if msglen < header::LEN || msglen > bytes.len() {
        return None;
    }

    Some(Message {
        msglen,
        version: *bytes.get(header::VERSION)?,
        kind: *bytes.get(header::KIND)?,
        index: u32::from(two(header::INDEX)?),
        flags: four(header::FLAGS)?,
        addrs: four(header::ADDRS)?,
    })
}

/// The bytes of a message that follow its header — its sockaddrs.
pub(crate) fn address_block<'a>(bytes: &'a [u8], message: &Message) -> Option<&'a [u8]> {
    bytes.get(header::LEN..message.msglen)
}

/// Whether a route says anything about reaching another machine.
///
/// The macOS counterpart of the Linux backend dropping everything that is not
/// `RouteType::Unicast`:
///
/// - `RTF_LLINFO` and `RTF_WASCLONED` are ARP and neighbour-discovery cache
///   entries, which outnumber the real routes many times over.
/// - `RTF_MULTICAST` routes are not a path to a peer.
/// - `RTF_BLACKHOLE` and `RTF_REJECT` exist precisely to *not* carry traffic.
///
/// `RTF_IFSCOPE` is deliberately absent. macOS adds a scoped default route per
/// interface when several are up, and those are real: dropping them would leave
/// a usable secondary link looking like it had no default route. Which one wins
/// is settled by the metric, not by hiding one.
pub(crate) fn is_reportable(flags: i32) -> bool {
    let unusable = rtf::LLINFO | rtf::WASCLONED | rtf::MULTICAST | rtf::BLACKHOLE | rtf::REJECT;
    flags & rtf::UP != 0 && flags & unusable == 0
}

/// Everything a routing message says that AutoNet models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteParts {
    /// The destination, or `None` for a default route.
    pub destination: Option<IpNetwork>,
    /// The next hop, or `None` for an on-link route.
    pub gateway: Option<IpAddr>,
    /// The source address the kernel associates with this route.
    pub preferred_source: Option<IpAddr>,
    /// Which family the route belongs to.
    pub family: Family,
    /// The interface index from `RTA_IFP`, when the message carried one.
    ///
    /// Only ever used to cross-check `rtm_index`; the header is authoritative.
    pub interface: Option<u32>,
}

/// Interpret one message's sockaddrs.
///
/// `fallback` is the family the dump was requested for, used when the
/// destination sockaddr is too short to declare one of its own.
///
/// `None` for a message this parser will not describe: a truncated address
/// block, a link-layer destination (an ARP entry rather than a route), or a
/// destination that is missing while the mask claims a non-zero prefix — the
/// last of which would otherwise become a default route that does not exist.
pub(crate) fn route_parts(block: &[u8], message: &Message, fallback: Family) -> Option<RouteParts> {
    let found = slots(block, message.addrs)?;

    // An ARP or ND entry is keyed by a link-layer address rather than by a
    // network. `is_reportable` drops most of them; this catches the rest without
    // letting one through as a route to nowhere.
    if found
        .destination
        .is_some_and(|sa| family_of(sa) == Some(af::LINK))
    {
        return None;
    }

    let destination_ip = found.destination.and_then(ip_of);
    let family = destination_ip.map_or(fallback, |ip| Family::of(&ip));

    // A host route's destination is a single address, and the kernel may not
    // bother sending a mask for it at all.
    let prefix = if message.flags & rtf::HOST != 0 {
        host_prefix(family)
    } else {
        found.netmask.map_or(0, |mask| prefix_len(mask, family))
    };

    // Normalised to `None` for a default route so that macOS and Linux produce
    // byte-identical JSON for the same network. `IpNetwork::is_default` accepts
    // either form, but `autonet routes --json` should not differ by platform.
    let destination = match destination_ip {
        Some(ip) => Some(IpNetwork::new(ip, prefix)).filter(|net| !net.is_default()),
        None if prefix == 0 => None,
        None => return None,
    };

    Some(RouteParts {
        destination,
        // Not filtered against `family`. A v4 route with a v6 gateway would mean
        // the sockaddr walk had gone wrong, and quietly dropping it here would
        // make that bug invisible to the live tests that check for exactly it.
        gateway: found.gateway.and_then(ip_of),
        preferred_source: found.source.and_then(ip_of),
        family,
        interface: found.interface.and_then(sdl_index),
    })
}

/// Walk a whole `NET_RT_DUMP` buffer and build the routes it describes.
///
/// `family` is the family the dump was requested for — macOS dumps one family
/// per call, so it is known, and it stands in when a message's destination
/// sockaddr is too short to declare a family of its own.
///
/// The walk stops at the first message it cannot make sense of: `rtm_msglen`
/// locates the *next* message, so past a bad header nothing is trustworthy.
/// Messages that parse but are not routes are skipped without ending the walk.
///
/// `metrics` supplies the metric per interface index. macOS has no per-route
/// metric — `rmx_hopcount` is not one and reads 0 on Darwin — so it comes from
/// [`crate::servicerank`] instead. A route naming an interface the map does not
/// cover takes [`servicerank::UNRANKED`]: no preference expressed, never a win.
pub(crate) fn routes(buffer: &[u8], family: Family, metrics: &HashMap<u32, u32>) -> Vec<Route> {
    let mut found = Vec::new();
    let mut rest = buffer;

    // `message` refuses a `msglen` below the header size, so `rest` strictly
    // shrinks on every iteration and this cannot spin.
    while let Some(head) = message(rest) {
        let (this, tail) = rest.split_at(head.msglen);
        rest = tail;

        if head.version != RTM_VERSION || !is_reportable(head.flags) {
            continue;
        }

        let Some(block) = address_block(this, &head) else {
            continue;
        };
        let Some(parts) = route_parts(block, &head, family) else {
            continue;
        };

        // `rtm_index` is authoritative — the kernel fills it on every message,
        // `RTA_IFP` is optional — and is the same ifindex namespace the
        // interface walk used, so this is a numeric join with no name matching.
        // `RTA_IFP` is the fallback only for a message naming no interface at
        // all, where zero would mean "interface 0", a device that does not
        // exist. `macos::route`'s ignored `rtm_index_and_rta_ifp_name_the_same_
        // interface` test checks the two against a live table.
        let interface_index = match head.index {
            0 => parts.interface.unwrap_or(0),
            index => index,
        };

        found.push(Route {
            destination: parts.destination,
            gateway: parts.gateway,
            metric: metrics
                .get(&interface_index)
                .copied()
                .unwrap_or(servicerank::UNRANKED),
            family: parts.family,
            preferred_source: parts.preferred_source,
            interface_index,
        });
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Builders for hand-constructed messages
    // -----------------------------------------------------------------------

    fn sockaddr_in(ip: &str) -> Vec<u8> {
        let mut sa = vec![0u8; 16];
        sa[sockaddr::LEN] = 16;
        sa[sockaddr::FAMILY] = 2;
        let octets = ip.parse::<Ipv4Addr>().unwrap().octets();
        sa[sockaddr::V4_ADDR..sockaddr::V4_ADDR + 4].copy_from_slice(&octets);
        sa
    }

    fn sockaddr_in6(ip: &str) -> Vec<u8> {
        let mut sa = vec![0u8; 28];
        sa[sockaddr::LEN] = 28;
        sa[sockaddr::FAMILY] = 30;
        let octets = ip.parse::<Ipv6Addr>().unwrap().octets();
        sa[sockaddr::V6_ADDR..sockaddr::V6_ADDR + 16].copy_from_slice(&octets);
        sa
    }

    fn sockaddr_dl(index: u16) -> Vec<u8> {
        let mut sa = vec![0u8; 20];
        sa[sockaddr::LEN] = 20;
        sa[sockaddr::FAMILY] = 18;
        sa[sockaddr::DL_INDEX..sockaddr::DL_INDEX + 2].copy_from_slice(&index.to_ne_bytes());
        sa
    }

    /// A netmask sockaddr holding only its significant bytes, as the kernel
    /// writes it. `family` is deliberately settable: real ones often say 0.
    fn netmask(significant: &[u8], family: u8) -> Vec<u8> {
        let len = sockaddr::V4_ADDR + significant.len();
        let mut sa = vec![0u8; len];
        sa[sockaddr::LEN] = u8::try_from(len).unwrap();
        sa[sockaddr::FAMILY] = family;
        sa[sockaddr::V4_ADDR..].copy_from_slice(significant);
        sa
    }

    /// The zero-length netmask the kernel writes for a default route.
    fn empty_sockaddr() -> Vec<u8> {
        Vec::new()
    }

    /// Concatenate sockaddrs with the padding the kernel would insert.
    fn block(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in parts {
            out.extend_from_slice(part);
            out.resize(out.len() + roundup(part.len()) - part.len(), 0);
        }
        out
    }

    fn header_bytes(flags: i32, addrs: i32, index: u16, block_len: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; header::LEN];
        let msglen = u16::try_from(header::LEN + block_len).unwrap();
        bytes[header::MSGLEN..header::MSGLEN + 2].copy_from_slice(&msglen.to_ne_bytes());
        bytes[header::VERSION] = RTM_VERSION;
        bytes[header::KIND] = 4; // RTM_GET
        bytes[header::INDEX..header::INDEX + 2].copy_from_slice(&index.to_ne_bytes());
        bytes[header::FLAGS..header::FLAGS + 4].copy_from_slice(&flags.to_ne_bytes());
        bytes[header::ADDRS..header::ADDRS + 4].copy_from_slice(&addrs.to_ne_bytes());
        bytes
    }

    /// A whole message: header plus address block, as `NET_RT_DUMP` returns it.
    fn message_bytes(flags: i32, addrs: i32, index: u16, parts: &[Vec<u8>]) -> Vec<u8> {
        let body = block(parts);
        let mut bytes = header_bytes(flags, addrs, index, body.len());
        bytes.extend_from_slice(&body);
        bytes
    }

    /// Parse a whole message the way the backend does.
    fn parse(bytes: &[u8], fallback: Family) -> Option<RouteParts> {
        let head = message(bytes)?;
        let body = address_block(bytes, &head)?;
        route_parts(body, &head, fallback)
    }

    // -----------------------------------------------------------------------
    // The alignment rule
    // -----------------------------------------------------------------------

    #[test]
    fn darwin_rounds_sockaddrs_up_to_four_bytes_not_eight() {
        // FreeBSD's version of this macro rounds to sizeof(long). Copying it
        // would misplace every sockaddr after the first on a 64-bit Mac.
        assert_eq!(roundup(1), 4);
        assert_eq!(roundup(4), 4);
        assert_eq!(roundup(5), 8);
        assert_eq!(roundup(7), 8);
        assert_eq!(roundup(16), 16);
        assert_eq!(roundup(20), 20);
        assert_eq!(roundup(28), 28);
    }

    #[test]
    fn a_zero_length_sockaddr_still_advances_the_cursor() {
        // The one that hangs the walk if it is treated as zero bytes wide.
        assert_eq!(roundup(0), 4);
    }

    // -----------------------------------------------------------------------
    // Default routes
    // -----------------------------------------------------------------------

    #[test]
    fn a_default_route_is_read_through_its_zero_length_netmask() {
        let bytes = message_bytes(
            rtf::UP | rtf::GATEWAY,
            rta::DST | rta::GATEWAY | rta::NETMASK,
            4,
            &[
                sockaddr_in("0.0.0.0"),
                sockaddr_in("192.168.1.1"),
                empty_sockaddr(),
            ],
        );

        let parts = parse(&bytes, Family::V4).unwrap();
        assert_eq!(parts.destination, None, "0.0.0.0/0 is reported as None");
        assert_eq!(parts.gateway, Some("192.168.1.1".parse().unwrap()));
        assert_eq!(parts.family, Family::V4);
    }

    #[test]
    fn the_ipv6_default_route_is_read_the_same_way() {
        let bytes = message_bytes(
            rtf::UP | rtf::GATEWAY,
            rta::DST | rta::GATEWAY | rta::NETMASK,
            4,
            &[
                sockaddr_in6("::"),
                sockaddr_in6("fe80::1"),
                empty_sockaddr(),
            ],
        );

        let parts = parse(&bytes, Family::V6).unwrap();
        assert_eq!(parts.destination, None);
        assert_eq!(parts.gateway, Some("fe80::1".parse().unwrap()));
        assert_eq!(parts.family, Family::V6);
    }

    #[test]
    fn a_link_local_gateway_loses_its_embedded_scope_id() {
        // What the kernel really writes for a v6 default route on interface 5.
        // Linux reports the clean form, so leaving this in would make the two
        // platforms disagree about the same gateway.
        let bytes = message_bytes(
            rtf::UP | rtf::GATEWAY,
            rta::DST | rta::GATEWAY | rta::NETMASK,
            5,
            &[
                sockaddr_in6("::"),
                sockaddr_in6("fe80:5::1"),
                empty_sockaddr(),
            ],
        );

        let parts = parse(&bytes, Family::V6).unwrap();
        assert_eq!(parts.gateway, Some("fe80::1".parse().unwrap()));
    }

    // -----------------------------------------------------------------------
    // Netmask truncation — the subtle one
    // -----------------------------------------------------------------------

    #[test]
    fn a_truncated_netmask_still_yields_the_right_prefix() {
        // /24 written as seven bytes: the kernel sends only the significant
        // part, so reading this as a `sockaddr_in` would run off the end.
        let bytes = message_bytes(
            rtf::UP,
            rta::DST | rta::NETMASK,
            4,
            &[sockaddr_in("192.168.1.0"), netmask(&[255, 255, 255], 0)],
        );

        let parts = parse(&bytes, Family::V4).unwrap();
        assert_eq!(
            parts.destination,
            Some(IpNetwork::new("192.168.1.0".parse().unwrap(), 24))
        );
    }

    #[test]
    fn a_mask_that_ends_mid_byte_is_counted_bit_by_bit() {
        // 255.255.240.0 is a /20. Counting whole bytes would say 16 or 24.
        let bytes = message_bytes(
            rtf::UP,
            rta::DST | rta::NETMASK,
            4,
            &[sockaddr_in("10.1.16.0"), netmask(&[255, 255, 240], 0)],
        );

        let parts = parse(&bytes, Family::V4).unwrap();
        assert_eq!(parts.destination.unwrap().prefix_len, 20);
    }

    #[test]
    fn the_netmask_declares_a_family_that_is_never_trusted() {
        // Real netmask sockaddrs frequently carry sa_family 0, and sometimes
        // something else entirely. Reading a v4 mask at the v6 offset would
        // silently produce /0 — a default route out of a LAN route.
        for claimed in [0u8, 30, 18, 200] {
            let bytes = message_bytes(
                rtf::UP,
                rta::DST | rta::NETMASK,
                4,
                &[
                    sockaddr_in("172.16.0.0"),
                    netmask(&[255, 255, 0, 0], claimed),
                ],
            );

            let parts = parse(&bytes, Family::V4).unwrap();
            assert_eq!(
                parts.destination.unwrap().prefix_len,
                16,
                "netmask claiming family {claimed}"
            );
        }
    }

    #[test]
    fn a_host_route_gets_a_full_length_prefix() {
        let bytes = message_bytes(
            rtf::UP | rtf::HOST,
            rta::DST,
            4,
            &[sockaddr_in("192.168.1.50")],
        );

        let parts = parse(&bytes, Family::V4).unwrap();
        assert_eq!(parts.destination.unwrap().prefix_len, 32);
    }

    // -----------------------------------------------------------------------
    // Positional parsing
    // -----------------------------------------------------------------------

    #[test]
    fn an_on_link_route_reports_no_gateway_rather_than_a_fabricated_one() {
        // macOS puts a sockaddr_dl in the gateway slot for a directly attached
        // network. Reading it as an address would invent an IP out of a
        // link-layer record; Linux simply omits RTA_GATEWAY for this case.
        let bytes = message_bytes(
            rtf::UP,
            rta::DST | rta::GATEWAY | rta::NETMASK,
            7,
            &[
                sockaddr_in("192.168.1.0"),
                sockaddr_dl(7),
                netmask(&[255, 255, 255], 0),
            ],
        );

        let parts = parse(&bytes, Family::V4).unwrap();
        assert_eq!(parts.gateway, None);
        assert_eq!(parts.destination.unwrap().prefix_len, 24);
    }

    #[test]
    fn a_gap_in_the_bitmask_does_not_shift_the_later_sockaddrs() {
        // DST and IFP with no GATEWAY between them. A walk that assumed a fixed
        // order would read the sockaddr_dl as the gateway.
        let bytes = message_bytes(
            rtf::UP,
            rta::DST | rta::IFP,
            9,
            &[sockaddr_in("10.0.0.0"), sockaddr_dl(9)],
        );

        let parts = parse(&bytes, Family::V4).unwrap();
        assert_eq!(parts.gateway, None);
        assert_eq!(parts.interface, Some(9));
    }

    #[test]
    fn slots_that_autonet_ignores_still_move_the_cursor() {
        // GENMASK and AUTHOR are not read, but their bytes sit between the ones
        // that are. Skipping them without consuming them would misread IFA.
        let bytes = message_bytes(
            rtf::UP,
            rta::DST | rta::GENMASK | rta::IFP | rta::IFA | rta::AUTHOR,
            3,
            &[
                sockaddr_in("10.0.0.0"),
                sockaddr_in("255.0.0.0"), // GENMASK, ignored
                sockaddr_dl(3),           // IFP
                sockaddr_in("10.0.0.7"),  // IFA
                sockaddr_in("10.0.0.1"),  // AUTHOR, ignored
            ],
        );

        let parts = parse(&bytes, Family::V4).unwrap();
        assert_eq!(parts.interface, Some(3));
        assert_eq!(parts.preferred_source, Some("10.0.0.7".parse().unwrap()));
    }

    #[test]
    fn sockaddrs_of_odd_length_are_padded_to_the_next_boundary() {
        // A seven-byte netmask occupies eight. If the walk advanced by seven,
        // the interface sockaddr would start one byte early and report nonsense.
        let bytes = message_bytes(
            rtf::UP,
            rta::DST | rta::NETMASK | rta::IFP,
            11,
            &[
                sockaddr_in("192.168.4.0"),
                netmask(&[255, 255, 255], 0), // seven bytes, padded to eight
                sockaddr_dl(11),
            ],
        );

        let parts = parse(&bytes, Family::V4).unwrap();
        assert_eq!(parts.interface, Some(11));
        assert_eq!(parts.destination.unwrap().prefix_len, 24);
    }

    // -----------------------------------------------------------------------
    // Refusals
    // -----------------------------------------------------------------------

    #[test]
    fn a_block_that_ends_mid_sockaddr_is_refused_not_guessed_at() {
        let full = block(&[sockaddr_in("10.0.0.0"), sockaddr_in("10.0.0.1")]);
        let head = message(&header_bytes(rtf::UP, rta::DST | rta::GATEWAY, 4, 0)).unwrap();

        // Every truncation of the second sockaddr must be refused rather than
        // read as a shorter address.
        for cut in 17..full.len() {
            assert_eq!(
                route_parts(&full[..cut], &head, Family::V4).map(|p| p.gateway),
                None,
                "block truncated to {cut} bytes"
            );
        }
    }

    #[test]
    fn a_message_shorter_than_its_header_is_refused() {
        let bytes = vec![0u8; header::LEN - 1];
        assert_eq!(message(&bytes), None);
    }

    #[test]
    fn a_message_length_that_overruns_the_buffer_is_refused() {
        // Otherwise the walk would read the next message's bytes as sockaddrs.
        let mut bytes = header_bytes(rtf::UP, rta::DST, 4, 0);
        bytes[header::MSGLEN..header::MSGLEN + 2].copy_from_slice(&9999u16.to_ne_bytes());
        assert_eq!(message(&bytes), None);
    }

    #[test]
    fn a_message_length_smaller_than_a_header_is_refused() {
        // A `msglen` of zero would leave the caller's walk stuck on this
        // message forever.
        let mut bytes = header_bytes(rtf::UP, rta::DST, 4, 0);
        bytes[header::MSGLEN..header::MSGLEN + 2].copy_from_slice(&0u16.to_ne_bytes());
        assert_eq!(message(&bytes), None);
    }

    #[test]
    fn an_arp_entry_keyed_by_a_link_address_is_not_a_route() {
        let bytes = message_bytes(
            rtf::UP | rtf::HOST,
            rta::DST | rta::GATEWAY,
            4,
            &[sockaddr_dl(4), sockaddr_dl(4)],
        );
        assert_eq!(parse(&bytes, Family::V4), None);
    }

    // -----------------------------------------------------------------------
    // Filtering
    // -----------------------------------------------------------------------

    #[test]
    fn cache_entries_and_dead_ends_are_not_reported_as_routes() {
        for flags in [
            rtf::UP | rtf::LLINFO,
            rtf::UP | rtf::WASCLONED,
            rtf::UP | rtf::MULTICAST,
            rtf::UP | rtf::BLACKHOLE,
            rtf::UP | rtf::REJECT,
            rtf::GATEWAY, // not up
        ] {
            assert!(!is_reportable(flags), "flags {flags:#x}");
        }
    }

    #[test]
    fn both_the_primary_and_the_scoped_default_route_survive_the_filter() {
        // With Wi-Fi and Ethernet both up, macOS keeps one unscoped default
        // route and a scoped one per secondary interface. Dropping the scoped
        // one would leave a usable link looking like it had no default route.
        const RTF_IFSCOPE: i32 = 0x0100_0000;
        assert!(is_reportable(rtf::UP | rtf::GATEWAY));
        assert!(is_reportable(rtf::UP | rtf::GATEWAY | RTF_IFSCOPE));
    }

    #[test]
    fn a_plain_static_route_is_reported() {
        assert!(is_reportable(rtf::UP | rtf::GATEWAY | rtf::HOST));
    }

    // -----------------------------------------------------------------------
    // Header fields
    // -----------------------------------------------------------------------

    #[test]
    fn the_header_reports_the_interface_and_length_the_kernel_wrote() {
        let parts = [sockaddr_in("0.0.0.0"), sockaddr_in("192.168.1.1")];
        let bytes = message_bytes(rtf::UP | rtf::GATEWAY, rta::DST | rta::GATEWAY, 42, &parts);

        let head = message(&bytes).unwrap();
        assert_eq!(head.index, 42);
        assert_eq!(head.version, RTM_VERSION);
        assert_eq!(head.msglen, bytes.len());
        assert_eq!(head.flags, rtf::UP | rtf::GATEWAY);
        assert_eq!(address_block(&bytes, &head).unwrap().len(), 32);
    }

    // -----------------------------------------------------------------------
    // Walking a whole dump
    // -----------------------------------------------------------------------

    /// A buffer shaped like what `NET_RT_DUMP` returns: a default route, an ARP
    /// cache entry, and an on-link LAN route, back to back.
    fn a_dump() -> Vec<u8> {
        let mut buffer = message_bytes(
            rtf::UP | rtf::GATEWAY,
            rta::DST | rta::GATEWAY | rta::NETMASK,
            4,
            &[
                sockaddr_in("0.0.0.0"),
                sockaddr_in("192.168.1.1"),
                empty_sockaddr(),
            ],
        );
        buffer.extend(message_bytes(
            rtf::UP | rtf::HOST | rtf::LLINFO | rtf::WASCLONED,
            rta::DST | rta::GATEWAY,
            4,
            &[sockaddr_in("192.168.1.23"), sockaddr_dl(4)],
        ));
        buffer.extend(message_bytes(
            rtf::UP,
            rta::DST | rta::GATEWAY | rta::NETMASK,
            4,
            &[
                sockaddr_in("192.168.1.0"),
                sockaddr_dl(4),
                netmask(&[255, 255, 255], 0),
            ],
        ));
        buffer
    }

    #[test]
    fn a_dump_yields_the_routes_and_drops_the_arp_cache() {
        let found = routes(&a_dump(), Family::V4, &no_metrics());

        assert_eq!(found.len(), 2, "the ARP entry is not a route");
        assert!(found[0].is_default());
        assert_eq!(found[0].gateway, Some("192.168.1.1".parse().unwrap()));
        assert_eq!(found[0].interface_index, 4);
        assert_eq!(
            found[1].destination,
            Some(IpNetwork::new("192.168.1.0".parse().unwrap(), 24))
        );
        assert_eq!(found[1].gateway, None, "on-link, so no next hop");
    }

    #[test]
    fn a_dump_that_is_cut_short_yields_the_messages_that_were_complete() {
        // The kernel should never hand back a partial message, but if the
        // buffer is short the walk must stop rather than read the remainder as
        // sockaddrs — `rtm_msglen` is what locates the next message, so past a
        // bad header there is nothing trustworthy left.
        let full = a_dump();
        let found = routes(&full[..full.len() - 8], Family::V4, &no_metrics());
        assert_eq!(found.len(), 1);
        assert!(found[0].is_default());
    }

    #[test]
    fn a_message_from_an_abi_this_build_does_not_know_is_skipped() {
        let mut buffer = a_dump();
        buffer[header::VERSION] = RTM_VERSION + 1;

        // Skipped, but its length still carries the walk to the next message,
        // so the LAN route after it survives.
        let found = routes(&buffer, Family::V4, &no_metrics());
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].destination,
            Some(IpNetwork::new("192.168.1.0".parse().unwrap(), 24))
        );
    }

    #[test]
    fn a_message_naming_no_interface_falls_back_to_its_ifp_sockaddr() {
        // `rtm_index` is authoritative, but zero is not an interface — it would
        // join to nothing. `RTA_IFP` carries the same ifindex namespace.
        let bytes = message_bytes(
            rtf::UP | rtf::GATEWAY,
            rta::DST | rta::GATEWAY | rta::IFP,
            0,
            &[
                sockaddr_in("0.0.0.0"),
                sockaddr_in("10.0.0.1"),
                sockaddr_dl(12),
            ],
        );

        assert_eq!(
            routes(&bytes, Family::V4, &no_metrics())[0].interface_index,
            12
        );
    }

    #[test]
    fn an_empty_dump_is_no_routes_rather_than_an_error() {
        // Airplane mode: the sysctl succeeds and returns nothing.
        assert!(routes(&[], Family::V4, &no_metrics()).is_empty());
    }

    #[test]
    fn a_routes_metric_comes_from_the_map_and_not_from_the_kernel() {
        // Nothing in a routing message carries a metric on Darwin, so it is
        // supplied per interface index. Both routes in the dump are on
        // interface 4 and must pick up its value.
        let metrics: HashMap<u32, u32> = [(4, 300)].into_iter().collect();
        let found = routes(&a_dump(), Family::V4, &metrics);

        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|route| route.metric == 300));
    }

    #[test]
    fn a_route_on_an_interface_the_map_does_not_cover_is_unranked() {
        // A route naming an unknown interface must take the worst metric, not
        // the best: defaulting to zero would let a route AutoNet cannot resolve
        // to an interface outrank every real one.
        let found = routes(&a_dump(), Family::V4, &no_metrics());
        assert!(found
            .iter()
            .all(|route| route.metric == servicerank::UNRANKED));
    }

    /// A machine whose service order told us nothing, for the tests that are
    /// about the parse rather than about ranking.
    fn no_metrics() -> HashMap<u32, u32> {
        HashMap::new()
    }

    #[test]
    fn the_scope_strip_leaves_ordinary_addresses_alone() {
        for address in ["fe80::1c4a:b2ff:fe3d:9e10", "2401:db8:1234::1", "::1"] {
            let ip: Ipv6Addr = address.parse().unwrap();
            assert_eq!(strip_embedded_scope_id(ip), ip, "{address}");
        }
    }
}
