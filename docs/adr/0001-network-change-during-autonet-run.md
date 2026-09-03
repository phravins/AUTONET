# 0001 — What happens when the network changes mid-run

- **Status:** Accepted
- **Date:** 2026-09-03
- **Affects:** M3 (`autonet run`), M4 (`autonet watch`), M5 (daemon), and the
  later mDNS work

## Context

`autonet run` (M3) injects `AUTONET_IP`, `AUTONET_HOST` and `AUTONET_URL` into a
child process and starts it. Environment variables are copied into the child at
`execve` time and cannot be changed afterwards by the parent — that is an
operating-system property, not an AutoNet limitation. So the moment the selected
address changes while the child is still running, the child is holding a value
that is no longer true.

This is not a rare edge case. It is the ordinary path: a laptop that suspends and
wakes on a different network, a Wi-Fi card that roams between access points, an
Ethernet cable going in, a VPN coming up. Stage 3's entire premise is that the
address moves.

Four responses have been proposed. This record picks one, because the choice is
an input to M3's design rather than a detail that can be settled at M4: whether
`autonet run` is a **supervisor** (it owns the child's lifetime and may end it)
or a **launcher** (it starts the child and never intervenes) determines its
process model, its signal handling, its exit-code semantics, and its dependency
set. That fork has to be resolved before the first line of M3 is written.

### What the codebase already fixes

These are constraints, not preferences, and each option is measured against them.

**The provider trait is frozen and synchronous.**
[`NetworkProvider`](../../crates/autonet-platform/src/lib.rs#L87) is `snapshot()`
plus `platform_name()`, with no subscription or callback method. Detecting a
change therefore means polling `snapshot()` and diffing — in *all four* options
equally. The trait constrains how a change is noticed; it does not favour any
answer to what is done about it. It is named here so a later reader does not
mistake it for the reason behind the decision.

**There is no async runtime.** The workspace dependencies are `anyhow`, `clap`,
`owo-colors`, `serde`, `serde_json`, `thiserror` and `toml`. No tokio, no hyper,
no IPC crate. Any option needing a server is a step change in the dependency
posture, not an increment.

**There is no signal handling.** `main() -> ExitCode` in
[`main.rs`](../../crates/autonet-cli/src/main.rs) dispatches a command and
returns. No `ctrlc`, no `signal-hook`. An option that supervises a child has to
build forwarding from nothing — and on Windows there is no `SIGTERM`:
`TerminateProcess` is ungraceful by construction, so a supervisor cannot offer
the same shutdown semantics across the three platforms AutoNet targets.

**The security posture binds two things.** From
[`architecture.md`](../architecture.md#security-posture): `autonet run` passes
arguments to the OS as an argument vector, never a shell string, unless a shell
is explicitly requested; and *"nothing opens a firewall port or exposes a service
as a side effect."*

**M3's own goal uses the word "unmodified"** — *"run an existing app unmodified
with `AUTONET_IP`, `AUTONET_HOST` and `AUTONET_URL` injected"*
([`README.md`](../../README.md)). Two of the four options require the app to
cooperate, which contradicts that word directly. The contradiction is real and is
confronted below rather than smoothed over.

## Options considered

### Option 1 — Restart the child process

`autonet run` supervises the child. On a selected-address change it terminates
the child and respawns it with the new environment.

**Cost.** Process-group management, signal forwarding, and a graceful-stop path
that cannot exist on Windows. Exit-code semantics become ambiguous: when the
child exits, `run` cannot distinguish a crash from the termination it caused
itself, so `$?` stops meaning what it means for every other command.

**Objections.**

- *It is destructive to state the tool cannot see.* Restarting drops in-flight
  requests, open WebSocket connections and in-memory sessions. For a hot-reloading
  dev server that is often survivable; for anything holding a connection or a
  migration it is not, and `autonet run` cannot tell which one it launched.
- *It nests two supervisors.* `autonet run npm run dev` puts AutoNet above a
  process that is already a supervisor watching files. A network event would then
  tear down the inner watcher and its port binding for a reason having nothing to
  do with the code.
- *It converts network flapping into restart storms.* Wi-Fi roaming between
  access points can change the selected address several times in seconds. A
  restart is far slower than the event that triggers it, so the child spends its
  life being killed. Debouncing helps and does not fix it.
- *It races the port.* The child's listening socket may still be in `TIME_WAIT`
  when the replacement starts, so the restart can simply fail to bind — a failure
  mode that appears only under the conditions the feature exists to handle.

### Option 2 — Write a file the app re-reads

`autonet run` writes the current address to a file and rewrites it on change
(atomically: write a temporary file, then rename). The app watches or re-reads
that file. In practice this means injecting one more variable,
`AUTONET_STATE_FILE`, pointing at the path.

**Cost.** Small and well understood. Atomic replacement and a path convention;
no supervision, no signals, no new dependencies.

**Objection.** It requires the app to cooperate. An app that re-reads a file is,
by definition, an app that was modified for AutoNet — the one thing M3 says it
will not require. As the *sole* answer it does not answer the question, because
the app it was asked about is the one that does nothing.

This option's cost is low enough, and its shape additive enough, that it returns
below as the designated extension rather than as a rejected alternative.

### Option 3 — The app queries a daemon

A background daemon holds current state; the app asks it over a Unix socket or
named pipe. This is M5.

**Cost.** The largest of the four by a wide margin: a daemon, a wire protocol, a
client, a lifetime and supervision story for the daemon itself, and — given the
current dependency set — most likely an async runtime. It also inverts the
milestone order, making M3's answer to "what happens when the network changes"
depend on work scheduled two milestones later.

**Objection.** It carries the same cooperation requirement as option 2 — the app
must poll or subscribe — at roughly twenty times the cost. Everything option 3
delivers for freshness, option 2 delivers with a file. The daemon is well
justified by *other* requirements (M6's SDKs need a stable local API); it is not
justified by this one.

### Option 4 — `autonet run` is launch-time-only; mDNS carries the moving case

The injected environment is a snapshot taken at spawn and documented as such.
`autonet run` starts the child, waits, and exits with the child's exit code. It
never kills or restarts it. The moving case is solved one layer up: the client
reaches the server by a `.local` name, and the mDNS advertisement follows the
address change.

**Cost.** Near zero. `run` stays a spawn-with-env launcher; `watch` stays
read-only.

**Objections.** Two, both real, both accepted below as consequences: an app that
*printed* its URL at startup goes on displaying a stale one, and an app that
*bound* to a specific address rather than `0.0.0.0` is genuinely broken by the
move and cannot be rescued by anything short of restarting it.

## Decision

**Option 4.** `autonet run` is a launcher, not a supervisor.

The reasoning in one line: **what must not go stale is the name the client
resolves, not the address the server believes it has.**

A dev server bound to `0.0.0.0` does not consult `AUTONET_IP` to decide what it
answers on. It answers on whatever address the interface currently holds.
`AUTONET_IP` exists so an app can *print a useful URL* and so tooling can know
what was selected — not to establish reachability. Reachability is established by
the kernel's binding, and it survives an address change on its own.

Stage 3's stated goal is that the phone connects by name and never learns an IP
at all. If the phone never learns an IP, a stale `AUTONET_IP` inside the server's
environment is not the failure that matters. The failure that matters is the
*name* resolving to an address that has moved, and that is precisely what mDNS
re-advertisement fixes.

Two supporting reasons. It is the only option whose cost is proportional to the
current dependency posture — no async runtime, no signal handling, no daemon. And
it is the only one that does not contradict M3's own "unmodified", because it
asks nothing of the application.

### What this commits

1. `autonet run` spawns the child with an argument vector, injects the
   environment once, waits, and exits with the child's exit code. It does not
   terminate or restart the child under any circumstance.
2. The injected variables are documented as **a snapshot taken at launch**, not
   as a live value. The documentation says so plainly rather than leaving the
   reader to discover it.
3. `autonet watch` (M4) is **read-only**. It observes and prints; it signals
   nothing and restarts nothing. This settles Stage 3 Task A's open question in
   the direction that task explicitly permits.
4. The moving case is addressed at the name layer by mDNS advertisement, which
   keeps the `.local` record in sync with the selected address.
5. `autonet doctor` (M3) gains a check for the one genuinely broken case: a
   server bound to a specific address rather than a wildcard. Doctor cannot fix
   it, but it is detectable, so it should be reported rather than left to fail
   silently later.

### The designated extension

**Option 2 is not rejected — it is deferred as an additive, opt-in escape
hatch.** If an app actually wants live freshness, `autonet run` can grow an
opt-in flag that writes an atomically-replaced state file and injects
`AUTONET_STATE_FILE`. That composes with this decision rather than reversing it:
the environment stays a launch-time snapshot, and the file is a separate,
explicitly requested channel for apps that choose to read it.

Adding it later **extends** this ADR and does not supersede it. Only a decision
to make `autonet run` supervise or restart its child would supersede it, and that
would be a new record.

## Consequences

**Good.**

- M3 stays small. No supervision, no signal forwarding, no platform-specific
  termination semantics, no new dependencies.
- `autonet run`'s exit code means exactly what the child's exit code means, with
  no ambiguity introduced by a restart the caller did not ask for.
- Nothing in this decision opens a port or exposes a service. mDNS advertisement
  does make the machine discoverable on the LAN, but it arrives as its own
  explicit, opt-in feature with its own analysis — it is not a side effect of
  this decision, and the security posture's requirement that such features be
  explicit is preserved.
- `autonet watch` being read-only makes it safe to run anywhere, including
  alongside a production process, because it cannot affect one.

**Bad, and accepted.**

- An app that prints its URL once at startup will display a stale URL after the
  network moves. Partially mitigated: `autonet watch` prints the new address, and
  the `.local` name keeps working regardless.
- An app bound to a specific address rather than `0.0.0.0` stops being reachable
  after a move, and AutoNet will not repair it. This is the real cost of the
  decision. `autonet doctor` will detect and warn; the remedy is the user's
  choice to bind a wildcard or restart the app.
- Anyone expecting `autonet run` to behave like `nodemon` will be surprised.
  Answered with documentation, not with behaviour.

**Neutral.**

- M5's daemon is unaffected. It remains justified by the SDKs and a stable local
  API; this record only declines to make mid-run freshness depend on it.

## What this record does not decide

- Whether `autonet watch` polls or uses platform change events. That is an
  implementation choice below this decision; both are read-only.
- The poll interval, the diffing rules, or the output format of `watch`.
- Anything about mDNS beyond the fact that it is where the moving case is
  handled — the crate, the hostname configuration and the advertisement's own
  security analysis are separate work.

The `[hostname]` configuration section that the mDNS work assumes **does not
exist yet**. [`Config`](../../crates/autonet-core/src/config.rs) has exactly two
sections, `selection` and `output`. Adding a third is a wire-format change and
follows the rules in [`json-schema.md`](../json-schema.md).
