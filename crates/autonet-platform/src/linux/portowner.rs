//! Which process holds a listening TCP port, on Linux.
//!
//! The kernel splits the answer across two interfaces, so this takes two steps.
//! `/proc/net/tcp{,6}` lists every listening socket on the machine with its
//! *inode* and its *owning uid*, and is world-readable. Turning an inode into a
//! pid means finding the descriptor that refers to it, and `/proc/<pid>/fd` is
//! mode `0500` — so the pid half is answerable only for this user's own
//! processes, or as root. `ss` leaves exactly the same blanks under exactly the
//! same conditions: the ceiling is the kernel's, not this implementation's.
//!
//! This is the first code in the crate to read a file's *contents*
//! ([`super::sysfs`] only tests for existence), so every path it opens is
//! either a constant or built from a number that has already been parsed.

use std::net::IpAddr;
use std::os::unix::fs::MetadataExt;

use super::procnet::{self, Listener};
use crate::portmatch::{self, Bound};
use crate::PortHolder;

/// The kernel's two TCP socket tables, one per family.
///
/// Both are read every time. A dual-stack listener on `[::]` occupies the IPv4
/// port as well but appears only in the v6 file, so reading one alone would
/// report a busy port as free.
const TABLES: [&str; 2] = ["/proc/net/tcp", "/proc/net/tcp6"];

/// Identify the process listening on `port` at `address` or its wildcard.
pub(crate) fn holder(address: IpAddr, port: u16) -> PortHolder {
    let mut rows = Vec::new();
    for table in TABLES {
        // An unreadable table answers "nothing here", the same way a missing
        // `/sys` answers "not wireless" in `sysfs`.
        if let Ok(text) = std::fs::read_to_string(table) {
            rows.extend(procnet::listeners(&text));
        }
    }

    let Some(found) = portmatch::collides(&rows, address, port) else {
        return PortHolder::NotListed;
    };

    // Our own uid, read from `/proc` rather than by calling `getuid`: this
    // crate takes no `libc` dependency outside macOS.
    let ours = std::fs::metadata("/proc/self").map(|meta| meta.uid()).ok();

    // Skip the scan when it is guaranteed to hit EACCES. Root is the exception
    // — it may read any process's descriptors — so it still gets to try.
    if let Some(ours) = ours {
        if ours != 0 && ours != found.uid {
            return PortHolder::OtherUser { uid: found.uid };
        }
    }

    if let Some(pid) = pid_owning(found.inode) {
        return match command_name(pid) {
            Some(name) => PortHolder::Named { pid, name },
            None => PortHolder::Unnamed { pid },
        };
    }

    // The row is real but no descriptor we may read points at it: the holder
    // exited between the two reads, or it belongs to someone else and we are
    // not root after all.
    if ours == Some(found.uid) {
        PortHolder::NotListed
    } else {
        PortHolder::OtherUser { uid: found.uid }
    }
}

impl Bound for Listener {
    fn address(&self) -> IpAddr {
        self.address
    }

    fn port(&self) -> u16 {
        self.port
    }
}

/// Find the process holding the socket with this inode.
///
/// Errors are skipped rather than reported at every level: `/proc` is a live
/// directory whose entries vanish underneath a walk as processes exit.
fn pid_owning(inode: u64) -> Option<u32> {
    let target = format!("socket:[{inode}]");

    std::fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .find(|&pid| holds(pid, &target))
}

/// Whether process `pid` has a descriptor pointing at `target`.
fn holds(pid: u32, target: &str) -> bool {
    // `pid` was parsed as a number before it reached this path, which rules out
    // traversal more firmly than any check on a string could.
    let Ok(descriptors) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        // EACCES for another user's process, ENOENT if it has since exited.
        return false;
    };

    descriptors
        .flatten()
        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
        .any(|link| link.to_str() == Some(target))
}

/// The short command name of a process, as the kernel records it.
///
/// `comm`, not `cmdline`: it is a single line with no embedded NULs, and the
/// full command line of another process is more than a port warning needs. The
/// kernel truncates it to fifteen characters, so `node` survives but a long
/// path would not have been shown here anyway.
fn command_name(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = comm.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_reports_the_address_and_port_collision_matches_on() {
        // The impl is two lines and could be transposed without complaint from
        // the compiler, which would silently match on the wrong field.
        let listener = Listener {
            address: "192.168.1.20".parse().unwrap(),
            port: 3000,
            uid: 1000,
            inode: 4242,
        };
        assert_eq!(listener.address(), listener.address);
        assert_eq!(listener.port(), listener.port);
    }

    #[test]
    fn this_process_names_itself_rather_than_erroring() {
        // `comm` for the test binary is whatever cargo named it; that it is
        // present and trimmed is all that is portable enough to assert.
        let name = command_name(std::process::id()).expect("a name for this process");
        assert!(!name.is_empty());
        assert_eq!(name.trim(), name, "the trailing newline should be gone");
    }

    #[test]
    fn a_pid_that_cannot_exist_is_absent_rather_than_a_panic() {
        assert!(command_name(u32::MAX).is_none());
        assert!(!holds(u32::MAX, "socket:[1]"));
    }
}
