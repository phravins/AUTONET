//! Windows network discovery through the IP Helper API.

mod adapters;
mod iftable;
mod route;
pub(crate) mod tcptable;

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
        // The adapter walk first: it assigns the indices routes are joined to.
        let adapters = adapters::interfaces()?;
        let routes = route::dump_routes(&adapters.links)?;

        Ok(NetworkState::new(adapters.interfaces, routes).captured_now())
    }

    fn platform_name(&self) -> &'static str {
        "windows-iphlpapi"
    }
}
