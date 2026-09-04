//! Why macOS reports that a port is busy but not who is holding it.
//!
//! **This platform answers [`PortHolder::Unsupported`], and that is a decision
//! rather than an omission.** It is stated here, in `--help`, and in the
//! warning `autonet run` prints, so nobody has to notice by comparing output
//! across machines that macOS says less than Linux and Windows do.
//!
//! The kernel interface exists: `proc_pidfdinfo` with `PROC_PIDFDSOCKETINFO`
//! fills a `socket_fdinfo`, whose `in_sockinfo` carries the local port. What
//! does not exist is a vetted definition of it. Neither `libc` nor any crate
//! already in this workspace declares `socket_fdinfo`, `in_sockinfo`,
//! `tcp_sockinfo`, `sockbuf_info`, `PROC_PIDFDSOCKETINFO` or `SOCKINFO_TCP`, so
//! using it means hand-writing roughly two hundred lines of `#[repr(C)]` with
//! two nested unions and a two-kilobyte struct.
//!
//! Three things make that a bad trade for a diagnostic:
//!
//! - The failure mode is silent. A struct of the right *size* with a wrong
//!   *offset* returns a plausible port number rather than an error, so the
//!   warning would name the wrong process with full confidence.
//! - There is no Mac in this project's loop to catch that. Every macOS claim
//!   here is type-checked by cross-compilation and CI, never observed.
//! - The crate today contains no `extern "C"` block and no `#[repr(C)]` struct
//!   at all — every foreign type comes from `libc` or `windows-sys`. This would
//!   not extend an existing pattern, it would introduce one.
//!
//! Shelling out to `lsof` is excluded by the project's standing rule against
//! parsing command output.
//!
//! What macOS *does* still do is detect the collision: the bind probe in the
//! CLI is pure `std` and works identically on all three platforms. Only the
//! attribution is missing.

use std::net::IpAddr;

use crate::PortHolder;

/// Report that this platform cannot name the holder of a port.
///
/// Takes the same arguments as the other backends so the dispatch in
/// [`crate::port_holder`] stays a plain match on the target.
pub(crate) fn holder(address: IpAddr, port: u16) -> PortHolder {
    let _ = (address, port);
    PortHolder::Unsupported {
        platform: std::env::consts::OS,
    }
}
