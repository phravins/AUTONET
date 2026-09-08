# AutoNet

**The IP address other devices on your network can actually reach.**

Before — the line you edit again every time the network changes:

```diff
- const API = "http://192.168.1.42:3000";   // office Wi-Fi, Monday
- const API = "http://192.168.0.115:3000";  // home, Tuesday
- const API = "http://172.20.10.4:3000";    // phone hotspot, on the train
+ const API = `http://${process.env.AUTONET_IP}:3000`;
```

After — the address, and the command that keeps it current:

```console
$ autonet ip
192.168.1.101

$ autonet run --port 3000 -- npm run dev    # AUTONET_IP set for the child

$ autonet status --port 3000
AutoNet linux-netlink

  Address    192.168.1.101/24
  Interface  wlo1 (wireless, up)
  Gateway    192.168.1.1
  Scope      private

  Local      http://127.0.0.1:3000
  Network    http://192.168.1.101:3000  ← open this from another device
```

---

## The problem

Your development machine's IP address changes constantly. Wi-Fi at the office,
Ethernet at the desk, a phone hotspot on the train, a VPN for the staging
environment — each one hands you a different address. So the address gets
hardcoded, and then it gets hardcoded again in the mobile app, the `.env` file,
the QR code, and the message you send a colleague.

The usual workarounds do not work:

- **`localhost` / `127.0.0.1`** is reachable only from the machine itself.
- **`0.0.0.0`** is a valid address to *bind* to and a useless address to *connect*
  to. It means "every interface"; it is not a destination.
- **"just take the first address"** returns `172.17.0.1` on any machine running
  Docker, and no phone on your Wi-Fi can reach a Docker bridge.

AutoNet answers a narrower and more useful question: **which of this machine's
addresses can another device on the same network open?**

## What it does

AutoNet enumerates every interface and route the kernel knows about, then scores
the candidates. It prefers interfaces that are up and own a default route,
prefers wired over wireless, prefers ordinary private LAN addresses, and ignores
loopback, link-local, Docker bridges, veth pairs, virtual-machine networks and —
unless you ask — VPN tunnels.

On a laptop with 23 interfaces — nine bridges and eleven veth pairs among them —
it returns the one address that works.

If nothing is reachable, it says so and exits non-zero. It does not invent a
plausible-looking answer.

## Install

One command on every platform, with no package manager to install first.

**Linux and macOS**

```sh
curl -fsSL https://raw.githubusercontent.com/phravins/AUTONET/main/scripts/install.sh | sh
```

**Windows**, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/phravins/AUTONET/main/scripts/install.ps1 | iex
```

Both do the same thing: work out your platform, download the matching release
archive, **verify it against the `SHA256SUMS` published with the release before
extracting anything**, install to a per-user directory, and add that directory
to your `PATH` if it is not already there. Neither uses `sudo` or asks for
administrator rights, and neither replaces an existing `autonet` without asking.
Three environment variables override the defaults:

| Variable | Effect |
|---|---|
| `AUTONET_VERSION` | Install this version instead of the latest release. |
| `AUTONET_INSTALL_DIR` | Install here instead of `/usr/local/bin`, `~/.local/bin` or `%LOCALAPPDATA%\Programs\autonet`. |
| `AUTONET_FORCE=1` | Replace an existing install without prompting. |

> **The macOS and Windows binaries are not code-signed or notarized.** On macOS
> that means a binary you download with a *browser* is quarantined by Gatekeeper
> and will not run until you clear it (`xattr -d com.apple.quarantine`); the
> installer above uses `curl`, which does not set that attribute, so it normally
> installs without one. On Windows it means SmartScreen may show an
> "unrecognised app" warning the first time you run `autonet.exe` — the binary
> carries no Authenticode signature, which is not the same as anything being
> wrong with it. Both installers check the published SHA256 before installing
> anything, and both print the exact command to clear the flag if it is actually
> set. Code-signing certificates are not in this repository and will not be.

### Other ways

Secondary to the one-liners above; useful if you already live in one of these.

```sh
cargo install autonet                       # from crates.io, builds from source
brew install --formula ./packaging/homebrew/autonet.rb
scoop install https://raw.githubusercontent.com/phravins/AUTONET/main/packaging/scoop/autonet.json
```

Or with Nix, for reproducible builds and the pinned development toolchain:

```sh
nix run github:phravins/AUTONET -- status
nix develop                                 # dev shell with the pinned toolchain
```

The resulting binary is an ordinary native executable with **no runtime
dependency on Nix**. To build from a checkout instead:

```sh
cargo install --path crates/autonet-cli     # the package is named `autonet`
```

## Commands

| Command | What it prints |
|---|---|
| `autonet status` | The selected address, its interface, gateway and scope. The default when no command is given. |
| `autonet ip` | The bare address and nothing else, for `$(...)` substitution — or, with `-p`, the URL to open. |
| `autonet interfaces` | Every interface, classified, with its addresses. |
| `autonet routes` | The routing table, default routes first. |
| `autonet run -- <cmd>` | Runs a command with `AUTONET_IP`, `AUTONET_HOST` and `AUTONET_URL` in its environment, and exits with the command's own exit code. The variables are a snapshot taken at launch; see [ADR 0001](docs/adr/0001-network-change-during-autonet-run.md). |
| `autonet doctor` | A checklist of what works and what does not, in plain language, with a summary line. |
| `autonet watch` | Prints the selected address, then prints it again each time it changes. Reads the kernel's own change notifications where the platform has them, and falls back to a timer where it does not. |
| `autonet advertise` | Publishes a `.local` name pointing at the selected address, and re-publishes it whenever the address moves. **This transmits** — it is off until `[hostname] enabled` says otherwise. See [ADR 0002](docs/adr/0002-mdns-advertisement.md). |

Common flags, accepted before or after any command:

| Flag | Effect |
|---|---|
| `--json` | Machine-readable output. See [the JSON contract](#the-json-contract). |
| `-f, --family <ipv4\|ipv6\|any>` | Which family to prefer. Default `ipv4`. |
| `-p, --port <PORT>` | Also render the URL to open on another device. Defaults to `output.default_port`. A hint about what to *print*, not about what a command will *bind* — `autonet run` warns if it is taken, and starts the command anyway. |
| `--qr` | Also render the URL as a QR code a phone camera can read — the address under `status`, the published name under `advertise`. Needs a port; refused with `--json`, where the payload is already `urls.network`. |
| `-i, --interface <NAME>` | Use only this interface. |
| `-x, --exclude <NAME>` | Never use this interface. Repeatable; a trailing `*` matches a prefix. |
| `--allow-vpn` | Stop penalising VPN tunnels. |
| `--allow-container` | Stop penalising Docker bridges and virtual networks. |
| `--allow-loopback` | Permit `127.0.0.1` / `::1`. |
| `-c, --config <PATH>` | Use this config file. |
| `-v, --verbose` | Show every candidate, its score, and the rules that produced it. |

### Checking a machine

```console
$ autonet doctor --port 3000
AutoNet doctor  linux-netlink

  [ ok ]  Operating system   linux, via linux-netlink
  [ ok ]  Network interface  wlo1 (wireless, up), and 3 more
  [ ok ]  IPv4 address       192.168.0.115/24 on wlo1
  [ ok ]  Default route      via 192.168.0.1 on wlo1
  [ ok ]  Selected address   192.168.0.115 (private) on wlo1, which is not
                             loopback
  [ ok ]  LAN reachable      1 address another device could reach
  [warn]  Port 3000          already in use on 192.168.0.115. It is held by
                             python3 (pid 67834).
  [ ?  ]  Bind address       AutoNet cannot see what address your server
                             binds. If it binds 192.168.0.115 specifically, it
                             stops answering when the network changes; if it
                             binds 0.0.0.0, it follows the change. Check your
                             program's host or bind setting.

1 warning, nothing failed, 1 not verified. AutoNet can give another device an
address that reaches this machine.
```

There are four verdicts, not three:

| | |
|---|---|
| `[ ok ]` | Checked, and fine. |
| `[warn]` | Checked, worth knowing about, not broken. |
| `[fail]` | Checked, and broken. Exit code `1`. |
| `[ ?  ]` | **Not checked.** AutoNet did not determine this. Never affects the exit code. |

The fourth exists because a row AutoNet could not verify is not a pass. Calling
it one would be a tick that means nothing.

**The bind-address row is always `[ ? ]`, and that is deliberate.** AutoNet
cannot see what address another program passes to `bind()`, and in the ordinary
case — running `doctor` *before* starting the server — there is no socket to
look at. So the row explains the distinction and leaves the answer to you: a
server bound to one specific address stops answering when the network changes,
and a server bound to the wildcard (`0.0.0.0` or `::`) follows it. It is
advice, not a measurement, and it is not presented as one. See
[ADR 0001](docs/adr/0001-network-change-during-autonet-run.md).

### Scripting

```sh
IP=$(autonet ip) || exit 1
npm run dev -- --host "$IP"
```

`autonet ip` writes exactly one line to stdout on success — the address, or
with `-p/--port` the URL (`http://192.168.1.101:3000`). Diagnostics always go to
stderr, and colour is disabled automatically when
stdout is not a terminal (and whenever `NO_COLOR` is set).

Exit codes:

| Code | Meaning |
|---|---|
| `0` | An address was selected. For `doctor`, nothing failed — warnings included. For `run`, the command itself exited `0`. |
| `1` | Nothing usable — the machine may simply be offline. Not a malfunction. For `doctor`, at least one check failed. |
| `2` | AutoNet could not do its job: the OS could not be queried, the configuration is invalid, or the command asked for something that does not exist. |

`autonet run` otherwise exits with the exit code of the command it ran, which
may be any value — `autonet run -- make test` returning `2` is the tests
failing, not AutoNet.

### Following the network as it changes

```console
$ autonet watch
AutoNet linux-netlink
Watching for address changes as linux-netlink-monitor reports them, and every 30s regardless. Ctrl-C to stop.

Current:  wlo1 (wireless) / 192.168.1.18
```

It prints the selection immediately, then prints again only when the answer
actually changes — plug in an Ethernet cable, bring up a VPN, walk out of range.
The kernel's own change notifications drive it where the platform has them
(netlink on Linux, `PF_ROUTE` on macOS, `NotifyIpInterfaceChange` on Windows),
with a 30-second sweep underneath so a missed notification costs half a minute
rather than the session. `--json` emits one object per line, ready for a pipe:

```console
$ autonet watch --json
{"schema_version":1,"source":"linux-netlink-monitor","change":"initial","captured_at":1788863108,"current":{"ip":"192.168.1.18","family":"ipv4","prefix_len":24,"scope":"private","interface":"wlo1","interface_index":3,"interface_kind":"wireless","gateway":"192.168.1.1","score":1415},"previous":null,"reason":null,"events":[]}
```

This is the same change pipeline `autonet advertise` republishes through.

### Explaining a surprising answer

```console
$ autonet status -v
  Considered
  INTERFACE ADDRESS       SCORE WHY
  wlo1      192.168.1.101 1415  default_route +1000, interface_kind +200, family_match +150, …

  Rejected
  INTERFACE       ADDRESS     REASON
  docker0         172.17.0.1  interface is down
  br-471fd10199e4 172.18.0.1  container or virtual interface with no route to anywhere
  lo              127.0.0.1   loopback address is not reachable from other devices
```

Every verdict is attributable to a named rule. There is no hidden heuristic.

### Being findable by name

An IP address has to be read off one screen and typed into another, and it stops
being true the moment the laptop moves. A name does not.

```console
$ autonet advertise --port 3000
Advertising real0-autonet.local
  Address   192.168.1.18
  Service   _http._tcp port 3000
  Open      http://real0-autonet.local:3000

This machine is now discoverable on the local network. Ctrl-C to stop and withdraw the record.
```

The name follows the address. Unplug the Ethernet cable and the record is
re-announced against the Wi-Fi address, through the same change pipeline
`autonet watch` uses; when nothing is reachable the record is withdrawn rather
than left pointing somewhere the machine no longer is.

**It transmits, so it is off until you say otherwise.** With `[hostname]
enabled` unset, `autonet advertise` refuses and prints the two lines to add to
your configuration file. There is no flag and no environment variable that turns
it on — consent to publish this machine belongs in a file you wrote.

The published name is `<hostname>-autonet.local`, not `<hostname>.local`. Your
operating system already owns the latter, and already answers it with *every*
address on *every* interface — frequently a Docker bridge nobody can reach.
AutoNet publishes one address, the selected one. Override the name with
`[hostname] name` or `AUTONET_HOSTNAME`.

`--qr` works here too, and this is the one case where a scanned code outlives
the address it was made from. It encodes the *name* — the responder answering it
is the process you are looking at — so the code stays good across the handover
described above, which is exactly what a code from `status` cannot promise. It
is drawn once, when the advertisement opens, because the address moving does not
change what the code says.

What goes on the wire: the name, the selected address, the port, and the service
type. Nothing else — no interface names, no MAC addresses, no scores. What does
not: AutoNet advertises only. It never browses, collects, or records what other
machines on the network are saying. See
[ADR 0002](docs/adr/0002-mdns-advertisement.md) for the security analysis,
including what this does *not* protect you from.

### Getting the URL onto a phone

```console
$ autonet status --qr --port 3000
AutoNet linux-netlink

  Address    192.168.1.18/24
  Interface  wlo1 (wireless, up)
  Gateway    192.168.1.1
  Scope      private

  Local      http://127.0.0.1:3000
  Network    http://192.168.1.18:3000  ← open this from another device

  Scan       http://192.168.1.18:3000
                                 
                                 
    █▀▀▀▀▀█  ▀▄█   ██ █▀▀▀▀▀█    
    █ ███ █  █▀▄▀ ▀▀  █ ███ █    
    █ ▀▀▀ █ ▄█   ▄▄█▄ █ ▀▀▀ █    
    ▀▀▀▀▀▀▀ ▀ █▄▀ ▀▄█ ▀▀▀▀▀▀▀    
    █ ▄▀▄█▀ ▀▄█▄▀▀▄  ▀▄█    ▄    
    ▄▀▀█  ▀█▀▀▄ █▄█ ▄ █▄▀  ▀▀    
     ▄▀█ ▄▀ ▄ ▄▄  █▀█▄█▀ █▄▀█    
    ▀▄▄ █ ▀▀ ▄▄█ ▄ ▄▀▄▄█ ▄▀▄▀    
    ▀▀  ▀▀▀▀▄▀  ██▀▀█▀▀▀███ ▄    
    █▀▀▀▀▀█ ▄▀ ▀█ ███ ▀ █▄▄█▀    
    █ ███ █ ▄█▄▄▀▀ ▄██▀▀█ ▄ ▀    
    █ ▀▀▀ █  ▄▄ ▀█▄▀▄▀ ▀▀█▀ ▀    
    ▀▀▀▀▀▀▀ ▀▀▀ ▀ ▀ ▀▀▀▀   ▀▀    
                                 
                                 
```

The code is the `Network` URL and nothing else. `--qr` adds to the output; it
never replaces it, so everything above is unchanged.

**It encodes the address, not the `.local` name**, even when `[hostname]` is
enabled. Setting `enabled = true` means this machine *may* advertise; the name
only resolves while `autonet advertise` is actually running, and `status`
prints and exits. A code carrying a name nothing is answering would scan
cleanly and then fail to load, which is worse than the address it replaced.

`autonet advertise --qr` encodes the name, for the mirror-image reason: there
the responder is running, so the name is the fact that holds still and the
address is the one that moves.

`--qr` needs a port — a code is only worth scanning if it opens something — and
is refused with `--json`, where the same string is already `urls.network`.

In a real terminal the code is painted black on white explicitly, so it scans
whether your theme is light or dark. With `NO_COLOR` set there is no colour to
force, the block characters have to carry the polarity themselves, and AutoNet
renders for a dark terminal and says so. See
[ADR 0003](docs/adr/0003-qr-code-contents.md).

## The JSON contract

`--json` is the intended way to use AutoNet from another language. Every payload
carries a `schema_version`, and the shape will stay backward compatible within a
version.

```console
$ autonet status --json | jq
{
  "schema_version": 1,
  "platform": "linux-netlink",
  "captured_at": 1788177606,
  "selected": {
    "ip": "192.168.1.101",
    "family": "ipv4",
    "prefix_len": 24,
    "scope": "private",
    "interface": "wlo1",
    "interface_index": 3,
    "interface_kind": "wireless",
    "gateway": "192.168.1.1",
    "score": 1415
  }
}
```

When nothing is selectable, `selected` is `null` and `error` explains why —
the document is still valid JSON, and the exit code is still `1`.

Hardware (MAC) addresses are **withheld by default** from `autonet interfaces
--json`. They are durable hardware identifiers, and answering "what address can
my phone reach" never requires publishing one. Pass `-v` if you need them.

## Configuration

The first of these that names a directory, so the same rule works everywhere:
`$XDG_CONFIG_HOME/autonet/config.toml`, then `%APPDATA%\autonet\config.toml`
(Windows), then `~/.config/autonet/config.toml`.

```toml
[selection]
prefer_family      = "ipv4"    # ipv4 | ipv6 | any
allow_loopback     = false
allow_link_local   = false
allow_vpn          = false
allow_container    = false
include_down       = false     # consider interfaces the kernel reports as down
exclude_interfaces = ["tailscale0", "br-*"]
prefer_interfaces  = []
# require_interface = "wlo1"   # omit entirely unless you mean it

[output]
# Accepted and validated, but nothing reads it yet: use --json per invocation.
format       = "text"          # text | json
default_port = 0               # 0 means none; used by -p when the flag is absent

[hostname]                     # NEW: requires this version or later, see below
enabled = false                # may this machine advertise itself on the LAN?
# name  = "laptop-autonet"     # unset derives <hostname>-autonet
service = "_http._tcp"         # the DNS-SD service type advertised
```

The `[hostname]` section is **new**, and because unknown keys are rejected
rather than ignored, a file containing it will not parse on an older `autonet`
binary. Sections default when absent, so the reverse — an older file on this
binary — is fine. See
[Changing the configuration file](docs/architecture.md#changing-the-configuration-file).

Unknown keys are rejected rather than ignored, so a typo fails loudly instead of
doing nothing.

Precedence, lowest to highest:

1. Built-in defaults
2. The configuration file
3. `AUTONET_FAMILY`, `AUTONET_INTERFACE`, `AUTONET_EXCLUDE_INTERFACES`,
   `AUTONET_ALLOW_VPN`, `AUTONET_ALLOW_CONTAINER`, `AUTONET_ALLOW_LOOPBACK`,
   `AUTONET_HOSTNAME`
4. Command-line flags

## How it decides

Two stages, deliberately separate.

**Disqualification** removes what is objectively unusable: an interface that is
down, a loopback or link-local address, an unspecified or multicast address, an
address of a family you did not ask for, an interface you excluded, or a
container/virtual interface with no route to anywhere.

**Scoring** ranks what remains:

| Signal | Δ |
|---|---|
| Owns the default route for this family | +1000 |
| Owns a default route for the other family | +400 |
| Ethernet | +250 |
| Wireless | +200 |
| Bridge | +150 |
| Preferred family match | +150 |
| Private address (RFC 1918 / ULA) | +100 |
| Global (public) address | +60 |
| Has a gateway | +25 |
| Explicitly preferred interface | +2000 |
| Container or virtual interface | −800 |
| VPN tunnel (unless allowed) | −300 |
| CGNAT (`100.64.0.0/10`) | −200 |
| IPv6 privacy-extension temporary address | −20 |
| Route metric | −min(metric, 1000)/10 |

Containers are **scored, not banned**. A machine whose real uplink is a bridge
(`br0` over `eth0`) still works, because owning the default route (+1000)
outweighs the container penalty (−800). But a Docker bridge on an offline laptop
has no default route at all, and is disqualified outright — returning
`172.17.0.1` there would be a plausible-looking lie.

`--allow-vpn` means *stop penalising*, not *prefer*. Wi-Fi (+200) still outranks
an allowed tunnel (+50). Use `--interface wg0` to insist on the tunnel.

Ties break deterministically: score, then interface index, then address. The
answer never depends on the order the kernel happened to enumerate things.

## Architecture

```
crates/autonet-core/       model, classification, selection, config, events
crates/autonet-platform/   NetworkProvider trait + the Linux, macOS and Windows backends
crates/autonet-cli/        clap commands, text and JSON rendering
tests/fixtures/            deterministic NetworkState snapshots
```

The rule that makes this testable: **`autonet-core` performs no OS calls.** It is
pure functions over a `NetworkState` value. `autonet-platform` is the only crate
that talks to the kernel, and its only job is to produce that value.

So the selection engine is tested against JSON fixtures — a Wi-Fi-only laptop, a
docked laptop, a machine behind a VPN, an offline machine running Docker, the
23-interface development host — and switching networks while the suite runs
cannot change a single result. Only the thin platform layer needs a live machine,
and its tests are `#[ignore]`d for that reason.

The CLI, the future daemon, and the future SDKs are all consumers of the same
core. None of them re-implements discovery or selection, which is what will let
them agree with each other.

## Platform support

| Platform | Status |
|---|---|
| Linux | Implemented and running, via netlink |
| macOS | Implemented, via SystemConfiguration and `PF_ROUTE` — **not yet verified on hardware** |
| Windows | Implemented, via the IP Helper API — **not yet verified on hardware** |

The two caveats are deliberate and not modesty. Both backends were written
without access to the machine they target. CI builds and tests them on
`macos-latest` and `windows-latest`, but those runners have no Wi-Fi radio, no
VPN and no dock — which is to say, none of the conditions the backends most need
to get right has ever been observed. Two checklists exist to change these lines,
and until someone runs them the lines stay as they are:

- [`docs/milestone-2a-acceptance.md`](docs/milestone-2a-acceptance.md), with
  [`scripts/macos-acceptance.sh`](scripts/macos-acceptance.sh)
- [`docs/milestone-2b-acceptance.md`](docs/milestone-2b-acceptance.md), with
  [`scripts/windows-acceptance.ps1`](scripts/windows-acceptance.ps1)

Unsupported platforms compile and fail at *runtime* with a clear message, so the
whole workspace can be built and tested from any machine.

## Roadmap

Shipped:

- **M1** discovery, selection, `status` / `ip` / `interfaces` / `routes`
- **M3** `autonet run` — run an existing app unmodified with `AUTONET_IP`,
  `AUTONET_HOST` and `AUTONET_URL` injected
- **M4** `autonet watch` — react to Wi-Fi ↔ Ethernet switches, VPNs coming up,
  cables being unplugged
- **M4a** `autonet advertise` — a `.local` name for the selected address,
  republished through M4's pipeline when it moves
- **M4b** `autonet status --qr` and `autonet advertise --qr` — the URL, or the
  published name, as a scannable code
- **M4c** `autonet doctor` — a plain-language checklist of what works and what
  does not
- **Stage 4** release archives for five targets, and one-command installers for
  Linux, macOS and Windows

Built, but not yet signed off on real hardware — these are honest gaps, not
formalities:

- **M2a** macOS backend — written, awaiting hardware acceptance
- **M2b** Windows backend — written, awaiting hardware acceptance
- **M4** a real Wi-Fi handover, observed end to end, rather than the synthetic
  interfaces the tests use
- **M4a** a second device resolving the published `.local` name
- **M4b** a phone camera actually scanning the code

Planned, in order:

- **M5** a local daemon with an HTTP API over a Unix socket / named pipe
- **M6** thin SDKs for Python, TypeScript, Java and .NET

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

# The live checks, which depend on the machine's actual network:
cargo test -p autonet-platform -- --ignored --nocapture
```

To confirm AutoNet agrees with the kernel on a Linux host:

```sh
diff <(autonet ip) <(ip route get 1.1.1.1 | grep -oP 'src \K\S+')
```

## Licence

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
