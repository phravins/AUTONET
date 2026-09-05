//! Operating-system network-discovery backends.

#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::doc_markdown)]

use std::net::IpAddr;

use autonet_core::model::NetworkState;

pub use crate::change::{change_source, ChangeSource};

/// Native notification that the network moved, where a platform has one.
pub mod change;

// Shared backend helpers.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod hwaddr;

// macOS interface classification.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod linktype;

// macOS routing-message parsing.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod rtparse;

// macOS service-order metrics.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod servicerank;

// Collision precedence for the port lookups, shared by Linux and Windows.
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod portmatch;

// Windows IP Helper parsing.
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod winparse;

// Windows route shaping.
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod winroute;

// Windows interface classification.
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod wintype;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;

/// An operating-system network query failed.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// AutoNet has no backend for this operating system yet.
    #[error("AutoNet has no network backend for {platform} yet")]
    Unsupported {
        /// The target the binary was built for, e.g. `"macos"`.
        platform: &'static str,
    },

    /// A query to the kernel failed.
    #[error("could not {operation}: {message}")]
    Query {
        /// What was being attempted, phrased to complete "could not …".
        operation: &'static str,
        /// The operating system's own explanation.
        message: String,
    },
}

impl PlatformError {
    /// Wrap an arbitrary platform error with the operation that produced it.
    pub(crate) fn query(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Query {
            operation,
            message: error.to_string(),
        }
    }
}

/// A source of network snapshots.
pub trait NetworkProvider: Send + Sync {
    /// Capture the current network configuration.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the kernel cannot be queried.
    fn snapshot(&self) -> Result<NetworkState, PlatformError>;

    /// A short name for the backend in use, for `--json` output and `doctor`.
    fn platform_name(&self) -> &'static str;
}

/// Build the backend for the current platform.
///
/// # Errors
///
/// Returns [`PlatformError::Unsupported`] on platforms without a backend, or
/// [`PlatformError::Query`] if the backend cannot be initialised.
pub fn provider() -> Result<Box<dyn NetworkProvider>, PlatformError> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::LinuxProvider::new()?))
    }

    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::MacosProvider::new()))
    }

    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(windows::WindowsProvider::new()))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Ok(Box::new(unsupported::UnsupportedProvider::new()))
    }
}

/// Who holds a listening TCP port, as far as this platform will say.
///
/// The variants are a ladder of decreasing certainty rather than alternatives:
/// each one is the best answer the operating system was willing to give, and
/// the caller is expected to phrase all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortHolder {
    /// A process this user may inspect, with the name it runs under.
    Named {
        /// The holding process.
        pid: u32,
        /// Its short command name, not a full path.
        name: String,
    },

    /// The holding process is known but the system would not name it.
    ///
    /// Windows can refuse `OpenProcess` on a protected process, and on any
    /// platform the process may exit between the lookup and the naming.
    Unnamed {
        /// The holding process.
        pid: u32,
    },

    /// Held by another account, which this user may not inspect.
    ///
    /// Linux reports the owning uid of every socket but only lets a process
    /// list another process's descriptors when it owns it, or is root.
    OtherUser {
        /// The account that opened the socket.
        uid: u32,
    },

    /// The platform was asked and reported no listener on that port.
    ///
    /// Not the same as "free": the probe and the lookup are separate calls, so
    /// a socket can appear or vanish between them.
    NotListed,

    /// This platform has no way to answer the question.
    ///
    /// Carries the target name so callers never have to name an operating
    /// system themselves.
    Unsupported {
        /// The target the binary was built for, e.g. `"macos"`.
        platform: &'static str,
    },
}

/// Identify the process listening on `port` at `address` or its family wildcard.
///
/// Infallible by design. Every failure — an unreadable `/proc`, a refused
/// handle, a platform with no interface for the question — degrades to a
/// variant rather than an error, because this only ever enriches a diagnostic
/// and must never turn a warning into one.
///
/// The answer is a snapshot and is inherently racy: it describes what was
/// listening at the moment of the call.
pub fn port_holder(address: IpAddr, port: u16) -> PortHolder {
    #[cfg(target_os = "linux")]
    {
        linux::portowner::holder(address, port)
    }

    #[cfg(target_os = "macos")]
    {
        macos::portowner::holder(address, port)
    }

    #[cfg(target_os = "windows")]
    {
        windows::tcptable::holder(address, port)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        // The same reasoning as `UnsupportedProvider`: compile everywhere, and
        // say so plainly rather than claiming a port is free.
        let _ = (address, port);
        PortHolder::Unsupported {
            platform: std::env::consts::OS,
        }
    }
}
