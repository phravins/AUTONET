//! A source of "the network moved, look again" notifications.
//!
//! Deliberately separate from [`NetworkProvider`](crate::NetworkProvider),
//! which `docs/adr/0001-network-change-during-autonet-run.md` froze at two
//! methods with no subscription and no callback. Widening a frozen trait to
//! serve one platform would make every backend carry a method only one of them
//! can answer.
//!
//! # Why this reports nothing about what changed
//!
//! A [`ChangeSource`] answers one question — "is it worth looking again?" — and
//! it never says what moved. That looks like throwing information away, and it
//! is, on purpose.
//!
//! The kernel's own change messages are rich enough to build a diff from
//! directly, and doing so would mean writing a second implementation of
//! [`autonet_core::event::diff`] behind a `#[cfg(target_os = "linux")]`. That
//! collides with the rule `docs/architecture.md` states for this crate: a
//! backend translates, it does not decide, because policy behind a `#[cfg]` is
//! policy no fixture can test. Two diff implementations would also be two
//! implementations that can disagree, and the one nobody can fixture-test is
//! the one that would drift.
//!
//! So the event is used only for its timing. Both platforms then run the
//! identical `snapshot()` → `diff()` → `select()` pipeline, and the event
//! source changes *when* that runs, never *what it concludes*.

use std::time::Duration;

use crate::PlatformError;

/// Something that can block until the operating system reports a change.
pub trait ChangeSource: Send {
    /// Block until the kernel reports a change, or until `timeout` elapses.
    ///
    /// `Ok(true)` means something moved and the caller should take a fresh
    /// snapshot. `Ok(false)` means the timeout won and nothing was reported;
    /// the caller may still look, and should, because a source can miss things.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the underlying subscription failed. A
    /// caller that can fall back to polling should do so rather than give up:
    /// losing the event source costs latency, not correctness.
    fn wait(&mut self, timeout: Duration) -> Result<bool, PlatformError>;

    /// A short name for the mechanism, so `autonet watch` can say which one it
    /// is actually using instead of leaving the user to guess.
    fn source_name(&self) -> &'static str;
}

/// Build the change source for the current platform, if it has one.
///
/// `Ok(None)` is the ordinary answer on a platform with no native event
/// source: macOS and Windows have none here, and their callers poll. It is not
/// a failure and nothing needs reporting.
///
/// # Errors
///
/// Returns [`PlatformError`] when a platform that *does* have a source could
/// not open it — a netlink socket refused inside a restricted container, say.
/// Distinguished from `Ok(None)` so that a caller can say which of the two
/// happened, since one is normal and the other is worth a line on stderr.
pub fn change_source() -> Result<Option<Box<dyn ChangeSource>>, PlatformError> {
    #[cfg(target_os = "linux")]
    {
        Ok(Some(
            Box::new(crate::linux::monitor::NetlinkMonitor::new()?),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(None)
    }
}
