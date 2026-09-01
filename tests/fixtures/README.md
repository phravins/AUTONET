# Selection fixtures

Every file here is a serialised `autonet_core::model::NetworkState` — byte-for-byte
what `autonet status --json` emits. They drive
[`crates/autonet-core/tests/fixtures.rs`](../../crates/autonet-core/tests/fixtures.rs),
which constructs no provider and touches no kernel: the harness reads JSON and runs
the selector, so it produces identical results on every platform. That is why CI
runs the same fixtures on `ubuntu-latest` and `macos-latest` and why a divergence
between them would be a bug in the selector, not in the data.

## Provenance

Fixture data is only as good as where it came from, and the three groups below are
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

### 3. Real captures — none yet

There is no capture from real hardware in this directory, and no file here should
ever be labelled as one unless it was produced by
`autonet status --json` on the machine it describes.

**Naming rule:** `macos-*` is synthetic. A genuine capture lands as
**`macos-real-<scenario>.json`**, written by the Milestone 2a hardware-acceptance
run. Until one exists, the VPN-over-macOS case is verified only in theory.

## Adding a fixture

Drop the JSON in this directory and add a row to the table above. The harness
enumerates `*.json` at runtime, so round-trip and order-independence coverage picks
a new file up automatically — but the provenance table does not, and an unlabelled
fixture is worse than no fixture.
