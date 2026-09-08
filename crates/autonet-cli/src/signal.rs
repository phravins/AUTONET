//! Termination handling shared by the long-running commands.
//!
//! Two commands outlive a single snapshot — `run`, which waits on a child, and
//! `watch`, which waits on the network — and both need Ctrl-C to mean "stop
//! tidily" rather than "die where you stand". They cannot each install their
//! own handler: `ctrlc::set_handler` may be called only once per process, so a
//! second call fails. One installer, shared by whichever commands are running,
//! is the only shape that works.
//!
//! It is shared rather than exclusive because `autonet run --state-file` runs
//! both at once: the child wait on the main thread and the network watch on a
//! second one. Handing both the same flag is also what makes Ctrl-C stop both.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::CliError;

/// The one flag, installed on first request and handed to every later caller.
static INTERRUPTED: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Arrange for a termination signal to set a flag instead of killing AutoNet.
///
/// The default disposition would kill the process immediately, which loses
/// whatever the caller still had to do — reporting the child's exit code in
/// `run`, withdrawing an advertisement in `advertise`, removing the state file
/// in either. The flag lets the caller's own loop notice and finish.
///
/// Idempotent: the second and later calls return the same flag rather than
/// installing a second handler, so a command that needs two loops can ask
/// twice. Every holder of the flag sees the same interrupt, which is the
/// behaviour those loops want — one Ctrl-C stops all of them.
///
/// Nothing here signals anything else, and no caller needs it to: a terminal
/// Ctrl-C is delivered by the operating system to every process in the
/// foreground group, so a child launched by `run` has already received a real
/// SIGINT of its own and can shut down gracefully.
///
/// # Errors
///
/// Returns [`CliError::Usage`] if the handler cannot be installed. Only the
/// first call can fail this way; once one has succeeded there is nothing left
/// to install.
pub(crate) fn install_signal_flag() -> Result<Arc<AtomicBool>, CliError> {
    // Held across the install so that two threads asking at once cannot both
    // reach `set_handler`, where the loser would get a spurious failure for a
    // handler that is, in fact, installed.
    let mut installed = INTERRUPTED.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(interrupted) = installed.as_ref() {
        return Ok(Arc::clone(interrupted));
    }

    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&interrupted);

    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))
        .map_err(|error| CliError::Usage(format!("cannot install a signal handler: {error}")))?;

    *installed = Some(Arc::clone(&interrupted));
    Ok(interrupted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_twice_hands_back_the_one_flag_rather_than_failing() {
        // `autonet run --state-file` asks once for its child wait and once,
        // from `watch::observe`, for its network watch. Two separate flags
        // would mean Ctrl-C stopped only one of the two loops.
        // `CliError` carries no `Debug` — its only rendering is the line it
        // prints on stderr — so the message is unwrapped by hand.
        let install = || {
            install_signal_flag()
                .unwrap_or_else(|error| panic!("{}", error.message().unwrap_or_default()))
        };

        let first = install();
        let second = install();

        assert!(Arc::ptr_eq(&first, &second));

        first.store(true, Ordering::SeqCst);
        assert!(
            second.load(Ordering::SeqCst),
            "both holders see one interrupt"
        );
        // Left as this process found it, so an unrelated test that reads the
        // flag is not told the test binary was interrupted.
        first.store(false, Ordering::SeqCst);
    }
}
