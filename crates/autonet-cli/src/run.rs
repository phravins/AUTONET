//! The four milestone-1 commands.
//!
//! Each one does the same three things: take a snapshot, ask `autonet-core` a
//! question about it, and render the answer. No command reimplements any part
//! of discovery or selection — that is the whole architectural point, and it is
//! what will let the daemon and the SDKs answer identically.

use std::fmt::Write as _;

use autonet_core::config::Config;
use autonet_core::model::{Interface, NetworkState, Route};
use autonet_core::select::{select, Candidate, Selection};
use autonet_platform::NetworkProvider;
use serde::Serialize;
use serde_json::json;

use crate::cli::GlobalArgs;
use crate::render::{table, Theme};
use crate::CliError;

/// Everything a command needs, assembled once in `main`.
pub struct Context {
    pub provider: Box<dyn NetworkProvider>,
    pub config: Config,
    pub theme: Theme,
}

impl Context {
    fn snapshot(&self) -> Result<NetworkState, CliError> {
        self.provider.snapshot().map_err(CliError::Platform)
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Show the selected address, and on failure explain why there wasn't one.
pub fn status(ctx: &Context, args: &GlobalArgs) -> Result<(), CliError> {
    let state = ctx.snapshot()?;
    check_requested_interface(ctx, &state)?;
    let selection = select(&state, &ctx.config.selection);

    if args.json {
        print!("{}", status_json(ctx, args, &state, &selection));
    } else {
        print!("{}", status_text(ctx, args, &state, &selection));
    }

    // The report is printed either way — a developer running `autonet status`
    // to find out *why* nothing works still needs to see the candidate list —
    // but the exit code still says failure so scripts are not misled.
    if selection.selected.is_none() {
        return Err(CliError::NoAddress(
            selection.failure_reason(&ctx.config.selection),
        ));
    }
    Ok(())
}

fn status_json(
    ctx: &Context,
    args: &GlobalArgs,
    state: &NetworkState,
    selection: &Selection,
) -> String {
    let mut payload = json!({
        "schema_version": autonet_core::SCHEMA_VERSION,
        "platform": ctx.provider.platform_name(),
        "captured_at": state.captured_at,
        "selected": selection.selected,
    });

    match &selection.selected {
        Some(selected) => {
            if let Some(port) = args.port {
                payload["urls"] = json!({
                    "local": format!("http://{}:{port}", local_host(selected.family)),
                    "network": selected.url(port, "http"),
                });
            }
        }
        None => payload["error"] = json!(selection.failure_reason(&ctx.config.selection)),
    }

    if args.verbose {
        payload["candidates"] = json!(selection.candidates);
    }

    to_json_line(&payload)
}

fn status_text(
    ctx: &Context,
    args: &GlobalArgs,
    state: &NetworkState,
    selection: &Selection,
) -> String {
    let theme = ctx.theme;
    let mut out = String::new();

    let _ = writeln!(
        out,
        "{} {}",
        theme.heading("AutoNet"),
        theme.muted(ctx.provider.platform_name())
    );
    out.push('\n');

    if let Some(selected) = &selection.selected {
        let interface_detail = format!(
            "{} ({}, {})",
            selected.interface,
            selected.interface_kind,
            state
                .interface_by_name(&selected.interface)
                .map_or_else(|| "?".to_string(), |i| i.state.to_string())
        );

        let _ = writeln!(
            out,
            "  {}  {}",
            theme.label("Address  "),
            theme.value(&format!("{}/{}", selected.ip, selected.prefix_len))
        );
        let _ = writeln!(out, "  {}  {interface_detail}", theme.label("Interface"));
        if let Some(gateway) = selected.gateway {
            let _ = writeln!(out, "  {}  {gateway}", theme.label("Gateway  "));
        }
        let _ = writeln!(out, "  {}  {}", theme.label("Scope    "), selected.scope);

        if let Some(port) = args.port {
            out.push('\n');
            let _ = writeln!(
                out,
                "  {}  http://{}:{port}",
                theme.label("Local    "),
                local_host(selected.family)
            );
            let _ = writeln!(
                out,
                "  {}  {}  {}",
                theme.label("Network  "),
                theme.value(&selected.url(port, "http")),
                theme.muted("← open this from another device")
            );
        }
    } else {
        let _ = writeln!(
            out,
            "  {} {}",
            theme.bad("No usable address."),
            selection.failure_reason(&ctx.config.selection)
        );
    }

    if args.verbose {
        out.push('\n');
        out.push_str(&candidate_table(selection, theme));
    }

    out
}

/// The scoring breakdown: every candidate, why it was rejected, or what it won
/// or lost points for.
///
/// This is the honesty check on the whole design. If AutoNet picks something
/// surprising, `-v` shows exactly which rule caused it instead of leaving the
/// user to guess.
fn candidate_table(selection: &Selection, theme: Theme) -> String {
    // Two tables rather than one. A single "verdict" column would be sized by
    // the longest rejection sentence, pushing the scoring breakdown — the part
    // worth reading — off the right of an 80-column terminal.
    let (eligible, rejected): (Vec<&Candidate>, Vec<&Candidate>) = selection
        .candidates
        .iter()
        .partition(|candidate| candidate.disqualified.is_none());

    let mut out = String::new();

    if !eligible.is_empty() {
        let rows: Vec<Vec<String>> = eligible
            .iter()
            .map(|candidate| {
                vec![
                    candidate.interface.clone(),
                    candidate.address.ip.to_string(),
                    candidate.score.to_string(),
                    reasons(candidate),
                ]
            })
            .collect();
        let _ = writeln!(out, "{}", theme.heading("  Considered"));
        out.push_str(&indent(&table(
            &["INTERFACE", "ADDRESS", "SCORE", "WHY"],
            &rows,
            theme,
        )));
    }

    if !rejected.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        let rows: Vec<Vec<String>> = rejected
            .iter()
            .map(|candidate| {
                vec![
                    candidate.interface.clone(),
                    candidate.address.ip.to_string(),
                    candidate
                        .disqualified
                        .map(|reason| reason.to_string())
                        .unwrap_or_default(),
                ]
            })
            .collect();
        let _ = writeln!(out, "{}", theme.heading("  Rejected"));
        out.push_str(&indent(&table(
            &["INTERFACE", "ADDRESS", "REASON"],
            &rows,
            theme,
        )));
    }

    out
}

fn reasons(candidate: &Candidate) -> String {
    candidate
        .reasons
        .iter()
        .map(|r| format!("{} {:+}", r.rule, r.delta))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// ip
// ---------------------------------------------------------------------------

/// Print the selected address and nothing else.
///
/// The contract this command exists to honour is `IP=$(autonet ip) || exit 1`:
/// on success stdout holds exactly one address and a newline, and on failure
/// stdout is empty and the exit code is non-zero.
pub fn ip(ctx: &Context, args: &GlobalArgs) -> Result<(), CliError> {
    let state = ctx.snapshot()?;
    check_requested_interface(ctx, &state)?;
    let mut selection = select(&state, &ctx.config.selection);

    let Some(selected) = selection.selected.take() else {
        if args.json {
            println!(
                "{}",
                to_json_line(&json!({
                    "schema_version": autonet_core::SCHEMA_VERSION,
                    "ip": serde_json::Value::Null,
                    "error": selection.failure_reason(&ctx.config.selection),
                }))
                .trim_end()
            );
        }
        return Err(CliError::NoAddress(
            selection.failure_reason(&ctx.config.selection),
        ));
    };

    if args.json {
        let mut payload = serde_json::to_value(&selected).unwrap_or_else(|_| json!({}));
        payload["schema_version"] = json!(autonet_core::SCHEMA_VERSION);
        if let Some(port) = args.port {
            payload["url"] = json!(selected.url(port, "http"));
        }
        print!("{}", to_json_line(&payload));
    } else if let Some(port) = args.port {
        println!("{}", selected.url(port, "http"));
    } else {
        println!("{}", selected.ip);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// interfaces
// ---------------------------------------------------------------------------

/// List every interface the machine has, classified.
pub fn interfaces(ctx: &Context, args: &GlobalArgs) -> Result<(), CliError> {
    let state = ctx.snapshot()?;

    if args.json {
        let interfaces: Vec<Interface> = state
            .interfaces
            .iter()
            .map(|i| redact(i, args.verbose))
            .collect();
        print!(
            "{}",
            to_json_line(&json!({
                "schema_version": autonet_core::SCHEMA_VERSION,
                "platform": ctx.provider.platform_name(),
                "captured_at": state.captured_at,
                "interfaces": interfaces,
            }))
        );
        return Ok(());
    }

    let theme = ctx.theme;
    let mut headers = vec!["IDX", "NAME", "KIND", "STATE", "ADDRESSES"];
    if args.verbose {
        headers.insert(4, "MAC");
    }

    let rows: Vec<Vec<String>> = state
        .interfaces
        .iter()
        .map(|interface| {
            let addresses = if interface.addresses.is_empty() {
                theme.muted("—")
            } else {
                interface
                    .addresses
                    .iter()
                    .map(|a| format!("{}/{}", a.ip, a.prefix_len))
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            let mut row = vec![
                interface.index.to_string(),
                interface.name.clone(),
                interface.kind.to_string(),
                state_cell(interface, theme),
            ];
            if args.verbose {
                row.push(interface.mac.clone().unwrap_or_else(|| "—".into()));
            }
            row.push(addresses);
            row
        })
        .collect();

    print!("{}", table(&headers, &rows, theme));
    Ok(())
}

fn state_cell(interface: &Interface, theme: Theme) -> String {
    let text = interface.state.to_string();
    match interface.state {
        autonet_core::model::InterfaceState::Up => theme.good(&text),
        autonet_core::model::InterfaceState::Down => theme.muted(&text),
        _ => theme.warn(&text),
    }
}

/// Drop the hardware address unless it was explicitly asked for.
///
/// A MAC address is a stable hardware identifier that outlives IP addresses and
/// networks. AutoNet has no need to publish one to answer "what address can my
/// phone reach", so it is withheld by default and available under `-v`.
fn redact(interface: &Interface, verbose: bool) -> Interface {
    if verbose {
        return interface.clone();
    }
    let mut copy = interface.clone();
    copy.mac = None;
    copy
}

// ---------------------------------------------------------------------------
// routes
// ---------------------------------------------------------------------------

/// List routing table entries, default routes first.
///
/// Routes are shown because they are the evidence behind the selection: an
/// interface with an address but no route is configured, not connected.
pub fn routes(ctx: &Context, args: &GlobalArgs) -> Result<(), CliError> {
    let state = ctx.snapshot()?;
    let mut routes: Vec<&Route> = state.routes.iter().collect();

    // Deterministic and useful: defaults on top because they are what the
    // selection engine actually weighs, then a stable order so consecutive runs
    // are diffable.
    routes.sort_by_key(|r| {
        (
            !r.is_default(),
            r.family,
            r.interface_index,
            r.metric,
            r.destination.map(|d| d.to_string()),
        )
    });

    if args.json {
        print!(
            "{}",
            to_json_line(&json!({
                "schema_version": autonet_core::SCHEMA_VERSION,
                "platform": ctx.provider.platform_name(),
                "captured_at": state.captured_at,
                "routes": routes,
            }))
        );
        return Ok(());
    }

    let theme = ctx.theme;
    let name_of = |index: u32| {
        state
            .interface_by_index(index)
            .map_or_else(|| format!("#{index}"), |i| i.name.clone())
    };

    let rows: Vec<Vec<String>> = routes
        .iter()
        .map(|route| {
            let destination = route
                .destination
                .map_or_else(|| theme.value("default"), |d| d.to_string());
            vec![
                destination,
                route
                    .gateway
                    .map_or_else(|| theme.muted("—"), |g| g.to_string()),
                name_of(route.interface_index),
                route.metric.to_string(),
                route.family.to_string(),
            ]
        })
        .collect();

    print!(
        "{}",
        table(
            &["DESTINATION", "GATEWAY", "INTERFACE", "METRIC", "FAMILY"],
            &rows,
            theme
        )
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Reject a `--interface` name the machine does not have.
///
/// Without this, a typo produces "the requested interface has no usable
/// addresses", which sends the user to look at their network when the problem
/// is their spelling. Distinguishing the two is worth one extra check.
fn check_requested_interface(ctx: &Context, state: &NetworkState) -> Result<(), CliError> {
    let Some(name) = &ctx.config.selection.require_interface else {
        return Ok(());
    };
    if state.interface_by_name(name).is_some() {
        return Ok(());
    }

    let available = state
        .interfaces
        .iter()
        .map(|i| i.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(CliError::Usage(format!(
        "no interface named {name:?}. This machine has: {available}"
    )))
}

/// The address a browser on *this* machine would use, as opposed to the one
/// AutoNet exists to find.
fn local_host(family: autonet_core::model::Family) -> &'static str {
    match family {
        autonet_core::model::Family::V4 => "127.0.0.1",
        autonet_core::model::Family::V6 => "[::1]",
    }
}

/// Serialize one JSON document followed by a newline.
///
/// Pretty-printing is deliberately avoided: the output is a wire format for
/// other programs, one document per line, so `autonet status --json | jq` works
/// and so `watch` can later stream documents down the same pipe unchanged.
fn to_json_line(value: &impl Serialize) -> String {
    match serde_json::to_string(value) {
        Ok(text) => format!("{text}\n"),
        // Unreachable for the types used here, but an unwrap in the output path
        // would turn a formatting bug into a crash on an otherwise good result.
        Err(error) => format!("{{\"schema_version\":1,\"error\":\"{error}\"}}\n"),
    }
}

fn indent(text: &str) -> String {
    text.lines().fold(String::new(), |mut out, line| {
        let _ = writeln!(out, "  {line}");
        out
    })
}
