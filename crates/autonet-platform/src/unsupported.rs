//! Placeholder backend for platforms AutoNet does not support yet.
//!
//! This exists so the workspace **compiles** everywhere: a developer on a
//! fourth platform can build, run the full test suite and work on the CLI
//! before a backend exists. Only commands needing live network state fail, and
//! with a clear message rather than a link error.

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
