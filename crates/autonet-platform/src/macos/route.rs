//! Dumping the kernel routing table over the BSD routing socket.
//!
//! Only the syscall: what the returned bytes *mean* is [`crate::rtparse`]'s,
//! which is not gated to macOS so its tests run on Linux — sockaddr
//! misalignment is the classic way a routing parser goes subtly wrong.
//!
//! `sysctl` rather than a `PF_ROUTE` socket read, because it returns the whole
//! table as one consistent snapshot with no descriptor to leak and no
//! partial-read handling. Nothing here parses OS *text*: `netstat` and `route`
//! output is localised and version-dependent.
//!
//! `NET_RT_DUMP` rather than `NET_RT_DUMP2`: the latter's `rt_msghdr2` differs
//! only in fields AutoNet does not model, `rt_metrics` is inline in plain
//! `rt_msghdr` too, and DUMP2 is absent from `libc`.
//!
//! **Unverified against a live routing table.** The MIB, two-call sizing and
//! message layout are checked against `libc` at compile time; that a real dump
//! behaves as described needs a Mac.

use std::collections::HashMap;
use std::io;
use std::ptr;

use autonet_core::model::{Family, Route};

use crate::rtparse;
use crate::PlatformError;

/// How many times to re-ask for the table when it grows between the sizing call
/// and the read.
///
/// The table can change under us — a DHCP lease, a VPN coming up — so `ENOMEM`
/// on the second call is occasional rather than exceptional. Bounded so a
/// churning table fails with an error instead of spinning.
const RESIZE_ATTEMPTS: usize = 4;

/// Every route the kernel knows, both families.
///
/// Two dumps, because `NET_RT_DUMP` takes a family in its MIB and returns only
/// that family's routes. An empty result is a legitimate answer.
///
/// `metrics` maps interface index to the metric each route should carry. Nothing
/// in a Darwin routing message supplies one, so it arrives already resolved by
/// [`crate::servicerank`]; taking a map keeps route parsing free of interface
/// names and the metric decision testable on Linux.
pub(crate) fn dump_routes(metrics: &HashMap<u32, u32>) -> Result<Vec<Route>, PlatformError> {
    let mut routes = Vec::new();
    for family in [Family::V4, Family::V6] {
        let table = dump(family)?;
        routes.extend(rtparse::routes(&table, family, metrics));
    }
    Ok(routes)
}

/// The address family number Darwin uses in the routing MIB.
const fn af(family: Family) -> libc::c_int {
    match family {
        Family::V4 => libc::AF_INET,
        Family::V6 => libc::AF_INET6,
    }
}

/// Ask the kernel for one family's routing table as a byte buffer.
///
/// The usual two-call `sysctl` dance: once with a null buffer to learn the
/// size, then again to read. The table can grow in between, so the read is
/// retried on `ENOMEM`.
fn dump(family: Family) -> Result<Vec<u8>, PlatformError> {
    // CTL_NET.PF_ROUTE.0.<family>.NET_RT_DUMP.0 — the third and sixth elements
    // are the protocol and the "flags" selector, both unused for a table dump.
    let mut mib: [libc::c_int; 6] = [
        libc::CTL_NET,
        libc::PF_ROUTE,
        0,
        af(family),
        libc::NET_RT_DUMP,
        0,
    ];
    let mib_len = libc::c_uint::try_from(mib.len()).unwrap_or(6);

    for _ in 0..RESIZE_ATTEMPTS {
        let mut needed: libc::size_t = 0;

        // SAFETY: `mib` is a live array of `mib_len` ints; a null `oldp` with a
        // valid `oldlenp` is the documented way to ask only for the size.
        let sized = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib_len,
                ptr::null_mut(),
                &raw mut needed,
                ptr::null_mut(),
                0,
            )
        };
        if sized != 0 {
            return Err(query(family, io::Error::last_os_error()));
        }

        if needed == 0 {
            // No routes at all. Airplane mode reaches here.
            return Ok(Vec::new());
        }

        // Slack, because the size the kernel just reported is a snapshot of a
        // table that is still live. Without it a single route appearing between
        // the two calls costs a whole extra round trip.
        let mut buffer = vec![0u8; needed + needed / 8 + 1024];
        let mut written: libc::size_t = buffer.len();

        // SAFETY: `buffer` is `written` bytes of writable memory, and
        // `written` is updated by the kernel to the amount actually used.
        let read = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib_len,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                &raw mut written,
                ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOMEM) {
                // The table outgrew the buffer. Ask again with fresh sizing.
                continue;
            }
            return Err(query(family, error));
        }

        // The kernel writes at most what it reported; the slack above is not
        // part of the dump and must not be walked as messages.
        buffer.truncate(written.min(buffer.len()));
        return Ok(buffer);
    }

    Err(query(
        family,
        io::Error::other(format!(
            "the routing table kept growing across {RESIZE_ATTEMPTS} attempts"
        )),
    ))
}

/// Describe a failed dump in terms of what was being asked for.
///
/// Spelled out per family rather than formatted: `PlatformError::Query` holds a
/// `&'static str`.
fn query(family: Family, error: io::Error) -> PlatformError {
    let operation = match family {
        Family::V4 => "dump the IPv4 routing table",
        Family::V6 => "dump the IPv6 routing table",
    };
    PlatformError::query(operation, error)
}

#[cfg(test)]
mod tests {
    use super::{dump, Family};
    use crate::rtparse;

    /// Cross-check `rtm_index` against the `RTA_IFP` sockaddr on a live table.
    ///
    /// Not in `tests/live.rs` because it cannot be: `RTA_IFP` is consumed inside
    /// [`crate::rtparse::routes`] and never crosses the platform boundary. Runs
    /// with the rest under `cargo test -p autonet-platform -- --ignored`.
    ///
    /// `RTA_IFP` sits immediately after the netmask, and a default route's
    /// netmask is the one sockaddr written with `sa_len == 0` — the only length
    /// at which Darwin's four-byte `ROUNDUP` and FreeBSD's eight-byte one
    /// disagree. So this is the most specific detector for the alignment rule
    /// the parser takes from `<net/route.h>` rather than from a running kernel.
    /// It also catches a genuine disagreement, which would mean routes are being
    /// joined to the wrong interface.
    ///
    /// `RTA_IFP` is optional, so a dump carrying none passes vacuously. The
    /// count is printed: a run reporting zero has proven nothing.
    #[test]
    #[ignore = "queries the live routing table"]
    fn rtm_index_and_rta_ifp_name_the_same_interface() {
        for family in [Family::V4, Family::V6] {
            let table = dump(family).expect("the routing table should be dumpable");
            let mut rest = table.as_slice();
            let mut compared = 0usize;

            while let Some(head) = rtparse::message(rest) {
                let (this, tail) = rest.split_at(head.msglen);
                rest = tail;

                if head.version != rtparse::RTM_VERSION || !rtparse::is_reportable(head.flags) {
                    continue;
                }
                let Some(block) = rtparse::address_block(this, &head) else {
                    continue;
                };
                let Some(parts) = rtparse::route_parts(block, &head, family) else {
                    continue;
                };
                let Some(from_sockaddr) = parts.interface else {
                    continue;
                };

                assert_eq!(
                    head.index, from_sockaddr,
                    "{family:?}: rtm_index says interface {} but RTA_IFP says {from_sockaddr} \
                     for {parts:?} — the sockaddr walk is misaligned",
                    head.index
                );
                compared += 1;
            }

            println!("{family:?}: cross-checked {compared} route(s) carrying RTA_IFP");
        }
    }
}
