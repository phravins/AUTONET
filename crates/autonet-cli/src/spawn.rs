//! `autonet run` — launch a command with the selected address in its
//! environment.
//!
//! A launcher, not a supervisor. It resolves an address once, starts the
//! command, waits, and exits with the command's own exit code. It never
//! restarts the child and never rewrites its environment, because the
//! environment of a running process cannot be rewritten from outside it and
//! pretending otherwise would mean killing the thing the user asked to run.
//! See `docs/adr/0001-network-change-during-autonet-run.md`.
//!
//! `--state-file` is that ADR's designated extension, not a departure from it.
//! It adds a second, opt-in channel beside the environment variables: a file
//! kept current while the child runs, which the child may re-read on its own
//! schedule. Nothing about the first channel changes — the variables are still
//! a launch-time snapshot, the child is still spawned once, and AutoNet still
//! never restarts or signals it. The state file gives a program that wants
//! liveness a way to have it *by its own choosing*, which is the distinction
//! the ADR turned on.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command as StdCommand, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use autonet_core::select::{select, SelectedAddress};

use crate::cli::GlobalArgs;
use crate::commands::{check_requested_interface, selection_document, Context};
use crate::port;
use crate::signal::install_signal_flag;
use crate::state::StateFile;
use crate::watch;
use crate::{exit, CliError};

/// How long the child is given to act on the signal it already received before
/// it is killed outright.
///
/// Only reached when the signal did *not* arrive through the terminal's process
/// group — a direct `kill` of the AutoNet process, say — because in that case
/// the child never saw it and would otherwise be waited on forever.
const GRACE: Duration = Duration::from_secs(5);

/// How often the wait loop wakes to re-check the signal flag.
const POLL: Duration = Duration::from_millis(50);

/// Run a command with `AUTONET_*` in its environment.
pub fn run(
    ctx: &Context,
    args: &GlobalArgs,
    command: &[String],
    state_file: Option<&Path>,
) -> Result<(), CliError> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| CliError::Usage("no command given. Try: autonet run -- npm start".into()))?;

    // Resolve before spawning. A machine with no usable address should fail
    // here, with the selector's own explanation, rather than start a server
    // that then reports an address nobody can reach.
    let state = ctx.snapshot()?;
    check_requested_interface(ctx, &state)?;
    let selection = select(&state, &ctx.config.selection);
    let Some(selected) = selection.selected else {
        return Err(CliError::NoAddress(
            selection.failure_reason(&ctx.config.selection),
        ));
    };

    // The port as it will actually be rendered: the flag if given, otherwise
    // `output.default_port`. Read once, so the warning below and the variable
    // handed to the child can never describe different ports.
    //
    // This uses the *parsed* flag, which clap separated from the trailing
    // command at the `--` boundary before this function was entered. So
    // `autonet run -- node server.js --port 3000` leaves this `None`: the port
    // belongs to the child, nothing is probed, and the argument is passed
    // through untouched.
    let port = args.port(&ctx.config);

    // Before the spawn, so a busy port is AutoNet's clear warning rather than
    // the child's bind failure several seconds later. A warning and not a
    // refusal: see `crate::port`.
    if let Some(port) = port {
        let check = port::inspect(selected.ip, port);
        if let Some(warning) = port::describe(&check, port) {
            eprintln!("autonet: {}: {warning}", ctx.theme.warn("warning"));
        }
    }

    // Before the spawn, not after. A Ctrl-C landing in the window between the
    // two would otherwise kill AutoNet at its default disposition and leave the
    // child running with nobody waiting on it.
    let interrupted = install_signal_flag()?;

    // Opened and written *before* the spawn, for the same reason the address is
    // resolved before it: a path that cannot be written should fail here, with
    // nothing launched, rather than leave a child polling for a file that will
    // never appear. It also means the file is already there for a program that
    // reads it during its own startup.
    let tracked = match state_file {
        Some(path) => {
            let file = StateFile::create(path)?;
            let opening = selection_document(
                ctx.provider.platform_name(),
                state.captured_at,
                Some(&selected),
                None,
                port,
            );
            file.write(&opening).map_err(|error| {
                CliError::Usage(format!(
                    "cannot write the state file {}: {error}",
                    file.path().display()
                ))
            })?;
            Some(file)
        }
        None => None,
    };

    // An argument vector, never a shell string: `program` names a file to
    // execute, and every element of `arguments` reaches it as one argument
    // however it is spelled. Nothing here interprets quotes, globs or `;`.
    let mut child = StdCommand::new(program)
        .args(arguments)
        .envs(env_vars(
            &selected,
            port,
            tracked.as_ref().map(StateFile::path),
        ))
        .spawn()
        .map_err(|error| spawn_error(program, &error))?;

    let (status, was_interrupted) = match &tracked {
        None => {
            let status = wait_for(&mut child, &interrupted);
            let seen = interrupted.load(Ordering::SeqCst);
            (status, seen)
        }
        // A scope rather than a detached thread: the tracker borrows `ctx`, and
        // joining it here is what guarantees nothing is still writing the file
        // when the removal below takes it away.
        Some(file) => std::thread::scope(|scope| {
            scope.spawn(|| track(ctx, args, file));

            let status = wait_for(&mut child, &interrupted);

            // Read before it is set, below. This is the value that decides
            // whether a child with no exit code was terminated or merely
            // failed, and setting the flag first would answer "terminated"
            // every time the state file was in use.
            let seen = interrupted.load(Ordering::SeqCst);

            // Now wake the tracker, so this scope can join. It is watching the
            // network on behalf of a program that has stopped reading. Never
            // skipped and never conditional: the scope does not return until
            // the tracker does, so a path that left the flag clear would hang.
            interrupted.store(true, Ordering::SeqCst);

            (status, seen)
        }),
    };

    // Before the `?` below, so the file goes away even when the wait itself
    // failed. The alternative is a file left behind claiming to be current
    // because the cleanup path was the one that did not run.
    if let Some(file) = &tracked {
        file.remove();
    }

    match exit_code(status?.code(), was_interrupted) {
        0 => Ok(()),
        code => Err(CliError::ChildExit(code)),
    }
}

/// Keep the state file current for as long as the child is running.
///
/// A consumer of [`watch::observe`], not a second detector: the file moves when
/// and only when `autonet watch` would have printed a line, using the same
/// snapshot, the same diff and the same event source. Anything else would be a
/// third opinion about what "the address changed" means.
///
/// Returns nothing, and cannot fail the command. The child is already running;
/// ADR 0001 forbids killing it, and failing `run` while it lives would report
/// an exit code for a process that had not exited. What a failure does instead
/// is take the file away and say so — see [`StateFile`] for why absence is the
/// honest answer.
fn track(ctx: &Context, args: &GlobalArgs, file: &StateFile) {
    let outcome = watch::observe(ctx, |change| {
        let document = selection_document(
            ctx.provider.platform_name(),
            change.captured_at,
            change.current,
            change.failure,
            args.port(&ctx.config),
        );

        if let Err(error) = file.write(&document) {
            file.remove();
            eprintln!(
                "autonet: cannot update the state file {}: {error}",
                file.path().display()
            );
        }

        Ok(())
    });

    // `Ok` is the interrupted case, where `run` is already on its way out and
    // will remove the file itself.
    if let Err(error) = outcome {
        file.remove();
        if let Some(message) = error.message() {
            eprintln!("autonet: the state file is no longer being updated: {message}");
        }
    }
}

/// The variables injected into the child, and nothing else.
///
/// Pure so that it is testable without spawning anything: the value of this
/// function is the exact contract `autonet run` advertises, and the IPv6
/// bracketing in particular is the sort of thing that is wrong for a year
/// before anyone notices.
fn env_vars(
    selected: &SelectedAddress,
    port: Option<u16>,
    state_file: Option<&Path>,
) -> Vec<(&'static str, OsString)> {
    let mut vars: Vec<(&'static str, OsString)> = vec![
        // Bare, so `ping $AUTONET_IP` and `--host $AUTONET_IP` work. An IPv6
        // address is *not* bracketed here; that is what AUTONET_HOST is for.
        ("AUTONET_IP", selected.ip.to_string().into()),
        // URL-safe: identical to AUTONET_IP for IPv4, bracketed for IPv6.
        ("AUTONET_HOST", selected.url_host().into()),
    ];

    // Only with a port. A URL without one would either be wrong or invite the
    // caller to append a port to a string that may already have a colon in it.
    if let Some(port) = port {
        // `None`: `run` publishes no name. ADR 0001 is explicit that this
        // variable is a launch-time snapshot, and a `.local` name here would
        // imply a freshness the child does not get.
        vars.push((
            "AUTONET_URL",
            crate::url::network_url(selected, port, None).into(),
        ));
    }

    // Only when `--state-file` was given. Its absence is how a program tells
    // that it has the three-variable snapshot and nothing more, which is the
    // default and remains the right answer for almost everything.
    //
    // `OsString`, not a lossy `display()`: this is a path the child is expected
    // to open, and a byte of it replaced with U+FFFD is a path to somewhere
    // else.
    if let Some(path) = state_file {
        vars.push(("AUTONET_STATE_FILE", path.as_os_str().to_os_string()));
    }

    vars
}

/// Wait for the child, giving it a grace period if a signal it may not have
/// seen arrives.
fn wait_for(
    child: &mut std::process::Child,
    interrupted: &AtomicBool,
) -> Result<ExitStatus, CliError> {
    let mut deadline: Option<Instant> = None;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                return Err(CliError::Usage(format!(
                    "cannot wait for the command: {error}"
                )))
            }
        }

        if interrupted.load(Ordering::SeqCst) {
            match deadline {
                // A signal delivered through the terminal reached the child too,
                // so the usual outcome is that it exits during this window and
                // the kill below never happens.
                None => deadline = Some(Instant::now() + GRACE),
                Some(at) if Instant::now() >= at => {
                    // Reached only when the child never got the signal, which
                    // means it was sent to the AutoNet process alone. Waiting
                    // longer would hang instead of ending.
                    let _ = child.kill();
                    return child.wait().map_err(|error| {
                        CliError::Usage(format!("cannot reap the command: {error}"))
                    });
                }
                Some(_) => {}
            }
        }

        std::thread::sleep(POLL);
    }
}

/// Map the child's reported code onto a process exit code.
///
/// Takes `ExitStatus::code()` rather than the status itself so that it is a
/// pure function of two values. `ExitStatus` cannot be constructed portably,
/// and reaching for one in a test would drag a `cfg` into the CLI that
/// architecture.md rule 2 keeps out of it.
fn exit_code(code: Option<i32>, interrupted: bool) -> u8 {
    match code {
        // Unix reports no code when a process dies from a signal, and reading
        // *which* signal needs `std::os::unix`, which rule 2 keeps out of the
        // CLI. The flag is the part we do know: our handler ran, so this was a
        // termination request, and 130 is the shell's convention for one. It is
        // reported for SIGTERM and SIGHUP too, where a shell would say 143 or
        // 129 -- distinguishing them is exactly the platform detail we do not
        // have. Better a consistent "terminated" than a fabricated precision.
        None => {
            if interrupted {
                130
            } else {
                exit::FAILED
            }
        }
        // Windows exit codes are a full i32 and are routinely values like
        // 0xC0000005, which no u8 can carry. Truncating would turn a crash into
        // whatever its low byte happens to be, including zero, so an
        // unrepresentable code is reported as a plain failure instead.
        Some(code) => u8::try_from(code).unwrap_or(exit::FAILED),
    }
}

/// Explain a failure to start, distinguishing "no such command" from the rest.
fn spawn_error(program: &str, error: &std::io::Error) -> CliError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return CliError::Usage(format!("command not found: {program}"));
    }
    CliError::Usage(format!("cannot run {program}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use autonet_core::model::{AddressScope, Family, InterfaceKind};

    use super::*;

    fn selected(ip: IpAddr, family: Family) -> SelectedAddress {
        SelectedAddress {
            ip,
            family,
            prefix_len: 24,
            scope: AddressScope::Private,
            interface: "wlan0".into(),
            interface_index: 3,
            interface_kind: InterfaceKind::Wireless,
            gateway: None,
            score: 1000,
        }
    }

    fn v4() -> SelectedAddress {
        selected(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), Family::V4)
    }

    fn v6() -> SelectedAddress {
        selected(
            IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            Family::V6,
        )
    }

    fn lookup<'a>(vars: &'a [(&'static str, OsString)], name: &str) -> Option<&'a str> {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_str().expect("test values are UTF-8"))
    }

    #[test]
    fn a_port_is_required_before_a_url_is_offered() {
        // A URL without a port would either be wrong or invite the caller to
        // append one to a string that may already contain a colon.
        let vars = env_vars(&v4(), None, None);
        assert_eq!(lookup(&vars, "AUTONET_URL"), None);
        assert_eq!(lookup(&vars, "AUTONET_IP"), Some("192.168.1.20"));
    }

    #[test]
    fn a_port_produces_a_url_that_can_be_opened() {
        let vars = env_vars(&v4(), Some(3000), None);
        assert_eq!(
            lookup(&vars, "AUTONET_URL"),
            Some("http://192.168.1.20:3000")
        );
    }

    #[test]
    fn only_the_url_form_of_an_ipv6_address_is_bracketed() {
        // `ping $AUTONET_IP` needs the bare form and `curl $AUTONET_URL` needs
        // the bracketed one. Conflating them breaks one of the two.
        let vars = env_vars(&v6(), Some(8080), None);
        assert_eq!(lookup(&vars, "AUTONET_IP"), Some("fd00::1"));
        assert_eq!(lookup(&vars, "AUTONET_HOST"), Some("[fd00::1]"));
        assert_eq!(lookup(&vars, "AUTONET_URL"), Some("http://[fd00::1]:8080"));
    }

    #[test]
    fn an_ipv4_host_is_not_decorated() {
        let vars = env_vars(&v4(), None, None);
        assert_eq!(lookup(&vars, "AUTONET_HOST"), Some("192.168.1.20"));
    }

    #[test]
    fn nothing_beyond_the_three_documented_variables_is_injected() {
        // The child's environment is the user's, not a place to leave notes.
        let names: Vec<&str> = env_vars(&v4(), Some(1), None)
            .iter()
            .map(|(key, _)| *key)
            .collect();
        assert_eq!(names, ["AUTONET_IP", "AUTONET_HOST", "AUTONET_URL"]);
    }

    #[test]
    fn the_state_file_adds_one_variable_and_only_when_it_was_asked_for() {
        // Injecting AUTONET_STATE_FILE unconditionally would tell every child
        // that a file exists, and almost every child would be wrong. Its
        // absence is how a program knows it has the snapshot and nothing more.
        let names = |vars: &[(&'static str, OsString)]| -> Vec<&'static str> {
            vars.iter().map(|(key, _)| *key).collect()
        };

        assert_eq!(
            names(&env_vars(&v4(), Some(1), None)),
            ["AUTONET_IP", "AUTONET_HOST", "AUTONET_URL"]
        );
        assert_eq!(
            names(&env_vars(
                &v4(),
                Some(1),
                Some(Path::new("/srv/app/.autonet/current.json"))
            )),
            [
                "AUTONET_IP",
                "AUTONET_HOST",
                "AUTONET_URL",
                "AUTONET_STATE_FILE"
            ]
        );
    }

    #[test]
    fn the_state_file_path_reaches_the_child_exactly_as_given() {
        // The child opens this string. Anything that reshapes it -- a lossy
        // conversion, a trailing separator, a quote -- is a path to a file
        // that is not the one AutoNet is writing.
        let vars = env_vars(
            &v4(),
            None,
            Some(Path::new("/srv/app/.autonet/current.json")),
        );
        assert_eq!(
            lookup(&vars, "AUTONET_STATE_FILE"),
            Some("/srv/app/.autonet/current.json")
        );
    }

    #[test]
    fn a_signalled_child_reports_the_shells_conventional_code() {
        // `ExitStatus::code()` is None when a signal killed the child. Our own
        // flag is the only evidence available about which signal it was.
        assert_eq!(exit_code(None, true), 130);
        assert_eq!(exit_code(None, false), exit::FAILED);
    }

    #[test]
    fn a_child_that_exited_normally_keeps_its_code_even_after_a_ctrl_c() {
        // The interrupt flag is a fallback for an unreadable status, not an
        // override. A child that handled SIGINT and exited 0 exited 0.
        assert_eq!(exit_code(Some(0), true), 0);
        assert_eq!(exit_code(Some(3), true), 3);
    }

    #[test]
    fn an_exit_code_too_large_for_a_byte_is_a_failure_not_its_low_byte() {
        // Windows reports i32 codes. STATUS_ACCESS_VIOLATION (0xC0000005,
        // i.e. -1073741819) truncates to 0x05, and 0xC0000100 (-1073741568)
        // truncates to zero, which would report a crash as a success. Written
        // signed because that is the type the platform actually hands back.
        assert_eq!(exit_code(Some(-1_073_741_819), false), exit::FAILED);
        assert_eq!(exit_code(Some(-1_073_741_568), false), exit::FAILED);
        assert_eq!(exit_code(Some(-1), false), exit::FAILED);
        assert_eq!(exit_code(Some(7), false), 7);
        assert_eq!(exit_code(Some(0), false), 0);
    }
}
