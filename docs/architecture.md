# Architecture

AutoNet exists because IP discovery keeps getting reimplemented — badly — inside
individual applications. The design's single organising rule is that discovery
happens **once**, in one place, and everything else consumes the result.

## Layers

```
        ┌──────────────────────────────────────────────┐
        │  CLI · daemon (M5) · SDKs (M6)               │  consumers
        └──────────────────────────────────────────────┘
                              │  NetworkState, Selection
        ┌──────────────────────────────────────────────┐
        │  autonet-core                                │  all policy
        │  model · classify · select · config · event  │
        └──────────────────────────────────────────────┘
                              │  NetworkState
        ┌──────────────────────────────────────────────┐
        │  autonet-platform                            │  all OS calls
        │  NetworkProvider → linux/netlink, …          │
        └──────────────────────────────────────────────┘
                              │
        ┌──────────────────────────────────────────────┐
        │  the operating system's networking stack     │
        └──────────────────────────────────────────────┘
```

Two rules hold the design together:

1. **`autonet-core` performs no OS calls.** It is pure functions over a
   `NetworkState` value.
2. **Nothing above `autonet-platform` contains platform-specific code.** No
   `#[cfg(target_os)]` appears in the core or the CLI.

Everything else follows from those.

## Why the core is pure

Because network state changes underneath you. That is the problem AutoNet
solves, and it is also what makes the problem hard to test: a test that asks the
real machine "which address wins?" produces a different answer on Wi-Fi, on
Ethernet, and on a train.

So `NetworkState` is a plain serializable value, and the selection engine is
tested against JSON snapshots in [`tests/fixtures/`](../tests/fixtures):

| Fixture | What it proves |
|---|---|
| `this-machine.json` | 23 interfaces — nine bridges, eleven veths, one Wi-Fi card — still returns the Wi-Fi address |
| `wifi-and-ethernet.json` | A docked laptop prefers the wired link |
| `wifi-plus-vpn.json` | A VPN owning a lower-metric default route does not hijack the answer |
| `docker-only.json` | An offline laptop running Docker reports failure, not `172.17.0.1` |
| `loopback-only.json`, `disconnected.json` | Failure is an answer, and it explains itself |
| `ipv6-only.json` | Stable addresses beat privacy-extension temporaries |
| `cgnat-hotspot.json` | A real LAN address beats carrier-grade NAT |

None of them can be perturbed by the host's actual network. The live checks that
genuinely need a machine live in `crates/autonet-platform/tests/live.rs` and are
`#[ignore]`d.

## The crates

### `autonet-core`

| Module | Responsibility |
|---|---|
| `model` | `NetworkState`, `Interface`, `Address`, `Route`, and the enums. The serialization here *is* the public wire format. |
| `classify` | Pure functions: which scope does this IP have, what kind of device is this? Every RFC boundary is a unit test. |
| `select` | Disqualification, then scoring. Returns the winner **and** every candidate with per-rule reasons. |
| `config` | TOML plus the `AUTONET_*` environment layer. Unknown keys are rejected. |
| `event` | `NetworkEvent` / `NetworkDiff`, and `diff()`. Read by `autonet watch`, and through it by `autonet advertise`. |

`select` returning the full candidate list rather than just a winner is
deliberate. It makes `-v` a rendering job, and it will make `autonet doctor` a
rendering job too, instead of a second implementation of the same logic that
drifts out of agreement with the first.

`diff()` matches interfaces **by name, not index**, so a USB adapter or a VPN
that comes back with a fresh kernel index is reported as one interface changing
rather than as a removal plus an addition.

### `autonet-platform`

```rust
pub trait NetworkProvider: Send + Sync {
    fn snapshot(&self) -> Result<NetworkState, PlatformError>;
    fn platform_name(&self) -> &'static str;
}
```

The trait is **synchronous** on purpose. The Linux backend needs an async
netlink client, so it owns a private current-thread Tokio runtime and hides it.
macOS and Windows are naturally blocking; making the trait async would tax three
platforms to suit one, and would push `async` into the CLI and every future SDK
binding for no benefit.

`Send + Sync` because the M5 daemon will share one provider across handlers.

The crate holds a second, narrower trait — for noticing that a snapshot has gone
out of date:

```rust
pub trait ChangeSource: Send {
    fn wait(&mut self, timeout: Duration) -> Result<bool, PlatformError>;
    fn source_name(&self) -> &'static str;
}
```

Deliberately not part of `NetworkProvider`. Taking a snapshot is universal;
being *told* about a change is not. Linux has netlink; macOS and Windows have
nothing wired up here yet and poll instead. So `change_source()` returns
`Result<Option<Box<dyn ChangeSource>>>`, where `None` means "this platform has
nothing to subscribe to" — the ordinary answer, not a failure — and an error
means "it has something and it could not be opened". Folding the two into one
would make a restricted container look like macOS.

`wait` returns a bool rather than the change itself. The source says *something
happened*; `NetworkProvider` is then asked what the state is now, and `diff()`
says what moved. Keeping the payload out of the trait is what stops a backend
becoming a second, divergent account of the network, and it is why an event
source can only change *when* the pipeline runs, never *what it concludes*.

### `autonet-cli` (the package is named `autonet`)

Parses flags, layers configuration, takes a snapshot, asks the core, renders.
`commands.rs` holds one function per command, with the ones that outgrew a
screen — `spawn.rs`, `watch.rs`, `advertise.rs` — in modules of their own;
`render.rs` holds the table and colour machinery. `state.rs` writes the optional
file `autonet run --state-file` keeps current, and is a consumer of `watch.rs`
rather than a second detector.

It contains no *discovery* logic and no `#[cfg(target_os)]`: it never asks the
operating system what the network looks like. There is exactly one place where
it puts packets on the wire — `advertise.rs` runs an mDNS responder on UDP
5353 — and one where it writes outside its own output streams — `state.rs`
writes the path `--state-file` names, and nothing else. Both are worth naming
here rather than leaving to be discovered.

It is not the layering breach it first looks like. Discovery is a *question*
about this machine that three operating systems answer through three unrelated
APIs, which is precisely why it sits behind `NetworkProvider`. Publishing a name
is an *action the user explicitly asked for*, its protocol is identical on every
platform, and it is implemented by a cross-platform crate rather than by
AutoNet. The rule that would be broken is "no platform-specific code above
`autonet-platform`", and there is none. If that ever stops being true — ADR 0002
names Windows as the likeliest place — the responder moves down behind a trait,
for exactly the reason the rule exists.

## Extension recipes

### Adding a platform

1. Write `src/<platform>/mod.rs` implementing `NetworkProvider`.
2. Add it to `provider()` behind a `#[cfg(target_os = "…")]`.

Nothing above `autonet-platform` changes. Unsupported platforms keep compiling
via `src/unsupported.rs`, which returns `PlatformError::Unsupported` at runtime —
so a developer on a platform without a backend can build and test the whole
workspace before one exists, as macOS and Windows developers could before theirs
did.

The backend's whole job is translation. It must not filter, prefer, or rank
anything: policy behind a `#[cfg]` is policy that cannot be fixture-tested.

### Adding a command

Add a variant to `cli::Command`, a function to `commands.rs` — or its own
module, once it outgrows a screen, as `watch` and `advertise` did — and a match
arm in `main.rs`. If the command needs a new *decision*, that decision belongs in
`autonet-core` with fixture tests, not in the renderer.

### Changing the wire format

`SCHEMA_VERSION` is stamped into every payload from the first release because
the SDKs will bind to it. Adding an optional field is fine; removing one, or
changing a type or a meaning, requires a version bump. See
[`json-schema.md`](json-schema.md). The `every_fixture_round_trips_through_serde`
test guards the format against accidental drift.

### Changing the configuration file

`config.toml` is a wire format too — a file written by a person under one
version of AutoNet and read by another. It has no `schema_version`, so the
rules in [`json-schema.md`](json-schema.md) transpose to it only in part:

| Rule from the JSON contract | Applies to `config.toml`? |
|---|---|
| Keys are never removed, retyped, or re-meaninged | Yes. |
| New optional keys may be added | Yes, provided the section and every key are `#[serde(default)]`, so a file written before them still parses. |
| *"Parsers must ignore unknown fields"* | **No — deliberately inverted.** `Config` is `#[serde(deny_unknown_fields)]` so a typo fails loudly instead of doing nothing. |
| A break increments `schema_version` | Not available. The file carries no version stamp. |

The consequence of the last two rows, which is easy to rediscover the hard way:
**a config file using a new section is a hard parse error on an older
binary.** Backward compatibility holds — an old file on a new binary is fine,
because every section defaults. Forward compatibility does not, and there is no
version to bump to signal it. That is the price of the loud-typo rule and it is
paid by every section added from here on, `[hostname]` included.

So: add a section, give it a `Default`, document it in the README's example
block, and note in the release notes that the file needs the newer binary.
`Config` derives `Eq`, so no floating-point setting may be added.

## Security posture

- The daemon (M5) will listen on the local machine only.
- The API will not accept command-execution requests.
- `autonet run` (M3) will pass arguments to the OS as an argument vector, never
  as a shell string, unless a shell is explicitly requested.
- Configuration needs no elevated privileges, and neither does reading network
  state: nothing in AutoNet requires root.
- Output is conservative by default. MAC addresses are withheld from
  `interfaces --json` unless `-v` is passed.
- Nothing opens a firewall port or exposes a service as a side effect. Any
  feature that makes an application LAN-reachable is explicit.
- `autonet advertise` (M4a) is the only feature that transmits, and it is a test
  of the rule above rather than an exception to it: it is off until the
  configuration file turns it on, and `hostname.enabled` deliberately has **no**
  environment-variable override. Consent to publish belongs in a file somebody
  wrote and can read back, not in a variable a parent process exported.

### Is advertising the same risk as binding the daemon to `0.0.0.0`?

They look alike, so the answer is written down rather than assumed.

**Not the same category.** The daemon's localhost-only rule exists because an
HTTP API *accepts input and acts on it*; every additional host that can reach it
is another host that can ask it to do something. The mDNS responder accepts a
question and answers with a name→address mapping for an address this machine
already answers on. It grants no access, opens no port for the application, and
changes nothing about what a peer could reach by typing the address it is being
handed. Anyone who can hear the announcement is already on the link and would
have found the machine with one ARP sweep.

**The same category in one respect**, which is why the toggle exists at all:
both are decisions about who can reach this machine, and neither is AutoNet's to
make on the user's behalf. So both default to off and both require a deliberate
act to enable.

**What is genuinely new**, with no counterpart on the list above: the machine
*names itself, unprompted and repeatedly, to the whole link* — including
whatever the operator chose to call it. That is an information disclosure rather
than an access grant, it is why `hostname.name` exists so the published name
need not be the system's own, and it is analysed in full in
[ADR 0002](adr/0002-mdns-advertisement.md).

## Milestones

| | Scope | Status |
|---|---|---|
| M1 | Workspace, Nix, Linux discovery, data model, selection engine, `status` / `ip` / `interfaces` / `routes`, `--json` | Complete |
| M2a | macOS backend — `getifaddrs`, SystemConfiguration, `PF_ROUTE` | Written; hardware acceptance outstanding |
| M2b | Windows backend — IP Helper (`GetAdaptersAddresses`, `GetIpForwardTable2`) | In progress |
| M3 | `autonet run` — inject `AUTONET_IP`, `AUTONET_HOST`, `AUTONET_URL` | Complete |
| M4 | `autonet watch` — network change events | Built; netlink verified against synthetic interfaces, a real Wi-Fi handover not yet observed |
| M4a | `autonet advertise` — a `.local` name for the selected address, re-announced through M4's pipeline | Built; record verified locally, resolution from a second device outstanding |
| M4b | `autonet status --qr` — the network URL as a scannable QR code | Built; phone-camera acceptance outstanding |
| M5 | Daemon with a local HTTP API over a Unix socket / named pipe | Planned |
| M6 | Python, TypeScript, Java and .NET SDKs — thin wrappers, never reimplementations | Planned |

Later: Docker awareness, IDE integration, installers.

Decisions that constrain a milestone's design, rather than its schedule, are
recorded in [`adr/`](adr/). M3's process model — whether `autonet run` supervises
its child or only launches it — is settled by
[ADR 0001](adr/0001-network-change-during-autonet-run.md). What that record
deferred to the name layer — the mDNS crate, the `[hostname]` configuration and
the advertisement's own security analysis — is settled by
[ADR 0002](adr/0002-mdns-advertisement.md). The one item *that* record left open
— what the QR code encodes, and why it is the address rather than the `.local`
name — is settled by [ADR 0003](adr/0003-qr-code-contents.md).
