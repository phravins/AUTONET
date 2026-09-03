//! Print a scrubbed network-state fixture as JSON.

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

    // Omit stable identifiers and volatile timestamps from fixtures.
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

/// Report capture failures to stderr.
fn fail(context: &str, error: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("capture: {context}: {error}");
    ExitCode::FAILURE
}
