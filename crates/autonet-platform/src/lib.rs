//! Operating-system network-discovery backends.

#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::doc_markdown)]

use autonet_core::model::NetworkState;

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
