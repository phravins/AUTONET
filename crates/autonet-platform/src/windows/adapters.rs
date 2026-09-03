//! Windows adapter and address enumeration.

use std::collections::HashMap;

use autonet_core::model::{Address, Interface, InterfaceFlags, InterfaceKind};
use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, ERROR_SUCCESS};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_NO_MULTICAST, IP_ADAPTER_UNICAST_ADDRESS_LH,
};
use windows_sys::Win32::NetworkManagement::Ndis::{
    IfOperStatusDormant, IfOperStatusDown, IfOperStatusLowerLayerDown, IfOperStatusNotPresent,
    IfOperStatusTesting, IfOperStatusUnknown, IfOperStatusUp,
};
use windows_sys::Win32::Networking::WinSock::{
    IpDadStateDuplicate, IpSuffixOriginRandom, AF_INET, AF_INET6, AF_UNSPEC,
};

use super::iftable::{self, Row};
use crate::hwaddr::format_mac;
use crate::winparse::{self, af, oper};
use crate::wintype::{self, Evidence};
use crate::PlatformError;

// Keep Linux-testable constants aligned with windows-sys.
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

/// Request unicast data only.
const FLAGS: u32 = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;

/// Initial buffer size recommended by Microsoft.
const INITIAL_BYTES: usize = 15_000;

/// Maximum buffer-growth attempts.
const MAX_ATTEMPTS: usize = 3;

/// Maximum UTF-16 units scanned in a friendly name.
const MAX_NAME_UNITS: usize = 512;

/// Return adapters and their addresses.
pub(crate) fn interfaces() -> Result<Vec<Interface>, PlatformError> {
    let Some(buffer) = adapter_buffer()? else {
        return Ok(Vec::new());
    };

    let rows = iftable::by_luid()?;

    Ok(walk(
        buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
        &rows,
    ))
}

/// Call `GetAdaptersAddresses` with an aligned, growable buffer.
fn adapter_buffer() -> Result<Option<Vec<u64>>, PlatformError> {
    let mut bytes = INITIAL_BYTES;

    for _ in 0..MAX_ATTEMPTS {
        let words = bytes.div_ceil(size_of::<u64>());
        let mut buffer: Vec<u64> = vec![0; words];
        let mut size = u32::try_from(words * size_of::<u64>()).unwrap_or(u32::MAX);

        // SAFETY: The aligned writable buffer matches the supplied byte length.
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
            // Preserve a non-shrinking retry size.
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

/// Walk and stably sort the adapter list by name.
fn walk(head: *const IP_ADAPTER_ADDRESSES_LH, rows: &HashMap<u64, Row>) -> Vec<Interface> {
    let mut interfaces = Vec::new();
    let mut current = head;

    while !current.is_null() {
        // SAFETY: `current` points into the API-owned result buffer.
        let adapter = unsafe { &*current };

        if let Some(interface) = interface_from(adapter, rows) {
            interfaces.push(interface);
        }

        current = adapter.Next;
    }

    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    interfaces
}

/// Convert one adapter into the shared model.
fn interface_from(
    adapter: &IP_ADAPTER_ADDRESSES_LH,
    rows: &HashMap<u64, Row>,
) -> Option<Interface> {
    let name = friendly_name(adapter.FriendlyName);
    if name.is_empty() {
        return None;
    }

    // SAFETY: The union's `u64` representation is always valid.
    let luid = unsafe { adapter.Luid.Value };
    let row = rows.get(&luid);

    let kind = wintype::classify(Evidence {
        if_type: adapter.IfType,
        tunnel_type: adapter.TunnelType,
        ndis: row.map(|row| row.ndis),
    });
    let is_loopback = kind == InterfaceKind::Loopback;

    // SAFETY: The union exposes the adapter's IPv4 index.
    let ipv4_index = unsafe { adapter.Anonymous1.Anonymous.IfIndex };
    // SAFETY: The union exposes the adapter flags.
    let flags = unsafe { adapter.Anonymous2.Flags };

    Some(Interface {
        name,
        // Prefer the IPv4 index when present.
        index: if ipv4_index == 0 {
            adapter.Ipv6IfIndex
        } else {
            ipv4_index
        },
        kind,
        state: winparse::interface_state(adapter.OperStatus),
        flags: InterfaceFlags {
            // Fall back to operational status if the table join missed.
            up: row.map_or(adapter.OperStatus == oper::UP, |row| row.admin_up),
            running: adapter.OperStatus == oper::UP,
            loopback: is_loopback,
            broadcast: row.is_some_and(|row| wintype::is_broadcast(row.access_type)),
            point_to_point: row.is_some_and(|row| wintype::is_point_to_point(row.access_type)),
            multicast: flags & IP_ADAPTER_NO_MULTICAST == 0,
        },
        mac: hardware_address(adapter),
        mtu: mtu_of(adapter),
        addresses: addresses_of(adapter),
    })
}

/// Convert a Windows friendly name to UTF-8.
fn friendly_name(name: PWSTR) -> String {
    if name.is_null() {
        return String::new();
    }

    let mut len = 0;
    // SAFETY: The bounded scan reads a Windows-owned UTF-16 string.
    while len < MAX_NAME_UNITS && unsafe { *name.add(len) } != 0 {
        len += 1;
    }

    // SAFETY: The preceding scan validated these units.
    let units = unsafe { std::slice::from_raw_parts(name, len) };
    winparse::wide_to_string(units)
}

/// Return a valid hardware address, if present.
fn hardware_address(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Option<String> {
    let len = usize::try_from(adapter.PhysicalAddressLength)
        .unwrap_or(0)
        .min(adapter.PhysicalAddress.len());

    format_mac(&adapter.PhysicalAddress[..len])
}

/// Return a valid MTU, if present.
fn mtu_of(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Option<u32> {
    match adapter.Mtu {
        0 | u32::MAX => None,
        mtu => Some(mtu),
    }
}

/// Return an adapter's unicast addresses.
fn addresses_of(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Vec<Address> {
    let mut addresses = Vec::new();
    let mut current: *const IP_ADAPTER_UNICAST_ADDRESS_LH = adapter.FirstUnicastAddress;

    while !current.is_null() {
        // SAFETY: `current` points into the API-owned result buffer.
        let unicast = unsafe { &*current };

        if let Some(address) = address_from(unicast) {
            addresses.push(address);
        }

        current = unicast.Next;
    }

    addresses
}

/// Convert one unicast address into the shared model.
fn address_from(unicast: &IP_ADAPTER_UNICAST_ADDRESS_LH) -> Option<Address> {
    // Duplicate-address detection prevents publishing an unusable address.
    if unicast.DadState == IpDadStateDuplicate {
        return None;
    }

    let socket = &unicast.Address;
    if socket.lpSockaddr.is_null() {
        return None;
    }

    let len = usize::try_from(socket.iSockaddrLength).ok()?;
    // SAFETY: Windows supplied this socket address and length.
    let bytes = unsafe { std::slice::from_raw_parts(socket.lpSockaddr.cast::<u8>(), len) };
    let ip = winparse::sockaddr_ip(bytes)?;

    Some(Address {
        is_temporary: unicast.SuffixOrigin == IpSuffixOriginRandom,
        ..Address::new(ip, winparse::prefix_len(unicast.OnLinkPrefixLength, &ip))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_address_families_are_the_ones_we_never_read() {
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
