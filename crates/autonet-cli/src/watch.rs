//! `autonet watch` — report changes to the selected address as they happen.
//!
//! Read-only, by construction as well as by promise. `observe` holds no child
//! process, no handle to anything it could signal, and no path to `kill`; the
//! most it can do is print. That is the commitment
//! `docs/adr/0001-network-change-during-autonet-run.md` made when it decided
//! `autonet run` would be a launcher and not a supervisor: watching is how you
//! learn the address moved, not a mechanism for reacting to it on your behalf.
//!
//! This module is also the change-detection pipeline the rest of the tool
//! builds on. [`observe`] takes a callback rather than printing directly, so
//! `autonet advertise` can hang an mDNS re-announcement off exactly the same
//! loop instead of growing a second one that could disagree with it.

use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use autonet_core::event::{diff, NetworkDiff, NetworkEvent};
use autonet_core::select::{select, SelectedAddress};
use serde_json::json;

use crate::cli::GlobalArgs;
use crate::commands::{check_requested_interface, to_json_line, Context};
use crate::render::Theme;
use crate::signal::install_signal_flag;
use crate::CliError;

/// How long to wait between looks at the network.
///
/// Two seconds is fast enough that a Wi-Fi handover is reported while the user
/// is still watching for it, and slow enough that an idle `autonet watch` is
/// not a background job anyone notices.
const INTERVAL: Duration = Duration::from_secs(2);

/// How often the wait wakes to re-check the interrupt flag.
///
/// Separate from [`INTERVAL`] so that Ctrl-C is answered promptly rather than
/// whenever the current interval happens to end.
const TICK: Duration = Duration::from_millis(100);

/// One observed move of the selected address.
///
/// `previous` is `None` for the first report, which describes the starting
/// state rather than a change. `current` is `None` when nothing is selectable
/// any more — an unplugged cable, a dropped association.
pub(crate) struct Change<'a> {
    /// What was selected before, if this is not the first report.
    pub previous: Option<&'a SelectedAddress>,
    /// What is selected now, if anything is.
    pub current: Option<&'a SelectedAddress>,
    /// Every underlying change between the two snapshots.
    pub diff: &'a NetworkDiff,
    /// When the snapshot behind `current` was taken, in Unix seconds.
    pub captured_at: Option<u64>,
}

impl Change<'_> {
    /// Whether this is the opening report rather than a change.
    fn is_initial(&self) -> bool {
        self.previous.is_none() && self.diff.is_empty()
    }
}

/// Watch the network and print every change to the selected address.
pub fn watch(ctx: &Context, args: &GlobalArgs) -> Result<(), CliError> {
    if !args.json {
        emit(&banner(ctx))?;
    }

    observe(ctx, |change| {
        if args.json {
            emit(&document(change))
        } else {
            emit(&render(change, ctx.theme))
        }
    })
}

/// Poll the network until interrupted, calling `on_change` whenever the
/// selected address moves.
///
/// The callback is invoked once at startup with the current selection, so a
/// caller does not need a separate code path to establish its starting state:
/// `watch` prints that first block, and `advertise` makes its first
/// registration from it.
///
/// A callback that returns [`CliError::Stopped`] ends the loop successfully —
/// that is how a closed output pipe is reported.
///
/// # Errors
///
/// Returns [`CliError::Platform`] if the very first snapshot fails,
/// [`CliError::Usage`] if `--interface` names something the machine does not
/// have, and whatever the callback returns.
pub(crate) fn observe(
    ctx: &Context,
    mut on_change: impl FnMut(&Change) -> Result<(), CliError>,
) -> Result<(), CliError> {
    // Installed before the first snapshot so that Ctrl-C during startup ends
    // the process the same way it ends the loop.
    let interrupted = install_signal_flag()?;

    // The first snapshot is allowed to be fatal; later ones are not. A machine
    // whose network cannot be queried at all is a different problem from an
    // interface disappearing mid-watch, which is the event being watched for.
    let mut state = ctx.snapshot()?;
    check_requested_interface(ctx, &state)?;
    let mut selected = select(&state, &ctx.config.selection).selected;

    let mut deliver = |change: &Change| -> Result<bool, CliError> {
        match on_change(change) {
            Ok(()) => Ok(true),
            Err(CliError::Stopped) => Ok(false),
            Err(error) => Err(error),
        }
    };

    let opening = NetworkDiff::default();
    let keep_going = deliver(&Change {
        previous: None,
        current: selected.as_ref(),
        diff: &opening,
        captured_at: state.captured_at,
    })?;
    if !keep_going {
        return Ok(());
    }

    loop {
        wait(&interrupted);
        if interrupted.load(Ordering::SeqCst) {
            return Ok(());
        }

        let fresh = match ctx.provider.snapshot() {
            Ok(fresh) => fresh,
            Err(error) => {
                // Reported, not fatal. A query can fail transiently while the
                // kernel is reconfiguring, which is precisely the moment this
                // loop exists to survive.
                let _ = writeln!(std::io::stderr(), "autonet: {error}");
                continue;
            }
        };

        let changes = diff(&state, &fresh);
        state = fresh;

        // The cheap gate first: most snapshots differ in ways that cannot move
        // the answer, and re-running the selector on every one of them would
        // burn work to reach the same conclusion.
        if !changes.affects_selection() {
            continue;
        }

        let current = select(&state, &ctx.config.selection).selected;
        if !differs(selected.as_ref(), current.as_ref()) {
            continue;
        }

        let keep_going = deliver(&Change {
            previous: selected.as_ref(),
            current: current.as_ref(),
            diff: &changes,
            captured_at: state.captured_at,
        })?;
        if !keep_going {
            return Ok(());
        }

        selected = current;
    }
}

/// Whether two selections differ in a way anyone downstream would act on.
///
/// Compared on the address and its interface, not with `==`.
/// [`SelectedAddress`] also carries the score that won it, and a score moves
/// for reasons the answer does not: a route metric changes, a rule starts or
/// stops firing. Reporting that would print two identical-looking blocks and,
/// in `advertise`, re-announce a record that never moved.
fn differs(previous: Option<&SelectedAddress>, current: Option<&SelectedAddress>) -> bool {
    match (previous, current) {
        (None, None) => false,
        (Some(previous), Some(current)) => {
            previous.ip != current.ip || previous.interface != current.interface
        }
        _ => true,
    }
}

/// Block until it is worth looking again, or until interrupted.
fn wait(interrupted: &AtomicBool) {
    let deadline = Instant::now() + INTERVAL;
    while Instant::now() < deadline {
        if interrupted.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(TICK);
    }
}

/// The opening lines, before anything has happened.
fn banner(ctx: &Context) -> String {
    let theme = ctx.theme;
    format!(
        "{} {}\n{}\n\n",
        theme.heading("AutoNet"),
        theme.muted(ctx.provider.platform_name()),
        theme.muted(&format!(
            "Watching for address changes every {}s. Ctrl-C to stop.",
            INTERVAL.as_secs()
        ))
    )
}

/// Render one change as the three-line block.
fn render(change: &Change, theme: Theme) -> String {
    let mut out = String::new();

    if !change.is_initial() {
        let _ = writeln!(
            out,
            "{} {}",
            theme.label("Previous:"),
            describe(change.previous)
        );
    }
    let _ = writeln!(
        out,
        "{} {}",
        theme.label("Current: "),
        match change.current {
            Some(current) => theme.value(&describe(Some(current))),
            None => theme.bad(&describe(None)),
        }
    );
    if let Some(reason) = reason(change.diff) {
        let _ = writeln!(out, "{} {reason}", theme.label("Reason:  "));
    }
    out.push('\n');

    out
}

/// One selection, as a person reads it.
///
/// Names the interface as well as its kind. The kind alone is what the eye
/// looks for — Wi-Fi against Ethernet — but a machine can have two of either,
/// and the name is the part that identifies which one.
fn describe(selected: Option<&SelectedAddress>) -> String {
    match selected {
        Some(selected) => format!(
            "{} ({}) / {}",
            selected.interface, selected.interface_kind, selected.ip
        ),
        None => "no usable address".to_string(),
    }
}

/// The single most explanatory event behind a change.
///
/// A handover produces a fistful of events at once — an address goes, another
/// arrives, the route follows, the interface changes state — and printing all
/// of them buries the one that answers "why". They are ranked by how much they
/// explain, and the winner is worded. The full list is still in `--json`.
///
/// The wording is prose for a human and is not part of the JSON contract; see
/// `docs/json-schema.md`.
fn reason(diff: &NetworkDiff) -> Option<String> {
    // `min_by_key` keeps the first of equally-ranked events, and `diff` emits
    // in a stable order, so the same change always produces the same sentence.
    diff.events.iter().min_by_key(|event| rank(event)).map(word)
}

/// How much an event explains, lowest first.
fn rank(event: &NetworkEvent) -> u8 {
    match event {
        // Where traffic leaves by is the closest thing to a cause.
        NetworkEvent::DefaultRouteChanged { .. } => 0,
        NetworkEvent::InterfaceRemoved { .. } => 1,
        NetworkEvent::InterfaceStateChanged { .. } => 2,
        NetworkEvent::AddressRemoved { .. } => 3,
        NetworkEvent::AddressAdded { .. } => 4,
        // An interface appearing is the least of it; it explains nothing until
        // it has an address, and that is a separate event.
        NetworkEvent::InterfaceAdded { .. } => 5,
    }
}

/// Word one event as a sentence fragment.
fn word(event: &NetworkEvent) -> String {
    match event {
        NetworkEvent::DefaultRouteChanged {
            family,
            from_interface,
            to_interface,
        } => match (from_interface, to_interface) {
            (Some(from), Some(to)) => format!("default {family} route moved from {from} to {to}"),
            (None, Some(to)) => format!("default {family} route appeared on {to}"),
            (Some(from), None) => format!("default {family} route on {from} went away"),
            // Unreachable: the diff only reports a route change when the two
            // sides differ, and they cannot both be absent.
            (None, None) => format!("default {family} route changed"),
        },
        NetworkEvent::InterfaceRemoved { interface } => format!("{interface} went away"),
        NetworkEvent::InterfaceStateChanged { interface, to, .. } => {
            format!("{interface} is now {to}")
        }
        NetworkEvent::AddressRemoved { interface, address } => {
            format!("{} was removed from {interface}", address.ip)
        }
        NetworkEvent::AddressAdded { interface, address } => {
            format!("{} was added to {interface}", address.ip)
        }
        NetworkEvent::InterfaceAdded { interface } => format!("{interface} appeared"),
    }
}

/// One JSON document for the stream.
///
/// `change` says which kind of document this is, so a consumer can skip the
/// opening one. The raw `events` array carries every underlying change, each
/// tagged with its own `event` field, for callers that want more than the
/// single sentence in `reason`.
fn document(change: &Change) -> String {
    to_json_line(&json!({
        "schema_version": autonet_core::SCHEMA_VERSION,
        "change": if change.is_initial() { "initial" } else { "selection" },
        "captured_at": change.captured_at,
        "previous": change.previous,
        "current": change.current,
        "reason": reason(change.diff),
        "events": change.diff.events,
    }))
}

/// Write one block to stdout, ending the watch tidily if the reader went away.
///
/// `print!` panics on a closed pipe, and closing the pipe is what
/// `autonet watch --json | head -3` does every single time. A reader that
/// stopped reading is an ordinary way for a stream to end, not a crash, so it
/// is reported as [`CliError::Stopped`] and exits zero.
fn emit(text: &str) -> Result<(), CliError> {
    let mut out = std::io::stdout().lock();
    if out.write_all(text.as_bytes()).is_err() || out.flush().is_err() {
        return Err(CliError::Stopped);
    }
    Ok(())
}
