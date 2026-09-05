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

/// The M1 command set plus `run`, `doctor` and `watch`.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Show the selected address and how it was chosen.
    ///
    /// The long help exists for `--qr`, whose two refusals are easier to read
    /// before running the command than after.
    #[command(long_about = "\
Show the selected address, its interface, gateway and scope. The default when \
no command is given.

With --port, it also prints the URL to open on this machine and the URL to \
open on another device. With --qr as well, the second of those is printed \
again as a QR code, so a phone camera can open it without anyone typing an \
address.

--qr encodes the selected IP address, not a .local name. A name only resolves \
while `autonet advertise` is running to publish it, and status does not \
advertise; a code that scans cleanly and then fails to load would be worse \
than the address it replaced.

--qr needs a port -- from --port or output.default_port -- and says so rather \
than encoding a URL with no port in it. It is refused with --json, because a \
QR code is a rendering and the string it encodes is already in that payload as \
urls.network.")]
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

    /// Print the selected address, then print it again whenever it changes.
    ///
    /// The long help states the read-only contract, because the obvious next
    /// question — "so will it restart my server?" — has a deliberate answer
    /// that the user should not have to discover by waiting for it not to
    /// happen.
    #[command(long_about = "\
Print the selected address now, and print it again every time it changes.
Runs until you stop it with Ctrl-C.

    autonet watch
    autonet watch --json

Each change is reported as what it was, what it is, and the single most \
explanatory reason it moved:

    Previous: wlo1 (wireless) / 192.168.1.20
    Current:  eno1 (ethernet) / 10.0.0.42
    Reason:   default ipv4 route moved from wlo1 to eno1

WATCH ONLY OBSERVES. It does not restart anything, does not signal any \
autonet run process, and does not rewrite any environment. That is a decision, \
not a missing feature: the environment of a running process cannot be changed \
from outside it, so the only way to \"update\" a server would be to kill it. \
See docs/adr/0001-network-change-during-autonet-run.md, and autonet advertise \
for the way an address change is meant to be absorbed -- at the name layer, \
where the client resolves it, rather than at the server.

With --json, one document per line, so it can be piped into a program that \
reads a stream. The `reason` field is prose for a human and is not part of the \
JSON contract; the `events` array is the machine-readable form of the same \
thing. See docs/json-schema.md.

Exits 0 when interrupted, because being interrupted is how it is meant to end.")]
    Watch,

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

    /// Publish a `.local` name pointing at the selected address.
    ///
    /// The long help is where the wire behaviour is stated, because this is the
    /// one command in the tool that transmits anything. A user should be able
    /// to read exactly what leaves this machine before running it, not after.
    #[command(long_about = "\
Publish a .local name for the selected address, and keep it pointed there.

    autonet advertise --port 3000

THIS TRANSMITS. It is the only command that does. AutoNet binds UDP 5353, \
joins the mDNS multicast groups, and announces four things to every device on \
the network segment: the name, the selected address, the port, and the service \
type. Nothing else -- no interface name, no MAC address, no score.

It opens no port for your application and changes no firewall rule. Whatever \
could reach this machine before can reach it after; the record supplies a NAME \
for an address the machine was already answering on. What is new is that the \
machine now names itself, unprompted, to the whole link -- including the \
hostname it derives that name from.

So it is off by default. Set enabled = true under [hostname] in your config \
file to turn it on; there is deliberately no environment variable and no flag \
that does it, because consent to publish belongs in a file you wrote.

The published name is <hostname>-autonet.local, not <hostname>.local. The \
plain name is already owned by Avahi on most Linux desktops and always by \
mDNSResponder on macOS, and those responders publish every address on every \
interface -- which is the problem AutoNet exists to fix, so it must not race \
them for the same name. Override it with `name` under [hostname].

The record follows the address. When the selected address moves -- Wi-Fi to \
Ethernet, a VPN coming up, a cable pulled -- the name is re-announced at the \
new address. That is the half of ADR 0001 that autonet run deliberately does \
not do: the environment inside a running program stays a launch-time snapshot, \
and the name is what stays true instead.

When no address is selectable the record is withdrawn rather than left \
pointing somewhere stale. On Ctrl-C it is withdrawn and the process exits 0.")]
    Advertise,
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

    /// Also print the network URL as a QR code a phone camera can open.
    ///
    /// Global for the reason `--port` is: a QR code is URL *rendering*, and
    /// `autonet --qr` has to work with `status` as the implicit default
    /// command. `interfaces` and `routes` ignore it exactly as they ignore
    /// `--port`.
    ///
    /// It encodes the selected address, never a `.local` name -- see
    /// `docs/adr/0003-qr-code-contents.md` and [`crate::url::network_url`].
    #[arg(long, global = true)]
    pub qr: bool,

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
