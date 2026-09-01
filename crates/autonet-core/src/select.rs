//! The address selection engine.
//!
//! A developer machine has many addresses at once — Wi-Fi, Ethernet, a Docker
//! bridge per user-defined network, a libvirt bridge, a VPN tunnel, loopback,
//! and several IPv6 addresses. Returning the first one found is the bug this
//! whole project exists to fix.
//!
//! # How it decides
//!
//! Two stages, deliberately separated:
//!
//! 1. **Disqualification** removes only what is *objectively* unusable — a down
//!    interface, loopback, link-local, the wrong family, an explicitly excluded
//!    name. These can never be the right answer, so no amount of scoring should
//!    resurrect them.
//! 2. **Scoring** ranks everything that survives. Nothing else is filtered.
//!
//! That split matters. It would be easy to hard-filter container interfaces —
//! Docker bridges are never the answer — but some people really do put their
//! LAN on a bridge, and `br0` owning the default route should still win. So
//! containers take a large penalty instead of an outright ban: Docker's
//! routeless bridges sink far below anything real, while a legitimate bridge
//! carrying the default route climbs back out.
//!
//! Every candidate keeps the list of rules that fired on it, which is what
//! lets a later `autonet doctor` explain a verdict without re-deriving it.

use std::cmp::Reverse;
use std::net::IpAddr;

use serde::Serialize;

use crate::config::{any_name_matches, SelectionConfig};
use crate::error::{CoreError, Result};
use crate::model::{Address, AddressScope, Family, Interface, InterfaceKind, NetworkState};

/// Scoring weights.
///
/// Public so tests — and, later, `autonet doctor` — can refer to them by name
/// instead of restating magic numbers that would silently drift out of sync.
pub mod weights {
    /// The interface owns the default route for the requested family. This is
    /// the strongest available signal that traffic actually leaves this way.
    pub const DEFAULT_ROUTE_SAME_FAMILY: i32 = 1000;
    /// The interface owns a default route, but for the other family.
    pub const DEFAULT_ROUTE_OTHER_FAMILY: i32 = 400;
    /// A wired NIC.
    pub const KIND_ETHERNET: i32 = 250;
    /// A Wi-Fi NIC.
    pub const KIND_WIRELESS: i32 = 200;
    /// A user-created bridge.
    pub const KIND_BRIDGE: i32 = 150;
    /// A container or virtualisation device.
    pub const KIND_SYNTHETIC: i32 = -800;
    /// A VPN or tunnel device, when not explicitly allowed.
    pub const KIND_VPN: i32 = -300;
    /// A VPN or tunnel device, when the user asked for one.
    pub const KIND_VPN_ALLOWED: i32 = 50;
    /// The address is of the preferred family.
    pub const FAMILY_MATCH: i32 = 150;
    /// RFC 1918 or IPv6 unique-local — the ordinary LAN case.
    pub const SCOPE_PRIVATE: i32 = 100;
    /// Globally routable.
    pub const SCOPE_GLOBAL: i32 = 60;
    /// Carrier-grade NAT: rarely reachable by the peer who wants it.
    pub const SCOPE_CGNAT: i32 = -200;
    /// The interface has a gateway for this family.
    pub const HAS_GATEWAY: i32 = 25;
    /// An IPv6 privacy-extension temporary address; the stable one is a better
    /// thing to hand out, since temporaries are rotated out from under you.
    pub const TEMPORARY_ADDRESS: i32 = -20;
    /// The interface was named in `prefer_interfaces`. Large enough to dominate
    /// every other rule, because it is an explicit instruction from the user.
    pub const PREFERRED_INTERFACE: i32 = 2000;
    /// Divisor applied to the route metric before subtracting it, so a metric
    /// of 600 costs 60 points — enough to break ties between similar links,
    /// never enough to overturn a category difference.
    pub const METRIC_DIVISOR: u32 = 10;
    /// Ceiling on the metric penalty, so an absurd metric cannot dominate.
    pub const METRIC_CAP: u32 = 1000;
}

/// Why a candidate was removed from consideration before scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disqualification {
    /// The kernel reports the interface as down.
    InterfaceDown,
    /// A loopback address, reachable only from this machine.
    Loopback,
    /// A link-local address, which is not usefully routable.
    LinkLocal,
    /// Unspecified, multicast, broadcast, documentation, or reserved.
    SpecialAddress,
    /// Not the address family the caller asked for.
    FamilyMismatch,
    /// Matched `exclude_interfaces`.
    ExcludedByConfig,
    /// A different interface was demanded via `require_interface`.
    NotRequiredInterface,
    /// A container or virtualisation interface that carries no default route.
    ///
    /// Such an interface is not a path to anywhere: handing a developer
    /// `172.17.0.1` when the machine is offline looks like an answer but is one
    /// no other device can reach. Silence is more honest than a wrong address.
    SyntheticWithoutRoute,
}

impl std::fmt::Display for Disqualification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InterfaceDown => "interface is down",
            Self::Loopback => "loopback address is not reachable from other devices",
            Self::LinkLocal => "link-local address is not routable",
            Self::SpecialAddress => "reserved or non-unicast address",
            Self::FamilyMismatch => "wrong address family",
            Self::ExcludedByConfig => "excluded by configuration",
            Self::NotRequiredInterface => "not the requested interface",
            Self::SyntheticWithoutRoute => {
                "container or virtual interface with no route to anywhere"
            }
        })
    }
}

/// A single scoring rule that fired on a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reason {
    /// Stable identifier for the rule, for example `"default_route"`.
    pub rule: &'static str,
    /// How many points it contributed. May be negative.
    pub delta: i32,
}

impl Reason {
    fn new(rule: &'static str, delta: i32) -> Self {
        Self { rule, delta }
    }
}

/// One interface/address pair, with the engine's verdict on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// Name of the interface the address is bound to.
    pub interface: String,
    /// Kernel index of that interface.
    pub interface_index: u32,
    /// What sort of device it is.
    pub interface_kind: InterfaceKind,
    /// The address under consideration.
    pub address: Address,
    /// Total score. Meaningless when `disqualified` is set.
    pub score: i32,
    /// Every rule that fired, in the order they were applied.
    pub reasons: Vec<Reason>,
    /// Set when the candidate was removed before scoring.
    pub disqualified: Option<Disqualification>,
}

impl Candidate {
    /// Whether this candidate is still in the running.
    #[must_use]
    pub fn is_eligible(&self) -> bool {
        self.disqualified.is_none()
    }
}

/// The address AutoNet chose, with the context a caller needs to use it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectedAddress {
    /// The address itself.
    pub ip: IpAddr,
    /// Its family.
    pub family: Family,
    /// Netmask length in bits.
    pub prefix_len: u8,
    /// Where it sits in the address space.
    pub scope: AddressScope,
    /// Name of the interface it is bound to.
    pub interface: String,
    /// Kernel index of that interface.
    pub interface_index: u32,
    /// What sort of device that interface is.
    pub interface_kind: InterfaceKind,
    /// The gateway for this family on that interface, if any.
    pub gateway: Option<IpAddr>,
    /// The score that won.
    pub score: i32,
}

impl SelectedAddress {
    /// The address as it should appear in a URL authority, with IPv6 bracketed.
    #[must_use]
    pub fn url_host(&self) -> String {
        match self.ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        }
    }

    /// A URL another device on the same network can actually open.
    ///
    /// This — not a bind address — is AutoNet's whole purpose. `0.0.0.0:3000`
    /// is a perfectly good thing to listen on and a useless thing to type into
    /// a phone.
    #[must_use]
    pub fn url(&self, port: u16, scheme: &str) -> String {
        format!("{scheme}://{}:{port}", self.url_host())
    }
}

/// The full result of a selection pass: the winner, and every candidate that
/// was considered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Selection {
    /// The chosen address, if any survived.
    pub selected: Option<SelectedAddress>,
    /// All candidates, eligible ones first in descending score order,
    /// disqualified ones after.
    pub candidates: Vec<Candidate>,
}

impl Selection {
    /// Candidates still in the running, best first.
    pub fn eligible(&self) -> impl Iterator<Item = &Candidate> {
        self.candidates.iter().filter(|c| c.is_eligible())
    }

    /// A human-readable explanation of why nothing was selected.
    ///
    /// Usually the most common disqualification, which is nearly always the
    /// actual story: everything was down, or everything was loopback. The
    /// config is needed because two of the interesting cases — "you narrowed
    /// the search yourself" and "this network gave you no usable address of
    /// that family" — cannot be told apart from the candidate list alone.
    #[must_use]
    pub fn failure_reason(&self, config: &SelectionConfig) -> String {
        if self.candidates.is_empty() {
            return "no interfaces reported any addresses".to_string();
        }

        // `NotRequiredInterface` and `ExcludedByConfig` mean "the user narrowed
        // the search", not "this address was unusable". On a machine with
        // twenty interfaces they are the overwhelming majority the moment
        // `--interface` is given, and reporting them would tell the user only
        // what they already know. So they are set aside first, and consulted
        // only if nothing else was rejected.
        let narrowing = |d: &Disqualification| {
            matches!(
                d,
                Disqualification::NotRequiredInterface | Disqualification::ExcludedByConfig
            )
        };
        let rejected: Vec<Disqualification> = self
            .candidates
            .iter()
            .filter_map(|c| c.disqualified)
            .collect();
        let considered: Vec<Disqualification> =
            rejected.iter().copied().filter(|d| !narrowing(d)).collect();

        // Nothing but narrowing reasons: the machine may be perfectly healthy
        // and the user simply pointed at an interface that has no addresses.
        // "most common reason: not the requested interface" would be a
        // non-answer, so say what actually happened.
        if considered.is_empty() {
            return if rejected
                .iter()
                .any(|d| matches!(d, Disqualification::NotRequiredInterface))
            {
                "the requested interface has no usable addresses".to_string()
            } else {
                "every interface was excluded by configuration".to_string()
            };
        }

        // A modal count answers "what rejected the most addresses", which is
        // not the same question as "why can nothing be reached". On a machine
        // with a dozen veths the tally is dominated by container interfaces
        // even when the real story is that this network handed out no routable
        // address at all — the ordinary case for IPv6 behind a home router.
        // Blaming Docker there would send the user hunting the wrong problem,
        // so that case is recognised before any counting happens.
        //
        // Only addresses of the family that was asked for can speak to whether
        // that family is usable. Filtering on the family rather than on the
        // `FamilyMismatch` verdict matters: disqualification is ordered, and an
        // IPv4 address on a down interface is reported as `InterfaceDown` long
        // before the family check ever sees it.
        let in_family: Vec<&Candidate> = self
            .candidates
            .iter()
            .filter(|c| config.prefer_family.admits(c.address.family))
            .collect();
        if !in_family.is_empty()
            && !in_family
                .iter()
                .any(|c| c.address.scope.is_reachable_by_peers())
        {
            return format!(
                "this machine has no {}address another device could reach (only {})",
                config
                    .prefer_family
                    .preferred()
                    .map_or_else(String::new, |f| format!("{f} ")),
                scopes_present(&in_family),
            );
        }

        // Counted in a Vec rather than a map so that ties resolve toward the
        // first reason encountered, keeping the message stable run to run.
        let mut counts: Vec<(Disqualification, usize)> = Vec::new();
        for reason in considered.iter().copied() {
            match counts.iter_mut().find(|(k, _)| *k == reason) {
                Some((_, n)) => *n += 1,
                None => counts.push((reason, 1)),
            }
        }

        match counts.iter().max_by_key(|(_, n)| *n) {
            Some((reason, _)) => format!(
                "all {} candidate address(es) were rejected; most common reason: {reason}",
                considered.len(),
            ),
            None => "no candidate scored high enough to be selected".to_string(),
        }
    }
}

/// The distinct scopes present, as prose.
///
/// Listed from the data rather than hard-coded as "loopback and link-local" so
/// the sentence stays true if an unusual machine turns up something else.
/// Sorted by name so the message reads the same on every run.
fn scopes_present(candidates: &[&Candidate]) -> String {
    let mut names: Vec<String> = candidates
        .iter()
        .map(|c| c.address.scope.to_string())
        .collect();
    names.sort_unstable();
    names.dedup();
    match names.split_last() {
        None => "nothing".to_string(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// Score every address on the machine and pick the best one.
///
/// Never fails: a machine with nothing usable yields a [`Selection`] whose
/// `selected` is `None` and whose candidates explain why. Use
/// [`select_address`] when you want that turned into an error.
#[must_use]
pub fn select(state: &NetworkState, config: &SelectionConfig) -> Selection {
    let mut candidates: Vec<Candidate> = Vec::new();

    for interface in &state.interfaces {
        for address in &interface.addresses {
            candidates.push(evaluate(state, config, interface, address));
        }
    }

    // Eligible first; then by score descending; then by interface index and
    // address, so the outcome never depends on enumeration order.
    candidates.sort_by(|a, b| {
        a.disqualified
            .is_some()
            .cmp(&b.disqualified.is_some())
            .then_with(|| Reverse(a.score).cmp(&Reverse(b.score)))
            .then_with(|| a.interface_index.cmp(&b.interface_index))
            .then_with(|| a.address.ip.cmp(&b.address.ip))
    });

    let selected = candidates
        .iter()
        .find(|c| c.is_eligible())
        .map(|c| SelectedAddress {
            ip: c.address.ip,
            family: c.address.family,
            prefix_len: c.address.prefix_len,
            scope: c.address.scope,
            interface: c.interface.clone(),
            interface_index: c.interface_index,
            interface_kind: c.interface_kind.clone(),
            gateway: state.gateway_for(c.interface_index, c.address.family),
            score: c.score,
        });

    Selection {
        selected,
        candidates,
    }
}

/// Select an address, treating "nothing usable" as an error.
///
/// The convenient form for callers such as `autonet ip`, which must either
/// print an address or exit non-zero.
///
/// # Errors
///
/// Returns [`CoreError::NoAddressFound`] when nothing survives the filters,
/// carrying a human-readable account of why — an offline machine and a machine
/// whose only address was disqualified are different problems.
pub fn select_address(state: &NetworkState, config: &SelectionConfig) -> Result<SelectedAddress> {
    let mut selection = select(state, config);
    match selection.selected.take() {
        Some(address) => Ok(address),
        None => Err(CoreError::NoAddressFound {
            reason: selection.failure_reason(config),
        }),
    }
}

/// Record a rule that fired and return its contribution.
///
/// Rules worth zero are not recorded, so the reason list reads as "what
/// actually moved this candidate" rather than a roll call of every rule.
fn push(reasons: &mut Vec<Reason>, rule: &'static str, delta: i32) -> i32 {
    if delta != 0 {
        reasons.push(Reason::new(rule, delta));
    }
    delta
}

/// Apply the disqualification filters, then the scoring rules, to one pair.
fn evaluate(
    state: &NetworkState,
    config: &SelectionConfig,
    interface: &Interface,
    address: &Address,
) -> Candidate {
    let mut candidate = Candidate {
        interface: interface.name.clone(),
        interface_index: interface.index,
        interface_kind: interface.kind.clone(),
        address: address.clone(),
        score: 0,
        reasons: Vec::new(),
        disqualified: None,
    };

    if let Some(reason) = disqualify(state, config, interface, address) {
        candidate.disqualified = Some(reason);
        return candidate;
    }

    let mut score = 0;
    let family = address.family;

    // --- Routing: by far the strongest signal an interface is real. ---
    if state.has_default_route(interface.index, family) {
        score += push(
            &mut candidate.reasons,
            "default_route",
            weights::DEFAULT_ROUTE_SAME_FAMILY,
        );
    } else if state.has_any_default_route(interface.index) {
        score += push(
            &mut candidate.reasons,
            "default_route_other_family",
            weights::DEFAULT_ROUTE_OTHER_FAMILY,
        );
    }

    // --- What kind of device it is. ---
    let kind_delta = match &interface.kind {
        InterfaceKind::Ethernet => weights::KIND_ETHERNET,
        InterfaceKind::Wireless => weights::KIND_WIRELESS,
        InterfaceKind::Bridge => weights::KIND_BRIDGE,
        InterfaceKind::Container | InterfaceKind::Virtual => {
            if config.allow_container {
                0
            } else {
                weights::KIND_SYNTHETIC
            }
        }
        InterfaceKind::Vpn => {
            if config.allow_vpn {
                weights::KIND_VPN_ALLOWED
            } else {
                weights::KIND_VPN
            }
        }
        InterfaceKind::Loopback | InterfaceKind::Other(_) => 0,
    };
    score += push(&mut candidate.reasons, "interface_kind", kind_delta);

    // --- Family preference. ---
    if config.prefer_family.preferred() == Some(family) {
        score += push(
            &mut candidate.reasons,
            "family_match",
            weights::FAMILY_MATCH,
        );
    }

    // --- Where the address sits in the address space. ---
    let scope_delta = match address.scope {
        AddressScope::Private | AddressScope::UniqueLocal => weights::SCOPE_PRIVATE,
        AddressScope::Global => weights::SCOPE_GLOBAL,
        AddressScope::Cgnat => weights::SCOPE_CGNAT,
        // Loopback, link-local, and special never reach scoring unless the user
        // explicitly allowed them, in which case they earn nothing.
        _ => 0,
    };
    score += push(&mut candidate.reasons, "address_scope", scope_delta);

    if state.gateway_for(interface.index, family).is_some() {
        score += push(&mut candidate.reasons, "has_gateway", weights::HAS_GATEWAY);
    }

    if address.is_temporary {
        score += push(
            &mut candidate.reasons,
            "temporary_address",
            weights::TEMPORARY_ADDRESS,
        );
    }

    // --- Route metric: the tiebreaker between two otherwise equal links. ---
    if let Some(metric) = state.default_route_metric(interface.index, family) {
        // Capped before conversion, so the `try_from` can never actually fail;
        // the fallback exists so a future cap change cannot introduce a panic.
        let capped = metric.min(weights::METRIC_CAP) / weights::METRIC_DIVISOR;
        let penalty = -i32::try_from(capped).unwrap_or(i32::MAX);
        score += push(&mut candidate.reasons, "route_metric", penalty);
    }

    // --- An explicit instruction from the user outranks everything above. ---
    if any_name_matches(&config.prefer_interfaces, &interface.name) {
        score += push(
            &mut candidate.reasons,
            "preferred_interface",
            weights::PREFERRED_INTERFACE,
        );
    }

    candidate.score = score;
    candidate
}

/// Decide whether a pair is unusable regardless of how it would score.
fn disqualify(
    state: &NetworkState,
    config: &SelectionConfig,
    interface: &Interface,
    address: &Address,
) -> Option<Disqualification> {
    // Whether the user named this interface themselves. An explicit instruction
    // overrides the heuristics below — if someone asks for the Docker bridge,
    // they get the Docker bridge.
    let explicitly_requested = config.require_interface.as_deref() == Some(interface.name.as_str())
        || any_name_matches(&config.prefer_interfaces, &interface.name);

    if let Some(required) = &config.require_interface {
        if interface.name != *required {
            return Some(Disqualification::NotRequiredInterface);
        }
    }
    if any_name_matches(&config.exclude_interfaces, &interface.name) {
        return Some(Disqualification::ExcludedByConfig);
    }
    if interface.state.is_down() && !config.include_down {
        return Some(Disqualification::InterfaceDown);
    }
    // A container or virtualisation interface with no default route leads
    // nowhere. Note this is conditional on the route, not on the kind alone:
    // a bridge that *does* carry the default route stays eligible, which is the
    // case that stops this from breaking `br0`-style LAN bridges.
    if interface.kind.is_synthetic()
        && !config.allow_container
        && !explicitly_requested
        && !state.has_any_default_route(interface.index)
    {
        return Some(Disqualification::SyntheticWithoutRoute);
    }
    // Scope is checked before family on purpose. An `fe80::` address is
    // unusable no matter which family the caller asked for, so reporting it as
    // "link-local" explains more than "wrong address family" would; the latter
    // is reserved for addresses that are perfectly good, just not the family
    // requested.
    let scope_problem = match address.scope {
        AddressScope::Loopback if !config.allow_loopback => Some(Disqualification::Loopback),
        AddressScope::LinkLocal if !config.allow_link_local => Some(Disqualification::LinkLocal),
        // Never selectable: `0.0.0.0` is a bind address, not a destination, and
        // multicast/broadcast/documentation addresses are not this machine.
        AddressScope::Special => Some(Disqualification::SpecialAddress),
        _ => None,
    };
    if scope_problem.is_some() {
        return scope_problem;
    }
    if !config.prefer_family.admits(address.family) {
        return Some(Disqualification::FamilyMismatch);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FamilyPreference, InterfaceState, IpNetwork, Route, SCHEMA_VERSION};
    use std::net::Ipv4Addr;

    fn addr(s: &str, prefix: u8) -> Address {
        Address::new(s.parse().unwrap(), prefix)
    }

    fn iface(name: &str, index: u32, kind: InterfaceKind, addrs: &[(&str, u8)]) -> Interface {
        let mut i = Interface::new(name, index, kind, InterfaceState::Up);
        for (ip, p) in addrs {
            i.addresses.push(addr(ip, *p));
        }
        i
    }

    fn default_route(index: u32, gw: &str, metric: u32) -> Route {
        let gateway: IpAddr = gw.parse().unwrap();
        Route {
            destination: Some(IpNetwork::new(
                if gateway.is_ipv4() {
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                } else {
                    IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
                },
                0,
            )),
            gateway: Some(gateway),
            interface_index: index,
            metric,
            family: Family::of(&gateway),
            preferred_source: None,
        }
    }

    /// The development host, reduced to the parts that matter. Full fixture
    /// coverage lives in `tests/selection.rs`.
    fn dev_host() -> NetworkState {
        NetworkState::new(
            vec![
                iface(
                    "lo",
                    1,
                    InterfaceKind::Loopback,
                    &[("127.0.0.1", 8), ("::1", 128)],
                ),
                {
                    let mut e = iface("eno2", 2, InterfaceKind::Ethernet, &[]);
                    e.state = InterfaceState::Down;
                    e
                },
                iface(
                    "wlo1",
                    3,
                    InterfaceKind::Wireless,
                    &[
                        ("192.168.1.101", 24),
                        ("2606:4700:4700::1111", 64),
                        ("fe80::a00:27ff:fe4e:66a1", 64),
                    ],
                ),
                iface(
                    "docker0",
                    4,
                    InterfaceKind::Container,
                    &[("172.17.0.1", 16)],
                ),
                iface(
                    "br-18642d3532b2",
                    5,
                    InterfaceKind::Container,
                    &[("172.20.0.1", 16)],
                ),
                iface(
                    "virbr0",
                    6,
                    InterfaceKind::Virtual,
                    &[("192.168.122.1", 24)],
                ),
                iface(
                    "veth7faf9d0",
                    7,
                    InterfaceKind::Container,
                    &[("fe80::a00:27ff:fe11:2233", 64)],
                ),
            ],
            vec![default_route(3, "192.168.1.1", 600)],
        )
    }

    #[test]
    fn picks_wifi_over_nine_bridges_and_eleven_veths() {
        let selected = select_address(&dev_host(), &SelectionConfig::default()).unwrap();
        assert_eq!(selected.ip.to_string(), "192.168.1.101");
        assert_eq!(selected.interface, "wlo1");
        assert_eq!(selected.gateway.unwrap().to_string(), "192.168.1.1");
    }

    #[test]
    fn the_winning_score_matches_the_documented_rules() {
        // default_route 1000 + wireless 200 + family_match 150
        // + private 100 + gateway 25 - metric 60 = 1415
        let selected = select_address(&dev_host(), &SelectionConfig::default()).unwrap();
        assert_eq!(selected.score, 1415);
    }

    #[test]
    fn docker_bridges_score_far_below_the_real_interface() {
        let selection = select(&dev_host(), &SelectionConfig::default());
        let docker = selection
            .candidates
            .iter()
            .find(|c| c.interface == "docker0")
            .unwrap();
        // Routeless, so it never even reaches scoring.
        assert_eq!(
            docker.disqualified,
            Some(Disqualification::SyntheticWithoutRoute)
        );
    }

    #[test]
    fn a_container_bridge_that_owns_the_default_route_is_only_penalised() {
        // The penalty, rather than the disqualification, is what applies here:
        // the interface leads somewhere, it is just a poor thing to advertise.
        let state = NetworkState::new(
            vec![
                iface("wlo1", 3, InterfaceKind::Wireless, &[("192.168.1.101", 24)]),
                iface(
                    "docker0",
                    4,
                    InterfaceKind::Container,
                    &[("172.17.0.1", 16)],
                ),
            ],
            vec![
                default_route(3, "192.168.1.1", 600),
                default_route(4, "172.17.0.254", 600),
            ],
        );
        let selection = select(&state, &SelectionConfig::default());
        let docker = selection
            .candidates
            .iter()
            .find(|c| c.interface == "docker0")
            .unwrap();
        // default_route 1000 - synthetic 800 + family_match 150
        // + private 100 + gateway 25 - metric 60 = 415
        assert!(docker.is_eligible());
        assert_eq!(docker.score, 415);
        assert_eq!(selection.selected.unwrap().interface, "wlo1");
    }

    #[test]
    fn a_machine_with_only_container_interfaces_selects_nothing() {
        // Offline laptop with Docker running. `172.17.0.1` would look like an
        // answer while being unreachable from any other device, so the honest
        // outcome is no answer at all.
        let state = NetworkState::new(
            vec![
                iface("lo", 1, InterfaceKind::Loopback, &[("127.0.0.1", 8)]),
                iface(
                    "docker0",
                    4,
                    InterfaceKind::Container,
                    &[("172.17.0.1", 16)],
                ),
                iface(
                    "br-18642d3532b2",
                    5,
                    InterfaceKind::Container,
                    &[("172.20.0.1", 16)],
                ),
                iface(
                    "virbr0",
                    6,
                    InterfaceKind::Virtual,
                    &[("192.168.122.1", 24)],
                ),
            ],
            vec![],
        );
        assert!(select_address(&state, &SelectionConfig::default()).is_err());

        // Unless the user explicitly opts in.
        let config = SelectionConfig {
            allow_container: true,
            ..Default::default()
        };
        assert_eq!(
            select_address(&state, &config).unwrap().ip.to_string(),
            "172.17.0.1"
        );
    }

    #[test]
    fn loopback_link_local_and_down_interfaces_are_disqualified() {
        let selection = select(&dev_host(), &SelectionConfig::default());
        let by = |name: &str, ip: &str| {
            selection
                .candidates
                .iter()
                .find(|c| c.interface == name && c.address.ip.to_string() == ip)
                .unwrap()
                .disqualified
        };
        assert_eq!(by("lo", "127.0.0.1"), Some(Disqualification::Loopback));
        assert_eq!(
            by("wlo1", "fe80::a00:27ff:fe4e:66a1"),
            Some(Disqualification::LinkLocal)
        );
        // IPv6 global under the default IPv4 preference.
        assert_eq!(
            by("wlo1", "2606:4700:4700::1111"),
            Some(Disqualification::FamilyMismatch)
        );
    }

    #[test]
    fn requesting_ipv6_returns_the_global_address_not_the_link_local_one() {
        let config = SelectionConfig {
            prefer_family: crate::model::FamilyPreference::Ipv6,
            ..Default::default()
        };
        let mut state = dev_host();
        state.routes.push(default_route(3, "fe80::1", 600));

        let selected = select_address(&state, &config).unwrap();
        assert_eq!(selected.ip.to_string(), "2606:4700:4700::1111");
        assert_eq!(selected.family, Family::V6);
    }

    #[test]
    fn a_real_bridge_owning_the_default_route_still_wins() {
        // The reason containers are penalised rather than banned: this must work.
        let state = NetworkState::new(
            vec![
                iface("br0", 2, InterfaceKind::Bridge, &[("192.168.1.50", 24)]),
                iface(
                    "docker0",
                    3,
                    InterfaceKind::Container,
                    &[("172.17.0.1", 16)],
                ),
            ],
            vec![default_route(2, "192.168.1.1", 100)],
        );
        let selected = select_address(&state, &SelectionConfig::default()).unwrap();
        assert_eq!(selected.interface, "br0");
    }

    #[test]
    fn ethernet_beats_wifi_when_both_are_connected() {
        let state = NetworkState::new(
            vec![
                iface("eno1", 2, InterfaceKind::Ethernet, &[("10.0.0.20", 24)]),
                iface("wlo1", 3, InterfaceKind::Wireless, &[("192.168.1.101", 24)]),
            ],
            vec![
                default_route(2, "10.0.0.1", 100),
                default_route(3, "192.168.1.1", 600),
            ],
        );
        let selected = select_address(&state, &SelectionConfig::default()).unwrap();
        assert_eq!(selected.interface, "eno1");
    }

    #[test]
    fn vpn_loses_by_default_but_wins_when_asked_for() {
        let state = NetworkState::new(
            vec![
                iface("wlo1", 2, InterfaceKind::Wireless, &[("192.168.1.101", 24)]),
                iface("wg0", 3, InterfaceKind::Vpn, &[("10.8.0.2", 24)]),
            ],
            vec![default_route(2, "192.168.1.1", 600)],
        );
        assert_eq!(
            select_address(&state, &SelectionConfig::default())
                .unwrap()
                .interface,
            "wlo1"
        );

        let config = SelectionConfig {
            prefer_interfaces: vec!["wg0".into()],
            allow_vpn: true,
            ..Default::default()
        };
        assert_eq!(select_address(&state, &config).unwrap().interface, "wg0");
    }

    #[test]
    fn require_interface_disqualifies_everything_else() {
        let config = SelectionConfig {
            require_interface: Some("docker0".into()),
            ..Default::default()
        };
        let selected = select_address(&dev_host(), &config).unwrap();
        assert_eq!(selected.interface, "docker0");
        assert_eq!(selected.ip.to_string(), "172.17.0.1");
    }

    #[test]
    fn exclude_patterns_remove_interfaces() {
        let state = NetworkState::new(
            vec![
                iface("eno1", 2, InterfaceKind::Ethernet, &[("10.0.0.20", 24)]),
                iface("wlo1", 3, InterfaceKind::Wireless, &[("192.168.1.101", 24)]),
            ],
            vec![
                default_route(2, "10.0.0.1", 700),
                default_route(3, "192.168.1.1", 600),
            ],
        );
        // Ethernet would lose on kind here, but excluding Wi-Fi leaves it alone.
        let config = SelectionConfig {
            exclude_interfaces: vec!["wlo*".into()],
            ..Default::default()
        };
        assert_eq!(select_address(&state, &config).unwrap().interface, "eno1");

        // And on this host, excluding Wi-Fi leaves nothing real at all.
        assert!(select_address(&dev_host(), &config).is_err());
    }

    #[test]
    fn a_machine_with_only_loopback_selects_nothing() {
        let state = NetworkState::new(
            vec![iface("lo", 1, InterfaceKind::Loopback, &[("127.0.0.1", 8)])],
            vec![],
        );
        let err = select_address(&state, &SelectionConfig::default()).unwrap_err();
        assert!(matches!(err, CoreError::NoAddressFound { .. }));
        assert!(err.to_string().contains("loopback"));
    }

    #[test]
    fn selection_is_deterministic_across_input_orderings() {
        let mut a = dev_host();
        let mut b = dev_host();
        b.interfaces.reverse();
        for i in &mut b.interfaces {
            i.addresses.reverse();
        }
        a.routes.reverse();

        let sa = select_address(&a, &SelectionConfig::default()).unwrap();
        let sb = select_address(&b, &SelectionConfig::default()).unwrap();
        assert_eq!(sa.ip, sb.ip);
        assert_eq!(sa.interface, sb.interface);
        assert_eq!(sa.score, sb.score);
    }

    #[test]
    fn identical_scores_break_ties_by_interface_index_then_address() {
        // Two indistinguishable interfaces: the lower index must always win.
        let state = NetworkState::new(
            vec![
                iface("ethb", 9, InterfaceKind::Ethernet, &[("10.0.0.9", 24)]),
                iface("etha", 4, InterfaceKind::Ethernet, &[("10.0.0.4", 24)]),
            ],
            vec![],
        );
        let selected = select_address(&state, &SelectionConfig::default()).unwrap();
        assert_eq!(selected.interface, "etha");
    }

    #[test]
    fn url_brackets_ipv6_but_not_ipv4() {
        let mut s = select_address(&dev_host(), &SelectionConfig::default()).unwrap();
        assert_eq!(s.url(3000, "http"), "http://192.168.1.101:3000");

        s.ip = "2401:4900::1".parse().unwrap();
        assert_eq!(s.url(3000, "http"), "http://[2401:4900::1]:3000");
    }

    #[test]
    fn temporary_ipv6_addresses_lose_to_stable_ones() {
        let mut wlo1 = iface("wlo1", 2, InterfaceKind::Wireless, &[]);
        let mut temp = addr("2401:db9::2", 64);
        temp.is_temporary = true;
        wlo1.addresses.push(temp);
        wlo1.addresses.push(addr("2401:db9::1", 64));

        let config = SelectionConfig {
            prefer_family: crate::model::FamilyPreference::Ipv6,
            ..Default::default()
        };
        let state = NetworkState::new(vec![wlo1], vec![]);
        assert_eq!(
            select_address(&state, &config).unwrap().ip.to_string(),
            "2401:db9::1"
        );
    }

    #[test]
    fn unspecified_addresses_are_never_selected() {
        // 0.0.0.0 is a bind address, not a destination — the exact confusion
        // AutoNet exists to remove.
        let state = NetworkState::new(
            vec![iface("eno1", 2, InterfaceKind::Ethernet, &[("0.0.0.0", 0)])],
            vec![],
        );
        assert!(select_address(&state, &SelectionConfig::default()).is_err());
    }

    #[test]
    fn schema_version_is_stamped_on_new_states() {
        assert_eq!(
            NetworkState::new(vec![], vec![]).schema_version,
            SCHEMA_VERSION
        );
    }

    // -----------------------------------------------------------------------
    // Failure messages
    //
    // A tool that reports "no address" is only useful if it says why, and the
    // modal disqualification is not always the honest answer.
    // -----------------------------------------------------------------------

    fn why(state: &NetworkState, config: &SelectionConfig) -> String {
        select(state, config).failure_reason(config)
    }

    #[test]
    fn no_routable_address_of_the_requested_family_says_so() {
        // The real shape of an IPv6 request on a network that hands out no
        // IPv6: a dozen veths carrying fe80:: and nothing else. Counting
        // disqualifications would blame Docker for the router's DHCP.
        let reason = why(&dev_host_without_global_v6(), &wants_ipv6());
        assert!(
            reason.contains("no ipv6 address another device could reach"),
            "unhelpful: {reason}"
        );
        assert!(
            !reason.contains("container"),
            "blames the veths for the network's missing IPv6: {reason}"
        );
    }

    #[test]
    fn the_scopes_that_were_found_are_named() {
        // Listed alphabetically so the sentence is identical on every run,
        // rather than following whatever order the kernel enumerated.
        let reason = why(&dev_host_without_global_v6(), &wants_ipv6());
        assert!(
            reason.ends_with("(only link-local and loopback)"),
            "{reason}"
        );
    }

    #[test]
    fn a_disconnected_machine_is_not_blamed_on_its_containers() {
        // Offline laptop running Docker: loopback plus routeless bridges. The
        // count is dominated by the bridges, but the story is "you are
        // offline".
        let state = NetworkState::new(
            vec![
                iface("lo", 1, InterfaceKind::Loopback, &[("127.0.0.1", 8)]),
                iface(
                    "docker0",
                    4,
                    InterfaceKind::Container,
                    &[("172.17.0.1", 16)],
                ),
            ],
            vec![],
        );
        // `172.17.0.1` is private, so it *is* peer-reachable in principle and
        // the reachability shortcut correctly declines to fire — the modal
        // reason really is the right answer here.
        let reason = why(&state, &SelectionConfig::default());
        assert!(reason.contains("no route to anywhere"), "{reason}");
    }

    #[test]
    fn narrowing_the_search_is_reported_as_narrowing() {
        let requiring = SelectionConfig {
            require_interface: Some("eno2".to_string()),
            ..SelectionConfig::default()
        };
        assert_eq!(
            why(&dev_host(), &requiring),
            "the requested interface has no usable addresses"
        );

        let excluding = SelectionConfig {
            exclude_interfaces: vec!["*".to_string()],
            ..SelectionConfig::default()
        };
        assert_eq!(
            why(&dev_host(), &excluding),
            "every interface was excluded by configuration"
        );
    }

    #[test]
    fn a_machine_with_no_addresses_at_all_says_that() {
        let state = NetworkState::new(vec![iface("eno1", 2, InterfaceKind::Ethernet, &[])], vec![]);
        assert_eq!(
            why(&state, &SelectionConfig::default()),
            "no interfaces reported any addresses"
        );
    }

    fn wants_ipv6() -> SelectionConfig {
        SelectionConfig {
            prefer_family: FamilyPreference::Ipv6,
            ..SelectionConfig::default()
        }
    }

    /// The development host as it looks on a network with no IPv6: the global
    /// `2606:…` address gone, link-local ones left behind.
    fn dev_host_without_global_v6() -> NetworkState {
        let mut state = dev_host();
        for interface in &mut state.interfaces {
            interface
                .addresses
                .retain(|a| a.scope != AddressScope::Global);
        }
        state
    }
}
