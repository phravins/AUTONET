# Milestone 2a — macOS hardware acceptance

Milestone 2a built a macOS backend without a Mac. Every claim in it rests on
Apple's headers, on the `macos-latest` CI runner (one interface, no Wi-Fi radio,
no VPN), and on fixtures written by the same person who wrote the code they
test — which means a wrong belief produces a fixture that agrees with it.

This document is where that stops being theory. Run
[`scripts/macos-acceptance.sh`](../scripts/macos-acceptance.sh) under each
network condition, fill in the columns, and write the closing statement at the
bottom. **Milestone 2b inherits whatever this page says.** An unrun row is
blank, not a tick.

## Before starting

- The tree is at a **pushed, CI-green commit**. A failure should be
  unambiguously the hardware, not an unbuilt change.
- The Mac has a Rust toolchain (`rustup show` resolves from
  `rust-toolchain.toml`).
- You know which physical link is which: `networksetup -listallhardwareports`
  maps a service name like "Wi-Fi" to its BSD device. AutoNet must reach the
  same answer **without** looking at the name, which is the point.

## Running it

```sh
scripts/macos-acceptance.sh wifi       # Wi-Fi associated, nothing else up
scripts/macos-acceptance.sh ethernet   # a wire, Wi-Fi off
scripts/macos-acceptance.sh both       # both up at once
scripts/macos-acceptance.sh vpn        # a tunnel up over whatever else is on
```

Each run writes `acceptance/<scenario>/` — every command's output, its exit
code, and a `SUMMARY.txt` that puts AutoNet's tables beside `ifconfig`,
`netstat -rn` and `networksetup -listnetworkserviceorder`. On macOS it also
writes `tests/fixtures/macos-real-<scenario>.json`, the first capture from real
hardware this project has ever had.

`acceptance/` is gitignored: it is evidence, and it contains MAC addresses
because the script passes `-v`. The committed fixture does not — the capture
tool strips them. It does **not** strip addresses, because those are what the
fixture is for, so read a capture before committing one taken on a network you
would rather not publish.

## The checklist

Mark each cell `ok`, `FAIL — <symptom>`, or `n/a`. Leave it blank if the run did
not happen; a blank is information.

| | Wi-Fi | Ethernet | Both | VPN |
|---|---|---|---|---|
| 1. Wi-Fi's interface reports `"kind": "wireless"` — never `"ethernet"` | | | | |
| 2. `autonet ip` returns the real LAN address (not loopback, Docker, or the tunnel) | | | | |
| 3. a default route is present, its gateway is populated, and it belongs to the interface `netstat -rn` says it does | | | | |
| 4. `autonet status` renders the same shape it does on Linux | | | | |
| 5. the 8 live tests: pass, fail, or **vacuous** (see below) | | | | |
| 6. with two same-kind links, the winner matches `-listnetworkserviceorder` | | | | |
| 7. Ethernet beats Wi-Fi **even with Wi-Fi dragged to the top of the service order** | | | | |
| 8. `utun*` classifies as `vpn`, and loses to the LAN address | | | | |

### Row 1 is the single most load-bearing claim in the milestone

macOS reports Wi-Fi as `IFT_ETHER`. The only thing that turns it into
`Wireless` is the SystemConfiguration type lookup in
[`scnetwork.rs`](../crates/autonet-platform/src/macos/scnetwork.rs), and until
this run that rested on forum evidence. If row 1 says `ethernet`, the SC lookup
returned nothing and the `ifi_type` fallback took over — a **Task 3** finding,
and a large one, because the fix is not a tweak: it means the classifier's
primary evidence source is unavailable and the layered table needs a different
second rung. Under no circumstances is the fix to look at the name `en0`.

### Row 5: passing is not the same as proving

Three of the live tests can pass while checking nothing, and only
`--nocapture`'s printed counts distinguish the two. `SUMMARY.txt` extracts them.

| test | passed vacuously when |
|---|---|
| `rtm_index_and_rta_ifp_name_the_same_interface` | it prints `cross-checked 0 route(s)` — no message carried `RTA_IFP` |
| `two_links_of_the_same_kind_are_ordered_by_the_service_order` | it prints `skipped:` — only one link of any kind was up |
| `a_temporary_address_is_a_plausible_privacy_address` | zero temporary addresses exist — legitimate with privacy extensions off |

Record those as `vacuous`, not `ok`. A vacuous pass leaves the assumption
exactly as unproven as it was before the run.

The first of those three is the sharpest detector the project has for Darwin's
four-byte sockaddr `ROUNDUP`, so it is worth arranging a run where it is *not*
vacuous. If every table comes back with zero `RTA_IFP`, say so in the closing
statement rather than counting the test as evidence.

### Row 7 needs a manual step

The script cannot reorder services. Do it by hand:

1. System Settings → Network → **⋯** → Set Service Order
2. Drag **Wi-Fi above Ethernet**, apply
3. Re-run `scripts/macos-acceptance.sh both`
4. Confirm `autonet ip` returns the **Ethernet** address, unchanged
5. Put the order back

This is Task 5's central design claim under test: a service-order rank is worth
10 points to the selector and the Ethernet/Wireless gap is 50, so the order is a
tie-breaker between same-kind links and can never overturn a category. If the
answer *does* flip to Wi-Fi, the metric scale is wrong — a **Task 5** finding.
If it flips and the metrics look right, the kind weights are being ignored,
which is a selector finding and worse.

### Row 8 is the scenario the whole backend exists for

Nothing in Tasks 3, 4 or 5 has ever seen a real `utun`. Three separate things
have to hold at once, and they are worth recording separately:

- **classification** — `utun*` is `vpn`. It reaches that through
  `IFF_POINTOPOINT`, since SystemConfiguration does not enumerate tunnels. A
  `utun` showing `other` is a **Task 3** finding.
- **routing** — if the VPN takes the default route, `autonet routes` shows the
  tunnel as its owner, matching `netstat -rn`. A default route that `netstat`
  shows and AutoNet does not is a **Task 4** finding.
- **selection** — `autonet ip` still returns the LAN address. The tunnel carries
  −300 for being a VPN and −100 for being absent from the service order. If the
  VPN wins, say which of the two penalties failed to apply.

**If no VPN is available, write "not tested" here and carry it into Milestone 2b
as a named risk.** Do not leave it looking verified.

## When something is wrong: trace it, do not patch it

The acceptance run's job is to find out what is true, not to fix it. A fix
folded into this run loses the information about which task's reasoning was
wrong — which is the part Milestone 2b needs.

| Symptom | Belongs to | First thing to look at |
|---|---|---|
| Wi-Fi shows `ethernet`; a `utun` shows `other` | **Task 3** | the SC type lookup returned nothing and `ifi_type` took over |
| a prefix with host bits set below its own length | **Task 4** | netmask truncation — `sa_len == 0` on a default route |
| a gateway that is a small integer (`0.0.0.7`) or the interface's own address | **Task 4** | the sockaddr walk is off by a slot: check `rtm_index_and_rta_ifp_name_the_same_interface` **first**, it is the most specific |
| `preferred_source` not bound to the route's own interface | **Task 4** | same walk, one slot further along (`RTA_IFA`) |
| a default route in `netstat -rn` that `autonet routes` omits | **Task 4** | `is_reportable` filtering, or netmask truncation |
| every metric `0`, or a metric that is not a multiple of 100 | **Task 5** | the interface-index → name → rank join in `snapshot()` |
| the wrong same-kind link wins | **Task 5** | AutoNet's service order against `-listnetworkserviceorder` |
| Ethernet loses to Wi-Fi | **Task 5**, then the selector | the metric scale, then the kind weights |

A real capture also joins the fixture corpus automatically — the harness
enumerates `tests/fixtures/*.json` at runtime. If adding one makes
`every_fixture_round_trips_through_serde` fail, that is a serialisation bug
(**Task 4**). If it makes `selection_does_not_depend_on_input_ordering` fail,
two candidates tie on score and the winner depends on input order (**Task 5**,
or the selector). Neither is a flake, and neither should be silenced by deleting
the fixture.

## Closing statement

Written from the filled-in table above, in three lists, with nothing rounded up.

**Hardware-verified** — things a run actually demonstrated:

- _(fill in)_

**Still synthetic-only** — things no run reached, including every scenario that
was unavailable:

- _(fill in)_

**Assumptions that turned out wrong** — fixtures or live-test assertions that
encoded a belief the hardware contradicted, and the task each traces to:

- _(fill in)_

Milestone 2b starts from this list. A claim that is not on the first list is not
verified, however reasonable it sounds.
