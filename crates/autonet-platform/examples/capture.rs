//! Print this machine's `NetworkState` as a fixture-shaped JSON document.
//!
//! ```sh
//! cargo run -p autonet-platform --example capture > tests/fixtures/macos-real-wifi.json
//! ```
//!
//! # Why this exists rather than a CLI flag
//!
//! A selection fixture is a whole [`NetworkState`], and no `autonet` subcommand
//! emits one: `status --json` reports the *decision* (`selected`, `urls`,
//! `candidates`), while `interfaces --json` and `routes --json` each carry one
//! half of the state. Stitching those two together would also mean two separate
//! `snapshot()` calls, and a tunnel coming up between them would produce a
//! fixture describing a machine that never existed. This takes one snapshot.
//!
//! It is deliberately an example and not a subcommand. The M1 command surface is
//! frozen, and capturing test data is a developer's job rather than a user's.
//!
//! # What is removed, and why
//!
//! **Hardware addresses.** A MAC outlives every IP address on the machine and
//! identifies the device itself, and these files get committed. AutoNet withholds
//! it from `interfaces --json` for the same reason, so a capture that reintroduced
//! it through the back door would undo that.
//!
//! **`captured_at`.** A timestamp would make every re-capture of an unchanged
//! network a diff, and the fixture harness treats the field as optional precisely
//! so that fixtures can be byte-stable.
//!
//! Nothing else is filtered. In particular the addresses stay, because they are
//! what the fixture is *for* — which is worth knowing before committing a capture
//! taken on a network you would rather not publish.

use std::process::ExitCode;

use autonet_platform::provider;

fn main() -> ExitCode {
    let provider = match provider() {
        Ok(provider) => provider,
        Err(error) => return fail("no backend for this platform", &error),
    };

    let mut state = match provider.snapshot() {
        Ok(state) => state,
        Err(error) => return fail("the kernel could not be queried", &error),
    };

    // See the module docs: identity out, timestamp out, addresses stay.
    for interface in &mut state.interfaces {
        interface.mac = None;
    }
    state.captured_at = None;

    match serde_json::to_string_pretty(&state) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => fail("the snapshot could not be serialised", &error),
    }
}

/// Report to stderr so that a failed capture cannot be mistaken for a fixture.
///
/// Everything this program means to produce goes to stdout, which is normally
/// redirected straight into a file.
fn fail(context: &str, error: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("capture: {context}: {error}");
    ExitCode::FAILURE
}
