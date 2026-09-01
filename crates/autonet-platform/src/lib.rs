//! Operating-system backends for AutoNet.
//!
//! This crate is the *only* place in the workspace that talks to the kernel.
//! Its entire job is to produce one value — an [`autonet_core::NetworkState`] —
//! and hand it upward. Everything above it (the selection engine, the CLI, the
//! daemon, the SDKs) operates on that value and never learns which platform it
//! came from.
//!
//! That boundary is what makes the project testable. `autonet-core` is pure
//! functions over a `NetworkState`, so its tests read snapshots from JSON and
//! cannot be perturbed by the machine switching Wi-Fi networks mid-run. Only
//! this crate has to be tested against a live host, and only this crate has to
//! be rewritten for macOS and Windows.
//!
//! # Adding a platform
//!
//! Implement [`NetworkProvider`] and wire it into [`provider`] behind a
//! `#[cfg(target_os = ...)]`. Nothing above this crate changes. Platforms
//! without a backend yet still *compile*, and fail at runtime with
//! [`PlatformError::Unsupported`] rather than at build time — a Windows
//! developer can build and test the whole workspace before the Windows
//! backend exists.

#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::must_use_candidate)]
// "AutoNet", "WireGuard" and "Docker" are product names, not code. Wrapping
// them in backticks would render prose as inline code throughout the docs.
#![allow(clippy::doc_markdown)]

use autonet_core::model::NetworkState;

// Shared by both real backends rather than living in either: what counts as a
// reportable MAC is a decision about AutoNet's output, and the two must not
// drift. Gated on the platforms that have a backend so that a target with none
// still compiles warning-free, as the module documentation above promises.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod hwaddr;

// Deciding what *sort* of device an interface is, on macOS. Compiled on Linux
// too — and unused there, hence the allow — so that the decision table and its
// tests run on the Linux job as well as the macOS one. Task 2 put its logic
// inside the `cfg`-gated backend and consequently could only be tested on a
// runner none of us can debug on; keeping the pure part out here is the fix.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod linktype;

// Reading BSD routing-socket messages, for the same reason and on the same
// terms as `linktype` above. It matters more here: sockaddr padding and
// netmask truncation fail *quietly*, producing plausible wrong addresses rather
// than an error, so its hand-built-buffer tests need to run somewhere the
// failure can actually be debugged.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod rtparse;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

/// Something went wrong while asking the operating system about its network.
///
/// Deliberately shallow: the underlying netlink / IOCTL / Win32 error types are
/// platform-specific, so they are rendered to strings at this boundary rather
/// than leaking a Linux type into a struct the CLI and daemon both handle.
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
///
/// Intentionally synchronous. The Linux backend needs an async netlink client,
/// but it hides its own runtime rather than forcing `async` on the CLI, the
/// selection engine and every future SDK binding. macOS and Windows backends
/// are naturally blocking, so async at this layer would be a tax paid by three
/// platforms to suit one.
///
/// `Send + Sync` because the M5 daemon will share a single provider across
/// request handlers.
pub trait NetworkProvider: Send + Sync {
    /// Capture the machine's current network configuration.
    ///
    /// Each call re-queries the kernel; nothing is cached, because the entire
    /// point of AutoNet is that this changes underneath you.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the kernel cannot be queried.
    fn snapshot(&self) -> Result<NetworkState, PlatformError>;

    /// A short name for the backend in use, for `--json` output and `doctor`.
    fn platform_name(&self) -> &'static str;
}

/// Build the backend for the platform this binary was compiled for.
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

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(Box::new(unsupported::UnsupportedProvider::new()))
    }
}
