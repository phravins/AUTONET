//! Windows network discovery through the IP Helper API.

mod adapters;
mod iftable;

use autonet_core::model::NetworkState;

use crate::{NetworkProvider, PlatformError};

/// Reads network state from Windows.
pub(crate) struct WindowsProvider;

impl WindowsProvider {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl NetworkProvider for WindowsProvider {
    fn snapshot(&self) -> Result<NetworkState, PlatformError> {
        // Route discovery is not implemented yet.
        Ok(NetworkState::new(adapters::interfaces()?, Vec::new()).captured_now())
    }

    fn platform_name(&self) -> &'static str {
        "windows-iphlpapi"
    }
}
