# Milestone 2b — Windows hardware acceptance

Milestone 2b built a Windows backend without a Windows machine. Every claim in
it rests on Microsoft's headers, on the `windows-latest` CI runner (one virtual
NIC, no Wi-Fi radio, no VPN, no dock), and on three fixtures written by the same
person who wrote the code they test — which means a wrong belief produces a
fixture that agrees with it.

Four things stay open, and no amount of CI or synthetic testing can close them:

1. Is `MIB_IPFORWARD_ROW2.Metric` meaningful, or present-but-uninformative like
   macOS's `rmx_hopcount`? **Documented only.**
2. Does the interface-metric tie-break actually distinguish two real adapters,
   and does the margin survive real-world values?
3. Does a real VPN adapter classify as `Vpn`? **No tunnel adapter has ever
   touched this code.**
4. Does the LUID join hold on a genuine dual-stack adapter, rather than on
   hand-typed synthetic indices?

This document is where that stops being theory. Run
[`scripts/windows-acceptance.ps1`](../scripts/windows-acceptance.ps1) under each
network condition, fill in the columns, and write the closing statement at the
bottom. **An unrun row is blank, not a tick.**

## Before starting

- The tree is at a **pushed, CI-green commit**. A failure should be
  unambiguously the hardware, not an unbuilt change.
- The machine has a Rust toolchain (`rustup show` resolves from
  `rust-toolchain.toml`) and the MSVC build tools.
- You know which physical link is which. `Get-NetAdapter` maps a name like
  "Wi-Fi" to its `ifIndex` and description. AutoNet must reach the same answer
  **without** looking at either — `wintype.rs` never reads `Description`,
  `Alias` or the adapter GUID, because matching `"TAP-"` or `"WireGuard"`
  classifies today's VPNs and misses tomorrow's. That is the point of the whole
  exercise, so use those names for your own bookkeeping and never as evidence
  that AutoNet got it right.

If PowerShell refuses to run an unsigned script:

```
powershell -ExecutionPolicy Bypass -File scripts\windows-acceptance.ps1 wifi
```

## Running it

```
scripts\windows-acceptance.ps1 wifi       # Wi-Fi associated, nothing else up
scripts\windows-acceptance.ps1 ethernet   # a wire, Wi-Fi off
scripts\windows-acceptance.ps1 both       # both up at once
scripts\windows-acceptance.ps1 vpn        # a tunnel up over whatever else is on
```

Each run writes `acceptance\<scenario>\` — every command's output, its exit
code, and a `SUMMARY.txt` that puts AutoNet's tables beside `Get-NetRoute`,
`netsh interface ipv4 show interfaces` and `route print`. It also writes
`tests\fixtures\windows-real-<scenario>.json`, the first capture from real
Windows hardware this project has ever had.

Two files have no macOS counterpart and are the reason this script is more than
a transcription:

- **`50-metric-crosscheck.txt`** — every route's `RouteMetric`,
  `InterfaceMetric`, their sum, and what AutoNet reported, in one table with a
  verdict. **This is the only thing that can answer open question 1.**
- **`51-kind-crosscheck.txt`** — every adapter's `InterfaceType`,
  `NdisPhysicalMedium`, `MediaType`, `HardwareInterface` and `Virtual` beside
  AutoNet's `kind`, plus the per-family `InterfaceMetric`. This is the evidence
  `wintype::classify` actually consumed, so a misclassification lands on a named
  rung of the ladder instead of needing a debugger.

`acceptance\` is gitignored: it is evidence, and it contains MAC addresses
because the script passes `-v`. The committed fixture does not — the capture
tool strips them. It does **not** strip addresses, because those are what the
fixture is for, so read a capture before committing one taken on a network you
would rather not publish.

## The checklist

Mark each cell with one of:

| Value | Means |
|---|---|
| `ok` | ran, and checked something |
| `FAIL — <symptom>` | ran, and the answer was wrong |
| `vacuous` | ran, passed, and checked nothing — see row 4 |
| `n/a — <condition> absent` | the machine could not present the condition |
| _(blank)_ | the run did not happen |

The last two are different facts and must not collapse into each other. A blank
means nobody looked; an `n/a` means somebody looked and the hardware was not
there. **A `skipped:` line in the live-test output is an `n/a`, never an `ok`.**

| | Wi-Fi | Ethernet | Both | VPN |
|---|---|---|---|---|
| 1. Wi-Fi reports `"kind": "wireless"` — never `ethernet`, never `other` | | | | |
| 2. `autonet ip` returns the real LAN address (not loopback, Hyper-V, WSL, or the tunnel) | | | | |
| 3. `autonet status` renders the same shape it does on Linux and macOS | | | | |
| 4. the 15 live tests: per test, not in aggregate | | | | |
| 5. every route's metric == `RouteMetric + InterfaceMetric` — **open question 1** | | | | |
| 6. the lower interface metric wins between same-kind links, and by what margin — **open question 2** | | | | |
| 7. a dual-stack adapter's v4 and v6 routes carry one `interface_index` — **open question 4** | | | | |
| 8. an on-link route reports `gateway: null`, not `0.0.0.0`, and still scores the default-route bonus | | | | |
| 9. the VPN adapter classifies `vpn` — not `ethernet`, not `virtual-ethernet` — **open question 3** | | | | |
| 10. `autonet ip` still returns the LAN address with the VPN up | | | | |

### Row 1 is cheap here, and that itself is worth confirming

macOS reports Wi-Fi as `IFT_ETHER` and needed a SystemConfiguration lookup to
recover the truth. Windows does not have that problem: NDIS reports
`IF_TYPE_IEEE80211` (71) for an 802.11 miniport, which is why
[`wintype.rs`](../crates/autonet-platform/src/wintype.rs) says plainly that
**WlanAPI is not needed and stays disabled**.

If row 1 comes back `ethernet` anyway, that reasoning is wrong and WlanAPI is
back on the table — a **Task 3** finding, and a large one. Check
`51-kind-crosscheck.txt`'s `IfType` and `NdisPhys` columns first: they say
whether Windows reported 71 and AutoNet ignored it, or Windows never reported it
at all. Under no circumstances is the fix to look at the adapter description.

### Row 4: the 15 live tests, and which can pass without proving anything

Eight tests are shared with Linux and macOS; seven are Windows-gated and **have
never executed anywhere**. `--nocapture` is not optional: several of them skip
themselves rather than assume a configuration, and the printed line is the only
thing that distinguishes "verified" from "had nothing to look at".

| Windows-gated test | passes vacuously / skips when |
|---|---|
| `the_loopback_adapter_is_classified_and_owns_no_reported_route` | never fully — the loopback assertion always fires; the multicast and loopback-route halves are vacuous on a machine reporting neither |
| `a_dual_stack_adapter_keeps_both_route_families_on_one_interface` | prints `skipped:` — no adapter has both an IPv6 address and IPv4 routes |
| `a_default_routes_next_hop_is_on_the_interface_it_names` | no IPv4 default route sits on a broadcast, non-loopback interface holding an IPv4 address |
| `an_on_link_route_reports_no_gateway_rather_than_the_unspecified_address` | prints `skipped: this machine reports no routes at all` |
| `every_route_metric_includes_its_interfaces_own_metric` | the snapshot has no routes on an up, non-loopback interface |
| `repeated_snapshots_select_the_same_interface` | prints `skipped:` — the network changed between the two snapshots |
| `two_links_of_the_same_kind_are_ordered_by_the_interface_metric` | prints `skipped:` — only one link of any kind is up. **This is the only test that can demonstrate open question 2.** |

One shared test also prints rather than asserting on Windows:
`a_default_route_normalises_and_names_a_source_bound_to_its_own_interface` emits
`note: on-link default route on ...` when the default route has no next hop.
Task 6 narrowed it deliberately — Windows reports a legitimately gateway-less
route as `None`, so demanding a gateway would fail a correct machine — but the
consequence is that the note, not a pass, is the information. Record it.

Record each of these as `vacuous` or `n/a`, not `ok`. A vacuous pass leaves the
assumption exactly as unproven as it was before the run.

### Row 5 settles a question no test can

`Route.metric` is already `winroute::effective_metric(row.Metric, interface
metric)` — a sum — and the model carries no separate interface-metric field.
Both candidate readings of `MIB_IPFORWARD_ROW2.Metric` produce a plausible
number, so no assertion over the sum can separate them. Task 6 said so plainly
rather than writing a weaker test and calling it coverage.

`50-metric-crosscheck.txt` separates them by arithmetic:

| AutoNet's metric equals | Reading | Finding |
|---|---|---|
| `RouteMetric + InterfaceMetric` | `Metric` is an offset, as Task 4 assumed | the assumption holds |
| `RouteMetric` alone | the interface metric never applied | **Task 4** — the LUID join, or `link.metric(family)` |
| `InterfaceMetric` alone | `row.Metric` is inert — `rmx_hopcount` again | **Task 4** — `winroute::effective_metric` is summing a constant |
| none of the three | something else entirely | paste the table verbatim |

A **split** between `sum` and `route` is its own finding: the join reached some
adapters and missed others, which is worse than a uniform failure because the
synthetic fixtures would never show it.

[`windows/route.rs:91`](../crates/autonet-platform/src/windows/route.rs#L91) is
the line under test.

### Row 6: the margin is probably worth **zero**, and that is checkable

[`select.rs:440`](../crates/autonet-core/src/select.rs#L440) is
`metric.min(1000) / 10` — **integer division**. Two adapters at metrics 25 and
28 both floor to `2` and receive an identical penalty. The "~3-point margin"
recorded from synthetic testing therefore very likely moves the score by **0
points, not 3**: the tie-break does nothing unless the two metrics straddle a
multiple of ten.

Pre-registered, to be confirmed or refuted by the numbers in
`51-kind-crosscheck.txt`:

- Windows computes automatic metrics from link speed, so a gigabit wire should
  land near 5–10 and a Wi-Fi radio near 30–50. A gap that size survives the
  divisor (3–4 points) and the tie-break works.
- Two links of the *same* kind and similar speed land within a few points of each
  other, the divisor eats the difference entirely, and the tie-break contributes
  nothing — the winner then falls to whatever `select` reaches next.

Ethernet-versus-Wi-Fi is decided long before this, by `KIND_ETHERNET` 250 against
`KIND_WIRELESS` 200. Row 6 only bites between two links of the same kind, which
is exactly what `two_links_of_the_same_kind_are_ordered_by_the_interface_metric`
covers, and only on a machine that has the hardware for it.

**Record the actual numbers, not just which adapter won.** The real-world size
of the margin is precisely what is unconfirmed. If the gap floors to zero, that
is a **Task 5 / selector** finding about `METRIC_DIVISOR` — the same finding on
Linux — not a Windows backend bug.

### Row 7 is the most serious possible failure in this task

A v4/v6 `interface_index` mismatch on a genuine dual-stack adapter is **the exact
silent-failure mode Task 4 was written to prevent**. Task 4 replaced a raw
`IfIndex`/`Ipv6IfIndex` join with a LUID join for this reason and no other. A
mismatch means the join is wrong against Windows-assigned indices — and every
synthetic fixture agreed with the bug, because the indices in them were typed by
hand.

Check it directly:

1. In `27-autonet-routes.json`, pick an interface index that appears with both
   `"family": "ipv4"` and `"family": "ipv6"`.
2. Confirm that index names one adapter in `25-autonet-interfaces.json`, and
   that the adapter holds addresses in both families.
3. Cross-check against `13-os-get-netroute.txt`: Windows' own `InterfaceIndex`
   for the same destinations.

If the machine has no dual-stack adapter, this is `n/a — no dual-stack adapter`
in every column and it carries into the closing statement as **still open**. Do
not enable IPv6 on a tunnel and call it dual-stack; the question is about one
physical adapter carrying both families.

### Row 9: what a real VPN can come back as

Nothing in Tasks 2, 3 or 4 has ever seen a tunnel adapter. Four outcomes are
reachable through `wintype::classify`, and they are not equally good. Written
down before the run so the result cannot be rationalised after it:

| AutoNet says | Reached via | Kind score | Verdict |
|---|---|---|---|
| `vpn` | `IfType` 131/23, a declared `TunnelType`, or `MediaType == TUNNEL` | −300 | **PASS** — expected for the Windows built-in VPN, IKEv2, SSTP |
| `vpn` | Ethernet family + `AccessType == POINT_TO_POINT` | −300 | **PASS** — the `IFF_POINTOPOINT` analogue; expected for WireGuard and OpenVPN in TUN mode |
| `{"other": "virtual-ethernet"}` | Ethernet family + `HardwareInterface == False` | **0** | **FAIL — Task 3 finding** |
| `ethernet` | Ethernet family, NDIS join missed or the hardware bit distrusted | **+250** | **FAIL — Task 3, and the worst case** |

The third row deserves its reasoning stated, because Task 3 chose it
deliberately. A TAP-mode adapter genuinely is a virtual Ethernet at the kernel
level, and `Other("virtual-ethernet")` avoids both the +250 that would let it
beat a real NIC and the −800 that would break Hyper-V hosts whose only
connectivity runs through a vEthernet switch.

But [`select.rs:407`](../crates/autonet-core/src/select.rs#L407) scores
`InterfaceKind::Other(_)` at **zero** — not the −300 VPN penalty, not the −800
synthetic one. So a TAP VPN that has taken the default route collects the full
`DEFAULT_ROUTE_SAME_FAMILY` 1000 while the displaced LAN link drops to
`DEFAULT_ROUTE_OTHER_FAMILY` 400 or to nothing, and `autonet ip` returns the
tunnel address. That is row 10 failing, with a concrete selector consequence,
which is why it is recorded as a **FAIL** rather than as documented behaviour
working as intended.

`51-kind-crosscheck.txt`'s `HwIf` and `Virt` columns are what separate the third
row from the fourth: whether `HardwareInterface` was read as `False`, or
distrusted entirely by `iftable::trust_hardware_bit` so the classifier fell back
to `IfType` alone.

Three things have to hold at once and are worth recording separately:

- **classification** — the adapter is `vpn` (row 9).
- **routing** — if the VPN took the default route, `autonet routes --json` shows
  the tunnel as its owner, matching `Get-NetRoute`. A default route Windows
  shows and AutoNet does not is a **Task 4** finding.
- **selection** — `autonet ip` still returns the LAN address (row 10). If the
  VPN wins, `21-autonet-status.json`'s per-candidate `reasons` say which weight
  failed to apply; name it.

**If no VPN is available, write "not tested" here and carry it into the closing
statement as a named risk.** Do not leave it looking verified — this is the
single hardest thing to verify on every platform so far, and
[`milestone-2a-acceptance.md`](milestone-2a-acceptance.md) already carries the
same instruction unresolved for macOS.

## When something is wrong: trace it, do not patch it

The acceptance run's job is to find out what is true, not to fix it. A fix folded
into this run loses the information about which task's reasoning was wrong —
which is the part that matters. Trace the symptom to the task it belongs to,
report observed versus expected, and leave the fix to a follow-up on that task.

| Symptom | Belongs to | First thing to look at |
|---|---|---|
| Wi-Fi shows `ethernet`, or a real NIC shows `other` | **Task 3** | `wintype::classify`'s rungs against `51-kind-crosscheck.txt` |
| a VPN shows `ethernet` or `virtual-ethernet` | **Task 3** | `AccessType` and the `HardwareInterface` bit; `iftable::trust_hardware_bit` |
| an adapter or address `ipconfig /all` shows and AutoNet omits | **Task 2** | `GetAdaptersAddresses` flags, then the `winparse` sockaddr walk |
| an address with the wrong prefix length or family | **Task 2** | `winparse::sockaddr_ip` and the `OnLinkPrefixLength` read |
| v4 and v6 routes on one adapter carry different `interface_index` | **Task 4** | the LUID join — `windows/route.rs:84-90`, `winroute::route_index` |
| a gateway of `0.0.0.0` or `::` instead of `null` | **Task 4** | `winroute::gateway`'s unspecified mapping |
| every metric `0` | **Task 4** | `link.map_or(0, …)` at `windows/route.rs:91` — the join missed the adapter |
| metric != `RouteMetric + InterfaceMetric` | **Task 4** | `winroute::effective_metric`, against `50-metric-crosscheck.txt`'s verdict |
| a route `route print` shows and AutoNet omits | **Task 4** | `winroute::is_reportable` — the loopback and multicast filter |
| the wrong same-kind link wins | **Task 4**, then the selector | the metrics in `51-kind-crosscheck.txt`, then `METRIC_DIVISOR` |
| `autonet ip` returns the tunnel | **the selector** | `21-autonet-status.json`'s `reasons`: which of `KIND_VPN` / `KIND_SYNTHETIC` never fired |

A real capture also joins the fixture corpus automatically — the harness
enumerates `tests/fixtures/*.json` at runtime. If adding one makes
`every_fixture_round_trips_through_serde` fail, that is a serialisation bug
(**Task 4**). If it makes `selection_does_not_depend_on_input_ordering` fail, two
candidates tie on score and the winner depends on input order (the selector, or
**Task 4**'s metric). Neither is a flake, and neither should be silenced by
deleting the fixture.

## Closing statement

Written from the filled-in table above, in four lists, with nothing rounded up.

**Hardware-verified** — things a run actually demonstrated:

- _(fill in)_

**Still synthetic-only** — things no run reached, including every scenario that
was unavailable:

- _(fill in)_

**Assumptions that turned out wrong** — fixtures or live-test assertions that
encoded a belief the hardware contradicted, and the task each traces to:

- _(fill in)_

**The four open questions.** Each one gets `resolved:` with the observed data, or
`still open:` naming the condition the machine could not present. There is no
third state, and "looks fine" is not an answer.

1. Is `MIB_IPFORWARD_ROW2.Metric` meaningful? — _(fill in)_
2. Does the interface-metric tie-break separate two real adapters, and by what
   margin? — _(fill in)_
3. Does a real VPN adapter classify as `Vpn`? — _(fill in)_
4. Does the LUID join hold on a genuine dual-stack adapter? — _(fill in)_

Milestone 2b closes on this list. Until it is filled in, what the milestone can
honestly claim is: **implemented on three platforms, CI-verified on single-NIC
VMs, and none of the four questions answered.** A claim that is not on the first
list is not verified, however reasonable it sounds.
