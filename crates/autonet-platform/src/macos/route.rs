//! Dumping the kernel routing table over the BSD routing socket.
//!
//! This module is only the syscall. Every decision about what the returned
//! bytes *mean* lives in [`crate::rtparse`], which is not gated to macOS so that
//! its tests run on the Linux CI job too — sockaddr misalignment is the classic
//! way a routing parser goes subtly wrong, and those tests are the only thing
//! standing between a padding mistake and plausible-looking wrong addresses.
//!
//! # `sysctl`, not a routing socket read
//!
//! `PF_ROUTE` can be opened as a socket and asked for the table one message at
//! a time, but the `sysctl` form returns the whole table in a single consistent
//! snapshot with no file descriptor to leak and no partial-read handling. It is
//! also what `netstat -rn` uses. AutoNet wants a snapshot, so this is the
//! natural fit.
//!
//! Nothing here parses OS *text*. `netstat` and `route` output is localised,
//! version-dependent, and would make AutoNet's answer depend on the user's
//! locale; this is the same rigour the Linux backend's netlink code applies.
//!
//! # `NET_RT_DUMP`, not `NET_RT_DUMP2`
//!
//! `NET_RT_DUMP2` returns `rt_msghdr2`, which differs from `rt_msghdr` only by
//! replacing `rtm_pid`/`rtm_seq`/`rtm_errno` with
//! `rtm_refcnt`/`rtm_parentflags`/`rtm_reserved`. AutoNet models none of those.
//! In particular `rt_metrics` is inline in the *plain* `rt_msghdr` as well, so
//! the usual reason to reach for DUMP2 does not apply. DUMP2 is also absent
//! from `libc` and would have to be hardcoded. So: `NET_RT_DUMP`, which `libc`
//! defines and which carries everything that is modelled.
//!
//! # Status
//!
//! **Unverified against a live routing table.** The `sysctl` MIB, the
//! two-call sizing and the message layout are read from Apple's headers and
//! checked against `libc` at compile time. That a real dump behaves as
//! described is confirmed only by running on a Mac.

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
/// on the second call is expected occasionally rather than exceptional. Bounded
/// so that a pathologically churning table fails with an error instead of
/// spinning forever.
const RESIZE_ATTEMPTS: usize = 4;

/// Every route the kernel knows, both families.
///
/// Two dumps, because `NET_RT_DUMP` takes an address family in its MIB and
/// returns only that family's routes. This mirrors the Linux backend, which
/// also dumps per-family rather than trusting `AF_UNSPEC` to return both.
///
/// An empty result is a legitimate answer, not a failure: with every interface
/// down there are no routes, and `NetworkState` already represents that as an
/// empty vector.
///
/// `metrics` maps interface index to the metric each route on it should carry.
/// Nothing in a routing message supplies one on Darwin, so it is passed in
/// already resolved by [`crate::servicerank`] from the network service order —
/// which is why this function takes a map rather than looking anything up:
/// route parsing stays free of interface names, and the one place that decides
/// what a metric *means* on macOS stays testable on Linux.
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
/// Spelled out per family rather than formatted, because `PlatformError::Query`
/// holds a `&'static str` — the error type is deliberately allocation-free for
/// the operation name.
fn query(family: Family, error: io::Error) -> PlatformError {
    let operation = match family {
        Family::V4 => "dump the IPv4 routing table",
        Family::V6 => "dump the IPv6 routing table",
    };
    PlatformError::query(operation, error)
}
