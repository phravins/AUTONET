//! End-to-end tests for the `autonet` binary.
//!
//! These assert the *contract* — exit codes, stream discipline, and the shape
//! of `--json` — rather than which address this particular machine has. What
//! address gets picked is decided by `autonet-core` and tested exhaustively
//! against fixtures; asserting it again here would only make the suite fail
//! whenever someone runs it on a train.
//!
//! Every invocation is isolated from the developer's own configuration, so a
//! `~/.config/autonet/config.toml` on the machine running the tests cannot
//! change the result.

use std::process::Command as StdCommand;

use assert_cmd::prelude::*;

/// The binary, insulated from the ambient environment.
fn autonet() -> StdCommand {
    let mut command = StdCommand::cargo_bin("autonet").expect("the autonet binary is built");
    // A config file in the developer's home directory would silently change
    // what these tests assert.
    command.env("HOME", "/nonexistent-autonet-test-home");
    command.env_remove("XDG_CONFIG_HOME");
    for name in [
        "AUTONET_FAMILY",
        "AUTONET_INTERFACE",
        "AUTONET_EXCLUDE_INTERFACES",
        "AUTONET_ALLOW_VPN",
        "AUTONET_ALLOW_CONTAINER",
        "AUTONET_ALLOW_LOOPBACK",
    ] {
        command.env_remove(name);
    }
    // Colour would corrupt the assertions below, and would also corrupt a
    // user's `$(autonet ip)`. Set explicitly so the test proves the parsing
    // rather than relying on stdout not being a terminal.
    command.env("NO_COLOR", "1");
    command
}

fn stdout_of(output: &std::process::Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("stdout is UTF-8")
}

// ---------------------------------------------------------------------------
// Contract that holds on every platform
// ---------------------------------------------------------------------------

#[test]
fn help_explains_what_the_tool_is_for() {
    let output = autonet().arg("--help").output().unwrap();
    assert!(output.status.success());

    let help = stdout_of(&output);
    for command in ["status", "ip", "interfaces", "routes"] {
        assert!(help.contains(command), "--help omits `{command}`");
    }
    assert!(help.contains("--json"), "--help omits --json");
}

#[test]
fn version_is_reported() {
    let output = autonet().arg("--version").output().unwrap();
    assert!(output.status.success());
    assert!(stdout_of(&output).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn an_unknown_flag_is_rejected_without_writing_to_stdout() {
    let output = autonet().arg("--not-a-flag").output().unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "usage errors must not pollute stdout: {:?}",
        stdout_of(&output)
    );
}

#[test]
fn an_unparseable_family_fails_rather_than_falling_back() {
    // Silently defaulting to IPv4 after being asked for something unrecognised
    // would hand the caller an address of the wrong family.
    let output = autonet().args(["ip", "--family", "ipv7"]).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

// ---------------------------------------------------------------------------
// Contract that needs a working backend
// ---------------------------------------------------------------------------

// Gated on the platforms with a backend: elsewhere `provider()` returns
// `Unsupported`, and asserting that a snapshot describes the machine would be
// asserting that AutoNet has been ported.
//
// Windows joins as of 2b Task 2, which is the first change to make its snapshot
// describe a real machine. Note what these tests do and do not prove there:
// they assert invariants — one address on stdout, a loopback interface present,
// MACs withheld without `-v`, JSON that round-trips — which a CI VM with one
// virtual NIC can satisfy. They cannot prove the *right* address was chosen,
// because a runner has only one to choose from.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod live {
    use std::net::IpAddr;

    use serde_json::Value;

    use super::*;

    /// Exit code 1 means "nothing selectable", which is a legitimate answer on
    /// a disconnected machine. Anything else from these commands is a failure.
    fn assert_selection_exit(output: &std::process::Output) {
        let code = output.status.code();
        assert!(
            code == Some(0) || code == Some(1),
            "unexpected exit {code:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn ip_prints_one_bare_address_and_nothing_else() {
        let output = autonet().arg("ip").output().unwrap();
        assert_selection_exit(&output);

        if output.status.success() {
            let text = stdout_of(&output);
            assert!(text.ends_with('\n'), "output should be newline-terminated");
            let trimmed = text.trim_end();
            assert!(!trimmed.contains('\n'), "expected a single line: {text:?}");
            trimmed
                .parse::<IpAddr>()
                .unwrap_or_else(|e| panic!("{trimmed:?} is not an IP address: {e}"));
        } else {
            // The `IP=$(autonet ip) || exit` idiom depends on this.
            assert!(output.stdout.is_empty(), "failure must leave stdout empty");
            assert!(!output.stderr.is_empty(), "failure must explain itself");
        }
    }

    #[test]
    fn ip_with_a_port_renders_a_usable_url() {
        let output = autonet().args(["ip", "--port", "3000"]).output().unwrap();
        assert_selection_exit(&output);

        if output.status.success() {
            let url = stdout_of(&output).trim_end().to_string();
            assert!(url.starts_with("http://"), "{url}");
            assert!(url.ends_with(":3000"), "{url}");
            assert!(
                !url.contains("0.0.0.0"),
                "a wildcard bind address is not a destination: {url}"
            );
        }
    }

    #[test]
    fn interfaces_json_describes_the_machine() {
        let output = autonet().args(["interfaces", "--json"]).output().unwrap();
        assert!(output.status.success());

        let value: Value = serde_json::from_str(stdout_of(&output)).expect("valid JSON");
        assert_eq!(value["schema_version"], 1);

        let interfaces = value["interfaces"].as_array().expect("an array");
        assert!(!interfaces.is_empty());
        assert!(
            interfaces.iter().any(|i| i["kind"] == "loopback"),
            "every machine has a loopback interface"
        );
        for interface in interfaces {
            assert!(interface["name"].is_string());
            assert!(interface["index"].is_u64());
        }
    }

    #[test]
    fn hardware_addresses_are_withheld_unless_asked_for() {
        // A MAC is a durable hardware identifier, and answering "what address
        // can my phone reach" never requires publishing one.
        let quiet = autonet().args(["interfaces", "--json"]).output().unwrap();
        let value: Value = serde_json::from_str(stdout_of(&quiet)).unwrap();
        assert!(
            value["interfaces"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["mac"].is_null()),
            "MAC addresses leaked without -v"
        );

        let verbose = autonet()
            .args(["interfaces", "--json", "-v"])
            .output()
            .unwrap();
        let value: Value = serde_json::from_str(stdout_of(&verbose)).unwrap();
        assert!(
            value["interfaces"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["mac"].is_string()),
            "-v should report hardware addresses"
        );
    }

    #[test]
    fn routes_json_is_well_formed() {
        let output = autonet().args(["routes", "--json"]).output().unwrap();
        assert!(output.status.success());

        let value: Value = serde_json::from_str(stdout_of(&output)).expect("valid JSON");
        assert_eq!(value["schema_version"], 1);
        for route in value["routes"].as_array().expect("an array") {
            assert!(route["interface_index"].is_u64());
            assert!(route["family"] == "ipv4" || route["family"] == "ipv6");
        }
    }

    #[test]
    fn status_json_carries_the_schema_version_even_when_nothing_is_selected() {
        let output = autonet().args(["status", "--json"]).output().unwrap();
        assert_selection_exit(&output);

        let value: Value = serde_json::from_str(stdout_of(&output)).expect("valid JSON");
        assert_eq!(value["schema_version"], 1);
        assert!(value["platform"].is_string());

        if output.status.success() {
            assert!(value["selected"]["ip"].is_string());
        } else {
            assert!(value["selected"].is_null());
            assert!(value["error"].is_string(), "a failure must say why");
        }
    }

    #[test]
    fn a_misspelled_interface_is_a_usage_error_not_a_missing_address() {
        // Exit 2, not 1: the machine's network is fine, the command was wrong.
        let output = autonet()
            .args(["ip", "--interface", "definitely-not-an-interface"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("no interface named"));
    }

    #[test]
    fn bare_autonet_behaves_like_status() {
        let bare = autonet().arg("--json").output().unwrap();
        let explicit = autonet().args(["status", "--json"]).output().unwrap();
        assert_eq!(bare.status.code(), explicit.status.code());

        let bare: Value = serde_json::from_str(stdout_of(&bare)).unwrap();
        let explicit: Value = serde_json::from_str(stdout_of(&explicit)).unwrap();
        assert_eq!(bare["selected"], explicit["selected"]);
    }

    #[test]
    fn output_is_free_of_escape_codes_when_it_is_not_a_terminal() {
        for args in [
            vec!["status"],
            vec!["interfaces"],
            vec!["routes"],
            vec!["status", "-v"],
        ] {
            let output = autonet().args(&args).output().unwrap();
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(
                !text.contains('\u{1b}'),
                "ANSI escapes in piped output of `autonet {}`",
                args.join(" ")
            );
        }
    }
}
