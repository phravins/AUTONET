//! The Linux backend, built on rtnetlink.
//!
//! Chosen over shelling out to `ip -j`, which would depend on iproute2 being
//! installed and on its JSON staying stable, and over `getifaddrs`, which
//! reports addresses but not routes. `rtnetlink` also subscribes to netlink
//! multicast groups, which is what `autonet watch` needs in M4.

mod netlink;
mod sysfs;

use autonet_core::model::NetworkState;
use tokio::runtime::Runtime;

use crate::{NetworkProvider, PlatformError};

/// Reads network state from the Linux kernel over netlink.
pub(crate) struct LinuxProvider {
    /// A private, single-threaded runtime.
    ///
    /// Built once and reused: a runtime per snapshot would cost a thread and an
    /// epoll instance on every `autonet ip`. `current_thread` because netlink is
    /// one socket doing three short dumps.
    runtime: Runtime,
}

impl LinuxProvider {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .map_err(|e| PlatformError::query("start the netlink runtime", e))?;
        Ok(Self { runtime })
    }
}

impl NetworkProvider for LinuxProvider {
    fn snapshot(&self) -> Result<NetworkState, PlatformError> {
        self.runtime.block_on(netlink::snapshot())
    }

    fn platform_name(&self) -> &'static str {
        "linux-netlink"
    }
}
