//! A netlink multicast subscription: the kernel says when to look again.
//!
//! This is `ip monitor`, reduced to the one bit AutoNet wants from it. The
//! socket joins the link, address and route groups, and every message that
//! arrives on it means the same thing: take a fresh snapshot.
//!
//! Nothing here decodes a message. The stream's item type is erased to `()` at
//! construction, one line below where the socket is opened, so "the payload is
//! never read" is enforced by the type system rather than promised in a
//! comment — there is no message left to be tempted by. The reasoning is in
//! `crate::change`.

use std::time::Duration;

use futures_util::stream::{Stream, StreamExt};
use rtnetlink::MulticastGroup;
use tokio::runtime::Runtime;

use crate::change::ChangeSource;
use crate::PlatformError;

/// The groups worth waking for.
///
/// Link state, addresses on both families, and routes on both families. That
/// is exactly the set `autonet_core::event::diff` compares, so waking for
/// anything else would be waking to conclude nothing changed.
///
/// Neighbour (ARP/NDP) and traffic-control groups are deliberately absent:
/// they are the loudest groups on a busy machine and cannot move the selected
/// address.
const GROUPS: [MulticastGroup; 5] = [
    MulticastGroup::Link,
    MulticastGroup::Ipv4Ifaddr,
    MulticastGroup::Ipv6Ifaddr,
    MulticastGroup::Ipv4Route,
    MulticastGroup::Ipv6Route,
];

/// How long to keep swallowing messages after the first one before looking.
///
/// One user-visible change is many netlink messages. Associating with a Wi-Fi
/// network produces a link state change, an address, a route and usually a
/// second address for IPv6, over a few tens of milliseconds. Snapshotting on
/// the first of them would catch the machine mid-reconfiguration and report an
/// interface that is up with no address yet, then report the address as a
/// second change moments later.
///
/// A quarter of a second is long enough to coalesce that burst and short
/// enough to stay far below the two-second poll it replaces. It is not long
/// enough to hide a genuinely slow sequence — a DHCP lease that arrives
/// seconds after the link comes up is two real changes, and is reported as two.
const SETTLE: Duration = Duration::from_millis(250);

/// A netlink socket subscribed to the groups that can move the answer.
pub(crate) struct NetlinkMonitor {
    /// A private, single-threaded runtime, as [`LinuxProvider`] holds.
    ///
    /// The connection task spawned onto it only makes progress while
    /// [`ChangeSource::wait`] is blocked on it. That is not a problem and is
    /// almost the point: between waits, messages accumulate in the kernel's
    /// own socket buffer rather than in ours.
    ///
    /// [`LinuxProvider`]: crate::linux::LinuxProvider
    runtime: Runtime,

    /// Wake-ups, with the message they came from already discarded.
    wakeups: Wakeups,
}

/// A stream of bare wake-ups. See the module comment for why the netlink
/// message that produced each one is not here.
type Wakeups = Box<dyn Stream<Item = ()> + Send + Unpin>;

impl NetlinkMonitor {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            // `wait` bounds itself with tokio::time, both for the caller's
            // ceiling and for the settle window above.
            .enable_time()
            .build()
            .map_err(|e| PlatformError::query("start the netlink monitor runtime", e))?;

        // Inside `enter()` because netlink-sys registers the socket with the
        // *ambient* tokio reactor as it is constructed, and panics outright if
        // there is none. `netlink::snapshot` never had to think about this: it
        // opens its connection inside an async fn, so it is already in context.
        // Here the socket outlives every individual call, so the runtime is
        // entered deliberately for the one statement that needs it.
        //
        // Found by running the binary, not by the type checker, which is happy
        // either way.
        let (connection, _handle, messages) = {
            let _reactor = runtime.enter();
            rtnetlink::new_multicast_connection(&GROUPS)
                .map_err(|e| PlatformError::query("subscribe to netlink change events", e))?
        };

        // The connection future drives the socket; without it nothing is ever
        // received. Unlike the per-snapshot dumps in `netlink::snapshot`, this
        // one lives as long as the monitor: it is not aborted here, because
        // dropping the runtime stops it, and the monitor owns the runtime.
        //
        // The handle is dropped. It exists to *send* requests, and this socket
        // only ever listens.
        runtime.spawn(connection);

        Ok(Self {
            runtime,
            // Where the payload goes. Every downstream type from here on knows
            // only that something happened.
            wakeups: Box::new(messages.map(|_| ())),
        })
    }
}

impl ChangeSource for NetlinkMonitor {
    fn wait(&mut self, timeout: Duration) -> Result<bool, PlatformError> {
        // Destructured so the async block borrows the two fields separately;
        // `self.runtime.block_on(async { self.wakeups… })` would try to
        // borrow all of `self` twice.
        let Self { runtime, wakeups } = self;

        runtime.block_on(async {
            match tokio::time::timeout(timeout, wakeups.next()).await {
                // Nothing arrived within the caller's ceiling.
                Err(_) => return Ok(false),
                // The stream ended, which means the connection task is gone and
                // no further event will ever arrive. Reported rather than
                // returned as a quiet `false`, which would look like an idle
                // network forever.
                Ok(None) => {
                    return Err(PlatformError::Query {
                        operation: "watch netlink for changes",
                        message: "the netlink subscription closed unexpectedly".to_string(),
                    })
                }
                Ok(Some(())) => {}
            }

            // Let the burst finish. See SETTLE.
            let settle = tokio::time::sleep(SETTLE);
            tokio::pin!(settle);
            loop {
                tokio::select! {
                    // Biased so that a machine producing messages faster than
                    // this loop consumes them still reaches its deadline; an
                    // unbiased select could keep choosing the ready message
                    // branch indefinitely.
                    biased;
                    () = &mut settle => break,
                    message = wakeups.next() => {
                        if message.is_none() {
                            break;
                        }
                    }
                }
            }

            Ok(true)
        })
    }

    fn source_name(&self) -> &'static str {
        "linux-netlink-monitor"
    }
}
