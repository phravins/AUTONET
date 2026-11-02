//! End-to-end tests of what AutoNet does inside a project: `init`, `dev`, `check`.
//!
//! Each runs the real binary in its own directory. What moving between networks
//! does to a running framework cannot be tested here, since it takes a network
//! namespace and the framework itself: see the verification in
//! `docs/adr/0004-project-integration.md`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Output};

use assert_cmd::prelude::*;
use serde_json::Value;

/// A directory that is gone when the test ends, pass or fail.
struct Project(PathBuf);

impl Project {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("autonet-project-{}-{name}", std::process::id()));
        // A leftover from a killed run must not decide this one's result.
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory is writable");
        Self(path)
    }

    fn write(&self, relative: &str, contents: &str) -> &Self {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
        self
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.0.join(relative))
            .unwrap_or_else(|error| panic!("{relative}: {error}"))
    }

    fn exists(&self, relative: &str) -> bool {
        self.0.join(relative).exists()
    }

    fn autonet(&self, arguments: &[&str]) -> Output {
        command(&self.0).args(arguments).output().unwrap()
    }

    fn git(&self, arguments: &[&str]) -> Output {
        StdCommand::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(arguments)
            .current_dir(&self.0)
            .env("PATH", path_with_autonet())
            .output()
            .expect("git is installed")
    }

    fn vite(name: &str) -> Self {
        let project = Self::new(name);
        project.write(
            "package.json",
            r#"{"scripts":{"dev":"vite"},"devDependencies":{"vite":"6.0.0"}}"#,
        );
        project
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn binary() -> PathBuf {
    StdCommand::cargo_bin("autonet")
        .expect("the autonet binary is built")
        .get_program()
        .into()
}

fn path_with_autonet() -> std::ffi::OsString {
    let directory = binary().parent().unwrap().to_path_buf();
    let existing = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(directory).chain(std::env::split_paths(&existing)))
        .unwrap()
}

fn command(directory: &Path) -> StdCommand {
    let mut command = StdCommand::new(binary());
    command.current_dir(directory);
    command.env("HOME", "/nonexistent-autonet-test-home");
    command.env_remove("XDG_CONFIG_HOME");
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
    command.env("NO_COLOR", "1");
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("the process exited normally")
}

/// A manifest that runs AutoNet itself as "the server", which exists on every OS.
fn manifest_running(arguments: &str) -> String {
    format!(
        "command = ['{}', {arguments}]\nport = 3000\n[[output]]\nformat = 'env'\npath = '.env.autonet'\n",
        binary().display()
    )
}

// --- check ---

#[test]
fn check_passes_a_project_with_no_addresses_in_it() {
    let project = Project::new("check-clean");
    project.write("src/api.js", "fetch(AutoNet.url + '/users')\n");

    let output = project.autonet(&["check"]);
    assert_eq!(code(&output), 0, "{}", stdout(&output));
    assert!(stdout(&output).contains("No hardcoded addresses in 1 file."));

    project.write("src/users.js", "fetch(AutoNet.url + '/users')\n");
    let output = project.autonet(&["check"]);
    assert!(stdout(&output).contains("No hardcoded addresses in 2 files."));
}

#[test]
fn check_reports_a_hardcoded_address_by_file_and_line_and_fails() {
    let project = Project::vite("check-hit");
    project.write(
        "src/api.js",
        "// the api\nfetch('http://192.168.1.18:3000')\n",
    );

    let output = project.autonet(&["check"]);
    assert_eq!(code(&output), 1);
    let text = stdout(&output);
    assert!(text.contains("src/api.js:2:"), "{text}");
    assert!(text.contains("192.168.1.18"), "{text}");
    assert!(
        text.contains("import.meta.env.VITE_AUTONET_URL"),
        "says what to use for this project: {text}"
    );
}

#[test]
fn check_accepts_an_address_marked_as_deliberate() {
    let project = Project::new("check-allow");
    project.write(
        "src/router.js",
        "const router = '192.168.1.1' // autonet:allow\n",
    );

    assert_eq!(code(&project.autonet(&["check"])), 0);
}

#[test]
fn check_skips_dependencies_outside_git() {
    let project = Project::new("check-deps");
    project.write("node_modules/x/index.js", "const a = '192.168.1.18'\n");
    project.write("build/out.js", "const a = '192.168.1.18'\n");

    assert_eq!(code(&project.autonet(&["check"])), 0);
}

#[test]
fn check_skips_its_own_generated_file() {
    let project = Project::new("check-generated");
    project.write(
        ".env.local",
        "# Generated by autonet. Do not edit.\nVITE_AUTONET_IP=192.168.1.18\n",
    );

    assert_eq!(code(&project.autonet(&["check"])), 0);
}

#[test]
fn check_json_is_one_document_with_every_finding() {
    let project = Project::new("check-json");
    project.write("a.js", "x = '10.1.2.3'\n");

    let output = project.autonet(&["check", "--json"]);
    assert_eq!(code(&output), 1);
    let document: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(document["files_checked"], 1);
    assert_eq!(document["findings"][0]["path"], "a.js");
    assert_eq!(document["findings"][0]["line"], 1);
    assert_eq!(document["findings"][0]["ip"], "10.1.2.3");
}

#[test]
fn check_staged_outside_git_says_so_rather_than_passing() {
    let project = Project::new("check-staged-nogit");
    let output = project.autonet(&["check", "--staged"]);
    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("Git repository"));
}

#[test]
fn check_staged_reads_the_index_and_not_the_working_tree() {
    let project = Project::new("check-staged");
    project.git(&["init", "-q"]);

    // Staged with an address, then fixed on disk and not re-staged: the commit
    // would still contain it.
    project.write("a.js", "x = '10.1.2.3'\n");
    project.git(&["add", "a.js"]);
    project.write("a.js", "x = AutoNet.ip\n");
    assert_eq!(code(&project.autonet(&["check", "--staged"])), 1);

    // And the other way round: on disk only, so the commit would not.
    project.git(&["add", "a.js"]);
    project.write("a.js", "x = '10.1.2.3'\n");
    assert_eq!(code(&project.autonet(&["check", "--staged"])), 0);
}

#[test]
fn check_in_git_ignores_what_git_ignores() {
    let project = Project::new("check-ignored");
    project.git(&["init", "-q"]);
    project.write(".gitignore", "secret.js\n");
    project.write("secret.js", "x = '10.1.2.3'\n");

    assert_eq!(code(&project.autonet(&["check"])), 0);
}

#[test]
fn check_in_git_sees_a_file_not_added_yet() {
    let project = Project::new("check-untracked");
    project.git(&["init", "-q"]);
    project.write("new.js", "x = '10.1.2.3'\n");

    assert_eq!(code(&project.autonet(&["check"])), 1);
}

// --- init ---

#[test]
fn init_dry_run_reports_the_changes_and_makes_none() {
    let project = Project::vite("init-dry");
    let before = project.entries();

    let output = project.autonet(&["--allow-loopback", "init", "--dry-run"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Would create"), "{text}");
    assert!(text.contains("autonet.toml"), "{text}");
    assert!(text.contains(".env.development.local"), "{text}");
    assert_eq!(project.entries(), before, "a dry run wrote something");
}

#[test]
fn init_writes_a_manifest_with_no_address_and_ignores_the_generated_file() {
    let project = Project::vite("init-real");

    let output = project.autonet(&["--allow-loopback", "init"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));

    let manifest = project.read("autonet.toml");
    assert!(manifest.contains(".env.development.local"), "{manifest}");
    assert!(!manifest.contains("127.0.0.1"), "{manifest}");
    assert!(project
        .read(".gitignore")
        .contains(".env.development.local"));
    assert!(project
        .read(".env.development.local")
        .contains("Generated by autonet"));
}

#[test]
fn a_mobile_app_is_told_about_its_empty_url_until_it_names_a_port() {
    let project = Project::new("init-port");
    project.write("pubspec.yaml", "name: app\n");

    let output = project.autonet(&["--allow-loopback", "init"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).contains("No port is set"), "{output:?}");
    assert!(project
        .read("lib/autonet.g.dart")
        .contains("String? url = null"));

    let manifest = project.read("autonet.toml");
    project.write("autonet.toml", &manifest.replace("# port =", "port ="));

    let output = project.autonet(&["--allow-loopback", "init"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(!stdout(&output).contains("No port is set"), "{output:?}");
    assert!(project.read("lib/autonet.g.dart").contains(":3000"));
}

#[test]
fn init_twice_changes_nothing_the_second_time() {
    let project = Project::vite("init-twice");
    project.git(&["init", "-q"]);

    assert_eq!(code(&project.autonet(&["--allow-loopback", "init"])), 0);
    let manifest = project.read("autonet.toml");
    let ignore = project.read(".gitignore");
    let hook = project.read(".git/hooks/pre-commit");

    let output = project.autonet(&["--allow-loopback", "init"]);
    assert_eq!(code(&output), 0);
    assert!(
        stdout(&output).contains("Already set up"),
        "{}",
        stdout(&output)
    );
    assert_eq!(project.read("autonet.toml"), manifest);
    assert_eq!(project.read(".gitignore"), ignore);
    assert_eq!(project.read(".git/hooks/pre-commit"), hook);
}

#[test]
fn init_refuses_a_generated_path_someone_already_uses_and_writes_nothing() {
    let project = Project::vite("init-owned");
    project.write(".env.development.local", "MY_SECRET=keep-me\n");
    let before = project.entries();

    let output = project.autonet(&["--allow-loopback", "init"]);
    assert_eq!(code(&output), 2);
    assert!(
        stderr(&output).contains("did not write it"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        project.read(".env.development.local"),
        "MY_SECRET=keep-me\n"
    );
    assert_eq!(project.entries(), before);
}

#[test]
fn init_keeps_a_hook_that_was_already_there() {
    let project = Project::vite("init-hook");
    project.git(&["init", "-q"]);
    project.write(".git/hooks/pre-commit", "#!/bin/sh\nnpm test\n");

    assert_eq!(code(&project.autonet(&["--allow-loopback", "init"])), 0);
    let hook = project.read(".git/hooks/pre-commit");
    assert!(hook.contains("npm test"), "{hook}");
    assert!(hook.contains("autonet check --staged"), "{hook}");
}

#[test]
fn init_leaves_a_hook_it_cannot_safely_edit_and_says_what_to_add() {
    let project = Project::vite("init-python-hook");
    project.git(&["init", "-q"]);
    let original = "#!/usr/bin/env python3\nprint('hi')\n";
    project.write(".git/hooks/pre-commit", original);

    let output = project.autonet(&["--allow-loopback", "init"]);
    assert_eq!(code(&output), 0);
    assert_eq!(project.read(".git/hooks/pre-commit"), original);
    assert!(
        stdout(&output).contains("autonet check --staged"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn init_leaves_hooks_alone_when_another_tool_manages_them() {
    let project = Project::vite("init-hookspath");
    project.git(&["init", "-q"]);
    project.git(&["config", "core.hooksPath", ".husky"]);

    let output = project.autonet(&["--allow-loopback", "init"]);
    assert_eq!(code(&output), 0);
    assert!(!project.exists(".husky/pre-commit"));
    assert!(!project.exists(".git/hooks/pre-commit"));
    assert!(
        stdout(&output).contains("core.hooksPath"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn init_has_no_json_form() {
    let project = Project::vite("init-json");
    assert_eq!(code(&project.autonet(&["init", "--json"])), 2);
    assert!(!project.exists("autonet.toml"));
}

#[test]
fn a_broken_package_json_is_reported_and_not_treated_as_no_project() {
    let project = Project::new("init-broken");
    project.write("package.json", "{ not json");

    let output = project.autonet(&["--allow-loopback", "init"]);
    assert_eq!(code(&output), 2);
    assert!(
        stderr(&output).contains("package.json"),
        "{}",
        stderr(&output)
    );
}

#[cfg(unix)]
#[test]
fn the_hook_from_init_stops_a_commit_with_an_address_and_allows_one_without() {
    let project = Project::vite("hook-e2e");
    project.git(&["init", "-q"]);
    assert_eq!(code(&project.autonet(&["--allow-loopback", "init"])), 0);

    project.write("a.js", "x = '192.168.1.18'\n");
    project.git(&["add", "."]);
    let blocked = project.git(&["commit", "-q", "-m", "with an address"]);
    assert!(!blocked.status.success(), "the commit went through");
    assert!(
        stdout(&blocked).contains("a.js:1:") || stderr(&blocked).contains("a.js:1:"),
        "{}{}",
        stdout(&blocked),
        stderr(&blocked)
    );

    project.write("a.js", "x = '192.168.1.18' // autonet:allow\n");
    project.git(&["add", "."]);
    let allowed = project.git(&["commit", "-q", "-m", "deliberate"]);
    assert!(allowed.status.success(), "{}", stderr(&allowed));

    // The generated file is ignored, so an address cannot reach history that way.
    let tracked = stdout(&project.git(&["ls-files"]));
    assert!(!tracked.contains(".env.development.local"), "{tracked}");
}

// --- dev ---

#[test]
fn dev_refuses_a_project_it_does_not_recognise_and_says_what_to_declare() {
    let project = Project::new("dev-unknown");
    let output = project.autonet(&["--allow-loopback", "dev"]);
    assert_eq!(code(&output), 2);
    assert!(
        stderr(&output).contains("autonet.toml"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn dev_will_not_guess_at_a_dev_script_it_cannot_read() {
    let project = Project::new("dev-script");
    project.write(
        "package.json",
        r#"{"scripts":{"dev":"./start.sh && vite"},"devDependencies":{"vite":"6.0.0"}}"#,
    );

    let output = project.autonet(&["--allow-loopback", "dev"]);
    assert_eq!(code(&output), 2);
    assert!(
        stderr(&output).contains("autonet.toml"),
        "{}",
        stderr(&output)
    );
    assert_eq!(project.entries(), ["package.json"], "wrote before refusing");
}

#[test]
fn dev_has_no_json_form() {
    let project = Project::vite("dev-json");
    assert_eq!(code(&project.autonet(&["dev", "--json"])), 2);
}

#[test]
fn dev_never_overwrites_a_file_autonet_did_not_write() {
    let project = Project::vite("dev-owned");
    project.write(".env.development.local", "MY_SECRET=keep-me\n");

    let output = project.autonet(&["--allow-loopback", "dev"]);
    assert_eq!(code(&output), 2);
    assert_eq!(
        project.read(".env.development.local"),
        "MY_SECRET=keep-me\n"
    );
}

#[test]
fn dev_writes_the_file_runs_the_command_and_reports_its_exit() {
    let project = Project::new("dev-run");
    project.write("autonet.toml", &manifest_running("'--version'"));

    let output = project.autonet(&["--allow-loopback", "dev", "--no-qr"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).contains("autonet"), "the command's output");

    let generated = project.read(".env.autonet");
    assert!(generated.contains("Generated by autonet"), "{generated}");
    assert!(generated.contains("AUTONET_PORT=3000"), "{generated}");
    assert!(
        project.exists(".env.autonet"),
        "stays after exit, because the source imports it"
    );
}

#[test]
fn dev_passes_on_the_exit_code_of_a_server_that_failed_by_itself() {
    let project = Project::new("dev-fail");
    // `autonet --no-such-flag` exits 2, on every OS.
    project.write("autonet.toml", &manifest_running("'--no-such-flag'"));

    let output = project.autonet(&["--allow-loopback", "dev", "--no-qr"]);
    assert_eq!(code(&output), 2, "{}", stderr(&output));
}

#[test]
fn dev_reports_a_command_that_does_not_exist() {
    let project = Project::new("dev-missing");
    project.write(
        "autonet.toml",
        "command = ['autonet-no-such-program']\n[[output]]\nformat = 'env'\npath = '.env.autonet'\n",
    );

    let output = project.autonet(&["--allow-loopback", "dev", "--no-qr"]);
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(stderr(&output).contains("autonet-no-such-program"));
}

#[test]
fn dev_warns_about_a_hardcoded_address_and_still_starts() {
    let project = Project::new("dev-warn");
    project.write("autonet.toml", &manifest_running("'--version'"));
    project.write("a.js", "x = '10.1.2.3'\n");

    let output = project.autonet(&["--allow-loopback", "dev", "--no-qr"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stderr(&output).contains("1 hardcoded address"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn dev_says_it_cannot_see_what_a_declared_command_binds() {
    let project = Project::new("dev-declared");
    project.write("autonet.toml", &manifest_running("'--version'"));

    let output = project.autonet(&["--allow-loopback", "dev", "--no-qr"]);
    assert!(stderr(&output).contains("0.0.0.0"), "{}", stderr(&output));
}

#[test]
fn dev_keeps_its_commentary_off_stdout() {
    let project = Project::new("dev-stdout");
    project.write("autonet.toml", &manifest_running("'--version'"));

    let output = project.autonet(&["--allow-loopback", "dev", "--no-qr"]);
    assert!(!stdout(&output).contains("Writing"), "{}", stdout(&output));
}

// --- Windows `.cmd` shims ---

/// `npm` on Windows is `npm.cmd`, which `Command` will not find by itself. Without
/// this, the README's `autonet run -- npm run dev` fails on Windows.
#[cfg(windows)]
#[test]
fn a_cmd_shim_on_path_can_be_run_by_its_bare_name() {
    let project = Project::new("cmd-shim");
    project.write("shim/autonet-shim.cmd", "@echo shim says %1\r\n");

    let existing = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(project.0.join("shim")).chain(std::env::split_paths(&existing)),
    )
    .unwrap();

    let output = command(&project.0)
        .env("PATH", path)
        .args(["--allow-loopback", "run", "--", "autonet-shim", "hello"])
        .output()
        .unwrap();
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("shim says hello"),
        "{}",
        stdout(&output)
    );
}
