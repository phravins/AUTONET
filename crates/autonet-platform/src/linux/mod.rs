//! The Linux backend, built on rtnetlink.
//!
//! Linux exposes network configuration through netlink rather than a stable
//! C API, and `rtnetlink` is the mature client for it. It was chosen over
//! shelling out to `ip -j` (which would make AutoNet depend on iproute2 being
//! installed and on its JSON staying stable) and over `getifaddrs` (which
//! reports addresses but not routes — and routes are the strongest signal that
//! an interface can actually reach anything).
//!
//! It also subscribes to netlink multicast groups, which is exactly what
//! `autonet watch` needs in M4. That is the second reason for the choice: the
//! change-notification story is already in the crate we are using.

mod netlink;
mod sysfs;

use autonet_core::model::NetworkState;
use tokio::runtime::Runtime;

use crate::{NetworkProvider, PlatformError};

/// Reads network state from the Linux kernel over netlink.
pub(crate) struct LinuxProvider {
    /// A private, single-threaded runtime.
    ///
    /// Built once and reused: creating a runtime per snapshot would cost a
    /// thread and an epoll instance on every `autonet ip`, and `watch` will
    /// call `snapshot` in a loop. It is `current_thread` because netlink is one
    /// socket doing three short dumps — a work-stealing pool would add
    /// scheduling overhead and buy nothing.
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
