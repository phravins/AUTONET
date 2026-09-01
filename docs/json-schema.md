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
| `urls` | object | Present only when `--port` was given. |
| `error` | string | Present only when `selected` is `null`. |
| `candidates` | array | Present only with `-v`. See [below](#candidates). |

`urls.local` is what a browser **on this machine** would open. `urls.network` is
the point of the whole tool: the URL another device can open. They are separate
fields because conflating them is the mistake AutoNet exists to prevent.

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

With `--port`, a `url` field is added. When nothing is selectable:

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

This array is the raw material for a future `autonet doctor`, which is why it is
part of the contract rather than a debugging aid.
