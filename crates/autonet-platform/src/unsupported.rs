//! Placeholder backend for platforms AutoNet does not support yet.
//!
//! This exists so the workspace **compiles** everywhere. Linux, macOS and
//! Windows now have backends of their own, so this is what a developer on any
//! fourth platform gets: build, run the full test suite, and work on the CLI
//! before a backend for it exists. Only commands that actually need live
//! network state fail, and they fail with a clear message instead of a link
//! error.

use autonet_core::model::NetworkState;

use crate::{NetworkProvider, PlatformError};

/// A provider that always reports [`PlatformError::Unsupported`].
pub(crate) struct UnsupportedProvider;

impl UnsupportedProvider {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl NetworkProvider for UnsupportedProvider {
    fn snapshot(&self) -> Result<NetworkState, PlatformError> {
        Err(PlatformError::Unsupported {
            platform: std::env::consts::OS,
        })
    }

    fn platform_name(&self) -> &'static str {
        "unsupported"
    }
}
