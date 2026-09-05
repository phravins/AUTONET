//! CLI command implementations.

use std::fmt::Write as _;

use autonet_core::config::Config;
use autonet_core::model::{Interface, NetworkState, Route};
use autonet_core::select::{select, Candidate, Selection};
use autonet_platform::NetworkProvider;
use serde::Serialize;
use serde_json::json;

use crate::cli::GlobalArgs;
use crate::doctor::{self, Diagnosis, Snapshot};
use crate::render::{table, Theme};
use crate::{qr, url, CliError};

/// Everything a command needs, assembled once in `main`.
pub struct Context {
    pub provider: Box<dyn NetworkProvider>,
    pub config: Config,
    pub theme: Theme,
}

impl Context {
    pub(crate) fn snapshot(&self) -> Result<NetworkState, CliError> {
        self.provider.snapshot().map_err(CliError::Platform)
    }
}

/// Show the selected address, and on failure explain why there wasn't one.
pub fn status(ctx: &Context, args: &GlobalArgs) -> Result<(), CliError> {
    // Both `--qr` refusals happen before the snapshot, so a request that cannot
    // be met costs nothing and says why on its own. Printing the whole report
    // first and then failing would bury the one line the user needs under
    // output they did not ask for.
    let qr_port = if args.qr {
        Some(check_qr_is_possible(ctx, args)?)
    } else {
        None
    };

    let state = ctx.snapshot()?;
    check_requested_interface(ctx, &state)?;
    let selection = select(&state, &ctx.config.selection);

    if args.json {
        print!("{}", status_json(ctx, args, &state, &selection));
    } else {
        print!("{}", status_text(ctx, args, &state, &selection));
        // Appended, never substituted: `--qr` adds a way to read the URL, and
        // takes nothing away from the report that was already there.
        if let (Some(selected), Some(port)) = (&selection.selected, qr_port) {
            print!("{}", status_qr(ctx, selected, port)?);
        }
    }

    // Preserve the report while returning a failing exit code.
    if selection.selected.is_none() {
        return Err(CliError::NoAddress(
            selection.failure_reason(&ctx.config.selection),
        ));
    }
    Ok(())
}

/// Refuse `--qr` where it cannot mean anything, before any work is done.
///
/// Two cases, and neither is a malfunction, so both are `Usage` rather than a
/// silent no-op. A flag that the user typed and that quietly did nothing is a
/// worse outcome than an exit code and a sentence.
///
/// Returns the port it established, so the rendering path below has no
/// impossible `None` branch to invent an answer for.
fn check_qr_is_possible(ctx: &Context, args: &GlobalArgs) -> Result<u16, CliError> {
    if args.json {
        return Err(CliError::Usage(
            "--qr renders a QR code to the terminal, so it has no meaning with \
             --json. The URL it would encode is already in that payload as \
             urls.network."
                .to_string(),
        ));
    }

    let Some(port) = args.port(&ctx.config) else {
        return Err(CliError::Usage(
            "--qr needs a port: pass --port, or set output.default_port in your \
             config file. A QR code is only useful if scanning it opens \
             something, and a URL without a port does not."
                .to_string(),
        ));
    };

    Ok(port)
}

/// The QR block appended to `status`, with the port [`check_qr_is_possible`]
/// established.
fn status_qr(
    ctx: &Context,
    selected: &autonet_core::select::SelectedAddress,
    port: u16,
) -> Result<String, CliError> {
    // `None`: `status` publishes no name, so the address is the only host that
    // is true right now. See `url::network_url` for the swap point.
    let url = url::network_url(selected, port, None);
    Ok(format!(
        "\n{}{}",
        qr::caption(&url, ctx.theme),
        qr::render(&url, ctx.theme)?
    ))
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
            if let Some(port) = args.port(&ctx.config) {
                payload["urls"] = json!({
                    "local": url::local_url(selected.family, port),
                    "network": url::network_url(selected, port, None),
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

        if let Some(port) = args.port(&ctx.config) {
            out.push('\n');
            let _ = writeln!(
                out,
                "  {}  {}",
                theme.label("Local    "),
                url::local_url(selected.family, port)
            );
            let _ = writeln!(
                out,
                "  {}  {}  {}",
                theme.label("Network  "),
                theme.value(&url::network_url(selected, port, None)),
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
        if let Some(port) = args.port(&ctx.config) {
            payload["url"] = json!(url::network_url(&selected, port, None));
        }
        print!("{}", to_json_line(&payload));
    } else if let Some(port) = args.port(&ctx.config) {
        println!("{}", url::network_url(&selected, port, None));
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

/// Run the diagnostic checklist and report it.
///
/// The only command that survives a failed snapshot: reporting that the
/// operating system could not be queried is precisely doctor's job, so the
/// checklist is printed with the reason on the first row and the rest marked
/// unverified, and the platform error is still returned so the exit code is
/// unchanged. Same shape as [`status`], which prints its report *and* returns
/// [`CliError::NoAddress`].
///
/// A failure of [`autonet_platform::provider`] itself is not covered here,
/// because `main` needs a provider to build a [`Context`] at all. On the three
/// supported platforms that call constructs a struct and, on Linux, a
/// single-threaded runtime; if it fails, the process is in trouble that a
/// checklist would not clarify. The reachable case — a platform with no
/// backend — returns a provider that fails at `snapshot`, which is the path
/// above.
pub fn doctor(ctx: &Context, args: &GlobalArgs) -> Result<(), CliError> {
    let platform = ctx.provider.platform_name();
    let os = std::env::consts::OS;

    let state = match ctx.provider.snapshot() {
        Ok(state) => state,
        Err(error) => {
            let diagnosis = Diagnosis {
                platform,
                os,
                network: Err(error.to_string()),
                port: None,
            };
            print!("{}", doctor_output(ctx, args, &diagnosis, None));
            return Err(CliError::Platform(error));
        }
    };

    check_requested_interface(ctx, &state)?;
    let selection = select(&state, &ctx.config.selection);

    // The one part of doctor that touches the network. Done here rather than
    // inside the checks so that `doctor::checks` stays pure and fixture-driven,
    // and only when there is an address to probe: `--port` without a selected
    // address has nothing to ask a question about, and the rows above already
    // say why.
    let probe = args
        .port(&ctx.config)
        .zip(selection.selected.as_ref())
        .map(|(port, selected)| (port, crate::port::inspect(selected.ip, port)));

    let diagnosis = Diagnosis {
        platform,
        os,
        network: Ok(Snapshot {
            state: &state,
            selection: &selection,
            config: &ctx.config.selection,
        }),
        port: probe.as_ref().map(|(port, check)| (*port, check)),
    };

    print!(
        "{}",
        doctor_output(ctx, args, &diagnosis, state.captured_at)
    );

    if doctor::healthy(&doctor::checks(&diagnosis)) {
        Ok(())
    } else {
        Err(CliError::Unhealthy)
    }
}

fn doctor_output(
    ctx: &Context,
    args: &GlobalArgs,
    diagnosis: &Diagnosis,
    captured_at: Option<u64>,
) -> String {
    let checks = doctor::checks(diagnosis);

    if !args.json {
        return doctor::report(diagnosis.platform, &checks, ctx.theme);
    }

    let mut payload = json!({
        "schema_version": autonet_core::SCHEMA_VERSION,
        "platform": diagnosis.platform,
        "os": diagnosis.os,
        "ok": doctor::healthy(&checks),
        "verdict": doctor::verdict(&checks).as_str(),
        "summary": doctor::summary(&checks),
        "checks": checks
            .iter()
            .map(|check| {
                json!({
                    "id": check.id,
                    "label": check.label,
                    "status": check.status.as_str(),
                    "detail": check.detail,
                })
            })
            .collect::<Vec<_>>(),
    });

    // Present only when there was a snapshot to date.
    if let Some(captured_at) = captured_at {
        payload["captured_at"] = json!(captured_at);
    }

    to_json_line(&payload)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Reject a `--interface` name the machine does not have.
///
/// Without this, a typo produces "the requested interface has no usable
/// addresses", which sends the user to look at their network when the problem
/// is their spelling. Distinguishing the two is worth one extra check.
pub(crate) fn check_requested_interface(
    ctx: &Context,
    state: &NetworkState,
) -> Result<(), CliError> {
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

/// Serialize one JSON document followed by a newline.
///
/// Pretty-printing is deliberately avoided: the output is a wire format for
/// other programs, one document per line, so `autonet status --json | jq` works
/// and so `autonet watch --json` streams documents down the same pipe without
/// the format changing.
pub(crate) fn to_json_line(value: &impl Serialize) -> String {
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
