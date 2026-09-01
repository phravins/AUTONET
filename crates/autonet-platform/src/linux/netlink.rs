//! Three netlink dumps — links, addresses, routes — joined into one snapshot.
//!
//! The kernel models these as separate tables keyed by interface index, and
//! this module reproduces that join rather than flattening it. Routes in
//! particular are kept as first-class records: "does this interface own a
//! default route?" is the single strongest evidence that an interface can
//! actually reach anything, and it is invisible to any address-only API such
//! as `getifaddrs`.
//!
//! Everything here is translation. No filtering, no preference, no scoring —
//! judging which address a peer should use is `autonet-core`'s job, and mixing
//! the two would put policy behind an `#[cfg(target_os)]` where it could not be
//! tested from fixtures.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use autonet_core::classify::classify_interface;
use autonet_core::model::{
    Address, Family, Interface, InterfaceFlags, InterfaceState, IpNetwork, NetworkState, Route,
};
use futures_util::TryStreamExt;
use netlink_packet_route::address::{
    AddressAttribute, AddressFlags, AddressHeaderFlags, AddressMessage,
};
use netlink_packet_route::link::{LinkAttribute, LinkFlags, LinkInfo, LinkMessage, State};
use netlink_packet_route::route::{RouteAddress, RouteAttribute, RouteMessage, RouteType};
use netlink_packet_route::AddressFamily;
use rtnetlink::{Handle, RouteMessageBuilder};

use crate::hwaddr::format_mac;
use crate::linux::sysfs;
use crate::PlatformError;

/// Capture the machine's network configuration over netlink.
pub(crate) async fn snapshot() -> Result<NetworkState, PlatformError> {
    let (connection, handle, _messages) = rtnetlink::new_connection()
        .map_err(|e| PlatformError::query("open a netlink socket", e))?;

    // The connection future drives the socket; the handle is useless until it
    // is running. It is aborted below rather than left to leak, because the
    // daemon will call `snapshot` repeatedly over a long-lived process.
    let pump = tokio::spawn(connection);
    let result = collect(&handle).await;
    drop(handle);
    pump.abort();

    result
}

async fn collect(handle: &Handle) -> Result<NetworkState, PlatformError> {
    // Links first: addresses and routes both reference interfaces by index, so
    // the index -> interface map has to exist before either can be attached.
    let mut interfaces = dump_links(handle).await?;
    attach_addresses(handle, &mut interfaces).await?;
    let routes = dump_routes(handle).await?;

    Ok(NetworkState::new(interfaces.into_values().collect(), routes).captured_now())
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

/// Dump every link, keyed by interface index.
///
/// A `BTreeMap` rather than a `HashMap`: it keeps interfaces in index order for
/// free, which makes `autonet interfaces` output stable between runs and makes
/// the selection engine's index-ascending tie-break deterministic in practice
/// as well as in principle.
async fn dump_links(handle: &Handle) -> Result<BTreeMap<u32, Interface>, PlatformError> {
    let mut stream = handle.link().get().execute();
    let mut interfaces = BTreeMap::new();

    while let Some(message) = stream
        .try_next()
        .await
        .map_err(|e| PlatformError::query("list network interfaces", e))?
    {
        if let Some(interface) = interface_from(&message) {
            interfaces.insert(interface.index, interface);
        }
    }

    Ok(interfaces)
}

fn interface_from(message: &LinkMessage) -> Option<Interface> {
    let index = message.header.index;
    let link_flags = message.header.flags;

    let mut name = None;
    let mut oper_state = None;
    let mut mac = None;
    let mut mtu = None;
    let mut link_kind = None;

    for attribute in &message.attributes {
        match attribute {
            LinkAttribute::IfName(value) => name = Some(value.clone()),
            LinkAttribute::OperState(value) => oper_state = Some(*value),
            LinkAttribute::Address(bytes) => mac = format_mac(bytes),
            LinkAttribute::Mtu(value) => mtu = Some(*value),
            LinkAttribute::LinkInfo(infos) => {
                link_kind = infos.iter().find_map(|info| match info {
                    LinkInfo::Kind(kind) => Some(kind.to_string()),
                    _ => None,
                });
            }
            _ => {}
        }
    }

    // A link with no name is not something we can report on or match a config
    // rule against, so it is dropped rather than given a synthetic name.
    let name = name?;

    let is_loopback = link_flags.contains(LinkFlags::Loopback);
    let kind = classify_interface(
        &name,
        link_kind.as_deref(),
        is_loopback,
        sysfs::is_wireless(&name),
    );

    Some(Interface {
        name,
        index,
        kind,
        state: interface_state(oper_state, link_flags),
        flags: InterfaceFlags {
            up: link_flags.contains(LinkFlags::Up),
            running: link_flags.contains(LinkFlags::Running),
            loopback: is_loopback,
            broadcast: link_flags.contains(LinkFlags::Broadcast),
            point_to_point: link_flags.contains(LinkFlags::Pointopoint),
            multicast: link_flags.contains(LinkFlags::Multicast),
        },
        mac,
        mtu,
        addresses: Vec::new(),
    })
}

/// Map the kernel's `IF_OPER_*` value onto AutoNet's coarser state.
///
/// `Unknown` is preserved rather than being resolved to up or down, because it
/// is what tunnel devices report while working perfectly: WireGuard, `tun`, and
/// loopback never set an operational state at all. The selection engine treats
/// only `Down` as disqualifying for exactly this reason — guessing here would
/// make `autonet ip --interface wg0` fail on a healthy tunnel.
fn interface_state(oper_state: Option<State>, flags: LinkFlags) -> InterfaceState {
    match oper_state {
        Some(State::Up) => InterfaceState::Up,
        Some(State::Down | State::NotPresent | State::LowerLayerDown) => InterfaceState::Down,
        Some(State::Dormant | State::Testing) => InterfaceState::Dormant,
        // No `IFLA_OPERSTATE` at all: fall back to the administrative flag,
        // which at least distinguishes a disabled link from an unknown one.
        None if !flags.contains(LinkFlags::Up) => InterfaceState::Down,
        _ => InterfaceState::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

async fn attach_addresses(
    handle: &Handle,
    interfaces: &mut BTreeMap<u32, Interface>,
) -> Result<(), PlatformError> {
    let mut stream = handle.address().get().execute();

    while let Some(message) = stream
        .try_next()
        .await
        .map_err(|e| PlatformError::query("list IP addresses", e))?
    {
        // An address whose interface did not appear in the link dump belongs to
        // a device that vanished between the two queries. Dropping it is
        // correct: reporting an address with no interface would hand the caller
        // something it cannot name or bind to.
        if let Some(interface) = interfaces.get_mut(&message.header.index) {
            if let Some(address) = address_from(&message) {
                interface.addresses.push(address);
            }
        }
    }

    Ok(())
}

fn address_from(message: &AddressMessage) -> Option<Address> {
    let mut local = None;
    let mut address = None;
    let mut flags = None;

    for attribute in &message.attributes {
        match attribute {
            AddressAttribute::Local(ip) => local = Some(*ip),
            AddressAttribute::Address(ip) => address = Some(*ip),
            AddressAttribute::Flags(value) => flags = Some(*value),
            _ => {}
        }
    }

    // `IFA_LOCAL` is this machine's address; `IFA_ADDRESS` is the *peer* on a
    // point-to-point link. Reading the wrong one hands back the far end of a
    // VPN tunnel — an address that belongs to somebody else's machine.
    let ip = local.or(address)?;

    // `IFA_FLAGS` is the authoritative 32-bit field; the header carries only
    // its low byte, and is used when the kernel omits the attribute. Both bits
    // read here happen to live in that low byte, so the fallback is exact.
    let header_flags = message.header.flags;
    let has = |wide: AddressFlags, narrow: AddressHeaderFlags| match flags {
        Some(flags) => flags.contains(wide),
        None => header_flags.contains(narrow),
    };

    // An address that failed duplicate-address detection is in use by another
    // host on the segment. Handing it out would send traffic to the wrong
    // machine, so it is dropped rather than merely deprioritised.
    if has(AddressFlags::Dadfailed, AddressHeaderFlags::Dadfailed) {
        return None;
    }

    // `IFA_F_TEMPORARY` and `IFA_F_SECONDARY` are the same bit. On IPv6 it
    // means a privacy-extension address, which is rotated out from under
    // whoever you gave it to; on IPv4 it just means a second address on the
    // interface, which is perfectly stable. Hence the family check.
    let is_temporary = ip.is_ipv6() && has(AddressFlags::Secondary, AddressHeaderFlags::Secondary);

    let mut result = Address::new(ip, message.header.prefix_len);
    result.is_temporary = is_temporary;
    Some(result)
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Dump the IPv4 and IPv6 routing tables.
///
/// Two dumps, because netlink's route dump is per-family — asking for
/// `AF_UNSPEC` returns IPv4 only on many kernels.
async fn dump_routes(handle: &Handle) -> Result<Vec<Route>, PlatformError> {
    let mut routes = Vec::new();

    let v4 = RouteMessageBuilder::<Ipv4Addr>::new().build();
    collect_routes(handle, v4, &mut routes).await?;

    let v6 = RouteMessageBuilder::<Ipv6Addr>::new().build();
    collect_routes(handle, v6, &mut routes).await?;

    Ok(routes)
}

async fn collect_routes(
    handle: &Handle,
    request: RouteMessage,
    routes: &mut Vec<Route>,
) -> Result<(), PlatformError> {
    let mut stream = handle.route().get(request).execute();

    while let Some(message) = stream
        .try_next()
        .await
        .map_err(|e| PlatformError::query("list routes", e))?
    {
        routes.extend(route_from(&message));
    }

    Ok(())
}

fn route_from(message: &RouteMessage) -> Option<Route> {
    // The local table is full of `local`, `broadcast` and `multicast` entries
    // that describe how the kernel handles traffic *to itself*. Only unicast
    // routes say anything about reaching another machine.
    if message.header.kind != RouteType::Unicast {
        return None;
    }

    let family = match message.header.address_family {
        AddressFamily::Inet => Family::V4,
        AddressFamily::Inet6 => Family::V6,
        // MPLS and bridge routes, which AutoNet has nothing to say about.
        _ => return None,
    };

    let mut destination = None;
    let mut gateway = None;
    let mut preferred_source = None;
    let mut interface_index = None;
    let mut metric = 0;

    for attribute in &message.attributes {
        match attribute {
            RouteAttribute::Destination(value) => destination = route_address(value),
            RouteAttribute::Gateway(value) => gateway = route_address(value),
            RouteAttribute::PrefSource(value) => preferred_source = route_address(value),
            RouteAttribute::Oif(index) => interface_index = Some(*index),
            RouteAttribute::Priority(value) => metric = *value,
            // A multipath route carries its interfaces in nexthop records
            // instead of `RTA_OIF`. Taking the first is enough for AutoNet's
            // purposes: the question being asked is only ever "does this
            // interface have a route out", not which hop a packet takes.
            RouteAttribute::MultiPath(hops) if interface_index.is_none() => {
                interface_index = hops.first().map(|hop| hop.interface_index);
            }
            _ => {}
        }
    }

    let interface_index = interface_index?;

    // A missing `RTA_DST` means a default route — but only when the prefix
    // length agrees. Anything else is a route we cannot describe, and inventing
    // `None` for it would make `is_default()` claim a default route that does
    // not exist.
    let destination = match destination {
        Some(ip) => Some(IpNetwork::new(ip, message.header.destination_prefix_length)),
        None if message.header.destination_prefix_length == 0 => None,
        None => return None,
    };

    Some(Route {
        destination,
        gateway,
        interface_index,
        metric,
        family,
        preferred_source,
    })
}

fn route_address(value: &RouteAddress) -> Option<IpAddr> {
    match value {
        RouteAddress::Inet(ip) => Some(IpAddr::V4(*ip)),
        RouteAddress::Inet6(ip) => Some(IpAddr::V6(*ip)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnels_reporting_no_operstate_stay_usable() {
        // WireGuard reports IF_OPER_UNKNOWN while carrying traffic. Resolving
        // that to `Down` would disqualify a working tunnel.
        assert_eq!(
            interface_state(Some(State::Unknown), LinkFlags::Up | LinkFlags::Running),
            InterfaceState::Unknown
        );
    }

    #[test]
    fn a_link_with_no_operstate_falls_back_to_the_admin_flag() {
        assert_eq!(
            interface_state(None, LinkFlags::empty()),
            InterfaceState::Down
        );
        assert_eq!(
            interface_state(None, LinkFlags::Up),
            InterfaceState::Unknown
        );
    }

    #[test]
    fn lower_layer_down_is_down() {
        // An Ethernet port with no cable in it.
        assert_eq!(
            interface_state(Some(State::LowerLayerDown), LinkFlags::Up),
            InterfaceState::Down
        );
    }
}
