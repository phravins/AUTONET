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

/// The M1 command set plus `run` and `doctor`. `watch` arrives in M4.
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

    /// Run a command with the LAN address in its environment.
    ///
    /// The long help is where the launch-time-snapshot semantics are stated to
    /// the user. It is required reading rather than a nicety: a variable that
    /// silently stops being true is worse than one that was never set, so the
    /// contract is spelled out where `autonet run --help` will show it.
    #[command(long_about = "\
Run a command with AUTONET_IP, AUTONET_HOST and AUTONET_URL in its environment.

    autonet run --port 3000 -- npm run dev

The command is executed directly, as an argument vector. It is never handed to \
a shell, so quoting, globs, pipes and semicolons are passed through to the \
program untouched rather than interpreted.

AUTONET_IP    the selected address, bare
AUTONET_HOST  the same address ready for a URL, with IPv6 bracketed
AUTONET_URL   http://HOST:PORT, set only when a port is known -- from --port, or from output.default_port in the config file

With a port, AutoNet checks whether it is already taken before starting the command, and says so up front rather than leaving the program to fail its own bind seconds later. Where the system will say, the holding process is named; on macOS AutoNet can tell that a port is busy but not what is holding it, and says which of the two it is telling you.

That check WARNS AND STARTS THE COMMAND ANYWAY. --port says what URL to print, not what the command will bind, so a busy port is a good guess about a problem rather than a fact about one -- and a port given after -- belongs to the command, is never parsed by AutoNet, and is never probed.

THESE ARE A SNAPSHOT TAKEN WHEN THE COMMAND STARTS. They are read once, before \
the program is launched, and they are never updated afterwards. If the network \
changes while the program is running -- Wi-Fi to Ethernet, a VPN coming up, a \
cable being pulled -- the values inside it go stale, and AutoNet will not \
restart the program or rewrite them.

That is a deliberate decision, not an oversight; see \
docs/adr/0001-network-change-during-autonet-run.md. A program that binds \
0.0.0.0 or :: is unaffected, because it answers on whatever address the \
interface currently holds. `autonet doctor` explains the case that is affected.

autonet run exits with the exit code of the command it ran.")]
    Run {
        /// The command to run, then its arguments.
        ///
        /// Put `--` before it so that flags belong to the command rather than
        /// to AutoNet: `autonet run -- vite --port 3000`.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            required = true,
            value_name = "COMMAND"
        )]
        command: Vec<String>,
    },

    /// Check whether this machine can be reached, and say what is wrong.
    ///
    /// The long help exists to set expectations about the last row, which is
    /// the only one in the tool that reports something AutoNet did not check.
    #[command(long_about = "\
Run a checklist over this machine's networking and say, in plain language, \
which layer is broken.

    autonet doctor
    autonet doctor --port 3000

Each row is one of four verdicts:

  [ ok ]  checked, and fine
  [warn]  checked, worth knowing about, not broken
  [fail]  checked, and broken
  [ ?  ]  NOT CHECKED -- AutoNet did not determine this

The last one is deliberate. A row AutoNet could not verify is not a pass, and \
saying so is more use than a tick that means nothing.

The bind-address row is always [ ? ]. AutoNet cannot see what address another \
program passes to bind(), so that row explains the distinction and leaves the \
answer to you: a server bound to one specific address stops answering when the \
network changes, and a server bound to the wildcard (0.0.0.0 or ::) follows \
it. It is advice, not a measurement, and it is not dressed up as one. See \
docs/adr/0001-network-change-during-autonet-run.md.

With --port, doctor also reports whether that port is already taken -- the same \
probe autonet run makes, and the same caveat: --port says what URL to print, \
not what any program will bind.

Exits 0 when nothing failed, warnings included, and 1 when something did.")]
    Doctor,
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
    ///
    /// Global on purpose. The port is an input to *URL rendering*, and URL
    /// rendering is not one command's concern: `status` prints local and
    /// network URLs, `ip` prints one, and `run` puts one in `AUTONET_URL`.
    /// `autonet status --port 3000` is the tool's headline example, so scoping
    /// the flag to `run` would break its front door. `interfaces` and `routes`
    /// accept it and have no URL to render, so they ignore it.
    ///
    /// It is a hint about what to *print*, never a statement about what a
    /// command will *bind* — which is why `autonet run` warns about a busy
    /// port rather than refusing to start.
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
    /// The port to render URLs for: the flag, else the config file.
    ///
    /// Kept out of [`Self::config`] because it is not a selection input — it
    /// changes nothing about which address is chosen, only how it is printed.
    ///
    /// **Zero means "none" in both layers.** `output.default_port = 0` is what
    /// the documented example config ships, and `http://192.168.1.20:0` is not
    /// a URL anyone can open; treating it as a real port would turn a
    /// placeholder into `AUTONET_URL`.
    ///
    /// The method and the field share the name deliberately: `self.port` is
    /// the flag as typed and `args.port(&config)` is the value to use, and it
    /// is the call sites that should read well.
    pub fn port(&self, config: &Config) -> Option<u16> {
        self.port
            .or(config.output.default_port)
            .filter(|port| *port != 0)
    }

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
