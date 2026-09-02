//! The second classification source: `GetIfTable2`, joined by LUID.
//!
//! [`crate::wintype`] explains *why* a second source is needed and why this is
//! the one chosen over `GetIfEntry2`, WlanAPI and WMI. This module is only the
//! mechanics: one call, one walk, one map.
//!
//! # Why LUID and not the interface index
//!
//! The brief left the join key open. It has to be the LUID, and the reason is
//! specific rather than stylistic: `Interface.index` is *already lossy*. Windows
//! numbers an adapter once per address family, and [`super::adapters`] collapses
//! `IfIndex` and `Ipv6IfIndex` into the model's single `index`, falling back to
//! zero when both are absent. Joining on that would silently mis-pair every
//! IPv6-only adapter and pile every index-less one onto key zero. The LUID is
//! one 64-bit value per interface regardless of family, it is what
//! `MIB_IF_ROW2` and `MIB_IPFORWARD_ROW2` both carry, and Microsoft documents it
//! as persistent where the index explicitly is not. Task 4 will want the same
//! key for routes.
//!
//! # Freeing the table
//!
//! `GetIfTable2` allocates; the caller must release it with `FreeMibTable`. That
//! is handled by a guard with a `Drop` impl rather than by a free at the end of
//! the function, so an early return or a panic in the walk cannot leak the
//! allocation.
//!
//! # Status
//!
//! **Type-checked and pinned to windows-sys, not hardware-verified.** One claim
//! here is weaker than the rest and is called out where it is made: the bit
//! position of `HardwareInterface`. See [`HARDWARE_INTERFACE`].

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

// Every constant `wintype` restates so that it can be built and tested off
// Windows, pinned here to windows-sys's own definitions. Same safety net as the
// block in `adapters.rs`: if Microsoft renumbers one, the Windows build fails
// rather than the classification table quietly filing a NIC under the wrong kind.
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

/// The `HardwareInterface` bit within `InterfaceAndOperStatusFlags`.
///
/// **This is the one asserted-not-confirmed ABI claim in Task 3, and it is
/// stated here rather than buried.** windows-sys models the union as a bare
/// `_bitfield: u8` with no accessors, so the bit has to be named by hand.
/// `netioapi.h` declares the eight `BOOLEAN … : 1` members in the order
/// `HardwareInterface, FilterInterface, ConnectorPresent, NotAuthenticated,
/// NotMediaConnected, Paused, LowPower, EndPointInterface`, and MSVC allocates
/// bitfields from the least-significant bit of the storage unit upward — so the
/// first-declared member is `0x01`.
///
/// That is an ABI rule, not a guess, but it is also not something this project
/// can *check* from Linux, and a wrong reading here would be quiet rather than
/// loud: it would reclassify real NICs as `virtual-ethernet` and strip 250
/// points from each. [`trust_hardware_bit`] is the runtime answer to that.
const HARDWARE_INTERFACE: u8 = 0x01;

/// What `GetIfTable2` adds for one interface.
pub(crate) struct Row {
    /// The classification evidence, in [`crate::wintype`]'s vocabulary.
    pub ndis: Ndis,
    /// `AdminStatus == NET_IF_ADMIN_STATUS_UP`.
    ///
    /// Task 2 had to write the same value into `flags.up` and `flags.running`
    /// because `GetAdaptersAddresses` reports only operational status. This is
    /// the missing half: administrative status is the true `IFF_UP` analogue,
    /// so `up` can now mean "switched on" and `running` "carrying traffic", as
    /// they do on Linux.
    pub admin_up: bool,
    /// `AccessType`, for the `broadcast` and `point_to_point` flags.
    pub access_type: i32,
}

/// Every interface on the machine, keyed by LUID.
///
/// # Errors
///
/// Returns [`PlatformError::Query`] if `GetIfTable2` fails.
///
/// A failure here is an error rather than a silent fallback to `IfType` alone,
/// which is a deliberate choice worth stating. Both calls go to the same DLL and
/// the same TCP/IP stack; if `GetAdaptersAddresses` succeeded and this did not,
/// something is wrong that AutoNet should say out loud. Degrading quietly would
/// mean answering with a classification known to be weaker — the case where a
/// TAP-mode VPN silently recovers its +250 — while looking exactly like a normal
/// run. Per-*row* absence is different and is handled by degrading, because an
/// adapter appearing between the two calls is ordinary rather than alarming.
pub(crate) fn by_luid() -> Result<HashMap<u64, Row>, PlatformError> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();

    // SAFETY: `table` is a valid, writable out-parameter. On success Windows
    // stores a pointer to an allocation it owns; the guard below takes charge of
    // releasing it, including if the walk panics.
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

/// Owns the allocation `GetIfTable2` returned.
struct Table(*mut MIB_IF_TABLE2);

impl Table {
    /// The rows, as a slice of the length Windows reported.
    fn rows(&self) -> &[MIB_IF_ROW2] {
        if self.0.is_null() {
            return &[];
        }

        // SAFETY: a non-null pointer from a successful `GetIfTable2` addresses a
        // fully initialised `MIB_IF_TABLE2`.
        let table = unsafe { &*self.0 };
        let Ok(len) = usize::try_from(table.NumEntries) else {
            return &[];
        };

        // SAFETY: `Table` is declared as `[MIB_IF_ROW2; 1]` but is a flexible
        // array of `NumEntries` rows written by Windows into one allocation, so
        // reading that many contiguous rows stays inside it. The borrow is tied
        // to `&self`, so the slice cannot outlive the free in `Drop`.
        unsafe { std::slice::from_raw_parts(table.Table.as_ptr(), len) }
    }
}

impl Drop for Table {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a successful `GetIfTable2` and has
            // not been freed — nothing else in this module calls `FreeMibTable`,
            // and `Table` is neither `Copy` nor `Clone`, so it is dropped once.
            unsafe { FreeMibTable(self.0.cast::<std::ffi::c_void>()) };
        }
    }
}

/// Turn the rows into the LUID-keyed map, honouring the hardware-bit check.
fn collect(rows: &[MIB_IF_ROW2]) -> HashMap<u64, Row> {
    let trusted = trust_hardware_bit(rows);

    rows.iter()
        .map(|row| {
            // SAFETY: `NET_LUID_LH` unions a `u64` with a bitfield of the same
            // width. Every bit pattern is a valid `u64`, so reading `Value` is
            // sound whichever member Windows wrote — and `Value` is the whole
            // point: it is the opaque identifier both tables agree on.
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
/// A cheap falsifier for the ABI claim [`HARDWARE_INTERFACE`] documents. The
/// software loopback interface is, by Microsoft's own definition, not a hardware
/// interface — so if our reading of the bit says it *is*, the reading is wrong,
/// whatever the cause: a bitfield order this code got backwards, a struct layout
/// change, a future member inserted ahead of it. When that happens every row's
/// `hardware_interface` becomes `None` and [`crate::wintype`] falls back to
/// `IfType` alone, which is exactly the behaviour Task 2 shipped — a weaker
/// answer, not a wrong one.
///
/// Be precise about what this proves: it can catch a misread, and it cannot
/// confirm a correct one. A machine with no loopback row in the table (which
/// should not happen) trusts the bit by default rather than disabling a working
/// signal on no evidence. Only the Task 7 hardware run settles it positively,
/// and Task 6's live tests are where that assertion belongs.
fn trust_hardware_bit(rows: &[MIB_IF_ROW2]) -> bool {
    !rows
        .iter()
        .filter(|row| row.Type == IF_TYPE_SOFTWARE_LOOPBACK)
        .any(is_hardware)
}

// The walk itself needs a live `iphlpapi.dll` and cannot be unit-tested. The
// decision table it feeds is `crate::wintype`, whose tests run on Linux.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hardware_bit_is_the_first_declared_member() {
        // A guard against someone "tidying" the constant. `netioapi.h` declares
        // HardwareInterface first and MSVC packs bitfields from the low bit, so
        // any other value here silently reads a different flag — ConnectorPresent
        // or NotMediaConnected — and reclassifies the machine.
        assert_eq!(HARDWARE_INTERFACE, 0x01);
        assert_eq!(HARDWARE_INTERFACE.count_ones(), 1);
    }

    #[test]
    fn an_empty_table_trusts_the_bit_and_yields_nothing() {
        // No loopback row means no evidence either way; disabling a working
        // signal on no evidence would be the wrong default.
        assert!(trust_hardware_bit(&[]));
        assert!(collect(&[]).is_empty());
    }
}
