//! Which process holds a listening TCP port, on Windows.
//!
//! `GetExtendedTcpTable` with `TCP_TABLE_OWNER_PID_LISTENER` answers the first
//! half directly, and unlike Linux it does so **across accounts**: the table
//! names the owning pid of every listener on the machine, not only this user's.
//! Naming that pid is a second call, `OpenProcess` + `QueryFullProcessImageNameW`,
//! and that one can be refused — a protected process stays a bare pid.
//!
//! Two decoding traps, both of which produce a plausible wrong answer rather
//! than an error:
//!
//! - `dwLocalPort` is a `u32` whose **low word holds the port in network byte
//!   order**. Truncating it gives a byte-swapped port; byte-swapping the whole
//!   `u32` gives a shifted one.
//! - `MIB_TCPTABLE_OWNER_PID` uses the C flexible-array idiom, declared as
//!   `[MIB_TCPROW_OWNER_PID; 1]`. Reading it as a one-element array finds only
//!   the first listener on the machine.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::portmatch::{self, Bound};
use crate::PortHolder;

/// A first guess at the table size, grown from what Windows reports.
const INITIAL_BYTES: usize = 8 * 1024;

/// Retries allowed while the table grows underneath the sizing call.
///
/// The same ceiling `adapters` uses: the list changes between the two calls
/// only if sockets are opening as fast as they are enumerated.
const MAX_ATTEMPTS: usize = 3;

/// Room for an extended-length path, in UTF-16 code units.
///
/// Windows paths are not bounded by `MAX_PATH` any more, and this is asked
/// once per warning, so one generous allocation is cheaper than a retry loop.
const PATH_UNITS: usize = 32 * 1024;

/// A listening socket from either table.
struct Listener {
    /// The address it is bound to, unspecified for a wildcard bind.
    address: IpAddr,
    /// The port it is listening on.
    port: u16,
    /// The process that opened it.
    pid: u32,
}

impl Bound for Listener {
    fn address(&self) -> IpAddr {
        self.address
    }

    fn port(&self) -> u16 {
        self.port
    }
}

/// Identify the process listening on `port` at `address` or its wildcard.
pub(crate) fn holder(address: IpAddr, port: u16) -> PortHolder {
    let mut rows = v4_listeners();
    rows.extend(v6_listeners());

    let Some(found) = portmatch::collides(&rows, address, port) else {
        return PortHolder::NotListed;
    };

    match image_name(found.pid) {
        Some(name) => PortHolder::Named {
            pid: found.pid,
            name,
        },
        // The table is racy by construction: the process may already be gone,
        // or be one this account may not open at all.
        None => PortHolder::Unnamed { pid: found.pid },
    }
}

/// Every IPv4 listener, or none if the table cannot be read.
///
/// A failure here degrades to "nothing known" rather than propagating: this
/// only ever decorates a warning.
fn v4_listeners() -> Vec<Listener> {
    let Some(buffer) = table_buffer(u32::from(AF_INET)) else {
        return Vec::new();
    };

    // SAFETY: A successful call filled the buffer with an initialized table,
    // and `Vec<u64>` is aligned for anything in it.
    let table = unsafe { &*buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>() };
    let Ok(len) = usize::try_from(table.dwNumEntries) else {
        return Vec::new();
    };

    // SAFETY: `table` is a flexible array of `dwNumEntries` rows.
    let rows: &[MIB_TCPROW_OWNER_PID] =
        unsafe { std::slice::from_raw_parts(table.table.as_ptr(), len) };

    rows.iter()
        .map(|row| Listener {
            address: IpAddr::V4(Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes())),
            port: port_of(row.dwLocalPort),
            pid: row.dwOwningPid,
        })
        .collect()
}

/// Every IPv6 listener, or none if the table cannot be read.
fn v6_listeners() -> Vec<Listener> {
    let Some(buffer) = table_buffer(u32::from(AF_INET6)) else {
        return Vec::new();
    };

    // SAFETY: As `v4_listeners`, for the other family's table.
    let table = unsafe { &*buffer.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>() };
    let Ok(len) = usize::try_from(table.dwNumEntries) else {
        return Vec::new();
    };

    // SAFETY: `table` is a flexible array of `dwNumEntries` rows.
    let rows: &[MIB_TCP6ROW_OWNER_PID] =
        unsafe { std::slice::from_raw_parts(table.table.as_ptr(), len) };

    rows.iter()
        .map(|row| Listener {
            // Already bytes in network order, unlike the v4 row's `u32`.
            address: IpAddr::V6(Ipv6Addr::from(row.ucLocalAddr)),
            port: port_of(row.dwLocalPort),
            pid: row.dwOwningPid,
        })
        .collect()
}

/// Call `GetExtendedTcpTable` into an aligned, growable buffer.
fn table_buffer(family: u32) -> Option<Vec<u64>> {
    let mut bytes = INITIAL_BYTES;

    for _ in 0..MAX_ATTEMPTS {
        let words = bytes.div_ceil(size_of::<u64>());
        let mut buffer: Vec<u64> = vec![0; words];
        let mut size = u32::try_from(words * size_of::<u64>()).unwrap_or(u32::MAX);

        // SAFETY: The aligned writable buffer matches the supplied byte length.
        // `border` is FALSE: the rows are matched, never displayed in order.
        let result = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast::<std::ffi::c_void>(),
                &raw mut size,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };

        match result {
            ERROR_SUCCESS => return Some(buffer),
            // Preserve a non-shrinking retry size, as the adapter walk does.
            ERROR_INSUFFICIENT_BUFFER => {
                bytes = bytes.max(usize::try_from(size).unwrap_or(usize::MAX));
            }
            _ => return None,
        }
    }

    None
}

/// Decode a `dwLocalPort` field.
///
/// The port lives in the low word in **network** order. Taking the low word by
/// value and re-reading its bytes big-endian is correct on either endianness,
/// and needs no `as` cast that could truncate the wrong half.
fn port_of(raw: u32) -> u16 {
    let low = u16::try_from(raw & 0xFFFF).unwrap_or(0);
    u16::from_be_bytes(low.to_ne_bytes())
}

/// A process handle that closes itself.
///
/// The same shape as `route::Table` and `FreeMibTable`: acquire in a
/// constructor, release in `Drop`, so no path out of the lookup leaks it.
struct Process(HANDLE);

impl Process {
    /// Open a process for querying, or give up.
    ///
    /// `PROCESS_QUERY_LIMITED_INFORMATION` rather than
    /// `PROCESS_QUERY_INFORMATION`: it is the weakest right that answers this
    /// question, and it succeeds against processes at a higher integrity level
    /// where the stronger right would not.
    fn open(pid: u32) -> Option<Self> {
        // SAFETY: A plain call by value; failure is signalled by a null handle.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        (!handle.is_null()).then_some(Self(handle))
    }

    /// The full image path of the process.
    fn image_path(&self) -> Option<String> {
        let mut buffer = vec![0u16; PATH_UNITS];
        let mut units = u32::try_from(buffer.len()).ok()?;

        // SAFETY: `buffer` is writable for `units` UTF-16 units, which is what
        // `units` is initialized to; the call writes the used length back.
        let ok = unsafe {
            QueryFullProcessImageNameW(
                self.0,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &raw mut units,
            )
        };

        if ok == 0 {
            return None;
        }

        // The written length excludes the terminator, so it is the exact slice.
        let units = usize::try_from(units).ok()?;
        Some(String::from_utf16_lossy(buffer.get(..units)?))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: This handle came from a successful `OpenProcess`.
        unsafe { CloseHandle(self.0) };
    }
}

/// The short name of the process's image, if it will say.
fn image_name(pid: u32) -> Option<String> {
    let path = Process::open(pid)?.image_path()?;
    let name = leaf(&path);
    (!name.is_empty()).then(|| name.to_owned())
}

/// The last component of a Windows path.
///
/// Trimmed to match Linux's `comm`, which is a bare command name: printing
/// `C:\Program Files\nodejs\node.exe` where Linux prints `node` would make the
/// same warning read differently on the two platforms for no added meaning.
/// Both separators are accepted because Win32 accepts both.
fn leaf(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

// The table walk needs a live `iphlpapi.dll`, so only the decoding is unit
// tested — and only on windows-latest, since this module compiles nowhere else.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_comes_from_the_low_word_in_network_order() {
        // 8080 is 0x1F90; the low word holds it big-endian, so the DWORD reads
        // 0x901F. Truncating instead would give 36895, and byte-swapping the
        // whole DWORD would give 0.
        assert_eq!(port_of(0x0000_901F), 8080);
        assert_eq!(port_of(0), 0);
        // High bytes are padding and must not reach the result.
        assert_eq!(port_of(0xFFFF_901F), 8080);
    }

    #[test]
    fn a_local_address_keeps_its_octet_order() {
        // `dwLocalAddr` is an `in_addr`: its bytes are the octets already.
        let raw = u32::from_ne_bytes([192, 168, 1, 20]);
        assert_eq!(
            IpAddr::V4(Ipv4Addr::from(raw.to_ne_bytes())),
            "192.168.1.20".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn an_image_path_is_reduced_to_its_file_name() {
        assert_eq!(leaf(r"C:\Program Files\nodejs\node.exe"), "node.exe");
        assert_eq!(leaf("node.exe"), "node.exe");
        assert_eq!(leaf(r"\Device\HarddiskVolume4/app.exe"), "app.exe");
        assert_eq!(leaf(""), "");
    }

    #[test]
    fn a_pid_that_cannot_exist_is_absent_rather_than_a_panic() {
        // 0 is the System Idle Process, which never opens for querying.
        assert!(image_name(0).is_none());
    }
}
