//! Network model, classification, configuration, and address selection.

#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::must_use_candidate)]
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
