//! Termination handling shared by the long-running commands.
//!
//! Two commands outlive a single snapshot — `run`, which waits on a child, and
//! `watch`, which waits on the network — and both need Ctrl-C to mean "stop
//! tidily" rather than "die where you stand". They cannot each install their
//! own handler: `ctrlc::set_handler` may be called only once per process, so
//! the second call would fail at runtime. One installer, called by whichever
//! command is running, is the only shape that works.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::CliError;

/// Arrange for a termination signal to set a flag instead of killing AutoNet.
///
/// The default disposition would kill the process immediately, which loses
/// whatever the caller still had to do — reporting the child's exit code in
/// `run`, withdrawing an advertisement in `advertise`. The flag lets the
/// caller's own loop notice and finish.
///
/// Nothing here signals anything else, and no caller needs it to: a terminal
/// Ctrl-C is delivered by the operating system to every process in the
/// foreground group, so a child launched by `run` has already received a real
/// SIGINT of its own and can shut down gracefully.
///
/// # Errors
///
/// Returns [`CliError::Usage`] if the handler cannot be installed, which
/// includes a second call in the same process.
pub(crate) fn install_signal_flag() -> Result<Arc<AtomicBool>, CliError> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&interrupted);

    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))
        .map_err(|error| CliError::Usage(format!("cannot install a signal handler: {error}")))?;

    Ok(interrupted)
}
