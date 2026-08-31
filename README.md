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

On a laptop with 22 interfaces, nine of them Docker bridges, it returns the one
address that works.

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

Common flags, accepted before or after any command:

| Flag | Effect |
|---|---|
| `--json` | Machine-readable output. See [the JSON contract](#the-json-contract). |
| `-f, --family <ipv4\|ipv6\|any>` | Which family to prefer. Default `ipv4`. |
| `-p, --port <PORT>` | Also render the URL to open on another device. |
| `-i, --interface <NAME>` | Use only this interface. |
| `-x, --exclude <NAME>` | Never use this interface. Repeatable; a trailing `*` matches a prefix. |
| `--allow-vpn` | Stop penalising VPN tunnels. |
| `--allow-container` | Stop penalising Docker bridges and virtual networks. |
| `--allow-loopback` | Permit `127.0.0.1` / `::1`. |
| `-c, --config <PATH>` | Use this config file. |
| `-v, --verbose` | Show every candidate, its score, and the rules that produced it. |

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
| `0` | An address was selected. |
| `1` | Nothing usable — the machine may simply be offline. Not a malfunction. |
| `2` | AutoNet could not do its job: the OS could not be queried, the configuration is invalid, or the command asked for something that does not exist. |

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

`~/.config/autonet/config.toml` (or `$XDG_CONFIG_HOME/autonet/config.toml`):

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
default_port = 0
```

Unknown keys are rejected rather than ignored, so a typo fails loudly instead of
doing nothing.

Precedence, lowest to highest:

1. Built-in defaults
2. The configuration file
3. `AUTONET_FAMILY`, `AUTONET_INTERFACE`, `AUTONET_EXCLUDE_INTERFACES`,
   `AUTONET_ALLOW_VPN`, `AUTONET_ALLOW_CONTAINER`, `AUTONET_ALLOW_LOOPBACK`
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
22-interface development host — and switching networks while the suite runs
cannot change a single result. Only the thin platform layer needs a live machine,
and its tests are `#[ignore]`d for that reason.

The CLI, the future daemon, and the future SDKs are all consumers of the same
core. None of them re-implements discovery or selection, which is what will let
them agree with each other.

## Platform support

| Platform | Status |
|---|---|
| Linux | Implemented, via netlink |
| macOS | Planned (M2) |
| Windows | Planned (M2) |

Unsupported platforms compile and fail at *runtime* with a clear message, so the
whole workspace can be built and tested from any machine.

## Roadmap

Milestone 1 (discovery, selection, `status` / `ip` / `interfaces` / `routes`) is
complete. Planned next, in order:

- **M2** macOS and Windows backends
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
