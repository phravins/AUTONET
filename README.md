# AutoNet

**The IP address other devices on your network can actually reach.**

```console
$ autonet ip
192.168.1.101

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

```sh
cargo install --path crates/autonet-cli    # installs the `autonet` binary
```

Or with Nix:

```sh
nix run github:osworks/autonet -- status
nix develop                                # dev shell with the pinned toolchain
```

Nix is used for reproducible builds and development environments only. The
resulting binary is an ordinary native executable with **no runtime dependency
on Nix**.

## Commands

| Command | What it prints |
|---|---|
| `autonet status` | The selected address, its interface, gateway and scope. The default when no command is given. |
| `autonet ip` | The bare address and nothing else, for `$(...)` substitution. |
| `autonet interfaces` | Every interface, classified, with its addresses. |
| `autonet routes` | The routing table, default routes first. |
| `autonet run -- <cmd>` | Runs a command with `AUTONET_IP`, `AUTONET_HOST` and `AUTONET_URL` in its environment, and exits with the command's own exit code. The variables are a snapshot taken at launch; see [ADR 0001](docs/adr/0001-network-change-during-autonet-run.md). |
| `autonet doctor` | A checklist of what works and what does not, in plain language, with a summary line. |
| `autonet advertise` | Publishes a `.local` name pointing at the selected address, and re-publishes it whenever the address moves. **This transmits** — it is off until `[hostname] enabled` says otherwise. See [ADR 0002](docs/adr/0002-mdns-advertisement.md). |

Common flags, accepted before or after any command:

| Flag | Effect |
|---|---|
| `--json` | Machine-readable output. See [the JSON contract](#the-json-contract). |
| `-f, --family <ipv4\|ipv6\|any>` | Which family to prefer. Default `ipv4`. |
| `-p, --port <PORT>` | Also render the URL to open on another device. Defaults to `output.default_port`. A hint about what to *print*, not about what a command will *bind* — `autonet run` warns if it is taken, and starts the command anyway. |
| `--qr` | Also render the network URL as a QR code a phone camera can read. Needs a port; refused with `--json`, where the payload is already `urls.network`. |
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

`autonet ip` writes exactly one address and a newline to stdout on success.
Diagnostics always go to stderr, and colour is disabled automatically when
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
format       = "text"          # text | json
default_port = 0             # 0 means none; used by -p when the flag is absent

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
crates/autonet-platform/   NetworkProvider trait + the Linux/netlink backend
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
| Windows | In progress (M2b), via the IP Helper API |

macOS's caveat is deliberate and not modesty. That backend was written without
access to a Mac; CI builds and tests it on `macos-latest`, but that runner has
no Wi-Fi radio and no VPN, so the two things the backend most needs to get right
have never been observed. [`docs/milestone-2a-acceptance.md`](docs/milestone-2a-acceptance.md)
is the checklist that will change this line, and until someone runs it the line
stays as it is.

Unsupported platforms compile and fail at *runtime* with a clear message, so the
whole workspace can be built and tested from any machine.

## Roadmap

Milestone 1 (discovery, selection, `status` / `ip` / `interfaces` / `routes`) is
complete. Planned next, in order:

- **M2a** macOS backend — written, awaiting hardware acceptance
- **M2b** Windows backend — in progress
- **M3** `autonet run` — run an existing app unmodified with `AUTONET_IP`,
  `AUTONET_HOST` and `AUTONET_URL` injected
- **M4** `autonet watch` — react to Wi-Fi ↔ Ethernet switches, VPNs coming up,
  cables being unplugged
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

MIT OR Apache-2.0.
