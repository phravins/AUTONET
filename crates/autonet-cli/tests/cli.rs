//! End-to-end CLI tests.

use std::process::Command as StdCommand;

use assert_cmd::prelude::*;

/// Build an isolated command invocation.
fn autonet() -> StdCommand {
    let mut command = StdCommand::cargo_bin("autonet").expect("the autonet binary is built");
    // Isolate the test from local configuration.
    command.env("HOME", "/nonexistent-autonet-test-home");
    command.env_remove("XDG_CONFIG_HOME");
    // Set on every Windows runner, and now consulted by `Config::default_path`.
    command.env_remove("APPDATA");
    for name in [
        "AUTONET_FAMILY",
        "AUTONET_INTERFACE",
        "AUTONET_EXCLUDE_INTERFACES",
        "AUTONET_ALLOW_VPN",
        "AUTONET_ALLOW_CONTAINER",
        "AUTONET_ALLOW_LOOPBACK",
        "AUTONET_HOSTNAME",
    ] {
        command.env_remove(name);
    }
    // Keep output free of terminal styling.
    command.env("NO_COLOR", "1");
    command
}

fn stdout_of(output: &std::process::Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("stdout is UTF-8")
}

#[test]
fn help_explains_what_the_tool_is_for() {
    let output = autonet().arg("--help").output().unwrap();
    assert!(output.status.success());

    let help = stdout_of(&output);
    for command in ["status", "ip", "interfaces", "routes", "run", "doctor"] {
        assert!(help.contains(command), "--help omits `{command}`");
    }
    assert!(help.contains("--json"), "--help omits --json");
}

#[test]
fn doctor_help_says_the_bind_row_is_advice_rather_than_a_measurement() {
    // The one row in the tool that reports something it did not check. If the
    // help ever stops saying so, the row starts reading as a verdict.
    let output = autonet().args(["doctor", "--help"]).output().unwrap();
    assert!(output.status.success());

    let help = stdout_of(&output);
    assert!(help.contains("NOT CHECKED"), "{help}");
    assert!(help.contains("cannot see what address"), "{help}");
    assert!(help.contains("0.0.0.0"), "{help}");
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

// These tests require a supported platform backend.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod live {
    use std::net::{IpAddr, TcpListener};

    use assert_cmd::cargo::cargo_bin;
    use serde_json::Value;

    use super::*;

    /// Accept success or the expected no-address exit code.
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

    /// Write a throwaway config file and hand back its path.
    ///
    /// No temp-directory crate: one file per test, named for the test, in the
    /// system temp directory, removed by the caller.
    fn config_file(tag: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("autonet-{tag}-{}.toml", std::process::id()));
        std::fs::write(&path, contents).expect("a writable temp directory");
        path
    }

    #[test]
    fn a_configured_default_port_renders_a_url_without_the_flag() {
        // `output.default_port` shipped in the documented example config and
        // was read by nothing until this task.
        let path = config_file("default-port", "[output]\ndefault_port = 4173\n");
        let output = autonet()
            .args(["ip", "--config"])
            .arg(&path)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);
        assert_selection_exit(&output);

        if output.status.success() {
            let url = stdout_of(&output).trim_end().to_string();
            assert!(url.starts_with("http://"), "{url}");
            assert!(url.ends_with(":4173"), "{url}");
        }
    }

    #[test]
    fn a_configured_port_of_zero_is_not_a_port() {
        // `default_port = 0` is what the README's example config contains, and
        // `http://192.168.1.20:0` is not a URL anyone can open.
        let path = config_file("zero-port", "[output]\ndefault_port = 0\n");
        let output = autonet()
            .args(["ip", "--config"])
            .arg(&path)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);
        assert_selection_exit(&output);

        if output.status.success() {
            let text = stdout_of(&output);
            assert!(
                !text.contains("http://"),
                "a placeholder became a URL: {text}"
            );
            assert!(!text.contains(":0"), "{text}");
        }
    }

    #[test]
    fn the_flag_beats_the_configured_default_port() {
        let path = config_file("port-precedence", "[output]\ndefault_port = 4173\n");
        let output = autonet()
            .args(["ip", "--port", "3000", "--config"])
            .arg(&path)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);
        assert_selection_exit(&output);

        if output.status.success() {
            assert!(stdout_of(&output).trim_end().ends_with(":3000"));
        }
    }

    #[test]
    fn run_warns_about_a_taken_port_before_the_command_starts() {
        // Held by this test, on the wildcard, so it is genuinely unavailable
        // to anything the child might bind.
        let held = TcpListener::bind("0.0.0.0:0").expect("a wildcard listener");
        let port = held.local_addr().unwrap().port().to_string();

        // The child is the autonet binary itself: present on every platform the
        // tests run on, and `--version` exits immediately without touching the
        // network.
        let output = autonet()
            .args(["run", "--port", &port, "--"])
            .arg(cargo_bin("autonet"))
            .arg("--version")
            .output()
            .unwrap();
        assert_selection_exit(&output);

        if output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("already in use") || stderr.contains("held elsewhere"),
                "no warning for a port this test is holding: {stderr}"
            );
            // A warning, not a refusal: the command still ran.
            assert!(stdout_of(&output).contains(env!("CARGO_PKG_VERSION")));
        }
    }

    #[test]
    fn a_port_after_the_double_dash_belongs_to_the_command_and_is_not_probed() {
        // The boundary test. The same port is held, but `--port` is on the far
        // side of `--`, so AutoNet never parses it, never probes it, and passes
        // it through to the child untouched.
        let held = TcpListener::bind("0.0.0.0:0").expect("a wildcard listener");
        let port = held.local_addr().unwrap().port().to_string();

        let output = autonet()
            .args(["run", "--"])
            .arg(cargo_bin("autonet"))
            .args(["--version", "--port", &port])
            .output()
            .unwrap();
        assert_selection_exit(&output);

        if output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stderr.contains("already in use") && !stderr.contains("held elsewhere"),
                "a port past the `--` boundary was probed anyway: {stderr}"
            );
            assert!(stdout_of(&output).contains(env!("CARGO_PKG_VERSION")));
        }
    }

    #[test]
    fn run_stays_quiet_about_a_port_that_is_free() {
        // A listener bound and released: the port is free again, so there is
        // nothing to say. Guards against a warning that always fires.
        let port = {
            let held = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
            held.local_addr().unwrap().port()
        }
        .to_string();

        let output = autonet()
            .args(["run", "--port", &port, "--"])
            .arg(cargo_bin("autonet"))
            .arg("--version")
            .output()
            .unwrap();
        assert_selection_exit(&output);

        if output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stderr.contains("in use") && !stderr.contains("held"),
                "a free port produced a warning: {stderr}"
            );
        }
    }

    #[test]
    fn output_is_free_of_escape_codes_when_it_is_not_a_terminal() {
        for args in [
            vec!["status"],
            vec!["interfaces"],
            vec!["routes"],
            vec!["status", "-v"],
            vec!["doctor"],
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

    #[test]
    fn doctor_reports_a_checklist_and_never_exits_two_on_a_supported_platform() {
        // Exit 2 means AutoNet could not do its job. Doctor's job is to
        // describe a machine, including a broken one, so on a platform with a
        // backend it should always manage that much.
        let output = autonet().arg("doctor").output().unwrap();
        assert_selection_exit(&output);

        let text = stdout_of(&output);
        for label in [
            "Operating system",
            "Network interface",
            "IPv4 address",
            "Default route",
            "Selected address",
            "LAN reachable",
            "Bind address",
        ] {
            assert!(text.contains(label), "doctor omitted {label:?}: {text}");
        }
        // Every row carries one of the four tokens and nothing else.
        assert!(text.contains("[ ?  ]"), "no unverified row: {text}");
    }

    #[test]
    fn doctors_json_carries_every_row_the_text_form_shows() {
        let text = autonet().arg("doctor").output().unwrap();
        let json = autonet().args(["doctor", "--json"]).output().unwrap();
        assert_eq!(text.status.code(), json.status.code());

        let value: Value = serde_json::from_str(stdout_of(&json)).expect("valid JSON");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["os"], std::env::consts::OS);
        assert!(value["summary"].as_str().is_some_and(|s| !s.is_empty()));

        let checks = value["checks"].as_array().expect("a checks array");
        let ids: Vec<&str> = checks
            .iter()
            .map(|c| c["id"].as_str().expect("a string id"))
            .collect();
        assert!(ids.contains(&"operating_system"), "{ids:?}");
        assert!(ids.contains(&"bind_address"), "{ids:?}");

        // The labels are what the text form prints, so the two cannot drift.
        let rendered = stdout_of(&text);
        for check in checks {
            let label = check["label"].as_str().expect("a string label");
            assert!(rendered.contains(label), "text form omits {label:?}");
        }

        // `ok` is the exit code, restated for a consumer that only reads JSON.
        assert_eq!(value["ok"], text.status.success());
    }

    #[test]
    fn doctor_reports_the_bind_distinction_without_claiming_to_have_checked_it() {
        let output = autonet().args(["doctor", "--json"]).output().unwrap();
        let value: Value = serde_json::from_str(stdout_of(&output)).expect("valid JSON");

        let bind = value["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .find(|c| c["id"] == "bind_address")
            .expect("a bind_address row");

        // Never anything else. AutoNet cannot see another process's bind().
        assert_eq!(bind["status"], "unknown");
        let detail = bind["detail"].as_str().expect("a detail");
        assert!(detail.contains("cannot see"), "{detail}");
    }

    #[test]
    fn doctor_warns_about_a_held_port_without_failing_the_run() {
        // Same probe `run` makes, and the same verdict: a busy port is a
        // strong hint, not a fault of this machine.
        let held = TcpListener::bind("0.0.0.0:0").expect("a wildcard listener");
        let port = held.local_addr().unwrap().port().to_string();

        let output = autonet()
            .args(["doctor", "--port", &port, "--json"])
            .output()
            .unwrap();
        assert_selection_exit(&output);

        let value: Value = serde_json::from_str(stdout_of(&output)).expect("valid JSON");
        let port_row = value["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .find(|c| c["id"] == "port");

        // Present only when an address was selected to probe against; on a
        // machine with no address the rows above already say why.
        if let Some(row) = port_row {
            assert_eq!(row["label"], format!("Port {port}"));
            assert_eq!(row["status"], "warn", "{row}");
            assert_eq!(value["verdict"], "warn");
            assert_eq!(value["ok"], true, "a busy port must not fail the run");
            assert!(output.status.success());
        }
    }

    #[test]
    fn doctor_says_nothing_about_a_port_that_is_free() {
        let port = {
            let held = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
            held.local_addr().unwrap().port()
        }
        .to_string();

        let output = autonet()
            .args(["doctor", "--port", &port, "--json"])
            .output()
            .unwrap();
        assert_selection_exit(&output);

        let value: Value = serde_json::from_str(stdout_of(&output)).expect("valid JSON");
        if let Some(row) = value["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .find(|c| c["id"] == "port")
        {
            assert_eq!(row["status"], "pass", "a free port produced {row}");
        }
    }

    /// This machine's loopback interface, named by the kernel's own flag.
    ///
    /// Never spelled `lo`. It is `lo` on Linux, `lo0` on macOS and
    /// `Loopback Pseudo-Interface 1` on Windows, so a literal turns a test
    /// about *behaviour* into a test about Linux — and on the other two it
    /// becomes a test of the "no interface named ..." usage error instead,
    /// which exits 2 and writes to stderr, failing on both counts for reasons
    /// that have nothing to do with what it set out to check. Asking AutoNet
    /// for the flag is the same rule the selector follows: classify from what
    /// the kernel reports, never from a naming pattern.
    ///
    /// `None` only when there is no backend to ask, which is the one case
    /// where this test has nothing to say.
    fn loopback_interface() -> Option<String> {
        let output = autonet().args(["interfaces", "--json"]).output().unwrap();
        if !output.status.success() {
            return None;
        }

        let value: Value = serde_json::from_str(stdout_of(&output)).expect("valid JSON");
        let name = value["interfaces"]
            .as_array()
            .expect("an interfaces array")
            .iter()
            .find(|i| i["flags"]["loopback"] == true)
            .and_then(|i| i["name"].as_str())
            .map(str::to_string);

        // Not a skip. Every platform with a backend has one, and its absence
        // would be a real finding rather than a reason to stay quiet.
        assert!(name.is_some(), "no interface carries the loopback flag");
        name
    }

    #[test]
    fn doctor_fails_rather_than_errors_when_it_is_told_to_use_loopback() {
        // A machine that can only offer 127.0.0.1 is a diagnosis, not a
        // malfunction: exit 1, and no `autonet:` line on top of the checklist
        // that already explained it.
        let Some(loopback) = loopback_interface() else {
            return;
        };

        let output = autonet()
            .args(["doctor", "--interface", &loopback])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(1), "{}", stdout_of(&output));
        assert!(stdout_of(&output).contains("[fail]"));
        assert!(
            output.stderr.is_empty(),
            "the checklist was duplicated on stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
