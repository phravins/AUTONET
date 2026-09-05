# 0003 — What the QR code encodes

- **Status:** Accepted
- **Date:** 2026-09-05
- **Affects:** `autonet status --qr`, the URL-rendering path shared by `status`,
  `ip`, `run` and `advertise`, and a future `autonet advertise --qr`

## Context

[ADR 0002](0002-mdns-advertisement.md) closes with an explicit open item:

> The QR code's contents. It will read `[hostname]` to decide whether to encode
> the `.local` name or the raw address, but that is a rendering decision, not
> this one.

This record makes that decision, and reaches the opposite conclusion from the
one ADR 0002 assumed.

The problem is the last hop. `autonet status --port 3000` prints
`http://192.168.1.18:3000`, and the device that needs it is a phone, which has
no way to consume a string on someone else's screen except by having a person
read it out and type it in. That is the step that produces `192.168.1.8`, and
the person then blames the network. A camera does not transpose digits.

## Decision

**`autonet status --qr` renders the network URL as terminal block characters,
always encoding the selected address, never a `.local` name; it is refused
alongside `--json`; and every URL in the CLI now comes from one function whose
`name` parameter is the point at which mDNS would be swapped in.**

### 1. The payload is always the IP URL — and not because mDNS is missing

ADR 0002 expected this record to consult `[hostname] enabled`. That is the wrong
input, and building it that way would have been a bug with a config file behind
it.

`hostname.enabled = true` means *"this machine may advertise"*. It does not mean
*"this machine is advertising right now"*. The name only resolves while
`autonet advertise` holds the responder, and `autonet status` does not
advertise — it prints and exits. A `status --qr` that read the config flag would
encode `http://real0-autonet.local:3000` on a machine where nothing is
answering that name: a QR code that scans perfectly, then fails to load. That is
strictly worse than the address it replaced, because the failure arrives after
the user has already been told it worked.

So the question is not "is mDNS configured" but **"is a responder for this name
alive in this process"**, and for `status` the answer is permanently no.

**Rejected: encode the `.local` name when `[hostname] enabled` is true.** The
config flag is consent to publish, not evidence of publishing. Consent and
liveness are different facts and must not share a switch.

**Rejected: probe whether the name currently resolves, then choose.** It makes
the output of `status` depend on a network round trip, makes it
nondeterministic, and adds a resolver to a command whose entire job is to be
instant and honest. It would also still be a race: resolvable at print time is
not resolvable at scan time.

### 2. The swap point is a parameter, not a lookup

`crates/autonet-cli/src/url.rs` is now the only place in the CLI that answers
"what URL reaches this machine". Six call sites used to build that string
independently — `status` in text and in JSON, `ip` in both, `AUTONET_URL` in
`spawn`, and `advertise`'s opening block.

```rust
pub(crate) fn network_url(selected: &SelectedAddress, port: u16, name: Option<&str>) -> String
```

`name` is `Some` **only when a responder in this process is publishing it**.
`advertise` passes `Some(&host)` today and gets the `.local` URL, which is why
this design is demonstrated rather than merely asserted; everything else passes
`None`. An `autonet advertise --qr` is therefore one argument, not a rewrite —
and it cannot accidentally acquire the config-flag bug above, because there is
no config flag in the signature to read.

### 3. `qrcode` 0.14, with default features off

| Candidate | Why not |
|---|---|
| `qr2term` | The obvious choice by name, and it drags in `crossterm` — a terminal-control framework — to colour two characters. `owo-colors` already does that job and is already here. |
| `fast_qr` | Capable, and offers nothing this needs. Speed is irrelevant when the input is one 24-byte URL typed by hand. |
| `qrcodegen` | Zero dependencies, and no renderer. It would mean hand-writing the half-block loop for no gain over a crate that ships one. |
| **`qrcode` 0.14** | **Chosen.** `render::unicode::Dense1x2` writes half-block characters straight to stdout — no image file, no viewer, no GUI. Its only optional dependencies are `image`, `svg` and `pic`, so `default-features = false` adds **zero transitive crates**. MSRV 1.67.1, under our 1.82. |

Error correction level **M** (~15%), quiet zone on. `L` would make the block
narrower, which matters on an 80-column terminal, but a code on a screen gets
photographed at an angle with glare on it, and recovering from that is what the
redundancy is for.

### 4. `--qr` with `--json` is refused, exit 2

The payload *is* `urls.network`, which is already in the schema. Adding a `qr`
field would put a second copy of one fact in the contract, where it can only
ever agree with the first, and would put ASCII art in a machine format. Refusing
and naming `urls.network` tells the caller where the answer already is.

**Rejected: ignore `--qr` silently under `--json`.** A flag that does nothing is
indistinguishable from a flag that failed.

### 5. Polarity is chosen for the camera, not the theme

This is the part that decides whether the feature works at all, and it is
invisible in a code review.

Half-block characters carry no colour of their own. `█` is whatever the
terminal's foreground is; a space is whatever its background is. So the naive
rendering — dark modules as blocks — is correct on a light terminal and
**inverted on a dark one**, which is the theme most developers use. Some
scanners read inverted codes; not all do, and the ones that fail do so silently,
which sends the user looking at their firewall.

Therefore: **when AutoNet is emitting colour, it paints the code black on bright
white explicitly**, and polarity stops depending on the user's palette. This is
the only place in the tool that overrides the terminal's own colours rather than
working with them, and the reason is that the reader here is a camera.

When colour is off — `NO_COLOR`, `TERM=dumb`, or output redirected — there is no
colour to force, so the block characters must carry the polarity themselves and
can only suit one theme. Dark is the one to suit, and the output says so in a
line naming the fix.

## Security analysis

`architecture.md`'s posture requires this, and here it is short: **`--qr` opens
nothing, binds nothing, transmits nothing and resolves nothing.** It is a
rendering of a string the same command already prints in plain text one line
above it. Unlike [ADR 0002](0002-mdns-advertisement.md), nothing goes on the
wire and no socket is held.

One thing is genuinely new and is worth naming: **a terminal screenshot now
carries a machine-readable LAN address.** Screen shares, pair-programming
recordings and bug-report screenshots have always contained
`http://192.168.1.18:3000` as text; they now contain it in a form a phone reads
in a fraction of a second without anyone noticing the line. This is a small
change in degree rather than in kind — the address was already in the frame, and
a private address is only useful to someone already on the network — but it is
the honest reason `--qr` is a flag the user asks for rather than something
`status` does by default.

## Consequences

**Good.**

- The last manual step in the tool's own pitch is gone. Nobody types an address
  off a screen.
- Six URL construction sites became one. IPv6 bracketing, the scheme, and the
  choice of host are now decided in a single place, and the JSON, the text
  output, `AUTONET_URL` and the QR payload cannot drift apart — verified
  byte-identical.
- The mDNS swap point exists, is exercised by a real caller, and is a parameter
  rather than a comment promising future work.
- Zero transitive dependencies, immediately after ADR 0002 added five.

**Bad, and accepted.**

- **A scanned code is a snapshot, with exactly the staleness
  [ADR 0001](0001-network-change-during-autonet-run.md) described.** The phone
  keeps the address it read; when the laptop moves networks the page 404s, and
  the user has a QR code that "worked a minute ago". Worse, the fix ADR 0001
  named for precisely this — *"the client reaches the server by a `.local` name,
  and the mDNS advertisement follows the address change"* — is the thing this
  record deliberately does not encode. That is not an oversight; encoding a name
  nothing answers fails immediately instead of eventually, which is worse. But
  the result is that `status --qr` is the convenient answer and the fragile one,
  and `advertise --qr` will be the durable one when it exists.
- **It needs a port, and refuses without one.** `status` is otherwise useful
  with no port at all. A QR code is only worth scanning if it opens something,
  so `--qr` is the one flag that turns an optional input into a required one.
- **~33 columns wide.** A version-2 code with its quiet zone fits 80×24
  comfortably and wraps into unscannable noise below about 40 columns. A longer
  URL — a hostname, or IPv6 — pushes to version 3 or beyond and gets wider.
  There is no narrower rendering; the modules are the modules.
- **With colour off, the rendering can only suit one terminal theme.** A light
  terminal with `NO_COLOR` set gets an inverted code. The output says so and
  names the fix, which is a caption, not a solution.
- **A screenshot now carries a scannable address.** See above.
- **One more direct dependency**, for a feature that is entirely cosmetic in the
  sense that the URL was already printed.

**Neutral.**

- The `--json` contract is unchanged. No field was added, removed or retyped, so
  `schema_version` does not move.
- `--qr` is a global flag, like `--port`, for the reason `--port`'s own
  documentation gives: URL rendering is not one command's concern. `interfaces`
  and `routes` ignore it exactly as they ignore `--port`.

## What this record does not decide

- **Whether `autonet advertise` grows `--qr`.** The swap point is built and the
  argument is one word; whether the command should print a code is a separate
  question about that command's output.
- **Anything about `run --qr`.** `run`'s output belongs to the child process.
- **Whether a wider code should be offered narrower** — a `--qr-compact`
  rendering using a full block per module in *both* axes, or a link shortener.
  Neither is needed at version 2.
