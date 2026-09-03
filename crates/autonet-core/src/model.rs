//! Serializable network-state data model.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Version of the JSON wire format.
pub const SCHEMA_VERSION: u32 = 1;

/// An IP address family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    /// IPv4.
    #[serde(rename = "ipv4")]
    V4,
    /// IPv6.
    #[serde(rename = "ipv6")]
    V6,
}

impl Family {
    /// The family an address belongs to.
    #[must_use]
    pub fn of(ip: &IpAddr) -> Self {
        match ip {
            IpAddr::V4(_) => Self::V4,
            IpAddr::V6(_) => Self::V6,
        }
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::V4 => "ipv4",
            Self::V6 => "ipv6",
        })
    }
}

/// Which family the caller would like, when both are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FamilyPreference {
    /// Prefer IPv4. The default: it is what most local development targets.
    #[default]
    #[serde(rename = "ipv4")]
    Ipv4,
    /// Prefer IPv6.
    #[serde(rename = "ipv6")]
    Ipv6,
    /// No preference; score both families on equal footing.
    Any,
}

impl FamilyPreference {
    /// The concrete family this preference favours, if it names one.
    #[must_use]
    pub fn preferred(self) -> Option<Family> {
        match self {
            Self::Ipv4 => Some(Family::V4),
            Self::Ipv6 => Some(Family::V6),
            Self::Any => None,
        }
    }

    /// Whether an address of `family` is admissible under this preference.
    ///
    /// A *preference* still excludes the other family outright: asking for IPv4
    /// and being handed an IPv6 address would break the caller's expectations
    /// far more than returning nothing does.
    #[must_use]
    pub fn admits(self, family: Family) -> bool {
        match self {
            Self::Any => true,
            Self::Ipv4 => family == Family::V4,
            Self::Ipv6 => family == Family::V6,
        }
    }
}

impl FromStr for FamilyPreference {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ipv4" | "v4" | "4" | "inet" => Ok(Self::Ipv4),
            "ipv6" | "v6" | "6" | "inet6" => Ok(Self::Ipv6),
            "any" | "both" => Ok(Self::Any),
            _ => Err(CoreError::Parse {
                kind: "FamilyPreference",
                value: s.to_string(),
            }),
        }
    }
}

/// Where an address sits in the IP address space.
///
/// This drives most of the selection engine: the difference between a LAN
/// address another device can reach and a link-local address it cannot is the
/// entire point of AutoNet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressScope {
    /// `127.0.0.0/8` or `::1` — reachable only from this machine.
    Loopback,
    /// `169.254.0.0/16` or `fe80::/10` — not routable, and useless in a URL.
    LinkLocal,
    /// RFC 1918 (`10/8`, `172.16/12`, `192.168/16`) — the normal LAN case.
    Private,
    /// RFC 6598 carrier-grade NAT (`100.64.0.0/10`).
    Cgnat,
    /// IPv6 unique local addresses (`fc00::/7`).
    UniqueLocal,
    /// Globally routable unicast.
    Global,
    /// Unspecified (`0.0.0.0`, `::`), multicast, broadcast, or reserved.
    Special,
}

impl AddressScope {
    /// Whether an address of this scope could plausibly be handed to another
    /// device on the LAN as a destination.
    #[must_use]
    pub fn is_reachable_by_peers(self) -> bool {
        matches!(
            self,
            Self::Private | Self::UniqueLocal | Self::Global | Self::Cgnat
        )
    }
}

impl fmt::Display for AddressScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Loopback => "loopback",
            Self::LinkLocal => "link-local",
            Self::Private => "private",
            Self::Cgnat => "cgnat",
            Self::UniqueLocal => "unique-local",
            Self::Global => "global",
            Self::Special => "special",
        })
    }
}

/// What sort of device an interface is.
///
/// Determined by the platform backend from the kernel's own link-type
/// information where possible, falling back to name heuristics. See
/// [`crate::classify::classify_interface`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceKind {
    /// A wired NIC.
    Ethernet,
    /// A Wi-Fi NIC.
    Wireless,
    /// The loopback device.
    Loopback,
    /// A user-created bridge (for example `br0` bridging a physical NIC).
    Bridge,
    /// A container runtime's bridge or virtual ethernet device: `docker0`,
    /// `br-<12 hex>`, `veth*`, `cni*`, `podman*`, `flannel*`.
    Container,
    /// A hypervisor or virtualisation device: `virbr*`, `vboxnet*`, `vmnet*`.
    Virtual,
    /// A tunnel or VPN device: `tun*`, `wg*`, `ppp*`, `tailscale*`, `zt*`.
    Vpn,
    /// Anything else, carrying the kernel's own link-type name.
    Other(String),
}

impl InterfaceKind {
    /// Whether this kind represents a container or virtualisation device that
    /// should not normally be offered to a developer as "your machine's address".
    #[must_use]
    pub fn is_synthetic(&self) -> bool {
        matches!(self, Self::Container | Self::Virtual)
    }
}

impl fmt::Display for InterfaceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ethernet => f.write_str("ethernet"),
            Self::Wireless => f.write_str("wireless"),
            Self::Loopback => f.write_str("loopback"),
            Self::Bridge => f.write_str("bridge"),
            Self::Container => f.write_str("container"),
            Self::Virtual => f.write_str("virtual"),
            Self::Vpn => f.write_str("vpn"),
            Self::Other(name) => f.write_str(name),
        }
    }
}

/// The operational state of a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceState {
    /// Administratively up and carrying a link.
    Up,
    /// Down, or up but with no carrier.
    Down,
    /// Up but not yet ready (for example, associating with an access point).
    Dormant,
    /// The driver does not report a meaningful state. Common for tunnel and
    /// loopback devices, which may still be perfectly usable.
    Unknown,
}

impl InterfaceState {
    /// Whether the interface is definitively unusable.
    ///
    /// Only [`InterfaceState::Down`] qualifies. `Unknown` is deliberately *not*
    /// treated as down: WireGuard and other tunnel devices report `Unknown`
    /// while being fully functional.
    #[must_use]
    pub fn is_down(self) -> bool {
        matches!(self, Self::Down)
    }
}

impl fmt::Display for InterfaceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Dormant => "dormant",
            Self::Unknown => "unknown",
        })
    }
}

/// Kernel link flags that matter to address selection.
///
/// A pile of booleans is the honest shape here: this mirrors a kernel bitfield
/// where each flag is independent, and collapsing them into an enum would
/// misrepresent combinations the kernel genuinely reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct InterfaceFlags {
    /// `IFF_UP` — administratively enabled.
    pub up: bool,
    /// `IFF_RUNNING` — the driver reports the link as operational.
    pub running: bool,
    /// `IFF_LOOPBACK`.
    pub loopback: bool,
    /// `IFF_BROADCAST`.
    pub broadcast: bool,
    /// `IFF_POINTOPOINT` — typical of tunnels and PPP links.
    pub point_to_point: bool,
    /// `IFF_MULTICAST`.
    pub multicast: bool,
}

/// A single IP address bound to an interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    /// The address itself.
    pub ip: IpAddr,
    /// Which family it belongs to.
    pub family: Family,
    /// Netmask length in bits (`24` for a `/24`).
    pub prefix_len: u8,
    /// Where the address sits in the address space.
    pub scope: AddressScope,
    /// Whether this is an IPv6 privacy-extension temporary address.
    #[serde(default)]
    pub is_temporary: bool,
}

impl Address {
    /// Build an address, deriving its family and scope from the IP itself.
    ///
    /// Prefer this over constructing the struct literally: it guarantees
    /// `family` and `scope` agree with `ip`.
    #[must_use]
    pub fn new(ip: IpAddr, prefix_len: u8) -> Self {
        Self {
            family: Family::of(&ip),
            scope: crate::classify::classify_address(&ip),
            ip,
            prefix_len,
            is_temporary: false,
        }
    }

    /// The address rendered as it would appear in a URL.
    ///
    /// IPv6 addresses are bracketed, so `[2401:db8::1]` rather than the
    /// ambiguous bare form that would collide with the port separator.
    #[must_use]
    pub fn url_host(&self) -> String {
        match self.ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.ip, self.prefix_len)
    }
}

/// A network interface and everything bound to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interface {
    /// The kernel's name for the device, for example `wlo1`.
    pub name: String,
    /// The kernel's interface index. Routes reference interfaces by this.
    pub index: u32,
    /// What sort of device this is.
    pub kind: InterfaceKind,
    /// Its operational state.
    pub state: InterfaceState,
    /// Kernel link flags.
    #[serde(default)]
    pub flags: InterfaceFlags,
    /// Hardware address, when the device has one.
    #[serde(default)]
    pub mac: Option<String>,
    /// Link MTU.
    #[serde(default)]
    pub mtu: Option<u32>,
    /// Every address bound to this interface.
    #[serde(default)]
    pub addresses: Vec<Address>,
}

impl Interface {
    /// A minimal interface, for tests and fixtures.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        index: u32,
        kind: InterfaceKind,
        state: InterfaceState,
    ) -> Self {
        Self {
            name: name.into(),
            index,
            kind,
            state,
            flags: InterfaceFlags::default(),
            mac: None,
            mtu: None,
            addresses: Vec::new(),
        }
    }

    /// Attach an address, builder-style.
    #[must_use]
    pub fn with_address(mut self, address: Address) -> Self {
        self.addresses.push(address);
        self
    }

    /// Addresses of a given family.
    pub fn addresses_of(&self, family: Family) -> impl Iterator<Item = &Address> {
        self.addresses.iter().filter(move |a| a.family == family)
    }
}

/// An IP network in CIDR form.
///
/// Serialized as a string (`"192.168.1.0/24"`) rather than a nested object,
/// because fixtures are meant to be read and edited by humans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct IpNetwork {
    /// The network address.
    pub addr: IpAddr,
    /// The prefix length in bits.
    pub prefix_len: u8,
}

impl IpNetwork {
    /// Construct a network from an address and prefix length.
    #[must_use]
    pub fn new(addr: IpAddr, prefix_len: u8) -> Self {
        Self { addr, prefix_len }
    }

    /// Whether this network covers the entire address space — that is, whether
    /// a route to it is a default route.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.prefix_len == 0
            && match self.addr {
                IpAddr::V4(v4) => v4 == Ipv4Addr::UNSPECIFIED,
                IpAddr::V6(v6) => v6 == Ipv6Addr::UNSPECIFIED,
            }
    }
}

impl fmt::Display for IpNetwork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix_len)
    }
}

impl FromStr for IpNetwork {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || CoreError::Parse {
            kind: "IpNetwork",
            value: s.to_string(),
        };
        let (addr, prefix) = s.split_once('/').ok_or_else(err)?;
        Ok(Self {
            addr: addr.parse().map_err(|_| err())?,
            prefix_len: prefix.parse().map_err(|_| err())?,
        })
    }
}

impl From<IpNetwork> for String {
    fn from(value: IpNetwork) -> Self {
        value.to_string()
    }
}

impl TryFrom<String> for IpNetwork {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

/// A routing table entry.
///
/// Routes are modelled explicitly because "does this interface own a default
/// route?" is the single strongest signal that an interface is actually usable
/// for talking to anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    /// The destination network. `None` means a default route (`0.0.0.0/0` or `::/0`).
    #[serde(default)]
    pub destination: Option<IpNetwork>,
    /// The next hop, when there is one.
    #[serde(default)]
    pub gateway: Option<IpAddr>,
    /// Index of the interface this route exits through.
    pub interface_index: u32,
    /// Route metric. Lower wins, so a wired link at metric 100 beats Wi-Fi at 600.
    #[serde(default)]
    pub metric: u32,
    /// Which family this route belongs to.
    pub family: Family,
    /// The preferred source address the kernel associates with this route.
    #[serde(default)]
    pub preferred_source: Option<IpAddr>,
}

impl Route {
    /// Whether this is a default route.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.destination.is_none_or(|d| d.is_default())
    }
}

/// A complete snapshot of the machine's network configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkState {
    /// Wire-format version. See [`SCHEMA_VERSION`].
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// When the snapshot was taken, as seconds since the Unix epoch.
    ///
    /// Optional so that fixtures — which must be byte-stable — can omit it.
    #[serde(default)]
    pub captured_at: Option<u64>,
    /// Every interface on the machine.
    pub interfaces: Vec<Interface>,
    /// Every route AutoNet cares about.
    #[serde(default)]
    pub routes: Vec<Route>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

impl NetworkState {
    /// An empty state, as a starting point for builders and fixtures.
    #[must_use]
    pub fn new(interfaces: Vec<Interface>, routes: Vec<Route>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            captured_at: None,
            interfaces,
            routes,
        }
    }

    /// Stamp the snapshot with the current wall-clock time.
    #[must_use]
    pub fn captured_now(mut self) -> Self {
        self.captured_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        self
    }

    /// Look up an interface by kernel index.
    #[must_use]
    pub fn interface_by_index(&self, index: u32) -> Option<&Interface> {
        self.interfaces.iter().find(|i| i.index == index)
    }

    /// Look up an interface by name.
    #[must_use]
    pub fn interface_by_name(&self, name: &str) -> Option<&Interface> {
        self.interfaces.iter().find(|i| i.name == name)
    }

    /// Every default route, for either family.
    pub fn default_routes(&self) -> impl Iterator<Item = &Route> {
        self.routes.iter().filter(|r| r.is_default())
    }

    /// The best default route for `family`: the one with the lowest metric.
    ///
    /// This is the closest thing the kernel offers to "which way does traffic
    /// actually leave this machine", and it anchors the selection engine.
    #[must_use]
    pub fn default_route_for(&self, family: Family) -> Option<&Route> {
        self.default_routes()
            .filter(|r| r.family == family)
            .min_by_key(|r| (r.metric, r.interface_index))
    }

    /// Whether `index` owns a default route for `family`.
    #[must_use]
    pub fn has_default_route(&self, index: u32, family: Family) -> bool {
        self.default_routes()
            .any(|r| r.interface_index == index && r.family == family)
    }

    /// Whether `index` owns a default route for *either* family.
    #[must_use]
    pub fn has_any_default_route(&self, index: u32) -> bool {
        self.default_routes().any(|r| r.interface_index == index)
    }

    /// The lowest metric among `index`'s default routes for `family`.
    #[must_use]
    pub fn default_route_metric(&self, index: u32, family: Family) -> Option<u32> {
        self.default_routes()
            .filter(|r| r.interface_index == index && r.family == family)
            .map(|r| r.metric)
            .min()
    }

    /// The gateway `index` uses for `family`, if any.
    #[must_use]
    pub fn gateway_for(&self, index: u32, family: Family) -> Option<IpAddr> {
        self.default_routes()
            .filter(|r| r.interface_index == index && r.family == family)
            .min_by_key(|r| r.metric)
            .and_then(|r| r.gateway)
    }
}
