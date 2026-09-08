# The JSON contract (`schema_version: 1`)

Every `--json` payload AutoNet emits carries a `schema_version`. This document
describes version 1, which is what the Python, TypeScript, Java and .NET SDKs
will bind to.

Output is **one JSON document per line**, not pretty-printed. That keeps
`autonet status --json | jq` working today and lets `autonet watch` stream
events down the same pipe later without changing the format.

## Compatibility promise

Within a schema version:

- Fields are never removed and never change type or meaning.
- New optional fields may be added. **Parsers must ignore unknown fields.**
- Enum variants may gain new members. Treat an unrecognised `kind` or `scope` as
  "something I do not know about" rather than as an error.

A change that breaks any of the above increments `schema_version`.

## `autonet status --json`

```json
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
  },
  "urls": {
    "local": "http://127.0.0.1:3000",
    "network": "http://192.168.1.101:3000"
  }
}
```

| Field | Type | Notes |
|---|---|---|
| `schema_version` | integer | Always present. |
| `platform` | string | Which backend produced this, e.g. `linux-netlink`. Diagnostic only; do not branch on it. |
| `captured_at` | integer \| null | Unix seconds at snapshot time. |
| `selected` | object \| null | `null` when nothing was selectable. |
| `urls` | object | Present only when a port is known, from `--port` or `output.default_port`. |
| `error` | string | Present only when `selected` is `null`. |
| `candidates` | array | Present only with `-v`. See [below](#candidates-with--v). |

`urls.local` is what a browser **on this machine** would open. `urls.network` is
the point of the whole tool: the URL another device can open. They are separate
fields because conflating them is the mistake AutoNet exists to prevent.

`urls.network` is also exactly what `--qr` encodes, which is why **`--qr` is
refused with `--json`** (exit 2) rather than adding a field. A `qr` key would be
a second copy of one fact that can only ever agree with the first, and a picture
has no place in a machine contract. See
[ADR 0003](adr/0003-qr-code-contents.md).

`autonet advertise --qr` encodes something different: `http://<published
name>:<port>`, the `.local` name rather than the address. That is the one place
the two disagree, and it is deliberate — under `advertise` the name is the
stable fact and the address is the one that moves, so a code scanned there
survives a handover that would invalidate a code scanned from `status`. The same
refusal applies: `advertise --qr --json` exits 2.

### Failure

```json
{
  "schema_version": 1,
  "platform": "linux-netlink",
  "captured_at": 1788177606,
  "selected": null,
  "error": "all 31 candidate address(es) were rejected; most common reason: interface is down"
}
```

Still valid JSON, still exit code `1`. A client should check `selected` for
`null` rather than assuming a non-zero exit means no output.

`error` is prose intended for a human, and its wording is **not** part of this
contract — do not parse it. It reports whichever of these is the real story:

| Situation | Wording |
|---|---|
| No interface reported any address | `no interfaces reported any addresses` |
| Nothing of the requested family is reachable | `this machine has no ipv6 address another device could reach (only link-local and loopback)` |
| `require_interface` matched, but that interface is unusable | `the requested interface has no usable addresses` |
| `exclude_interfaces` removed everything | `every interface was excluded by configuration` |
| Otherwise | `all N candidate address(es) were rejected; most common reason: …` |

The middle rows exist because the modal disqualification is not always the
honest answer. Asking for IPv6 on a network that provides none, on a machine
running Docker, would otherwise blame a dozen veth pairs for the router's DHCP.
Use `candidates` (with `-v`) when you need to reason about a failure in code.

## `autonet ip --json`

The selected address on its own, with `schema_version` merged in — the smallest
useful document.

```json
{"schema_version":1,"ip":"192.168.1.101","family":"ipv4","prefix_len":24,"scope":"private","interface":"wlo1","interface_index":3,"interface_kind":"wireless","gateway":"192.168.1.1","score":1415}
```

With a port known, from `--port` or `output.default_port`, a `url` field is added. When
nothing is selectable:

```json
{"schema_version":1,"ip":null,"error":"…"}
```

## `autonet interfaces --json`

```json
{
  "schema_version": 1,
  "platform": "linux-netlink",
  "captured_at": 1788177606,
  "interfaces": [
    {
      "name": "wlo1",
      "index": 3,
      "kind": "wireless",
      "state": "up",
      "flags": {
        "up": true,
        "running": true,
        "loopback": false,
        "broadcast": true,
        "point_to_point": false,
        "multicast": true
      },
      "mac": null,
      "mtu": 1500,
      "addresses": [
        {
          "ip": "192.168.1.101",
          "family": "ipv4",
          "prefix_len": 24,
          "scope": "private",
          "is_temporary": false
        }
      ]
    }
  ]
}
```

`mac` is `null` unless `-v` was passed. A hardware address is a durable
identifier that outlives any IP address, and AutoNet has no need to publish one
in order to answer its question, so it is withheld by default.

## `autonet routes --json`

```json
{
  "schema_version": 1,
  "platform": "linux-netlink",
  "captured_at": 1788177606,
  "routes": [
    {
      "destination": null,
      "gateway": "192.168.1.1",
      "interface_index": 3,
      "metric": 600,
      "family": "ipv4",
      "preferred_source": "192.168.1.101"
    }
  ]
}
```

`destination` is `null` for a default route and a CIDR string (`"192.168.1.0/24"`)
otherwise. Default routes are listed first.

Routes are joined to interfaces by `interface_index`, which is the kernel's own
index — the same key `interfaces[].index` uses.

## `autonet doctor --json`

```json
{
  "schema_version": 1,
  "platform": "linux-netlink",
  "os": "linux",
  "captured_at": 1788540702,
  "ok": true,
  "verdict": "warn",
  "summary": "1 warning, nothing failed, 1 not verified. AutoNet can give another device an address that reaches this machine.",
  "checks": [
    { "id": "operating_system", "label": "Operating system", "status": "pass", "detail": "linux, via linux-netlink" },
    { "id": "network_interface", "label": "Network interface", "status": "pass", "detail": "wlo1 (wireless, up), and 3 more" },
    { "id": "ipv4_address", "label": "IPv4 address", "status": "pass", "detail": "192.168.0.115/24 on wlo1" },
    { "id": "default_route", "label": "Default route", "status": "pass", "detail": "via 192.168.0.1 on wlo1" },
    { "id": "selected_address", "label": "Selected address", "status": "pass", "detail": "192.168.0.115 (private) on wlo1, which is not loopback" },
    { "id": "lan_candidate", "label": "LAN reachable", "status": "pass", "detail": "1 address another device could reach" },
    { "id": "port", "label": "Port 3000", "status": "warn", "detail": "already in use on 192.168.0.115. It is held by python3 (pid 67834)." },
    { "id": "bind_address", "label": "Bind address", "status": "unknown", "detail": "AutoNet cannot see what address your server binds. …" }
  ]
}
```

`os` is the operating system this binary was built for; `platform` is the
backend that read the machine. They differ in kind: two backends could exist for
one OS.

`captured_at` is present only when a snapshot was taken. When the operating
system could not be queried, the field is absent, `operating_system` reports
`fail` with the reason in its `detail`, and every other row is `unknown`.

**Key off `id`, never off `label` or `detail`.** `id` is part of the contract
and stable; `label` carries a value for the port row (`"Port 3000"`) and
`detail` is prose written for a person to read, and both may be reworded.

The rows are always present and always in this order. The `port` row is the one
exception: it appears only when a port is known — from `--port` or
`output.default_port` — *and* an address was selected to probe it against.

`ok` is `true` when nothing failed, which is the same condition as exit code
`0`. It is restated here so a consumer that only reads stdout does not have to
inspect the exit status.

### `status`

| Value | Meaning |
|---|---|
| `pass` | Checked, and fine. |
| `warn` | Checked, worth knowing about, not broken. |
| `fail` | Checked, and broken. |
| `unknown` | **Not checked.** AutoNet did not determine this. |

`unknown` is not a pass and not a failure. It never contributes to `verdict`,
and `verdict` is therefore one of `pass`, `warn` or `fail` only.

The `bind_address` row is always `unknown`. AutoNet cannot observe what address
another process passes to `bind()`, so that row is guidance rather than a
measurement, and reporting it as a pass would claim a check that never
happened. See
[ADR 0001](adr/0001-network-change-during-autonet-run.md).

## `autonet watch --json`

One JSON document per line, written as it happens and flushed immediately, so
`autonet watch --json | while read -r line; do …; done` works. The stream does
not end on its own; it ends when you stop it, or when the reader closes the pipe
(which is an ordinary exit `0`, not a crash).

```json
{
  "schema_version": 1,
  "change": "initial",
  "captured_at": 1788844483,
  "source": "linux-netlink-monitor",
  "previous": null,
  "current": {
    "ip": "192.168.1.18",
    "family": "ipv4",
    "prefix_len": 24,
    "scope": "private",
    "interface": "wlo1",
    "interface_index": 3,
    "interface_kind": "wireless",
    "gateway": "192.168.1.1",
    "score": 1415
  },
  "reason": null,
  "events": []
}
```

| Field | Type | Meaning |
|---|---|---|
| `change` | string | `"initial"` for the opening document, `"selection"` for every one after it. |
| `captured_at` | integer \| null | When the snapshot behind `current` was taken, in Unix seconds. |
| `source` | string | What noticed. See below. |
| `previous` | object \| null | The selection before this change. Always `null` on the opening document. |
| `current` | object \| null | The selection now. `null` when nothing is selectable — the "no network" report. |
| `reason` | string \| null | One sentence naming the most explanatory event. `null` when there are none. |
| `events` | array | Every underlying change, each tagged with its own `event` field. |

`previous` and `current` are the same object as `status --json`'s `selected`.

The first document describes the **starting state**, not a change, which is what
`change: "initial"` is for: a consumer that only wants transitions skips it, and
one that wants to render current state immediately does not have to wait for the
network to move first. It is the only document with `previous: null`, and the
only one whose `events` array is empty.

A document is written when the **selected address changes** — a different IP, or
the same IP on a different interface. Events that do not change the answer (a
route metric moving, a container interface appearing) are observed and
deliberately produce no line: `watch` reports the answer changing, not the
network twitching.

### `source`

Which mechanism noticed, named per document rather than fixed for the run:

| Value | Meaning |
|---|---|
| `linux-netlink-monitor` | A kernel netlink subscription. Sub-second. |
| `polling` | A timer. The platform has no event source, or the subscription was lost. |

**It can change mid-stream.** If an event source fails partway through, `watch`
says so once on stderr and keeps going on the timer — losing it costs latency,
not correctness — and subsequent documents say `polling`. A consumer needs this
field to know what silence means: thirty seconds of nothing is unremarkable
under a timer and worth noticing under netlink.

### `events[]`

Each element is an object with an `event` discriminator:

| `event` | Other fields |
|---|---|
| `interface_added` | `interface` |
| `interface_removed` | `interface` |
| `interface_state_changed` | `interface`, `from`, `to` |
| `address_added` | `interface`, `address` |
| `address_removed` | `interface`, `address` |
| `default_route_changed` | `family`, `from_interface`, `to_interface` — either may be `null` |

`address` is the same object as `interfaces[].addresses[]`. Interfaces are
matched between snapshots **by name, not kernel index**, so an adapter that
returns with a fresh index reports as one interface changing rather than as a
removal plus an addition.

A single handover produces several events at once. `reason` picks the most
explanatory one and words it; `events` keeps all of them.

## The state file (`autonet run --state-file`)

Not a fourth payload shape: **the state file contains the
[`autonet status --json`](#autonet-status---json) document, unchanged.** Both
are produced by one function, so they cannot drift. Everything above about
`selected`, `urls`, `error` and the enumerations applies verbatim.

The differences are in the file's *lifecycle*, not its contents, and they are
part of the contract:

| | |
|---|---|
| **Where** | The path given to `--state-file`, made absolute. Handed to the child as `AUTONET_STATE_FILE`. |
| **When it changes** | Once before the child is launched, then whenever [`autonet watch`](#autonet-watch---json) would have emitted a line. Same snapshot, same diff, same event source. |
| **Atomicity** | Written to a sibling temporary and renamed into place. A reader gets the previous complete document or the next one — never a partial write, and never an empty file. |
| **`candidates`** | Never present. `-v` is a `status` option; the file is rewritten on every network change and a candidate list would grow each write without answering the question it is read for. |
| **Absence** | AutoNet is not maintaining this file. It is removed when the command exits — for any reason, including a failure — and removed again if an update fails. |

Absence is deliberately the only "stop" signal: there is no `"stale": true`
marker to check, because a file that says it is stale is still a file a careless
reader will parse and use. The one case AutoNet cannot cover is being killed
outright (`SIGKILL`, power loss), where no cleanup runs at all; a reader that
cares should check the file's modification time.

Reading it is the same as reading `status --json`:

```sh
jq -r '.selected.ip // empty' "$AUTONET_STATE_FILE"
```

An unset `AUTONET_STATE_FILE` means the program was launched without
`--state-file`, and `AUTONET_IP` is all there is. That is the ordinary case; see
[ADR 0001](adr/0001-network-change-during-autonet-run.md) for why.

## Enumerations

### `family`

`"ipv4"` · `"ipv6"`

### `scope`

| Value | Meaning |
|---|---|
| `loopback` | `127.0.0.0/8`, `::1` |
| `link_local` | `169.254.0.0/16`, `fe80::/10` |
| `private` | RFC 1918, or an IPv6 unique local address |
| `cgnat` | `100.64.0.0/10` — carrier-grade NAT, usually not reachable inbound |
| `unique_local` | `fc00::/7` |
| `global` | A publicly routable address |
| `special` | Unspecified, multicast, documentation, benchmarking, reserved |

Only `private`, `unique_local` and `global` describe an address another device
might reach.

### `state`

`"up"` · `"down"` · `"dormant"` · `"unknown"`

`unknown` is not a synonym for down. WireGuard, `tun` devices and loopback report
no operational state at all while working perfectly, so `unknown` interfaces stay
eligible.

### `kind`

`"ethernet"` · `"wireless"` · `"loopback"` · `"bridge"` · `"container"` ·
`"virtual"` · `"vpn"` · `{"other": "<kernel link kind>"}`

Classification comes primarily from the kernel's own link kind
(`IFLA_INFO_KIND` on Linux), with names used only to disambiguate — notably
`br-<12 hex digits>`, which distinguishes a bridge Docker created from a bridge
you created and called `br0`.

## `candidates` (with `-v`)

```json
{
  "interface": "wlo1",
  "interface_index": 3,
  "interface_kind": "wireless",
  "address": { "ip": "192.168.1.101", "family": "ipv4", "prefix_len": 24, "scope": "private", "is_temporary": false },
  "score": 1415,
  "reasons": [
    { "rule": "default_route", "delta": 1000 },
    { "rule": "interface_kind", "delta": 200 }
  ],
  "disqualified": null
}
```

`disqualified` is `null` for an eligible candidate, otherwise one of:

| Value | Meaning |
|---|---|
| `interface_down` | The kernel reports the interface as down. |
| `loopback` | Reachable only from this machine. |
| `link_local` | Not routable. |
| `special_address` | Unspecified, multicast, or otherwise not a host address. |
| `family_mismatch` | A perfectly good address of the family you did not ask for. |
| `excluded_by_config` | Matched `exclude_interfaces`. |
| `not_required_interface` | `require_interface` named a different interface. |
| `synthetic_without_route` | A container or virtual interface with no route to anywhere. |

Address-level and interface-level checks are ordered, so a veth carrying only a
link-local address reports the interface-level reason. Both are true; the
broader one is reported.

This array is the raw material `autonet doctor` reads, which is why it is part
of the contract rather than a debugging aid.
