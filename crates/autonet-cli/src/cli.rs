//! Command-line surface and the configuration precedence chain.

use std::path::PathBuf;

use autonet_core::config::Config;
use autonet_core::model::FamilyPreference;
use clap::{Args, Parser, Subcommand};

/// Print the LAN address other devices can actually reach.
///
/// The long help doubles as the project's elevator pitch, because the most
/// common misunderstanding — that `0.0.0.0` or `127.0.0.1` is "the address" —
/// is the reason AutoNet exists.
#[derive(Debug, Parser)]
#[command(
    name = "autonet",
    version,
    about = "Find the IP address other devices on your network can reach",
    long_about = "\
AutoNet reports the address a phone, tablet or colleague on the same network \
can actually open — not the loopback address, and not the 0.0.0.0 wildcard \
your server binds to.

It ignores Docker bridges, veth pairs, virtual machine networks, link-local \
addresses and, by default, VPN tunnels, so that switching between Wi-Fi, \
Ethernet and a hotspot does not require editing any source code.

Every command accepts --json, which is the intended way to consume AutoNet \
from another language."
)]
pub struct Cli {
    /// What to do. Defaults to `status`.
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub global: GlobalArgs,
}

impl Cli {
    /// The command to run, applying the `status` default.
    pub fn command(&self) -> Command {
        self.command.clone().unwrap_or(Command::Status)
    }
}

/// The M1 command set. `run`, `watch` and `doctor` arrive in later milestones.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Show the selected address and how it was chosen.
    Status,

    /// Print only the selected IP address.
    ///
    /// Writes a bare address and nothing else to stdout, so it composes:
    /// `IP=$(autonet ip) || exit 1`.
    Ip,

    /// List network interfaces and their addresses.
    Interfaces,

    /// List routing table entries.
    Routes,
}

/// Flags accepted before or after any subcommand.
///
/// The booleans are independent switches; collapsing them into enums would
/// change what the user types.
#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct GlobalArgs {
    /// Emit machine-readable JSON.
    ///
    /// This is the interoperability contract: every payload carries a
    /// `schema_version`, and its shape will stay backward compatible.
    #[arg(long, global = true)]
    pub json: bool,

    /// Address family to prefer: ipv4, ipv6, or any.
    #[arg(short = 'f', long, global = true, value_name = "FAMILY")]
    pub family: Option<FamilyPreference>,

    /// Render URLs for this port, both local and LAN-reachable.
    #[arg(short = 'p', long, global = true, value_name = "PORT")]
    pub port: Option<u16>,

    /// Use only this interface, whatever else the machine has.
    #[arg(short = 'i', long, global = true, value_name = "NAME")]
    pub interface: Option<String>,

    /// Never select this interface. Repeatable; a trailing `*` matches a prefix.
    #[arg(short = 'x', long = "exclude", global = true, value_name = "NAME")]
    pub exclude: Vec<String>,

    /// Read configuration from this file instead of the default location.
    #[arg(short = 'c', long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Consider VPN tunnels.
    ///
    /// Stops penalising them; it does not promote them. Use `--interface wg0`
    /// to insist on a tunnel.
    #[arg(long, global = true)]
    pub allow_vpn: bool,

    /// Consider Docker bridges, veth pairs and virtual machine networks.
    #[arg(long, global = true)]
    pub allow_container: bool,

    /// Consider loopback addresses.
    #[arg(long, global = true)]
    pub allow_loopback: bool,

    /// Explain the decision: show every candidate, its score, and the rules
    /// that produced it.
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

impl GlobalArgs {
    /// Build the effective configuration.
    ///
    /// Precedence, lowest to highest: built-in defaults, the config file, the
    /// `AUTONET_*` environment variables, then these flags.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration file or an `AUTONET_*` variable
    /// cannot be parsed.
    pub fn config(&self) -> autonet_core::Result<Config> {
        let mut config = Config::load_layered(self.config.as_deref())?;

        if let Some(family) = self.family {
            config.selection.prefer_family = family;
        }
        if let Some(interface) = &self.interface {
            config.selection.require_interface = Some(interface.clone());
        }
        if !self.exclude.is_empty() {
            config
                .selection
                .exclude_interfaces
                .extend(self.exclude.iter().cloned());
        }
        // Opt-in switches: a bare flag turns the feature on, an absent flag
        // leaves the lower layers alone. There is deliberately no
        // `--no-allow-vpn`; only the user's own config file can turn these on.
        if self.allow_vpn {
            config.selection.allow_vpn = true;
        }
        if self.allow_container {
            config.selection.allow_container = true;
        }
        if self.allow_loopback {
            config.selection.allow_loopback = true;
        }

        Ok(config)
    }
}
