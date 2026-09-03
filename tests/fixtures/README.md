# Selection fixtures

Every file here is a serialised `autonet_core::model::NetworkState`. They drive
[`crates/autonet-core/tests/fixtures.rs`](../../crates/autonet-core/tests/fixtures.rs),
which constructs no provider and touches no kernel: the harness reads JSON and runs
the selector, so it produces identical results on every platform. That is why CI
runs the same fixtures on `ubuntu-latest` and `macos-latest` and why a divergence
between them would be a bug in the selector, not in the data.

## Provenance

Fixture data is only as good as where it came from, and the groups below are
**not** equally strong evidence. Nothing here is a capture from real hardware.

### 1. OS-agnostic — hand-built, no operating system assumed

The original set. Interface names are Linux-flavoured (`wlo1`, `eno1`, `wg0`)
because they were written on Linux, but nothing about the scenarios is
Linux-specific; they exercise the selector's policy, not any backend.

| File | Scenario |
|---|---|
| `this-machine.json` | one Wi-Fi card under nine bridges, eleven veth pairs, a down NIC |
| `wifi-only.json` | a laptop on Wi-Fi |
| `ethernet-only.json` | a desktop on a wire |
| `wifi-and-ethernet.json` | docked laptop: Ethernet must win |
| `wifi-plus-vpn.json` | a tunnel must not hijack the LAN address |
| `docker-only.json` | offline with Docker up: silence beats `172.17.0.1` |
| `loopback-only.json` | nothing but `lo` |
| `disconnected.json` | every interface down |
| `ipv6-only.json` | no IPv4 anywhere |
| `cgnat-hotspot.json` | CGNAT loses to an ordinary private address |

### 2. macOS-shaped — synthetic but targeted

**Hand-built. Never compared against a real Mac.** Added in Milestone 2a Task 6 to
exercise a combination the GitHub `macos-latest` runner cannot produce: several
same-kind links at different service-order ranks with a tunnel up at the same time.

| File | Scenario |
|---|---|
| `macos-dock-and-vpn.json` | `en0` + `en5` (both Ethernet, ranks 1 and 0) plus an active `utun4` |
| `macos-wifi-and-ethernet.json` | Wi-Fi ranked *above* Ethernet in the service order |

Metrics in these files are what
[`servicerank.rs`](../../crates/autonet-platform/src/servicerank.rs) would
synthesize — 100 per service-order rank, `METRIC_CAP` (1000) for an interface the
order does not mention — because macOS has no per-route metric of its own.

Both files deliberately break the naive name heuristic (`en0` is Wi-Fi in one and
Ethernet in the other; the better-ranked link has the *higher* interface index),
matching the backend's rule that an interface name is only ever a join key and
never evidence about what a device is.

**What they do not establish.** They encode what the macOS backend is *expected* to
emit. If that expectation is wrong, the fixtures agree with the bug and pass. They
prove the selector combines interface classification and the service-order
tie-break correctly; they prove nothing about whether a real Mac feeds it input of
this shape.

### 3. Windows-shaped — synthetic but targeted

**Hand-built. Never compared against a real Windows machine.** Added in Milestone
2b Task 5, for the two risks the Windows backend actually surfaced.

| File | Scenario |
|---|---|
| `windows-dual-stack-indices.json` | an adapter whose `IfIndex` and `Ipv6IfIndex` disagree, with a route in each family |
| `windows-on-link-route.json` | a default route with no next hop, beside an otherwise identical one that has a gateway |
| `windows-dock-and-vpn.json` | two Ethernet links at different interface metrics, plus a WireGuard tunnel and a Hyper-V switch |

They are shaped the way the Windows backend emits, which differs from the older
files in three visible ways: a default route's `destination` is `null` rather than
`"0.0.0.0/0"` ([`winroute::destination`](../../crates/autonet-platform/src/winroute.rs)
normalises it), an on-link route's `gateway` is `null` where Windows itself
reports `0.0.0.0`, and `metric` is already the documented sum of the route's own
offset and the owning adapter's interface metric — so the values are Windows
automatic-metric numbers (5 for a gigabit link, 25–35 for a slower one, 5000-odd
for a Hyper-V switch), not the 100-per-rank values the `macos-*` files use.

`windows-dock-and-vpn.json` is also the only fixture using `InterfaceKind::Other`,
which serialises as `{ "other": "virtual-ethernet" }` — the classification
[`wintype`](../../crates/autonet-platform/src/wintype.rs) gives an Ethernet-typed
adapter with no hardware behind it.

**What they do not establish.** The same caveat as the macOS group, and one more
that is specific to Windows: the LUID→index join these fixtures depend on happens
in `autonet-platform`, and a fixture is a `NetworkState` that has already been
through it. No fixture can falsify that join — by the time the data is JSON the
LUID is gone and only an index survives. What
`windows-dual-stack-indices.json` pins is the *selector-level consequence* of a
correct join, with a control case that fails loudly if the pre-join index is used
instead. The join itself is unit-tested in `winroute::route_index`, on Linux.

Nor do they say anything about whether Windows really reports what they assume:
whether `Metric` is meaningful, whether the interface metric moves on a real
dock, whether a real VPN adapter classifies as `Vpn`. That is the hardware
acceptance run's job and nothing else's.

### 4. Real captures — none yet

There is no capture from real hardware in this directory, and no file here should
ever be labelled as one unless it came off the machine it describes.

**Naming rule:** `macos-*` and `windows-*` are synthetic. A genuine capture lands
as **`macos-real-<scenario>.json`** or **`windows-real-<scenario>.json`**, written
by the corresponding hardware-acceptance run
([`docs/milestone-2a-acceptance.md`](../../docs/milestone-2a-acceptance.md); the
Windows one is Milestone 2b Task 7 and does not exist yet). Until one exists, the
VPN case is verified only in theory on both platforms.

## Capturing one

```sh
cargo run -p autonet-platform --example capture > tests/fixtures/macos-real-wifi.json
```

**Not `autonet status --json`** — an earlier version of this file said that, and
it was wrong. No CLI subcommand emits a whole `NetworkState`: `status --json`
reports the *decision* (`selected`, `urls`, `candidates`), and `interfaces --json`
and `routes --json` each carry one half of the state. Merging those two would
also mean two separate snapshots, so a link changing in between would produce a
fixture describing a machine that never existed.
[`examples/capture.rs`](../../crates/autonet-platform/examples/capture.rs) takes
one snapshot, strips MAC addresses, and clears `captured_at` so re-capturing an
unchanged network is not a diff.

It does **not** strip IP addresses — those are the fixture's whole content — so a
capture publishes the interface names, addresses and prefixes of the machine it
came from. Read one before committing it.

## Adding a fixture

Drop the JSON in this directory and add a row to the table above. The harness
enumerates `*.json` at runtime, so round-trip and order-independence coverage picks
a new file up automatically — but the provenance table does not, and an unlabelled
fixture is worse than no fixture.
