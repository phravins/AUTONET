//! `autonet dev` — run a project, and keep its address file current.
//!
//! ADR 0001 still holds: the child is started once, never restarted, and never
//! signalled because of a network change. What changes on a move is a file, which
//! is the opt-in live channel that ADR named, and the framework's own reload does
//! the rest. See `docs/adr/0004-project-integration.md`.
//!
//! Everything AutoNet says goes to stderr, because stdout belongs to the dev
//! server. `flutter run` also has the terminal's stdin, for its `r` key.

use std::io::{IsTerminal as _, Write as _};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use autonet_core::generate::Values;
use autonet_core::model::{AddressScope, Family};
use autonet_core::project::{notes, plan, DevPlan, EnvValue, Manifest, Refusal, NO_PORT_NOTE};
use autonet_core::select::{select, SelectedAddress};
use autonet_platform::{request_stop, resolve_program};

use crate::autodetect::{read_facts, read_manifest};
use crate::check::scan_project;
use crate::cli::GlobalArgs;
use crate::commands::{check_requested_interface, Context};
use crate::render::Theme;
use crate::signal::install_signal_flag;
use crate::spawn::{exit_code, spawn_error, GRACE, POLL};
use crate::watch::{self, Change};
use crate::{outputs, port, qr, CliError};

/// How long a signal from the terminal gets to do its work before AutoNet assumes
/// it was sent to AutoNet alone, and passes it on.
const SIGNAL_REACH: Duration = Duration::from_secs(1);

/// What `dev` needs on hand each time the network moves.
struct Session<'a> {
    root: &'a Path,
    plan: &'a DevPlan,
    port: Option<u16>,
    show_qr: bool,
    theme: Theme,
    /// The family the server was told to listen on, if it was told.
    bound_family: Option<Family>,
}

/// Run the project in the current directory.
pub fn dev(
    ctx: &Context,
    args: &GlobalArgs,
    allow_public: bool,
    no_qr: bool,
) -> Result<(), CliError> {
    if args.json {
        return Err(CliError::Usage(
            "dev has no --json form, because the dev server's own output shares the terminal. \
             Use `autonet watch --json` to follow the address."
                .into(),
        ));
    }

    let root = std::env::current_dir()
        .map_err(|error| CliError::Usage(format!("cannot read the current directory: {error}")))?;
    let facts = read_facts(&root)?;
    let manifest = read_manifest(&root)?;

    let state = ctx.snapshot()?;
    check_requested_interface(ctx, &state)?;
    let selected = select(&state, &ctx.config.selection).selected;

    let family = selected.as_ref().map(|selected| selected.family);
    let bind = bind_address(family);
    let dev_plan = plan(
        &facts,
        manifest.as_ref(),
        args.port.filter(|port| *port != 0),
        bind,
    )
    .map_err(|Refusal(message)| CliError::Usage(message))?;
    let port = dev_plan.port.or_else(|| args.port(&ctx.config));

    if let Some(selected) = &selected {
        if !dev_plan.may_start_on(selected.scope, allow_public) {
            return Err(CliError::Usage(format!(
                "{} is reachable from the internet, so a dev server listening on every interface would be too. \
                 Nothing was started. Pass --allow-public if that is what you want.",
                selected.ip
            )));
        }
    }

    // All of them before any of them, so a refusal changes nothing.
    outputs::check_ownership(&root, &dev_plan.outputs)?;

    let values = selected.as_ref().map_or_else(
        || outputs::offline_values(port),
        |selected| outputs::current_values(selected, port),
    );
    for output in &dev_plan.outputs {
        outputs::write(&root, output, &values)
            .map_err(|error| CliError::Usage(format!("cannot write {}: {error}", output.path)))?;
    }

    let session = Session {
        root: &root,
        plan: &dev_plan,
        port,
        show_qr: !no_qr && (args.qr || std::io::stderr().is_terminal()),
        theme: ctx.theme,
        bound_family: dev_plan
            .binds_wildcard
            .then(|| family.unwrap_or(Family::V4)),
    };

    print_opening(&session, manifest.as_ref(), selected.as_ref(), &values);
    for note in notes(dev_plan.kind, &facts) {
        say_line(&format!("{} {note}", ctx.theme.label("Note    ")));
    }
    if port.is_none() && !dev_plan.outputs.is_empty() {
        say_line(&format!("{} {NO_PORT_NOTE}", ctx.theme.label("Note    ")));
    }
    say_line("");

    // Before the spawn: a Ctrl-C between the two would otherwise kill AutoNet and
    // leave the server running with nobody waiting on it.
    let interrupted = install_signal_flag()?;

    match &dev_plan.command {
        Some(command) => run_primary(ctx, &session, command, values, bind, &interrupted),
        None if dev_plan.background.is_empty() => {
            say("Keeping the file current. Ctrl-C to stop.");
            watch::observe(ctx, track(&session, values))
        }
        None => run_background_only(ctx, &session, values, bind, &interrupted),
    }
}

/// Run the project's own command, keeping its files current until it ends.
fn run_primary(
    ctx: &Context,
    session: &Session,
    command: &[String],
    values: Values,
    bind: &str,
    interrupted: &AtomicBool,
) -> Result<(), CliError> {
    let mut background = start_background(session, &values, bind)?;

    let mut primary = match spawn(command, session, &values, bind, false) {
        Ok(child) => child,
        Err(error) => {
            stop_all(&mut background);
            return Err(error);
        }
    };

    let (status, was_interrupted) = std::thread::scope(|scope| {
        scope.spawn(|| {
            if let Err(error) = watch::observe(ctx, track(session, values)) {
                if let Some(message) = error.message() {
                    say(&format!(
                        "the address file is no longer being updated: {message}"
                    ));
                }
            }
        });

        let status = wait_for_primary(&mut primary, &mut background, interrupted);
        let seen = interrupted.load(Ordering::SeqCst);
        stop_all(&mut background);
        // Wakes the observer, so this scope can join. Never skipped: the scope
        // does not return until the observer does.
        interrupted.store(true, Ordering::SeqCst);
        (status, seen)
    });

    // Ctrl-C is how this command is meant to end, and npm reports being
    // interrupted as a failure of its own. A server that fell over by itself still
    // reports its own code.
    if was_interrupted {
        return Ok(());
    }
    match exit_code(status?.code(), false) {
        0 => Ok(()),
        code => Err(CliError::ChildExit(code)),
    }
}

/// Run only the declared background commands, until Ctrl-C.
fn run_background_only(
    ctx: &Context,
    session: &Session,
    values: Values,
    bind: &str,
    interrupted: &AtomicBool,
) -> Result<(), CliError> {
    let mut services = start_background(session, &values, bind)?;
    std::thread::scope(|scope| {
        let observer = scope.spawn(|| watch::observe(ctx, track(session, values)));
        while !interrupted.load(Ordering::SeqCst) && !observer.is_finished() {
            report_finished(&mut services);
            std::thread::sleep(POLL);
        }
        stop_all(&mut services);
        interrupted.store(true, Ordering::SeqCst);
        observer
            .join()
            .unwrap_or_else(|_| Err(CliError::Usage("the network watch stopped".into())))
    })
}

/// The wildcard address for a family, IPv4 when nothing is selected yet.
fn bind_address(family: Option<Family>) -> &'static str {
    match family {
        Some(Family::V6) => "::",
        _ => "0.0.0.0",
    }
}

fn start_background(
    session: &Session,
    values: &Values,
    bind: &str,
) -> Result<Vec<Child>, CliError> {
    let mut children = Vec::new();
    for command in &session.plan.background {
        match spawn(command, session, values, bind, true) {
            Ok(child) => children.push(child),
            Err(error) => {
                stop_all(&mut children);
                return Err(error);
            }
        }
    }
    Ok(children)
}

/// Start one command as an argument vector, with the address in its environment.
fn spawn(
    command: &[String],
    session: &Session,
    values: &Values,
    bind: &str,
    background: bool,
) -> Result<Child, CliError> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| CliError::Usage("the command is empty".into()))?;

    let mut child = Command::new(resolve_program(program));
    child
        .args(arguments)
        .env("AUTONET_IP", &values.ip)
        .env("AUTONET_HOST", &values.host)
        .current_dir(session.root);
    if let Some(url) = &values.url {
        child.env("AUTONET_URL", url);
    }
    for (name, value) in &session.plan.env {
        let value = match value {
            EnvValue::Ip => values.ip.clone(),
            EnvValue::Bind => bind.to_string(),
            EnvValue::Port => values
                .port
                .map_or_else(String::new, |port| port.to_string()),
        };
        child.env(name, value);
    }
    if background {
        // Two programs reading one terminal each get half of what is typed.
        child.stdin(Stdio::null());
    }
    child.spawn().map_err(|error| spawn_error(program, &error))
}

/// Wait for the main command, stopping it if a signal arrives that it did not get.
fn wait_for_primary(
    primary: &mut Child,
    background: &mut Vec<Child>,
    interrupted: &AtomicBool,
) -> Result<ExitStatus, CliError> {
    let mut signalled_at: Option<Instant> = None;
    loop {
        match primary.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                return Err(CliError::Usage(format!(
                    "cannot wait for the command: {error}"
                )))
            }
        }

        report_finished(background);

        if interrupted.load(Ordering::SeqCst) {
            let at = *signalled_at.get_or_insert_with(Instant::now);
            if at.elapsed() >= SIGNAL_REACH {
                stop_all(std::slice::from_mut(primary));
                return primary
                    .wait()
                    .map_err(|error| CliError::Usage(format!("cannot reap the command: {error}")));
            }
        }

        std::thread::sleep(POLL);
    }
}

/// Say once if a background command has ended, and stop watching it.
///
/// It is reported and not fatal, because the main command is still running and
/// ADR 0001 forbids AutoNet from ending it. The person has to know, though: a
/// mobile app whose API has died looks exactly like a network problem.
fn report_finished(background: &mut Vec<Child>) {
    background.retain_mut(|child| match child.try_wait() {
        Ok(Some(status)) => {
            say(&format!("a background command exited: {status}"));
            false
        }
        _ => true,
    });
}

/// Ask each child to stop, wait for the grace period, then kill what is left.
fn stop_all(children: &mut [Child]) {
    for child in children.iter_mut() {
        if matches!(child.try_wait(), Ok(None)) {
            request_stop(child.id());
        }
    }
    let deadline = Instant::now() + GRACE;
    for child in children.iter_mut() {
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The callback that keeps the files current for as long as this runs.
///
/// Never returns an error. The server is running, and ADR 0001 says AutoNet does
/// not stop it because a file could not be written; it says so and tries again on
/// the next change.
fn track<'a>(
    session: &'a Session<'a>,
    written: Values,
) -> impl FnMut(&Change) -> Result<(), CliError> + 'a {
    let mut last = written;
    move |change: &Change| {
        let next = match change.current {
            Some(current) => outputs::current_values(current, session.port),
            // The last values, because they may yet become true again and nothing
            // better is known. The flag is what says they cannot be relied on.
            None => Values {
                online: false,
                ..last.clone()
            },
        };

        // The opening report usually matches what was written just before it. When
        // the network moved in that gap it does not, and it is a real change.
        if next == last {
            return Ok(());
        }

        let mut updated = Vec::new();
        for output in &session.plan.outputs {
            match outputs::write(session.root, output, &next) {
                Ok(_) => updated.push(output.path.as_str()),
                Err(error) => say(&format!("cannot update {}: {error}", output.path)),
            }
        }
        print_change(session, change, &next, &updated);
        last = next;
        Ok(())
    }
}

/// The block printed before anything is started.
fn print_opening(
    session: &Session,
    manifest: Option<&Manifest>,
    selected: Option<&SelectedAddress>,
    values: &Values,
) {
    let theme = session.theme;
    let plan = session.plan;
    let line = |label: &str, text: &str| say_line(&format!("{} {text}", theme.label(label)));

    if let Some(kind) = plan.kind {
        line("Detected", &theme.value(kind.label()));
    }
    for output in &plan.outputs {
        line("Writing ", &output.path);
    }
    if let Some(kind) = plan.kind {
        line("Read it ", kind.usage());
    }

    if plan.binds_wildcard {
        line(
            "Exposes ",
            &theme.warn("this dev server to everyone on your network"),
        );
    } else if manifest
        .is_some_and(|manifest| manifest.command.is_some() || !manifest.background.is_empty())
    {
        line(
            "Listen  ",
            "on 0.0.0.0, or other devices cannot connect. AutoNet cannot see what this command binds.",
        );
    }

    match selected {
        Some(selected) => {
            if let Some(port) = session.port {
                if plan.runs_something() {
                    let check = port::inspect(selected.ip, port);
                    if let Some(warning) = port::describe(&check, port) {
                        line("Warning ", &theme.warn(&warning));
                    }
                }
            }
            print_address(session, values);
        }
        None => line(
            "Network ",
            &theme.bad("none yet. Starting anyway, and the file is marked offline."),
        ),
    }

    let hardcoded = scan_project(session.root).finding_count();
    if hardcoded > 0 {
        line(
            "Warning ",
            &theme.warn(&format!(
                "{hardcoded} hardcoded address{} in this project. Run `autonet check`.",
                if hardcoded == 1 { "" } else { "es" }
            )),
        );
    }
}

/// The block printed each time the address moves.
fn print_change(session: &Session, change: &Change, values: &Values, updated: &[&str]) {
    let theme = session.theme;
    let line = |label: &str, text: &str| say_line(&format!("{} {text}", theme.label(label)));

    say_line("");
    let Some(current) = change.current else {
        let reason = change.failure.unwrap_or("nothing is selectable");
        line("Changed ", &theme.bad(&format!("no network: {reason}")));
        if !updated.is_empty() {
            line("Updated ", &updated.join(", "));
        }
        line(
            "Keeping ",
            "the last values, marked offline. The server is still running.",
        );
        say_line("");
        return;
    };

    let from = change.previous.map_or_else(
        || "no network".to_string(),
        |previous| format!("{} {}", previous.interface, previous.ip),
    );
    let reason =
        watch::reason(change.diff).map_or_else(String::new, |reason| format!(" ({reason})"));
    line(
        "Changed ",
        &format!(
            "{from} {} {} {}{reason}",
            theme.muted("->"),
            current.interface,
            current.ip
        ),
    );
    if !updated.is_empty() {
        line("Updated ", &updated.join(", "));
    }

    print_address(session, values);

    if current.scope == AddressScope::Global {
        line(
            "Warning ",
            &theme.bad("this address is reachable from the internet, and so is this server. Press Ctrl-C if you did not mean that."),
        );
    }
    if session
        .bound_family
        .is_some_and(|family| family != current.family)
    {
        line(
            "Warning ",
            &theme.warn(
                "the server was told to listen on the other IP version. Restart `autonet dev`.",
            ),
        );
    }
    if let Some(kind) = session.plan.kind {
        line("Action  ", kind.action());
    }
    say_line("");
}

/// The address, as a URL when there is a port to build one from.
fn print_address(session: &Session, values: &Values) {
    let theme = session.theme;
    let Some(url) = &values.url else {
        say_line(&format!(
            "{} {}",
            theme.label("Address "),
            theme.value(&values.ip)
        ));
        return;
    };
    say_line(&format!("{} {}", theme.label("Network "), theme.value(url)));
    if session.show_qr {
        match qr::block(url, theme) {
            Ok(block) => say_line(block.trim_end()),
            Err(error) => {
                if let Some(message) = error.message() {
                    say(&message);
                }
            }
        }
    }
}

/// One message from AutoNet, labelled, among the server's own output.
fn say(message: &str) {
    say_line(&format!("autonet: {message}"));
}

/// One line on stderr, ignoring a closed pipe. Losing a status line must not take
/// the dev server down with it.
fn say_line(text: &str) {
    let _ = writeln!(std::io::stderr(), "{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wildcard_matches_the_family_of_the_address() {
        assert_eq!(bind_address(Some(Family::V4)), "0.0.0.0");
        assert_eq!(bind_address(Some(Family::V6)), "::");
    }

    #[test]
    fn with_no_network_the_server_still_gets_a_wildcard() {
        assert_eq!(bind_address(None), "0.0.0.0");
    }
}
