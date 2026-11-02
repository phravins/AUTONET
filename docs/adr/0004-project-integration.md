# 0004 — AutoNet inside the project being developed

- **Status:** Accepted
- **Date:** 2026-10-31
- **Affects:** `autonet init`, `autonet dev`, `autonet check`, `autonet.toml`, and
  how `autonet run` finds a program
- **Extends:** [ADR 0001](0001-network-change-during-autonet-run.md), which stays
  in force

## Context

AutoNet answered the question correctly and left the project with the problem. A
developer could get the right address from `autonet ip`, or launch a server with
`autonet run`, and still had an address to get into the app:

- The variables from `autonet run` are a launch-time snapshot, so they go stale
  when the laptop moves from Wi-Fi to a hotspot.
- The README's own example used `process.env.AUTONET_IP` in code that also runs in
  a browser, where nothing provides it.
- A mobile or desktop app is not launched by `autonet run` at all, and its address
  is compiled in or read from a file.
- Nothing stopped an address being typed into source and committed, so the problem
  came back with each new line of code.

The requirement is that AutoNet works inside the project: it detects the network
change, runs the project with no address anywhere in it, and rejects addresses
written by hand.

## Decision

**Three commands. `autonet dev` starts the project once and keeps a generated file
current, `autonet check` rejects hand-written addresses, and `autonet init` wires
both into a project.**

### 1. The live channel is a generated file, and the child is still never restarted

ADR 0001 decided that AutoNet launches and does not supervise, and named an opt-in
file as the one live channel. `autonet dev` is that design applied to a project.
The child starts once and is never signalled because of a network change. What
changes on a move is a file, and the framework's own reload picks it up. For
projects with no such reload, AutoNet says what to do instead: press `r` for
Flutter, rebuild for native Android and iOS.

**Rejected: restart the dev server on a network change.** ADR 0001's reasons all
still apply. It would also lose the state a developer most wants kept during a dev
session, and the frameworks turned out to do this better themselves.

**Rejected: a local proxy with a stable address.** The app's own device still has
to reach it, so this moves the problem and adds a process in every request's path.

### 2. Writing to project files is new, and it is narrow

AutoNet has never written into a project before. Now it does, under these limits:

- It writes only the paths the project's plan names, and a manifest's paths must be
  relative and stay inside the project. Otherwise a checked-in `autonet.toml` could
  overwrite files elsewhere on the machine.
- It never overwrites a file it did not write. Each generated file carries a marker,
  and every path is checked before any is written, so a refusal changes nothing.
  `.env.development.local` is a name people use for their own secrets, and replacing
  one would lose them with no way back.
- A file whose contents would not change is left alone, because its modification
  time is what a watcher reacts to.
- Writes are atomic, using the same rename as the state file.
- `autonet init` also writes `autonet.toml`, `.gitignore` and a pre-commit hook. It
  computes every change before making any, and `--dry-run` shares all of that code
  up to the last step.

**Rejected: rewrite the hardcoded address in source for the developer.** The right
replacement depends on the code around it, and a wrong automatic edit is worse than
a clear error. `autonet check` names the file, the line and what to use instead.

### 3. Generated files stay on exit, unlike the state file

ADR 0001's state file is removed when AutoNet stops, because absence is its one
"stop" signal. A generated file does the opposite. Source code imports it, so a
missing file is a build failure. It stays, and an `online: false` value is what says
it cannot be relied on. With no network, the file keeps the last values, because
they may yet become true again. With no network ever, it holds loopback, which at
least works on this machine.

**Rejected: use the same lifecycle for both.** A reader of the state file chose to
read it and handles it being gone. An `import` cannot.

### 4. Exposing the server is the stated purpose, and it says so

`architecture.md` says nothing exposes a service as a side effect. `autonet dev`
binds the wildcard on purpose, since otherwise a phone cannot connect, so it prints
that it is exposing the server before anything starts. On a publicly routable
address the wildcard means the internet, so it refuses unless given `--allow-public`.
If the address becomes public while it runs, it warns and carries on, since ADR 0001
forbids it from stopping the child. A plan that only keeps a file current exposes
nothing, and is never refused.

### 5. Detected or declared, never guessed

A `dev` script that is not plainly the framework's own command is refused, with the
`autonet.toml` that would declare it. Adding `--host` to an unknown command could
break it, or leave it on loopback or on the wrong port while AutoNet prints a URL
that does not answer.

For the same reason, a script that fixes a different port than the one requested is
a refusal, and Vite is passed `--strictPort` so that it cannot move to a free port
without saying so.

### 6. `check` leans towards silence, and the hook does not block on its absence

A false alarm on every commit is what gets a hook deleted, after which it catches
nothing. So the scanner ignores anything only shaped like an address, along with
prose, logs and lockfiles, and `autonet:allow` exempts a line.

`--staged` reads the index and not the working tree. Otherwise an address removed
from the file but still staged would pass, and one added but not staged would stop
a commit that does not contain it.

A hook lives in one clone, so a teammate without AutoNet gets a warning rather than
a blocked commit. CI is where the check is enforced for everyone.

### 7. Stopping means the whole process tree

`npm run dev` is several processes deep, and signalling npm alone left Vite running
and holding the port. This was found by running it, not by reading it. `autonet dev`
lists the tree and sends one `SIGTERM` to all of it, or uses `taskkill /T` on
Windows, and only when it is itself ending. That logic is in `autonet-platform`,
under rule 2. So is resolving npm's `.cmd` shim through `PATHEXT`, which `autonet
run` now shares.

## Verification

End to end, each in a network namespace (`unshare -Urn`) with `autonet dev` left
running while the only address moved from `dummy0` 192.168.50.10 to `dummy1`
10.77.0.5. Each time, the generated file changed, the new URL printed, the new
address served the new value, and the server was not restarted by AutoNet.

| Framework | Version | Result |
|---|---|---|
| Vite | 6.3.5 | Passed. Vite restarts its own server on the env file. |
| Next.js | 15.3.5, 16.4.3 | Passed. Next logs "Reload env". |
| Astro | 5.18.2 | Passed. |
| Nuxt | 4.6.3 | Passed, through a generated module and hot reload. |
| SvelteKit | 2.70.3 | Passed, reading `$env/static/public`. |
| SvelteKit | 3.1.1 | Passed, once declared in `src/env.js` and read from `$app/env/public`. |

Vite, Next.js, Astro and Nuxt also exited `0` on `SIGTERM` with the port free.

Also checked for real:

- **Next.js 16 cross-origin blocking.** Without `allowedDevOrigins`, an asset request
  with a LAN `Origin` gets `403` and the hot reload socket is refused. With the
  wildcard patterns AutoNet suggests, assets return `200` and `/_next/hmr` returns
  `101`, while real cross-site requests still get `403`. Next.js 15 only warns.
- **SvelteKit 3 undeclared variables.** The page returns `200` with an empty value and
  no error, which is why AutoNet prints a note.
- **The terminal.** Run under a real pty, a typed key reaches the child's stdin and
  Ctrl-C ends it in 0.1s with exit `0`.
- **The hook.** A commit containing an address is stopped, and one without passes.

**Not run for real:** Angular, Create React App, Electron, Tauri, Expo, React Native,
Flutter, native Android and iOS, and Django. Their detection and generated files have
tests, but the toolchains were not available, so what each framework does with its
file on a move is expected rather than observed. **No project type has been tried from
a real phone or across real Wi-Fi.** The move above is synthetic, in the same way as
M4's. Windows `.cmd` handling has a `#[cfg(windows)]` test that runs only in CI, and
macOS and Windows have the standing gaps listed in the README.

## Security analysis

**What is new is access, on purpose.** A dev server has no authentication and often
serves source maps and an evaluation endpoint, and this command makes one reachable
from the network. That is the point of it, and it is why section 4 exists. It is one
step on from `autonet run`, which binds nothing itself.

**A checked-in `autonet.toml` is a command someone else chose.** Running `autonet dev`
in a cloned repository runs what its manifest says. This is the same trust as `npm
run dev` in the same repository, so it adds no new risk, but it is the reason output
paths may not leave the project: a file's contents are generated, and its location
should not become a way to reach outside.

**The hook runs on every commit**, and it only reads. It sends nothing anywhere and
changes nothing.

Generated files hold the same address `autonet status` prints. They are gitignored so
that it does not reach the repository, and `autonet check` skips them by their marker.

## Consequences

**Good.**

- A project holds no address, and the check keeps it that way.
- The running app follows a network change without a restart, on the five frameworks
  verified here.
- There is still one change detector. `dev` is a consumer of `watch::observe`, like
  `advertise` and `--state-file`.
- `autonet run` keeps its behaviour, and gains working npm shims on Windows.

**Bad, and accepted.**

- **It depends on how each framework reloads**, which AutoNet does not control and
  which changes between releases. SvelteKit 3 and Next.js 16 both changed what a
  project must declare, and each needed a note that a later version may make wrong.
- **Most project types are unverified.** See above. The README marks each one.
- **Generated files exist only where AutoNet has run.** A fresh clone or a CI job has
  none, and code importing one will not build until `autonet init` or `autonet dev`
  has run.
- **Development values only.** A production build has no generated value and needs its
  own.
- **Native apps need a rebuild, and a device holds the old address until then.**
  AutoNet says so and cannot improve on it.
- **Devices must be able to reach each other.** A phone on mobile data and a laptop on
  Wi-Fi cannot, and networks with client isolation block this too.
- **The scanner is a text search.** It will miss an address built from parts, and will
  sometimes need an `autonet:allow`.
- **The hook needs a version of AutoNet that has `check`.** An older binary earlier on
  `PATH` fails with an unrecognised subcommand, and that blocks the commit.

**Neutral.**

- `schema_version` does not move. `check --json` is a new document, and no existing
  field changed.
- `autonet.toml` is a separate schema from the user's `config.toml`, because one
  describes a project and is committed, and the other describes a person's machine.

## What this record does not decide

- **Tunnels**, for a phone on mobile data. Out of scope, and the docs say so.
- **npm or PyPI packages.** Distribution stays crates.io and GitHub releases.
- **The M5 daemon and M6 SDKs.** A generated module does much of what a thin SDK
  would for reading the address, which may change what M6 needs to be.
- **Changing firewall rules or platform cleartext settings.** AutoNet reports these
  and leaves them alone.
