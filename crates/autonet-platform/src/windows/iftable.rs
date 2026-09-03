//! Windows interface-table lookup keyed by LUID.

use std::collections::HashMap;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE1394, IF_TYPE_IEEE80211,
    IF_TYPE_IEEE80216_WMAN, IF_TYPE_IEEE8023AD_LAG, IF_TYPE_OTHER, IF_TYPE_PPP,
    IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL, IF_TYPE_WWANPP, IF_TYPE_WWANPP2, MIB_IF_ROW2,
    MIB_IF_TABLE2,
};
use windows_sys::Win32::NetworkManagement::Ndis::{
    NdisMedium802_3, NdisMediumLoopback, NdisMediumNative802_11, NdisMediumTunnel,
    NdisPhysicalMedium802_3, NdisPhysicalMediumNative802_11, NdisPhysicalMediumUnspecified,
    NdisPhysicalMediumWirelessLan, NET_IF_ACCESS_BROADCAST, NET_IF_ACCESS_LOOPBACK,
    NET_IF_ACCESS_POINT_TO_MULTI_POINT, NET_IF_ACCESS_POINT_TO_POINT, NET_IF_ADMIN_STATUS_UP,
    TUNNEL_TYPE_6TO4, TUNNEL_TYPE_DIRECT, TUNNEL_TYPE_IPHTTPS, TUNNEL_TYPE_ISATAP,
    TUNNEL_TYPE_NONE, TUNNEL_TYPE_OTHER, TUNNEL_TYPE_TEREDO,
};

use crate::wintype::{access, if_type, media, medium, tunnel, Ndis};
use crate::PlatformError;

// Keep Linux-testable constants aligned with windows-sys.
const _: () = {
    assert!(if_type::OTHER == IF_TYPE_OTHER);
    assert!(if_type::ETHERNET_CSMACD == IF_TYPE_ETHERNET_CSMACD);
    assert!(if_type::PPP == IF_TYPE_PPP);
    assert!(if_type::SOFTWARE_LOOPBACK == IF_TYPE_SOFTWARE_LOOPBACK);
    assert!(if_type::IEEE80211 == IF_TYPE_IEEE80211);
    assert!(if_type::TUNNEL == IF_TYPE_TUNNEL);
    assert!(if_type::IEEE1394 == IF_TYPE_IEEE1394);
    assert!(if_type::IEEE8023AD_LAG == IF_TYPE_IEEE8023AD_LAG);
    assert!(if_type::IEEE80216_WMAN == IF_TYPE_IEEE80216_WMAN);
    assert!(if_type::WWANPP == IF_TYPE_WWANPP);
    assert!(if_type::WWANPP2 == IF_TYPE_WWANPP2);

    assert!(access::LOOPBACK == NET_IF_ACCESS_LOOPBACK);
    assert!(access::BROADCAST == NET_IF_ACCESS_BROADCAST);
    assert!(access::POINT_TO_POINT == NET_IF_ACCESS_POINT_TO_POINT);
    assert!(access::POINT_TO_MULTI_POINT == NET_IF_ACCESS_POINT_TO_MULTI_POINT);

    assert!(medium::UNSPECIFIED == NdisPhysicalMediumUnspecified);
    assert!(medium::WIRELESS_LAN == NdisPhysicalMediumWirelessLan);
    assert!(medium::NATIVE_802_11 == NdisPhysicalMediumNative802_11);
    assert!(medium::ETHERNET_802_3 == NdisPhysicalMedium802_3);

    assert!(media::ETHERNET_802_3 == NdisMedium802_3);
    assert!(media::TUNNEL == NdisMediumTunnel);
    assert!(media::NATIVE_802_11 == NdisMediumNative802_11);
    assert!(media::LOOPBACK == NdisMediumLoopback);

    assert!(tunnel::NONE == TUNNEL_TYPE_NONE);
    assert!(tunnel::OTHER == TUNNEL_TYPE_OTHER);
    assert!(tunnel::DIRECT == TUNNEL_TYPE_DIRECT);
    assert!(tunnel::SIX_TO_FOUR == TUNNEL_TYPE_6TO4);
    assert!(tunnel::ISATAP == TUNNEL_TYPE_ISATAP);
    assert!(tunnel::TEREDO == TUNNEL_TYPE_TEREDO);
    assert!(tunnel::IPHTTPS == TUNNEL_TYPE_IPHTTPS);
};

/// `HardwareInterface` is the first bit in the Windows flag byte.
const HARDWARE_INTERFACE: u8 = 0x01;

/// Additional interface-table data.
pub(crate) struct Row {
    /// Classification evidence.
    pub ndis: Ndis,
    /// Administrative state.
    pub admin_up: bool,
    /// Link access type.
    pub access_type: i32,
}

/// Return every interface keyed by LUID.
pub(crate) fn by_luid() -> Result<HashMap<u64, Row>, PlatformError> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();

    // SAFETY: `table` is a valid out-parameter.
    let result = unsafe { GetIfTable2(&raw mut table) };

    if result != ERROR_SUCCESS {
        return Err(PlatformError::query(
            "enumerate network interfaces",
            format!("Windows error {result}"),
        ));
    }

    let table = Table(table);
    Ok(collect(table.rows()))
}

/// Owns the `GetIfTable2` allocation.
struct Table(*mut MIB_IF_TABLE2);

impl Table {
    /// Return rows reported by Windows.
    fn rows(&self) -> &[MIB_IF_ROW2] {
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
            // SAFETY: This allocation came from `GetIfTable2`.
            unsafe { FreeMibTable(self.0.cast::<std::ffi::c_void>()) };
        }
    }
}

/// Build a LUID-keyed map.
fn collect(rows: &[MIB_IF_ROW2]) -> HashMap<u64, Row> {
    let trusted = trust_hardware_bit(rows);

    rows.iter()
        .map(|row| {
            // SAFETY: The union's `u64` representation is always valid.
            let luid = unsafe { row.InterfaceLuid.Value };

            (
                luid,
                Row {
                    ndis: Ndis {
                        access_type: row.AccessType,
                        physical_medium: row.PhysicalMediumType,
                        media_type: row.MediaType,
                        hardware_interface: trusted.then(|| is_hardware(row)),
                    },
                    admin_up: row.AdminStatus == NET_IF_ADMIN_STATUS_UP,
                    access_type: row.AccessType,
                },
            )
        })
        .collect()
}

/// Read the `HardwareInterface` bit for one row.
fn is_hardware(row: &MIB_IF_ROW2) -> bool {
    row.InterfaceAndOperStatusFlags._bitfield & HARDWARE_INTERFACE != 0
}

/// Whether the `HardwareInterface` bit can be believed on this machine.
///
/// The software loopback interface is by definition not hardware, so a reading
/// that says otherwise is wrong — whatever the cause. Every row's
/// `hardware_interface` then becomes `None` and [`crate::wintype`] falls back to
/// `IfType` alone: a weaker answer rather than a wrong one.
///
/// This can catch a misread; it cannot confirm a correct one. A table with no
/// loopback row trusts the bit rather than disabling a working signal on no
/// evidence.
fn trust_hardware_bit(rows: &[MIB_IF_ROW2]) -> bool {
    !rows
        .iter()
        .filter(|row| row.Type == IF_TYPE_SOFTWARE_LOOPBACK)
        .any(is_hardware)
}

// The walk needs a live `iphlpapi.dll`; the decision table it feeds is
// `crate::wintype`, whose tests run on Linux.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hardware_bit_is_the_first_declared_member() {
        // Any other value silently reads a different flag and reclassifies the
        // machine.
        assert_eq!(HARDWARE_INTERFACE, 0x01);
        assert_eq!(HARDWARE_INTERFACE.count_ones(), 1);
    }

    #[test]
    fn an_empty_table_trusts_the_bit_and_yields_nothing() {
        assert!(trust_hardware_bit(&[]));
        assert!(collect(&[]).is_empty());
    }
}
