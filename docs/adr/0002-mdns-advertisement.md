# 0002 — Advertising the selected address as a `.local` name

- **Status:** Accepted
- **Date:** 2026-09-05
- **Affects:** Stage 3 (`autonet advertise`), the `[hostname]` config section,
  the QR code in `autonet status`, and M5's daemon

## Context

[ADR 0001](0001-network-change-during-autonet-run.md) made `autonet run` a
launcher rather than a supervisor. It accepted that the environment injected
into a child goes stale, on one condition, stated as its reasoning in a single
line: *"what must not go stale is the name the client resolves, not the address
the server believes it has."* It then committed the moving case to a layer that
did not exist — *"the client reaches the server by a `.local` name, and the mDNS
advertisement follows the address change"* — and explicitly deferred everything
about how: *"the crate, the hostname configuration and the advertisement's own
security analysis are separate work."*

This record is that work. Until it landed, ADR 0001's central promise was an
IOU, and `autonet doctor` was telling users about a failure mode whose remedy
had not been built.

**Evidence the system responder is not the answer**, observed on the machine
this was written on, both before and after the change:

```
$ avahi-resolve -n real0.local
real0.local     172.17.0.1        # the Docker bridge
$ autonet ip
192.168.1.18                      # what a phone can actually reach
```

Avahi publishes every address on every interface under `<hostname>.local` and
leaves the client to pick. On a laptop running Docker that is frequently the
wrong answer, and it is the exact mistake — *"a container bridge is not the
address anyone can reach"* — that the whole selection engine exists to avoid.
The question this record answers is therefore not "how do we get mDNS", which
every desktop already has, but "how do we publish **one** address, the chosen
one, and keep it correct as it moves."

## Decision

**A standalone `autonet advertise` command, using the pure-Rust `mdns-sd`
crate, publishing `<hostname>-autonet.local`, off until a config file says
otherwise, and re-announcing through `autonet watch`'s existing pipeline.**

Five sub-decisions, each with its alternative.

### 1. A separate verb, not a flag on `run`

`autonet advertise` joins `status`, `ip`, `interfaces`, `routes`, `run`,
`watch` and `doctor` as another verb naming what AutoNet does.

**Rejected: `autonet run --advertise`.** ADR 0001 commits `run` to spawn, wait,
exit with the child's code, and intervene in nothing. A flag that made it hold
an mDNS responder for the child's lifetime would make `run` two things and put
a footnote on that commitment. More importantly,
[`architecture.md`](../architecture.md#security-posture) requires that *"any
feature that makes an application LAN-reachable will be explicit"* — and a
separate verb is the most explicit shape the CLI has. A flag on an existing
command is how a capability arrives by accident.

**Rejected: both.** Two surfaces for one capability doubles the consent
question and the documentation for no capability gained.

### 2. `mdns-sd`, over the system responder and three other crates

| Candidate | Why not |
|---|---|
| Register with Avahi / `mDNSResponder` over D-Bus or DNSSD | The most "native" answer, and the wrong one. Handing our record to the system daemon means it is published beside that daemon's own `<hostname>.local` records — the ones already pointing at the Docker bridge — under its policy, not ours. It also needs a system daemon present, which is not true on Windows and not guaranteed on a minimal Linux box. |
| `zeroconf`, `astro-dnssd` | Bind `avahi-sys` / `bonjour-sys`. Needs Avahi headers on Linux and the Bonjour SDK on Windows, breaking both the pure-Rust build and [`flake.nix`](../../flake.nix)'s network-free sandbox. |
| `libmdns`, `searchlight` | Require tokio in the CLI, which has none. The netlink monitor's runtime is private to `autonet-platform` on Linux and deliberately does not leak upward. |
| **`mdns-sd` 0.21** | **Chosen.** Self-contained pure-Rust responder, no system daemon, MSRV 1.71 under our 1.82. `default-features = false` drops async and logging, leaving three new transitive crates. One `register()` publishes the A/AAAA record *and* SRV, PTR and TXT, and re-registering the same fullname re-announces — which is exactly the update path this record needs. |

It lives in `autonet-cli`, not `autonet-platform`. It is cross-platform pure
Rust and introduces no `#[cfg(target_os)]` above the platform crate, so
architecture.md's second rule holds. This is the `ctrlc` precedent, and
`hostname` — used for the default name — follows it for the same reason.

**Cross-platform maturity is not even, and pretending otherwise would be the
dishonest part of this record.**

- **Linux** is the strongest case and the only one exercised. Avahi already
  holds UDP 5353; `mdns-sd` sets `SO_REUSEADDR`/`SO_REUSEPORT` and shares it.
  Two responders answering for *different* names is legal under RFC 6762, which
  is what makes sub-decision 3 load-bearing rather than cosmetic.
- **macOS** should behave the same way — `mDNSResponder` always runs and always
  owns `<hostname>.local` — but `SO_REUSEPORT` semantics differ from Linux's and
  no Mac was available. Reasoned about, not tested.
- **Windows** is the weakest. `SO_REUSEPORT` does not exist and the fallback to
  `SO_REUSEADDR` has different semantics; Windows 10+ ships its own `.local`
  resolver; Bonjour may or may not be installed; and interface enumeration plus
  multicast joins are historically the flakiest path there. **If one platform
  eventually needs a different implementation, this is the one** — and the place
  for it would be a `#[cfg]`-gated backend inside `autonet-platform`, never in
  the CLI.

### 3. `<hostname>-autonet.local`, not `<hostname>.local`

The obvious name is the plain one, and it is unavailable. Avahi owns
`real0.local` on most Linux desktops and `mDNSResponder` always owns it on
macOS. Publishing the same name means RFC 6762 conflict resolution either
renames our record at runtime — to `real0-2.local`, which nobody can predict —
or leaves two records racing, so a client gets the system responder's
every-address answer about half the time. That is the failure this command
exists to fix; it must not reintroduce it.

The suffix costs discoverability: `real0-autonet.local` is not a name anyone
would guess. Mitigated, not eliminated, by `advertise` printing the full name
and URL as its first output, by `[hostname] name` overriding it, and later by
the QR code, which removes the need to type any name at all.

**Rejected: try the plain name and fall back on conflict.** It would make the
published name nondeterministic — a user could not know it before running the
command — and it would depend on `mdns-sd`'s host-record conflict detection
behaving a particular way on three platforms, only one of which is testable
here.

### 4. Off by default, and the command refuses rather than assuming consent

`[hostname] enabled` defaults to `false`. `autonet advertise` on a machine that
has not set it exits 2 with the config path and the two lines to add.

**Rejected: treat typing the command as the opt-in.** It is friendlier, and it
makes the default-off setting protect nothing: installing AutoNet would mean one
word at a prompt puts a name for this machine on the LAN. It also leaves every
future consumer — `status --qr`, a possible `run --advertise`, M5's daemon —
to re-decide for itself what counts as consent, instead of consulting one
switch. The cost is one refusal per machine, paid once; the refusal message is
the documentation.

There is deliberately **no `AUTONET_ADVERTISE`** environment variable and no
flag that turns it on. Consent to publish belongs in a file the user wrote, not
in an environment a parent process can set.

### 5. Re-advertisement reuses the watch pipeline; it does not add one

`advertise` is `watch::observe` with a responder on the callback instead of a
printer — the same `snapshot()` → `diff()` → `affects_selection()` → `select()`
→ `differs()` path, with the netlink event source underneath it on Linux. There
is no second change-detection mechanism to drift out of step with the first, and
because `observe` already invokes its callback once at startup, there is a
single registration code path rather than a startup path plus an update path.

When nothing is selectable the record is **withdrawn**, not left standing. A
name pointing at an address the machine no longer holds is precisely the
staleness ADR 0001 pushed onto this layer to solve.

`enable_addr_auto()` — mdns-sd's convenience that fills in every host address
automatically — is **never called**, because it is the Avahi behaviour this
record rejects, reimplemented inside our own responder.

## Security analysis

`architecture.md`'s posture requires this, and the honest answer has two halves.

**This is not the same category as opening a firewall port.** Advertisement
opens no port for the application, changes no firewall rule, and makes nothing
reachable that was not reachable already. Anything that could reach
`192.168.1.18:3000` before can reach it after. The record supplies a *name* for
an address the machine was already answering on, and AutoNet never asks the
operating system to permit anything.

**But it is not nothing, and three things are genuinely new.**

1. **AutoNet binds UDP 5353** and joins `224.0.0.251` / `ff02::fb`. That is a
   LAN-visible listening socket held by AutoNet, answering queries, for as long
   as the command runs. Where a firewall blocks 5353 it binds and is simply
   never asked anything — it does not punch a hole.
2. **It announces, unprompted, to the whole link.** Name, selected address,
   port and service type go to every device on the segment, and again each time
   the address moves. Before this, a passive observer had to see traffic from
   this machine. This is what the command is *for*, but it belongs in the same
   category as the posture's rule that MAC addresses are withheld without `-v`:
   information published because it was asked for, not by default.
3. **The name derives from the machine's hostname**, which is often a person's
   name or an employer's asset tag. `real0-autonet.local` on a café network
   tells the room something. Mitigated by `[hostname] name` and by the default
   being off; not eliminated.

What is deliberately *not* published: no TXT properties at all, so the interface
name, the interface kind, the score, the gateway and the MAC address stay off
the wire. The address and the port are the answer to the question; the rest is
AutoNet's own bookkeeping.

What this does not do: no browsing or discovery of other machines — this
advertises only, and collects nothing; no writes to the system responder's
configuration; no root; no persistent state; and the record is withdrawn on
exit rather than left to age out.

mDNS is unauthenticated by design. Any device on the link can publish a
conflicting record or answer a query first. **AutoNet does not defend against a
hostile local network and does not claim to** — a `.local` name is a
convenience on a network you already trust, and anything stronger belongs to
TLS, not to name resolution.

## Consequences

**Good.**

- ADR 0001's central promise is now kept. The address a client resolves follows
  the network; only the environment inside an already-running process does not,
  which is what that record said and documented.
- One address is published, and it is the selected one. On this machine the
  published name resolves to `192.168.1.18` while the system's own
  `real0.local` still resolves to `172.17.0.1`.
- No new change-detection code. `advertise` inherits netlink's sub-second
  reaction on Linux and the poll everywhere else, for free and without a second
  implementation.
- Nothing was added to `autonet-platform`, and no `#[cfg(target_os)]` entered
  the CLI.

**Bad, and accepted.**

- **The published name is not the one users expect.** `real0-autonet.local`
  must be read off the terminal or a QR code; `real0.local` will keep resolving
  to whatever the system responder thinks, including the Docker bridge. AutoNet
  cannot fix the system responder without becoming one.
- **Windows is unverified and is the weakest platform.** The port-sharing
  semantics differ and no Windows machine was available. This may need a
  platform-specific answer later.
- **The record exists only while the command runs.** There is no daemon and no
  autostart, so `autonet advertise` is one more thing to remember to leave
  running. That is a deliberate consequence of not building M5 early.
- **The machine's hostname goes on the wire.** See the analysis above. The
  toggle and the override are the mitigations, and neither removes the fact.
- **`[hostname]` breaks forward compatibility of the config file.** `Config` is
  `deny_unknown_fields` with no version stamp, so a file using this section is
  a hard parse error on an older binary. Documented in architecture.md's
  "Changing the configuration file"; the cost is paid by every future section.
- **Five new crates in the lock**, three of them transitive to `mdns-sd`. The
  dependency posture was previously seven direct crates; this is the largest
  single addition so far.

**Neutral.**

- M5's daemon is unaffected. If it eventually advertises, it consults the same
  `hostname.enabled` switch, which is part of why that switch exists.

## What this record does not decide

- Whether AutoNet ever *browses* mDNS. This publishes only.
- Whether the daemon should hold the advertisement instead of a foreground
  command. That is M5's question, and reversing this record's foreground shape
  would need its own number.
- The QR code's contents. It will read `[hostname]` to decide whether to encode
  the `.local` name or the raw address, but that is a rendering decision, not
  this one.
