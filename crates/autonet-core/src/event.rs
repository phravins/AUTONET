//! The vocabulary of network-state changes.
//!
//! Milestone 1 does not watch anything — there is no runtime here, no thread,
//! no netlink subscription. What this module provides is the *shape* of a
//! change, plus [`diff`], which derives one by comparing two snapshots.
//!
//! Defining this now is deliberate. `autonet watch` and the daemon both need to
//! answer "what changed, and does the selected address need recomputing?", and
//! settling that vocabulary while the model is still small keeps a later
//! milestone from reaching back in and reshaping the core.

use serde::Serialize;

use crate::model::{Address, Family, NetworkState};

/// A single observed change between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NetworkEvent {
    /// An interface appeared.
    InterfaceAdded {
        /// Name of the interface.
        interface: String,
    },
    /// An interface disappeared.
    InterfaceRemoved {
        /// Name of the interface.
        interface: String,
    },
    /// An interface changed operational state — the Wi-Fi/Ethernet handover case.
    InterfaceStateChanged {
        /// Name of the interface.
        interface: String,
        /// Its state before.
        from: String,
        /// Its state now.
        to: String,
    },
    /// An address was bound to an interface.
    AddressAdded {
        /// Name of the interface.
        interface: String,
        /// The new address.
        address: Address,
    },
    /// An address was unbound.
    AddressRemoved {
        /// Name of the interface.
        interface: String,
        /// The address that went away.
        address: Address,
    },
    /// The default route for a family moved to a different interface, or
    /// appeared, or vanished. The single most important trigger for
    /// recomputing the selected address.
    DefaultRouteChanged {
        /// Which family's default route moved.
        family: Family,
        /// Interface that previously carried it, if any.
        from_interface: Option<String>,
        /// Interface that carries it now, if any.
        to_interface: Option<String>,
    },
}

/// The complete set of changes between two snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NetworkDiff {
    /// Every change observed, in a stable order.
    pub events: Vec<NetworkEvent>,
}

impl NetworkDiff {
    /// Whether anything changed at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Whether the change is significant enough to warrant re-running the
    /// selection engine.
    ///
    /// Address and default-route changes always are. A bare interface
    /// appearing with no addresses is not.
    #[must_use]
    pub fn affects_selection(&self) -> bool {
        self.events.iter().any(|e| {
            matches!(
                e,
                NetworkEvent::AddressAdded { .. }
                    | NetworkEvent::AddressRemoved { .. }
                    | NetworkEvent::DefaultRouteChanged { .. }
                    | NetworkEvent::InterfaceStateChanged { .. }
                    | NetworkEvent::InterfaceRemoved { .. }
            )
        })
    }
}

/// Compare two snapshots and describe what changed.
///
/// Interfaces are matched by name rather than index, because a device that is
/// removed and re-added — a USB Ethernet adapter, a VPN reconnecting — comes
/// back with a fresh index but the same name, and reporting that as
/// remove-plus-add would be technically true and practically useless.
#[must_use]
pub fn diff(previous: &NetworkState, current: &NetworkState) -> NetworkDiff {
    let mut events = Vec::new();

    for old in &previous.interfaces {
        match current.interface_by_name(&old.name) {
            None => events.push(NetworkEvent::InterfaceRemoved {
                interface: old.name.clone(),
            }),
            Some(new) => {
                if old.state != new.state {
                    events.push(NetworkEvent::InterfaceStateChanged {
                        interface: old.name.clone(),
                        from: old.state.to_string(),
                        to: new.state.to_string(),
                    });
                }
                for a in &old.addresses {
                    if !new.addresses.contains(a) {
                        events.push(NetworkEvent::AddressRemoved {
                            interface: old.name.clone(),
                            address: a.clone(),
                        });
                    }
                }
                for a in &new.addresses {
                    if !old.addresses.contains(a) {
                        events.push(NetworkEvent::AddressAdded {
                            interface: new.name.clone(),
                            address: a.clone(),
                        });
                    }
                }
            }
        }
    }

    for new in &current.interfaces {
        if previous.interface_by_name(&new.name).is_none() {
            events.push(NetworkEvent::InterfaceAdded {
                interface: new.name.clone(),
            });
            for a in &new.addresses {
                events.push(NetworkEvent::AddressAdded {
                    interface: new.name.clone(),
                    address: a.clone(),
                });
            }
        }
    }

    for family in [Family::V4, Family::V6] {
        let before = default_route_interface(previous, family);
        let after = default_route_interface(current, family);
        if before != after {
            events.push(NetworkEvent::DefaultRouteChanged {
                family,
                from_interface: before,
                to_interface: after,
            });
        }
    }

    NetworkDiff { events }
}

/// Name of the interface carrying the best default route for `family`.
fn default_route_interface(state: &NetworkState, family: Family) -> Option<String> {
    state
        .default_route_for(family)
        .and_then(|r| state.interface_by_index(r.interface_index))
        .map(|i| i.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Interface, InterfaceKind, InterfaceState, IpNetwork, Route};
    use std::net::{IpAddr, Ipv4Addr};

    fn wifi(ip: &str) -> Interface {
        Interface::new("wlo1", 3, InterfaceKind::Wireless, InterfaceState::Up)
            .with_address(Address::new(ip.parse().unwrap(), 24))
    }

    fn route(index: u32, gw: &str, metric: u32) -> Route {
        Route {
            destination: Some(IpNetwork::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)),
            gateway: Some(gw.parse().unwrap()),
            interface_index: index,
            metric,
            family: Family::V4,
            preferred_source: None,
        }
    }

    #[test]
    fn identical_snapshots_produce_no_events() {
        let s = NetworkState::new(vec![wifi("192.168.1.101")], vec![route(3, "192.168.1.1", 600)]);
        assert!(diff(&s, &s).is_empty());
    }

    #[test]
    fn a_new_ip_on_the_same_interface_is_a_swap() {
        // The core scenario: same Wi-Fi card, different network, new lease.
        let before = NetworkState::new(vec![wifi("192.168.1.101")], vec![]);
        let after = NetworkState::new(vec![wifi("10.0.0.55")], vec![]);
        let d = diff(&before, &after);

        assert_eq!(d.events.len(), 2);
        assert!(matches!(d.events[0], NetworkEvent::AddressRemoved { .. }));
        assert!(matches!(d.events[1], NetworkEvent::AddressAdded { .. }));
        assert!(d.affects_selection());
    }

    #[test]
    fn moving_the_default_route_between_interfaces_is_reported() {
        // Docking a laptop: Wi-Fi hands over to Ethernet.
        let eth = Interface::new("eno1", 2, InterfaceKind::Ethernet, InterfaceState::Up)
            .with_address(Address::new("10.0.0.20".parse().unwrap(), 24));

        let before = NetworkState::new(vec![wifi("192.168.1.101")], vec![route(3, "192.168.1.1", 600)]);
        let after = NetworkState::new(
            vec![eth, wifi("192.168.1.101")],
            vec![route(2, "10.0.0.1", 100), route(3, "192.168.1.1", 600)],
        );

        let d = diff(&before, &after);
        let moved = d
            .events
            .iter()
            .find(|e| matches!(e, NetworkEvent::DefaultRouteChanged { .. }))
            .expect("default route change should be reported");
        assert_eq!(
            *moved,
            NetworkEvent::DefaultRouteChanged {
                family: Family::V4,
                from_interface: Some("wlo1".into()),
                to_interface: Some("eno1".into()),
            }
        );
        assert!(d.affects_selection());
    }

    #[test]
    fn disconnecting_reports_the_route_going_away() {
        let before = NetworkState::new(vec![wifi("192.168.1.101")], vec![route(3, "192.168.1.1", 600)]);
        let after = NetworkState::new(vec![], vec![]);
        let d = diff(&before, &after);

        assert!(d
            .events
            .iter()
            .any(|e| matches!(e, NetworkEvent::InterfaceRemoved { .. })));
        assert!(d.events.iter().any(|e| matches!(
            e,
            NetworkEvent::DefaultRouteChanged { to_interface: None, .. }
        )));
    }

    #[test]
    fn an_interface_reappearing_with_a_new_index_is_not_a_remove_and_add() {
        let mut renumbered = wifi("192.168.1.101");
        renumbered.index = 42;
        let before = NetworkState::new(vec![wifi("192.168.1.101")], vec![]);
        let after = NetworkState::new(vec![renumbered], vec![]);
        assert!(diff(&before, &after).is_empty());
    }

    #[test]
    fn a_bare_interface_appearing_does_not_affect_selection() {
        let empty = Interface::new("veth0", 9, InterfaceKind::Container, InterfaceState::Up);
        let before = NetworkState::new(vec![], vec![]);
        let after = NetworkState::new(vec![empty], vec![]);
        let d = diff(&before, &after);
        assert!(!d.is_empty());
        assert!(!d.affects_selection());
    }
}
