//! `autonet doctor` — the diagnostic checklist, as pure functions.
//!
//! `status` answers *what is the address*. When there is no address, or not the
//! one the user expected, `status -v` prints a scoring table that assumes the
//! reader already understands the selector. Doctor is the plain-language
//! version: which layer is broken, and one sentence about what that means.
//!
//! Everything the checks read is gathered by the caller and handed in.
//! [`checks`] opens no socket and makes no syscall, which is what lets the same
//! assertions run against a JSON fixture on every platform.
//!
//! ## Why there are four statuses and not three
//!
//! The checklist this command implements was specified as pass/fail/warn. A row
//! AutoNet *did not verify* is none of those. Marking it `pass` claims a check that never happened, and
//! marking it `warn` means doctor never comes back clean, which teaches people
//! to stop reading it. [`Status::Unknown`] says the true thing — AutoNet did not
//! determine this — and never affects the exit code. It carries the
//! bind-address guidance, and every row below the first when the snapshot
//! itself could not be taken.
//!
//! ## The bind-address row is advice, not detection
//!
//! ADR 0001 accepted one real cost: a server bound to a specific address rather
//! than a wildcard stops answering when the network moves, and `autonet run`
//! will not repair it. Doctor is where that surfaces. It cannot be *detected*:
//! nothing on this side of the process boundary can see what argument a program
//! passes to `bind()`, and in the ordinary case — `autonet doctor --port 3000`
//! before the server is started — there is not even a socket to inspect. So the
//! row explains the distinction and points at the user's own configuration,
//! marked [`Status::Unknown`] because that is what it is.

use std::fmt::Write as _;

use autonet_core::config::SelectionConfig;
use autonet_core::model::{AddressScope, Family, Interface, InterfaceKind, NetworkState, Route};
use autonet_core::select::{SelectedAddress, Selection};

use crate::port::{self, Availability, PortCheck};
use crate::render::Theme;

/// Total width the text report wraps to.
///
/// Fixed rather than read from the terminal: it keeps the rendering a pure
/// function of its input, so a test asserts the same layout everywhere.
const LINE_WIDTH: usize = 78;

/// The narrowest detail column worth wrapping into.
const MIN_DETAIL: usize = 24;

/// The verdict on one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Checked, and fine.
    Pass,
    /// Checked, and worth knowing about, but not broken.
    Warn,
    /// Checked, and broken.
    Fail,
    /// **Not checked.** AutoNet could not or did not determine this.
    Unknown,
}

impl Status {
    /// The stable name used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Unknown => "unknown",
        }
    }

    /// The fixed-width token used in the text report.
    ///
    /// Words rather than `✓`/`✗`: the checklist is printed on Windows consoles
    /// whose code page is not always UTF-8, and a mangled tick in the status
    /// column would be worse than a plain one. All four are exactly six
    /// characters, so colouring the token cannot shift the columns after it.
    pub fn token(self) -> &'static str {
        match self {
            Self::Pass => "[ ok ]",
            Self::Warn => "[warn]",
            Self::Fail => "[fail]",
            Self::Unknown => "[ ?  ]",
        }
    }

    /// The token, coloured for a terminal.
    fn paint(self, theme: Theme) -> String {
        match self {
            Self::Pass => theme.good(self.token()),
            Self::Warn => theme.warn(self.token()),
            Self::Fail => theme.bad(self.token()),
            Self::Unknown => theme.muted(self.token()),
        }
    }
}

/// One line of the checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Stable identifier, for JSON consumers. Never changes wording.
    pub id: &'static str,
    /// Human-readable name, shown in the text report.
    ///
    /// Owned rather than `&'static str` for the one row whose name carries a
    /// value: `Port 3000`. The `id` stays static, so JSON consumers key off a
    /// name that does not vary with input.
    pub label: String,
    /// What the check concluded.
    pub status: Status,
    /// One sentence of explanation. Always present, including on a pass.
    pub detail: String,
}

impl Check {
    fn new(
        id: &'static str,
        label: impl Into<String>,
        status: Status,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            status,
            detail: detail.into(),
        }
    }
}

/// A network snapshot and what the selector made of it.
///
/// Borrowed rather than owned so the caller keeps ownership of the snapshot it
/// already took: doctor reads existing state, it does not gather any.
pub struct Snapshot<'a> {
    /// The machine's interfaces and routes.
    pub state: &'a NetworkState,
    /// The selector's full result, candidates included.
    pub selection: &'a Selection,
    /// The configuration the selection ran under.
    pub config: &'a SelectionConfig,
}

/// Everything the checks read.
pub struct Diagnosis<'a> {
    /// The backend's own name, for example `linux-netlink`.
    pub platform: &'a str,
    /// The target this binary was built for, for example `linux`.
    pub os: &'a str,
    /// The snapshot, or the reason there is not one.
    pub network: Result<Snapshot<'a>, String>,
    /// The port under examination and what probing it found, if a port is known.
    pub port: Option<(u16, &'a PortCheck)>,
}

/// The checks below the first, in order.
///
/// One list, used by both the normal path and the no-snapshot path, so the two
/// cannot drift apart and leave a JSON consumer with a row that appears only
/// sometimes.
const DEFERRED: [(&str, &str); 5] = [
    ("network_interface", "Network interface"),
    ("ipv4_address", "IPv4 address"),
    ("default_route", "Default route"),
    ("selected_address", "Selected address"),
    ("lan_candidate", "LAN reachable"),
];

/// Run every check.
pub fn checks(diagnosis: &Diagnosis) -> Vec<Check> {
    let mut out = vec![operating_system(diagnosis)];

    let Ok(snapshot) = &diagnosis.network else {
        // The OS row already carries the reason. Repeating it five times would
        // bury it; claiming these five failed would blame the network for a
        // fault that is AutoNet's.
        for (id, label) in DEFERRED {
            out.push(Check::new(
                id,
                label,
                Status::Unknown,
                // ASCII only: this is printed, and the Windows console
                // code page is not always UTF-8.
                "not checked - there is no network snapshot to check",
            ));
        }
        out.push(bind_address(None));
        return out;
    };

    out.push(network_interface(snapshot));
    out.push(ipv4_address(snapshot));
    out.push(default_route(snapshot));
    out.push(selected_address(snapshot));
    out.push(lan_candidate(snapshot));

    if let Some((port, check)) = diagnosis.port {
        out.push(port_row(port, check));
    }

    out.push(bind_address(snapshot.selection.selected.as_ref()));
    out
}

/// The worst thing that happened, ignoring what was never checked.
///
/// [`Status::Unknown`] is deliberately not a candidate: it is the absence of a
/// verdict, not a bad one, and a clean run always carries at least one because
/// the bind-address row is always unknown.
pub fn verdict(checks: &[Check]) -> Status {
    if checks.iter().any(|c| c.status == Status::Fail) {
        Status::Fail
    } else if checks.iter().any(|c| c.status == Status::Warn) {
        Status::Warn
    } else {
        Status::Pass
    }
}

/// Whether the exit code should be a success.
///
/// Only a real failure counts. A warning is something to read, not a reason for
/// a script to stop, and an unknown was never checked at all.
pub fn healthy(checks: &[Check]) -> bool {
    verdict(checks) != Status::Fail
}

/// The plain-language line under the checklist.
///
/// Two clauses: how the run went, then what it means for the question AutoNet
/// exists to answer. The second is read off the selected-address row rather
/// than from the tally, because "three checks failed" and "you have no usable
/// address" are different facts and only one of them is what the user came for.
pub fn summary(checks: &[Check]) -> String {
    let count = |status: Status| checks.iter().filter(|c| c.status == status).count();
    let (failed, warned, unknown) = (
        count(Status::Fail),
        count(Status::Warn),
        count(Status::Unknown),
    );

    let tally = if failed > 0 {
        format!("{failed} check{} failed", plural(failed))
    } else if warned > 0 {
        format!("{warned} warning{}, nothing failed", plural(warned))
    } else {
        // Not `checks.len()`: an unverified row did not pass, and rounding it
        // up into the total would be the exact dishonesty `Unknown` exists to
        // prevent.
        format!("All {} checks passed", count(Status::Pass))
    };
    let tally = if unknown > 0 {
        format!("{tally}, {unknown} not verified")
    } else {
        tally
    };

    let outcome = match checks.iter().find(|c| c.id == "selected_address") {
        Some(check) => match check.status {
            Status::Pass => "AutoNet can give another device an address that reaches this machine.",
            Status::Warn => "The selected address is loopback, so only this machine can reach it.",
            Status::Fail => "AutoNet has no address to offer.",
            Status::Unknown => "AutoNet could not get far enough to look for an address.",
        },
        None => "AutoNet could not get far enough to look for an address.",
    };

    format!("{tally}. {outcome}")
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

// ---------------------------------------------------------------------------
// The checks
// ---------------------------------------------------------------------------

fn operating_system(diagnosis: &Diagnosis) -> Check {
    let (status, detail) = match &diagnosis.network {
        Ok(_) => (
            Status::Pass,
            format!("{}, via {}", diagnosis.os, diagnosis.platform),
        ),
        Err(error) => (
            Status::Fail,
            format!(
                "{} is not supported, or could not be queried: {error}",
                diagnosis.os
            ),
        ),
    };
    Check::new("operating_system", "Operating system", status, detail)
}

/// Is there anything that could carry traffic off this machine?
///
/// Loopback is excluded because it is always present and never an answer, and
/// down interfaces because a cable that is out is not a network.
fn network_interface(snapshot: &Snapshot) -> Check {
    let usable: Vec<&Interface> = snapshot
        .state
        .interfaces
        .iter()
        .filter(|i| is_usable_link(i))
        .collect();

    let Some(first) = usable.first() else {
        return Check::new(
            "network_interface",
            "Network interface",
            Status::Fail,
            "no interface is both up and something other than loopback",
        );
    };

    let named = format!("{} ({}, {})", first.name, first.kind, first.state);
    let detail = if usable.len() == 1 {
        named
    } else {
        format!("{named}, and {} more", usable.len() - 1)
    };

    Check::new(
        "network_interface",
        "Network interface",
        Status::Pass,
        detail,
    )
}

/// Whether an interface could plausibly carry traffic to another device.
///
/// Split out so the rule is stated once and cannot drift between the two
/// checks that apply it.
fn is_usable_link(interface: &Interface) -> bool {
    interface.kind != InterfaceKind::Loopback && !interface.state.is_down()
}

fn ipv4_address(snapshot: &Snapshot) -> Check {
    let addressed = |family: Family| -> Option<String> {
        snapshot
            .state
            .interfaces
            .iter()
            .filter(|i| is_usable_link(i))
            .find_map(|i| {
                i.addresses_of(family)
                    .next()
                    .map(|a| format!("{a} on {}", i.name))
            })
    };

    if let Some(found) = addressed(Family::V4) {
        return Check::new("ipv4_address", "IPv4 address", Status::Pass, found);
    }

    let Some(found) = addressed(Family::V6) else {
        return Check::new(
            "ipv4_address",
            "IPv4 address",
            Status::Fail,
            "no interface off loopback holds an address of either family",
        );
    };

    // Not a failure: AutoNet works over IPv6, and if the user asked for IPv6
    // then the absence of IPv4 is the configuration working, not a fault.
    let asked_for_v6 = snapshot.config.prefer_family.preferred() == Some(Family::V6);
    let status = if asked_for_v6 {
        Status::Pass
    } else {
        Status::Warn
    };
    Check::new(
        "ipv4_address",
        "IPv4 address",
        status,
        format!("none; this machine is IPv6-only ({found}), which works if the other device speaks IPv6"),
    )
}

fn default_route(snapshot: &Snapshot) -> Check {
    let routes: Vec<&Route> = snapshot.state.default_routes().collect();

    let Some(best) = routes.first() else {
        return Check::new(
            "default_route",
            "Default route",
            Status::Fail,
            "none; this machine has no route off its own link",
        );
    };

    let via = best
        .gateway
        .map_or_else(|| "on-link".to_string(), |g| format!("via {g}"));
    let interface = snapshot
        .state
        .interface_by_index(best.interface_index)
        .map_or_else(|| format!("#{}", best.interface_index), |i| i.name.clone());
    let detail = if routes.len() == 1 {
        format!("{via} on {interface}")
    } else {
        format!("{via} on {interface}, and {} more", routes.len() - 1)
    };

    Check::new("default_route", "Default route", Status::Pass, detail)
}

fn selected_address(snapshot: &Snapshot) -> Check {
    let Some(selected) = &snapshot.selection.selected else {
        return Check::new(
            "selected_address",
            "Selected address",
            Status::Fail,
            snapshot.selection.failure_reason(snapshot.config),
        );
    };

    if selected.scope == AddressScope::Loopback {
        return Check::new(
            "selected_address",
            "Selected address",
            Status::Warn,
            format!(
                "{} is a loopback address, reachable only from this machine; it was selected because loopback is allowed",
                selected.ip
            ),
        );
    }

    Check::new(
        "selected_address",
        "Selected address",
        Status::Pass,
        format!(
            "{} ({}) on {}, which is not loopback",
            selected.ip, selected.scope, selected.interface
        ),
    )
}

fn lan_candidate(snapshot: &Snapshot) -> Check {
    let reachable = snapshot
        .selection
        .eligible()
        .filter(|c| c.address.scope.is_reachable_by_peers())
        .count();

    if reachable == 0 {
        return Check::new(
            "lan_candidate",
            "LAN reachable",
            Status::Fail,
            "no address on this machine is in a range another device on the network could reach",
        );
    }

    Check::new(
        "lan_candidate",
        "LAN reachable",
        Status::Pass,
        format!(
            "{reachable} address{} another device could reach",
            if reachable == 1 { "" } else { "es" }
        ),
    )
}

/// The port row, from Task 2's probe.
///
/// Warns rather than fails for the same reason `autonet run` does: `--port`
/// says what URL to print, not what any program will bind, so a busy port is a
/// strong hint and not a fault of this machine.
fn port_row(port: u16, check: &PortCheck) -> Check {
    let (status, detail) = match (check.on_address, check.on_wildcard) {
        (Availability::InUse, _) => (Status::Warn, format!("already in use on {}", check.address)),
        (Availability::Free, Availability::InUse) => (
            Status::Warn,
            format!(
                "free on {}, but held elsewhere on this machine; a server binding {} will fail",
                check.address, check.wildcard
            ),
        ),
        (Availability::Free, _) => (
            Status::Pass,
            format!("nothing is listening on {}", check.address),
        ),
        // A privileged port refused with EACCES says nothing about whether it
        // is busy, and reporting it either way would be a guess.
        (Availability::Undetermined, _) => (
            Status::Unknown,
            format!("AutoNet may not bind {port} as this user, so it could not be tested"),
        ),
    };

    let detail = match port::holder_note(&check.holder) {
        Some(note) if status != Status::Pass => format!("{detail}. {note}"),
        _ => detail,
    };

    Check::new("port", format!("Port {port}"), status, detail)
}

/// ADR 0001's check, as guidance.
///
/// See the module documentation for why this cannot be a detection and is not
/// dressed up as one.
fn bind_address(selected: Option<&SelectedAddress>) -> Check {
    let detail = match selected {
        // Loopback gets its own sentence. The wildcard advice below is about
        // an address that *moves*, and 127.0.0.1 never does; telling the user
        // it might would be a plainly wrong claim in the one row of this
        // command that is supposed to be careful about what it claims.
        Some(address) if address.scope == AddressScope::Loopback => format!(
            "AutoNet cannot see what address your server binds. Bound to {} it is reachable \
             only from this machine; bound to the wildcard it answers on every address this \
             machine has. Check your program's host or bind setting.",
            address.ip
        ),
        Some(address) => {
            let wildcard = if address.ip.is_ipv4() {
                "0.0.0.0"
            } else {
                "::"
            };
            format!(
                "AutoNet cannot see what address your server binds. If it binds {} \
                 specifically, it stops answering when the network changes; if it binds \
                 {wildcard}, it follows the change. Check your program's host or bind setting.",
                address.ip
            )
        }
        None => "AutoNet cannot see what address your server binds. A server bound to one \
                 specific address stops answering when the network changes; one bound to the \
                 wildcard follows it. Check your program's host or bind setting."
            .to_string(),
    };

    Check::new("bind_address", "Bind address", Status::Unknown, detail)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The text report: the checklist, then the summary line.
pub fn report(platform: &str, checks: &[Check], theme: Theme) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {}",
        theme.heading("AutoNet doctor"),
        theme.muted(platform)
    );
    out.push('\n');

    let label_width = checks
        .iter()
        .map(|c| c.label.chars().count())
        .max()
        .unwrap_or(0);
    // Two spaces of indent, the six-character token, two spaces, the padded
    // label, two spaces. The token is coloured but never width-padded, so an
    // escape code cannot push the columns after it out of line.
    let gutter = 2 + 6 + 2 + label_width + 2;
    let detail_width = LINE_WIDTH.saturating_sub(gutter).max(MIN_DETAIL);

    for check in checks {
        let wrapped = wrap(&check.detail, detail_width);
        let (first, rest) = wrapped
            .split_first()
            .expect("wrap yields at least one line");

        let line = format!(
            "  {}  {:label_width$}  {first}",
            check.status.paint(theme),
            check.label
        );
        let _ = writeln!(out, "{}", line.trim_end());

        for continuation in rest {
            let _ = writeln!(out, "{:gutter$}{continuation}", "");
        }
    }

    out.push('\n');
    for line in wrap(&summary(checks), LINE_WIDTH) {
        let _ = writeln!(out, "{line}");
    }
    out
}

/// Greedy word wrap.
///
/// A word longer than `width` gets a line of its own rather than being broken:
/// the long tokens here are addresses and interface names, and splitting
/// `2401:db8::1` across two lines would make it unusable.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();

    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use autonet_core::model::{Address, FamilyPreference, InterfaceState, IpNetwork};
    use autonet_core::select::select;

    use super::*;

    // ---- builders -------------------------------------------------------
    //
    // Hand-built states rather than fixtures: these tests are about the
    // wording of one check at a time, and a fixture would drag in five other
    // interfaces per assertion. Whole-machine scenarios live in
    // `tests/doctor.rs`.

    fn addr(ip: &str, prefix_len: u8) -> Address {
        Address::new(ip.parse().expect("a valid address"), prefix_len)
    }

    fn iface(
        name: &str,
        index: u32,
        kind: InterfaceKind,
        state: InterfaceState,
        addrs: &[(&str, u8)],
    ) -> Interface {
        let mut interface = Interface::new(name, index, kind, state);
        for (ip, prefix_len) in addrs {
            interface.addresses.push(addr(ip, *prefix_len));
        }
        interface
    }

    fn loopback() -> Interface {
        iface(
            "lo",
            1,
            InterfaceKind::Loopback,
            InterfaceState::Up,
            &[("127.0.0.1", 8), ("::1", 128)],
        )
    }

    fn default_route(index: u32, gateway: &str) -> Route {
        let gateway: IpAddr = gateway.parse().expect("a valid gateway");
        Route {
            destination: Some(IpNetwork::new(
                if gateway.is_ipv4() {
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                } else {
                    IpAddr::V6(Ipv6Addr::UNSPECIFIED)
                },
                0,
            )),
            gateway: Some(gateway),
            interface_index: index,
            metric: 100,
            family: Family::of(&gateway),
            preferred_source: None,
        }
    }

    /// A machine with one working Wi-Fi link: every check should pass.
    fn healthy_state() -> NetworkState {
        NetworkState::new(
            vec![
                loopback(),
                iface(
                    "wlo1",
                    3,
                    InterfaceKind::Wireless,
                    InterfaceState::Up,
                    &[("192.168.1.20", 24), ("fe80::1", 64)],
                ),
            ],
            vec![default_route(3, "192.168.1.1")],
        )
    }

    /// Run the checks the way the command does, with no port probe.
    fn run(state: &NetworkState, config: &SelectionConfig) -> Vec<Check> {
        let selection = select(state, config);
        checks(&Diagnosis {
            platform: "test-provider",
            os: "testos",
            network: Ok(Snapshot {
                state,
                selection: &selection,
                config,
            }),
            port: None,
        })
    }

    fn run_default(state: &NetworkState) -> Vec<Check> {
        run(state, &SelectionConfig::default())
    }

    #[track_caller]
    fn row<'a>(checks: &'a [Check], id: &str) -> &'a Check {
        checks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no check with id {id:?}"))
    }

    #[track_caller]
    fn assert_status(checks: &[Check], id: &str, expected: Status) {
        let check = row(checks, id);
        assert_eq!(
            check.status, expected,
            "{id}: expected {expected:?}, got {:?} — {}",
            check.status, check.detail
        );
    }

    // ---- the happy path -------------------------------------------------

    #[test]
    fn a_working_machine_passes_every_check_it_actually_makes() {
        let checks = run_default(&healthy_state());

        for id in [
            "operating_system",
            "network_interface",
            "ipv4_address",
            "default_route",
            "selected_address",
            "lan_candidate",
        ] {
            assert_status(&checks, id, Status::Pass);
        }
        assert_eq!(verdict(&checks), Status::Pass);
        assert!(healthy(&checks));
    }

    #[test]
    fn every_check_carries_a_detail_even_when_it_passes() {
        // A bare "ok" with no explanation is the failure mode this command
        // exists to avoid: the point is to say what was found, not just that
        // something was.
        for check in run_default(&healthy_state()) {
            assert!(
                !check.detail.trim().is_empty(),
                "{} had no detail",
                check.id
            );
            assert!(!check.label.trim().is_empty(), "{} had no label", check.id);
        }
    }

    // ---- one check at a time --------------------------------------------

    #[test]
    fn a_machine_with_only_loopback_has_no_usable_interface() {
        let state = NetworkState::new(vec![loopback()], vec![]);
        let checks = run_default(&state);

        assert_status(&checks, "network_interface", Status::Fail);
        assert_status(&checks, "ipv4_address", Status::Fail);
        assert_status(&checks, "default_route", Status::Fail);
        assert_status(&checks, "selected_address", Status::Fail);
        assert_status(&checks, "lan_candidate", Status::Fail);
    }

    #[test]
    fn a_down_interface_does_not_count_as_a_usable_link() {
        // The disconnected laptop: an address is still bound to an interface
        // whose link is down. Counting it would report a healthy IPv4 setup on
        // a machine with no network at all.
        let state = NetworkState::new(
            vec![
                loopback(),
                iface(
                    "wlo1",
                    3,
                    InterfaceKind::Wireless,
                    InterfaceState::Down,
                    &[("169.254.12.34", 16)],
                ),
            ],
            vec![],
        );
        let checks = run_default(&state);

        assert_status(&checks, "network_interface", Status::Fail);
        assert_status(&checks, "ipv4_address", Status::Fail);
    }

    #[test]
    fn an_up_and_addressed_machine_with_no_route_still_fails_the_route_check() {
        // The Docker-only case: interfaces up, addresses bound, and nothing
        // that reaches anything. The two checks must disagree, or the
        // checklist tells the user nothing status could not.
        let state = NetworkState::new(
            vec![
                loopback(),
                iface(
                    "docker0",
                    4,
                    InterfaceKind::Container,
                    InterfaceState::Up,
                    &[("172.17.0.1", 16)],
                ),
            ],
            vec![],
        );
        let checks = run_default(&state);

        assert_status(&checks, "network_interface", Status::Pass);
        assert_status(&checks, "ipv4_address", Status::Pass);
        assert_status(&checks, "default_route", Status::Fail);
        // The address exists and is even private, but the selector rejects
        // container bridges, so nothing was chosen.
        assert_status(&checks, "selected_address", Status::Fail);
    }

    #[test]
    fn the_interface_row_counts_the_rest_rather_than_listing_them() {
        let mut state = healthy_state();
        state.interfaces.push(iface(
            "eno1",
            2,
            InterfaceKind::Ethernet,
            InterfaceState::Up,
            &[("10.0.0.5", 24)],
        ));

        let checks = run_default(&state);
        let detail = &row(&checks, "network_interface").detail;
        assert!(detail.contains("and 1 more"), "{detail}");
    }

    /// A machine with a working IPv6 link and no IPv4 at all.
    fn ipv6_only_state() -> NetworkState {
        NetworkState::new(
            vec![
                loopback(),
                iface(
                    "eno1",
                    2,
                    InterfaceKind::Ethernet,
                    InterfaceState::Up,
                    &[("2401:db8::5", 64)],
                ),
            ],
            vec![default_route(2, "2401:db8::1")],
        )
    }

    #[test]
    fn no_ipv4_at_all_warns_rather_than_fails_when_ipv6_is_present() {
        // AutoNet works over IPv6. A machine with only IPv6 is unusual, not
        // broken, and marking the address row failed would send the user
        // hunting for a fault that is not there.
        let checks = run_default(&ipv6_only_state());

        assert_status(&checks, "ipv4_address", Status::Warn);
        assert!(row(&checks, "ipv4_address").detail.contains("IPv6-only"));

        // The machine as a whole still fails, because the default preference
        // is a hard IPv4 filter and there is no IPv4 address to select. That
        // is the selector's decision, not this row's: the two must be allowed
        // to disagree, or the checklist cannot tell the user that the family
        // is the problem rather than the network.
        assert_status(&checks, "selected_address", Status::Fail);
    }

    #[test]
    fn asking_for_ipv6_makes_the_absence_of_ipv4_a_pass() {
        let checks = run(&ipv6_only_state(), &ipv6_config());

        assert_status(&checks, "ipv4_address", Status::Pass);
        // And with the family it was told to prefer, the machine is healthy.
        assert_status(&checks, "selected_address", Status::Pass);
        assert!(healthy(&checks));
    }

    fn ipv6_config() -> SelectionConfig {
        SelectionConfig {
            prefer_family: FamilyPreference::Ipv6,
            ..SelectionConfig::default()
        }
    }

    #[test]
    fn a_selected_loopback_address_warns_rather_than_passing() {
        // Reachable only under --allow-loopback, and the whole point of
        // AutoNet is that 127.0.0.1 is not an answer to the question. Passing
        // it silently would be the tool endorsing the thing it was built to
        // correct.
        let state = NetworkState::new(vec![loopback()], vec![]);
        let config = SelectionConfig {
            allow_loopback: true,
            ..SelectionConfig::default()
        };
        let checks = run(&state, &config);

        assert_status(&checks, "selected_address", Status::Warn);
        assert!(row(&checks, "selected_address").detail.contains("loopback"));
        // Still not something another device can reach.
        assert_status(&checks, "lan_candidate", Status::Fail);
    }

    #[test]
    fn a_failed_selection_reports_the_selectors_own_reason() {
        let state = NetworkState::new(vec![loopback()], vec![]);
        let config = SelectionConfig::default();
        let selection = select(&state, &config);

        let checks = run(&state, &config);
        let detail = &row(&checks, "selected_address").detail;
        assert_eq!(detail, &selection.failure_reason(&config));
    }

    #[test]
    fn the_lan_row_says_something_the_selection_row_does_not() {
        // Two rows that always agree are one row. When both fail they must
        // still explain different things, or the checklist is padding.
        let state = NetworkState::new(vec![loopback()], vec![]);
        let checks = run_default(&state);

        assert_ne!(
            row(&checks, "selected_address").detail,
            row(&checks, "lan_candidate").detail
        );
    }

    // ---- the port row ---------------------------------------------------

    fn port_check(on_address: Availability, on_wildcard: Availability) -> PortCheck {
        PortCheck {
            address: "192.168.1.20".parse().expect("a valid address"),
            wildcard: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            on_address,
            on_wildcard,
            holder: autonet_platform::PortHolder::Named {
                pid: 4821,
                name: "python3".to_string(),
            },
        }
    }

    #[test]
    fn a_free_port_passes_and_does_not_name_a_holder() {
        let check = port_row(3000, &port_check(Availability::Free, Availability::Free));

        assert_eq!(check.status, Status::Pass);
        assert_eq!(check.label, "Port 3000");
        // `inspect` skips the lookup when nothing collided, but a stale holder
        // must not leak into a passing row even if one is present.
        assert!(!check.detail.contains("python3"), "{}", check.detail);
    }

    #[test]
    fn a_held_port_warns_and_names_the_holder() {
        let check = port_row(3000, &port_check(Availability::InUse, Availability::InUse));

        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("already in use"));
        assert!(check.detail.contains("python3 (pid 4821)"));
    }

    #[test]
    fn a_port_free_here_but_held_on_the_wildcard_is_reported_as_such() {
        let check = port_row(3000, &port_check(Availability::Free, Availability::InUse));

        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("0.0.0.0"), "{}", check.detail);
    }

    #[test]
    fn a_port_that_could_not_be_tested_is_unknown_rather_than_free() {
        // `--port 80` as an ordinary user is refused with EACCES before
        // anything looks at whether it is busy. Calling that "free" would be
        // a guess presented as a measurement.
        let check = port_row(
            80,
            &port_check(Availability::Undetermined, Availability::Undetermined),
        );

        assert_eq!(check.status, Status::Unknown);
        assert!(healthy(&[check]));
    }

    #[test]
    fn the_port_row_is_absent_when_no_port_was_given() {
        assert!(run_default(&healthy_state()).iter().all(|c| c.id != "port"));
    }

    // ---- the bind-address row -------------------------------------------

    #[test]
    fn the_bind_row_is_always_unknown_and_never_claims_to_have_checked() {
        // The ADR asked for this row; what it cannot be is a detection. See
        // the module documentation. If this ever becomes a pass, something has
        // started claiming knowledge of another process's bind() call.
        let checks = run_default(&healthy_state());
        let bind = row(&checks, "bind_address");

        assert_eq!(bind.status, Status::Unknown);
        assert!(bind.detail.contains("cannot see"));
        assert!(bind.detail.contains("0.0.0.0"));
        assert!(!bind.detail.contains("is bound"), "{}", bind.detail);
    }

    #[test]
    fn the_bind_row_names_the_wildcard_of_the_selected_family() {
        let checks = run(&ipv6_only_state(), &ipv6_config());
        let detail = &row(&checks, "bind_address").detail;

        assert!(detail.contains("::,"), "{detail}");
        assert!(!detail.contains("0.0.0.0"), "{detail}");
    }

    #[test]
    fn the_bind_row_does_not_tell_a_loopback_user_their_address_will_move() {
        // 127.0.0.1 does not change when the network does, so the wildcard
        // advice would be a false statement here.
        let state = NetworkState::new(vec![loopback()], vec![]);
        let config = SelectionConfig {
            allow_loopback: true,
            ..SelectionConfig::default()
        };
        let checks = run(&state, &config);
        let detail = &row(&checks, "bind_address").detail;

        assert!(!detail.contains("stops answering"), "{detail}");
        assert!(detail.contains("only from this machine"), "{detail}");
    }

    // ---- no snapshot ----------------------------------------------------

    #[test]
    fn a_platform_that_could_not_be_queried_fails_one_row_and_leaves_the_rest_unknown() {
        let checks = checks(&Diagnosis {
            platform: "unsupported",
            os: "plan9",
            network: Err("plan9 is not supported".to_string()),
            port: None,
        });

        assert_status(&checks, "operating_system", Status::Fail);
        assert!(row(&checks, "operating_system").detail.contains("plan9"));

        // Not `fail`: nothing about the network was established, and blaming
        // the network for AutoNet's own inability to look would be wrong.
        for (id, _) in DEFERRED {
            assert_status(&checks, id, Status::Unknown);
        }
        assert!(!healthy(&checks));
    }

    #[test]
    fn the_rows_are_the_same_ids_whether_or_not_a_snapshot_was_taken() {
        // A JSON consumer should not have to handle a row appearing only
        // sometimes. Only the port row is conditional, and it is absent from
        // both of these.
        let with = run_default(&healthy_state());
        let without = checks(&Diagnosis {
            platform: "unsupported",
            os: "plan9",
            network: Err("no".to_string()),
            port: None,
        });

        let ids = |checks: &[Check]| checks.iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(ids(&with), ids(&without));
    }

    // ---- fixtures -------------------------------------------------------
    //
    // The same JSON snapshots `autonet-core` scores against, run through the
    // whole checklist. `autonet-cli` has no library target, so these live here
    // rather than in `tests/`; they are still ordinary data-driven tests with
    // no live network involved, which is what makes them mean the same thing
    // on all three CI runners.

    fn fixture(name: &str) -> NetworkState {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    fn fixture_names() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .expect("the fixture directory")
            .map(|entry| entry.expect("a directory entry").path())
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .map(|path| {
                path.file_stem()
                    .expect("a file stem")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_working_wifi_machine_passes_the_whole_checklist() {
        let checks = run_default(&fixture("wifi-only"));

        assert_eq!(verdict(&checks), Status::Pass, "{checks:#?}");
        assert!(healthy(&checks));
    }

    #[test]
    fn a_machine_with_only_docker_bridges_is_up_addressed_and_still_unreachable() {
        // The case that makes doctor worth having: every interface is up, every
        // one has an address, and none of it can be reached from another
        // device. A single pass/fail verdict would say nothing useful here.
        let checks = run_default(&fixture("docker-only"));

        assert_status(&checks, "network_interface", Status::Pass);
        assert_status(&checks, "ipv4_address", Status::Pass);
        assert_status(&checks, "default_route", Status::Fail);
        assert_status(&checks, "selected_address", Status::Fail);
        assert_status(&checks, "lan_candidate", Status::Fail);
    }

    #[test]
    fn a_disconnected_laptop_fails_at_the_link_rather_than_at_the_selector() {
        let checks = run_default(&fixture("disconnected"));

        assert_status(&checks, "network_interface", Status::Fail);
        assert_status(&checks, "default_route", Status::Fail);
        assert_status(&checks, "selected_address", Status::Fail);
    }

    #[test]
    fn a_machine_with_nothing_but_loopback_fails_from_the_first_row_down() {
        let checks = run_default(&fixture("loopback-only"));

        assert_status(&checks, "network_interface", Status::Fail);
        assert_status(&checks, "ipv4_address", Status::Fail);
        assert_status(&checks, "selected_address", Status::Fail);
    }

    #[test]
    fn an_ipv6_only_fixture_warns_about_ipv4_rather_than_failing() {
        assert_status(
            &run_default(&fixture("ipv6-only")),
            "ipv4_address",
            Status::Warn,
        );
    }

    #[test]
    fn a_wired_machine_passes_the_whole_checklist_too() {
        // A second healthy shape, so a pass is not an artefact of one fixture.
        let checks = run_default(&fixture("wifi-and-ethernet"));

        assert_eq!(verdict(&checks), Status::Pass, "{checks:#?}");
        assert_status(&checks, "default_route", Status::Pass);
    }

    #[test]
    fn every_fixture_produces_the_same_rows_in_the_same_order() {
        // No fixture may make a row appear or disappear: the port row is the
        // only conditional one, and none of these carry a port. A machine that
        // silently drops a check is worse than one that reports a failure.
        let expected: Vec<&str> = run_default(&fixture("wifi-only"))
            .iter()
            .map(|c| c.id)
            .collect();

        for name in fixture_names() {
            let checks = run_default(&fixture(&name));
            let ids: Vec<&str> = checks.iter().map(|c| c.id).collect();
            assert_eq!(ids, expected, "{name}");

            // And every row is renderable: a panic in the report path would
            // turn a diagnosis into a crash on exactly the broken machine the
            // user ran this on.
            let text = report("fixture", &checks, Theme::plain());
            assert!(text.contains("AutoNet doctor"), "{name}");
            for line in text.lines() {
                assert!(line.chars().count() <= LINE_WIDTH, "{name}: {line:?}");
            }
        }
    }

    // ---- verdict and summary --------------------------------------------

    fn synthetic(statuses: &[Status]) -> Vec<Check> {
        statuses
            .iter()
            .map(|status| Check::new("synthetic", "Synthetic", *status, "detail"))
            .collect()
    }

    #[test]
    fn the_verdict_is_the_worst_thing_that_was_actually_checked() {
        assert_eq!(
            verdict(&synthetic(&[Status::Pass, Status::Pass])),
            Status::Pass
        );
        assert_eq!(
            verdict(&synthetic(&[Status::Pass, Status::Warn])),
            Status::Warn
        );
        assert_eq!(
            verdict(&synthetic(&[Status::Warn, Status::Fail, Status::Pass])),
            Status::Fail
        );
    }

    #[test]
    fn an_unknown_row_can_never_make_the_verdict_worse() {
        // Otherwise the always-unknown bind row would mean doctor never comes
        // back clean, and a checklist that always complains gets ignored.
        assert_eq!(
            verdict(&synthetic(&[Status::Pass, Status::Unknown])),
            Status::Pass
        );
        assert!(healthy(&synthetic(&[Status::Unknown, Status::Unknown])));
    }

    #[test]
    fn the_summary_does_not_count_an_unverified_row_as_a_pass() {
        let line = summary(&synthetic(&[Status::Pass, Status::Pass, Status::Unknown]));

        assert!(line.contains("All 2 checks passed"), "{line}");
        assert!(line.contains("1 not verified"), "{line}");
    }

    #[test]
    fn the_summary_says_what_the_result_means_not_only_how_many_rows_failed() {
        let clean = summary(&run_default(&healthy_state()));
        assert!(clean.contains("another device"), "{clean}");

        let broken = summary(&run_default(&NetworkState::new(vec![loopback()], vec![])));
        assert!(broken.contains("5 checks failed"), "{broken}");
        assert!(broken.contains("no address to offer"), "{broken}");
    }

    #[test]
    fn the_summary_reports_warnings_separately_from_failures() {
        let line = summary(&synthetic(&[Status::Pass, Status::Warn]));

        assert!(line.contains("1 warning, nothing failed"), "{line}");
        assert!(!line.contains("checks failed"), "{line}");
    }

    // ---- rendering ------------------------------------------------------

    #[test]
    fn the_report_lines_up_and_wraps_without_colour() {
        let text = report(
            "test-provider",
            &run_default(&healthy_state()),
            Theme::plain(),
        );

        // Every status token occupies the same columns, so the labels after
        // them align. This is the property `render::table` cannot give us,
        // because it sizes columns by counting escape bytes.
        let starts: Vec<usize> = text
            .lines()
            .filter(|line| line.contains("[ ok ]") || line.contains("[ ?  ]"))
            .map(|line| line.find('[').expect("a token"))
            .collect();
        assert!(starts.windows(2).all(|w| w[0] == w[1]), "{text}");

        for line in text.lines() {
            assert!(!line.contains('\u{1b}'), "escape code in plain output");
            assert_eq!(line, line.trim_end(), "trailing whitespace: {line:?}");
        }
    }

    #[test]
    fn a_long_detail_wraps_under_its_own_column_rather_than_running_off() {
        let text = report(
            "test-provider",
            &run_default(&healthy_state()),
            Theme::plain(),
        );
        let bind = text
            .lines()
            .position(|line| line.contains("Bind address"))
            .expect("the bind row");
        let lines: Vec<&str> = text.lines().collect();

        // The row is long enough to need more than one line...
        let continuation = lines[bind + 1];
        assert!(continuation.starts_with("      "), "{continuation:?}");
        // ...and the continuation starts in the detail column, not at the
        // margin, so the checklist still reads as a list.
        let column = lines[bind].find("AutoNet").expect("the detail column");
        assert_eq!(
            continuation.len() - continuation.trim_start().len(),
            column,
            "{continuation:?}"
        );
    }

    #[test]
    fn the_report_never_exceeds_its_own_width() {
        let text = report(
            "test-provider",
            &run_default(&healthy_state()),
            Theme::plain(),
        );

        for line in text.lines() {
            assert!(
                line.chars().count() <= LINE_WIDTH,
                "{} chars: {line:?}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn a_coloured_report_carries_styling_without_disturbing_the_layout() {
        let checks = run_default(&healthy_state());
        let plain = report("test-provider", &checks, Theme::plain());
        let painted = report("test-provider", &checks, Theme::coloured());

        assert!(painted.contains('\u{1b}'));
        // Same words, same wrapping: only the escapes differ.
        assert_eq!(
            strip_escapes(&painted).replace(' ', ""),
            plain.replace(' ', "")
        );
    }

    fn strip_escapes(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    // ---- the wrapper ----------------------------------------------------

    #[test]
    fn wrapping_breaks_on_spaces_and_keeps_every_word() {
        let wrapped = wrap("one two three four five", 9);

        assert_eq!(wrapped, vec!["one two", "three", "four five"]);
    }

    #[test]
    fn a_word_longer_than_the_column_gets_its_own_line_rather_than_being_split() {
        // The long tokens here are addresses. `2401:db8::1` broken across two
        // lines is not an address any more.
        let wrapped = wrap("at 2401:db8:aaaa:bbbb::1 now", 6);

        assert!(
            wrapped.contains(&"2401:db8:aaaa:bbbb::1".to_string()),
            "{wrapped:?}"
        );
    }

    #[test]
    fn wrapping_always_yields_at_least_one_line() {
        // `report` splits the first line off every detail; an empty vector
        // would panic there rather than printing a blank row.
        assert_eq!(wrap("", 10), vec![String::new()]);
        assert_eq!(wrap("   ", 10), vec![String::new()]);
    }
}
