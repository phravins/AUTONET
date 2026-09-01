//! # AutoNet core
//!
//! The network state model, address classification, and selection engine that
//! every other part of AutoNet is built on.
//!
//! ## Why this crate exists
//!
//! Applications hardcode a machine's IP address, and that address changes every
//! time the developer moves between Wi-Fi, Ethernet, a hotspot, or a VPN.
//! AutoNet's answer is to implement the awkward part — deciding *which* of a
//! machine's many addresses is the one another device can actually reach —
//! exactly once, here, and let the CLI, the daemon, and every language SDK
//! consume it rather than each reinventing it slightly differently.
//!
//! ## The distinction that shapes everything
//!
//! A **bind address** and a **reachable address** are not the same thing. A
//! server can listen on `0.0.0.0:3000` quite happily, but a phone on the same
//! network cannot open `http://0.0.0.0:3000`. It needs
//! `http://192.168.1.101:3000`. AutoNet finds the second kind.
//!
//! ## Design rule: no OS calls
//!
//! This crate never asks the operating system anything about the network. It
//! operates on a [`NetworkState`] value, which a platform backend builds from
//! the kernel — or which a test deserializes from a JSON fixture. That is what
//! makes the selection engine deterministically testable: switching Wi-Fi
//! networks mid-run cannot make the test suite flake.
//!
//! ## Example
//!
//! ```
//! use autonet_core::{config::SelectionConfig, select::select_address};
//! use autonet_core::model::{Address, Interface, InterfaceKind, InterfaceState, NetworkState};
//!
//! let state = NetworkState::new(
//!     vec![
//!         Interface::new("lo", 1, InterfaceKind::Loopback, InterfaceState::Up)
//!             .with_address(Address::new("127.0.0.1".parse().unwrap(), 8)),
//!         Interface::new("docker0", 4, InterfaceKind::Container, InterfaceState::Up)
//!             .with_address(Address::new("172.17.0.1".parse().unwrap(), 16)),
//!         Interface::new("wlo1", 3, InterfaceKind::Wireless, InterfaceState::Up)
//!             .with_address(Address::new("192.168.1.101".parse().unwrap(), 24)),
//!     ],
//!     vec![],
//! );
//!
//! let selected = select_address(&state, &SelectionConfig::default()).unwrap();
//! assert_eq!(selected.ip.to_string(), "192.168.1.101");
//! assert_eq!(selected.url(3000, "http"), "http://192.168.1.101:3000");
//! ```

#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::must_use_candidate)]
// "AutoNet", "WireGuard" and "Docker" are product names, not code. Wrapping
// them in backticks would render prose as inline code throughout the docs.
#![allow(clippy::doc_markdown)]

pub mod classify;
pub mod config;
pub mod error;
pub mod event;
pub mod model;
pub mod select;

pub use config::{Config, OutputFormat, SelectionConfig};
pub use error::{CoreError, Result};
pub use event::{diff, NetworkDiff, NetworkEvent};
pub use model::{
    Address, AddressScope, Family, FamilyPreference, Interface, InterfaceFlags, InterfaceKind,
    InterfaceState, IpNetwork, NetworkState, Route, SCHEMA_VERSION,
};
pub use select::{select, select_address, Candidate, Disqualification, SelectedAddress, Selection};
