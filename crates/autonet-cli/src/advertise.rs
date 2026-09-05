//! `autonet advertise` — publish a `.local` name for the selected address.
//!
//! This is what
//! [ADR 0001](../../../docs/adr/0001-network-change-during-autonet-run.md)
//! promised and did not deliver. That record made `autonet run` a launcher
//! rather than a supervisor, and accepted the resulting staleness on one
//! condition: *"what must not go stale is the name the client resolves, not the
//! address the server believes it has."* It then committed the moving case to
//! the name layer and left the name layer unbuilt. This module builds it. See
//! [ADR 0002](../../../docs/adr/0002-mdns-advertisement.md).
//!
//! # Why not just use the system responder
//!
//! Because it answers a different question. Avahi and `mDNSResponder` publish
//! *every* address on *every* interface under `<hostname>.local` and leave the
//! client to guess. On the machine this was written on, `real0.local` resolves
//! to `172.17.0.1` — the Docker bridge — while the address a phone can reach is
//! `192.168.1.18`. AutoNet publishes exactly the one the selector chose, which
//! is the entire point of the tool.
//!
//! # One pipeline, not two
//!
//! Re-advertisement runs through [`crate::watch::observe`] unchanged: the same
//! snapshot, the same `diff`, the same `affects_selection` gate, the same
//! `select`. `advertise` is `watch` with a responder on the callback instead of
//! a printer. It adds no change detection of its own, so there is no second
//! implementation to drift out of step with the first.

use std::fmt::Write as _;
use std::io::Write as _;
use std::time::Duration;

use autonet_core::config::HostnameConfig;
use autonet_core::select::SelectedAddress;
use mdns_sd::{ServiceDaemon, ServiceInfo};

use crate::cli::GlobalArgs;
use crate::commands::Context;
use crate::watch::{self, Change};
use crate::CliError;

/// How long to wait for the daemon to confirm the record was withdrawn.
///
/// Short on purpose. The goodbye packet is a courtesy — it tells the LAN to
/// forget the record now rather than in two minutes — and a courtesy is not
/// worth hanging a Ctrl-C on. If the daemon does not answer in this long,
/// AutoNet exits anyway and the record ages out on its TTL.
const GOODBYE: Duration = Duration::from_millis(500);

/// The suffix that keeps AutoNet's record distinct from the system responder's.
///
/// Not decoration. Avahi owns `<hostname>.local` on most Linux desktops and
/// `mDNSResponder` always owns it on macOS. Publishing the same name means
/// RFC 6762 conflict resolution either renames this record at runtime or leaves
/// two racing, and a client then gets the system responder's answer — every
/// address on every interface — about half the time. That is the failure this
/// command exists to fix, so it must not reintroduce it.
const SUFFIX: &str = "-autonet";

/// Publish a `.local` name for the selected address, and keep it current.
///
/// # Errors
///
/// Returns [`CliError::Usage`] if `hostname.enabled` is false or no port is
/// known, and whatever [`watch::observe`] returns otherwise.
pub fn advertise(ctx: &Context, args: &GlobalArgs) -> Result<(), CliError> {
    let config = &ctx.config.hostname;

    if !config.enabled {
        return Err(CliError::Usage(disabled(args)));
    }

    // Required, not defaulted. `ServiceInfo` cannot be built without a port,
    // and a service record naming no port advertises nothing anyone can open.
    let Some(port) = args.port(&ctx.config) else {
        return Err(CliError::Usage(
            "advertise needs a port: pass --port, or set output.default_port. \
             It is published in the service record, so a device that finds this \
             machine knows what to open."
                .to_string(),
        ));
    };

    let name = instance_name(config);
    let host = format!("{name}.local.");

    let daemon = ServiceDaemon::new()
        .map_err(|e| CliError::Usage(format!("could not start the mDNS responder: {e}")))?;

    // Tracks what is currently on the wire, so the exit path knows whether
    // there is anything to withdraw. `register` re-announces, so there is no
    // separate "first time" branch — see the callback below.
    let mut published: Option<String> = None;

    let result = watch::observe(ctx, |change| {
        if let Some(selected) = change.current {
            let info = service_info(&name, &host, config, selected, port)?;
            let fullname = info.get_fullname().to_string();
            daemon
                .register(info)
                .map_err(|e| CliError::Usage(format!("could not publish {name}.local: {e}")))?;
            report(ctx, change, &host, selected, port);
            published = Some(fullname);
        } else {
            // No usable address means the honest record is no record. Leaving
            // the last one standing would point the name at an address this
            // machine no longer holds, which is precisely the staleness ADR
            // 0001 pushed onto mDNS to solve.
            if let Some(fullname) = published.take() {
                let _ = daemon.unregister(&fullname);
            }
            withdrawn(ctx, change);
        }
        Ok(())
    });

    // Withdraw before exiting rather than letting the record age out, so a
    // phone that just resolved the name does not keep a dead answer cached.
    if let Some(fullname) = published {
        if let Ok(status) = daemon.unregister(&fullname) {
            let _ = status.recv_timeout(GOODBYE);
        }
    }
    if let Ok(status) = daemon.shutdown() {
        let _ = status.recv_timeout(GOODBYE);
    }

    result
}

/// The instance name to publish, without the `.local` suffix.
///
/// The configured name wins; otherwise the machine's own hostname with
/// [`SUFFIX`] appended. A hostname that cannot be read at all falls back to
/// `autonet`, because failing the command over a cosmetic default would be a
/// worse answer than a name that works.
pub(crate) fn instance_name(config: &HostnameConfig) -> String {
    if let Some(name) = &config.name {
        return name.clone();
    }

    let machine = hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        // A machine whose hostname is already fully qualified would otherwise
        // produce `laptop.example.com-autonet.local.`, which is a different
        // name from the one anybody would type.
        .and_then(|name| name.split('.').next().map(str::to_string))
        .filter(|name| !name.is_empty());

    match machine {
        Some(machine) => format!("{machine}{SUFFIX}"),
        None => "autonet".to_string(),
    }
}

/// Qualify the configured service type for the wire.
///
/// DNS-SD types are written `_http._tcp` everywhere a person is expected to
/// read one — in the README, in the config file, in `avahi-browse` output — but
/// the protocol wants `_http._tcp.local.`, and mdns-sd rejects anything else.
/// Making the user type the domain would be asking them to know a detail the
/// tool already knows, so the friendly form is accepted and completed here.
/// A value that already carries the domain is passed through untouched.
fn service_type(configured: &str) -> String {
    let trimmed = configured.trim_end_matches('.');
    // Compared as a DNS label rather than with `ends_with(".local")`, which
    // clippy reads as a filename-extension test and which would also match a
    // service type ending in something like `_my.local`.
    if trimmed.rsplit('.').next() == Some("local") {
        format!("{trimmed}.")
    } else {
        format!("{trimmed}.local.")
    }
}

/// Build the record set: A/AAAA for the host, plus SRV, PTR and TXT for the
/// service. One `register` publishes all of them.
///
/// **`enable_addr_auto` is deliberately never called.** That mdns-sd
/// convenience fills the address records in from every interface on the host,
/// which is exactly the system responder's behaviour this command exists to
/// replace. The address here is the one the selector chose, and nothing else.
fn service_info(
    name: &str,
    host: &str,
    config: &HostnameConfig,
    selected: &SelectedAddress,
    port: u16,
) -> Result<ServiceInfo, CliError> {
    ServiceInfo::new(
        &service_type(&config.service),
        name,
        host,
        selected.ip,
        port,
        // No TXT properties. Anything put here is broadcast to the whole link,
        // and the security posture's rule that output is conservative by
        // default applies on the wire at least as strongly as it does on a
        // terminal. The A record and the port are the answer; the interface
        // name, the score and the gateway are AutoNet's business.
        None,
    )
    .map_err(|e| CliError::Usage(format!("could not build the record for {name}.local: {e}")))
}

/// What to print when the record is published or re-published.
///
/// The first block says what went on the wire, because a command that makes
/// this machine discoverable should not be quiet about having done it. Later
/// blocks say what moved, so the operator can see the re-advertisement happen
/// rather than trusting that it did.
fn report(ctx: &Context, change: &Change, host: &str, selected: &SelectedAddress, port: u16) {
    let theme = ctx.theme;
    let name = host.trim_end_matches('.');
    let mut out = String::new();

    if change.is_initial() {
        let _ = writeln!(
            out,
            "{} {}",
            theme.heading("Advertising"),
            theme.value(name)
        );
        let _ = writeln!(
            out,
            "  {}  {}",
            theme.label("Address "),
            theme.value(&selected.ip.to_string())
        );
        let _ = writeln!(
            out,
            "  {}  {}",
            theme.label("Service "),
            theme.value(&format!("{} port {port}", ctx.config.hostname.service))
        );
        let _ = writeln!(
            out,
            "  {}  {}",
            theme.label("Open    "),
            theme.value(&format!("http://{name}:{port}"))
        );
        let _ = writeln!(
            out,
            "\n{}",
            theme.muted(
                "This machine is now discoverable on the local network. \
                 Ctrl-C to stop and withdraw the record."
            )
        );
    } else {
        let _ = writeln!(
            out,
            "{} {} {} {} ({})",
            theme.heading("Re-advertised"),
            theme.value(name),
            theme.muted("\u{2192}"),
            theme.value(&selected.ip.to_string()),
            theme.muted(&watch::reason(change.diff).unwrap_or_else(|| "address changed".into()))
        );
    }

    let _ = std::io::stdout().write_all(out.as_bytes());
}

/// Say the record was pulled, and why.
fn withdrawn(ctx: &Context, change: &Change) {
    if change.is_initial() {
        // Nothing was ever published, so there is no record to explain — only
        // a machine with no address to advertise.
        return;
    }
    let _ = writeln!(
        std::io::stdout(),
        "{} {}",
        ctx.theme.warn("Withdrawn"),
        ctx.theme
            .muted("no usable address; the name resolves to nothing until one returns")
    );
}

/// The refusal when `hostname.enabled` is false.
///
/// Refusing rather than treating the typed command as consent is the point:
/// otherwise the default-off setting protects nothing, since one word at a
/// prompt would put a name for this machine on the LAN. The message carries
/// the path and the exact text to add, so the refusal is also the instructions.
///
/// The path is `--config` when it was given, because naming the default file to
/// someone who explicitly pointed at a different one would send them to edit
/// the wrong thing and then wonder why nothing changed.
fn disabled(args: &GlobalArgs) -> String {
    let path = args.config.as_ref().map_or_else(
        || {
            autonet_core::Config::default_path().map_or_else(
                || "your config file".to_string(),
                |p| p.display().to_string(),
            )
        },
        |p| p.display().to_string(),
    );

    format!(
        "advertising is off. It publishes this machine's name, address and port \
         to every device on the network, so it is off until you say otherwise.\n\
         \n\
         Add this to {path}:\n\
         \n    [hostname]\n    enabled = true\n"
    )
}
