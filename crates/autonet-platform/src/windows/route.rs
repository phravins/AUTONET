//! Windows routing-table enumeration.

use std::collections::HashMap;

use autonet_core::model::{Family, Route};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIpForwardTable2, MIB_IPFORWARD_ROW2, MIB_IPFORWARD_TABLE2,
};
use windows_sys::Win32::Networking::WinSock::{AF_UNSPEC, SOCKADDR_INET};

use super::adapters::AdapterLink;
use crate::{winparse, winroute, PlatformError};

/// Return every route Windows reports, in both families.
///
/// One call covers IPv4 and IPv6, where macOS needs a dump per family. `links`
/// comes from the adapter walk and carries the index each interface was given;
/// see [`winroute::route_index`] for why the row's own index is only a fallback.
pub(crate) fn dump_routes(links: &HashMap<u64, AdapterLink>) -> Result<Vec<Route>, PlatformError> {
    let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();

    // SAFETY: `table` is a valid out-parameter.
    let result = unsafe { GetIpForwardTable2(AF_UNSPEC, &raw mut table) };

    if result != ERROR_SUCCESS {
        return Err(PlatformError::query(
            "enumerate network routes",
            format!("Windows error {result}"),
        ));
    }

    let table = Table(table);
    Ok(table
        .rows()
        .iter()
        .filter_map(|row| route_from(row, links))
        .collect())
}

/// Owns the `GetIpForwardTable2` allocation.
struct Table(*mut MIB_IPFORWARD_TABLE2);

impl Table {
    /// Return rows reported by Windows.
    fn rows(&self) -> &[MIB_IPFORWARD_ROW2] {
        if self.0.is_null() {
            return &[];
        }

        // SAFETY: A successful call returns an initialized table.
        let table = unsafe { &*self.0 };
        let Ok(len) = usize::try_from(table.NumEntries) else {
            return &[];
        };

        // SAFETY: `Table` is a flexible array of `NumEntries` rows.
        unsafe { std::slice::from_raw_parts(table.Table.as_ptr(), len) }
    }
}

impl Drop for Table {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: This allocation came from `GetIpForwardTable2`.
            unsafe { FreeMibTable(self.0.cast::<std::ffi::c_void>()) };
        }
    }
}

/// Convert one row into the shared model, or skip it.
fn route_from(row: &MIB_IPFORWARD_ROW2, links: &HashMap<u64, AdapterLink>) -> Option<Route> {
    let prefix = winparse::sockaddr_ip(inet_bytes(&row.DestinationPrefix.Prefix))?;

    if !winroute::is_reportable(row.Loopback, prefix) {
        return None;
    }

    // From the destination itself, not from which table the row arrived in: one
    // call returns both families.
    let family = Family::of(&prefix);

    // SAFETY: The union's `u64` representation is always valid.
    let luid = unsafe { row.InterfaceLuid.Value };
    let link = links.get(&luid);

    Some(Route {
        destination: winroute::destination(prefix, row.DestinationPrefix.PrefixLength),
        gateway: winparse::sockaddr_ip(inet_bytes(&row.NextHop)).and_then(winroute::gateway),
        interface_index: winroute::route_index(row.InterfaceIndex, link.map(|link| link.index)),
        metric: winroute::effective_metric(row.Metric, link.map_or(0, |link| link.metric(family))),
        family,
        // `MIB_IPFORWARD_ROW2` has no equivalent of Linux's `RTA_PREFSRC` or
        // macOS's `RTA_IFA`. Left absent rather than invented; nothing in
        // `select` reads it, so this is a reporting gap only.
        preferred_source: None,
    })
}

/// View a `SOCKADDR_INET` as bytes for [`winparse::sockaddr_ip`].
///
/// The type is a union, so there is no field to read without first deciding
/// which arm is live — which is the question `sockaddr_ip` answers from the
/// leading `sa_family`.
fn inet_bytes(address: &SOCKADDR_INET) -> &[u8] {
    // SAFETY: Every union arm is plain data, so the whole object is
    // initialized bytes for its own size.
    unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(address).cast::<u8>(),
            size_of::<SOCKADDR_INET>(),
        )
    }
}

// The walk needs a live `iphlpapi.dll`; the shaping it feeds is
// `crate::winroute`, whose tests run on Linux.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sockaddr_inet_is_large_enough_to_hold_either_family() {
        // `sockaddr_ip` reads 24 bytes for a v6 address. A union smaller than
        // that would silently truncate every IPv6 gateway.
        assert!(size_of::<SOCKADDR_INET>() >= 24);
    }
}
