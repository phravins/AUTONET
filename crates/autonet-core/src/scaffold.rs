//! What `autonet init` would change in a project, computed without touching it.
//!
//! Each function takes a file's current text and returns the text it should
//! become, or nothing if it is already right. That is what makes `init` safe to
//! run twice and `--dry-run` truthful: both follow from comparing the two.

use std::fmt::Write as _;

use crate::generate::{Format, OutputSpec};
use crate::project::{Kind, Manifest};

/// The opening line of the block `init` adds to a pre-commit hook.
const HOOK_BEGIN: &str = "# >>> autonet >>>";
/// The closing line of that block.
const HOOK_END: &str = "# <<< autonet <<<";

/// The body of that block.
///
/// A missing `autonet` warns rather than blocks. A hook lives in one clone, and
/// stopping a teammate's commit because of a tool they never installed would get
/// it removed. CI is where the check is enforced for everyone.
const HOOK_BODY: &str = r#"if command -v autonet >/dev/null 2>&1; then
    autonet check --staged || exit 1
else
    echo "autonet is not installed, so hardcoded addresses were not checked" >&2
fi"#;

/// Offered in a manifest for a project with no server of its own.
const NO_PORT_COMMENT: &str =
    "\n# Uncomment for the URL of a local API this app calls. Without it, `url` is empty.\n# port = 3000\n";

/// Shells that can run [`HOOK_BODY`].
const COMPATIBLE_SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ash", "ksh"];

/// The outcome of adding the check to a pre-commit hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookChange {
    /// Replace the hook with this text.
    Write(String),
    /// The check is already there.
    AlreadyInstalled,
    /// The hook is not a shell script, so it cannot be edited safely.
    Incompatible,
}

/// Add the check to a pre-commit hook, keeping whatever it already does.
///
/// Placed straight after the shebang, not at the end: an existing hook that ends
/// with `exit 0` would otherwise never reach it.
pub fn hook_with_check(existing: Option<&str>) -> HookChange {
    let block = format!("{HOOK_BEGIN}\n{HOOK_BODY}\n{HOOK_END}\n");

    let Some(existing) = existing.filter(|text| !text.trim().is_empty()) else {
        return HookChange::Write(format!("#!/bin/sh\n{block}"));
    };
    if existing.contains(HOOK_BEGIN) {
        return HookChange::AlreadyInstalled;
    }

    let (first, rest) = existing.split_once('\n').unwrap_or((existing, ""));
    if !first.starts_with("#!") {
        // No shebang means Git runs it with sh.
        return HookChange::Write(format!("#!/bin/sh\n{block}{existing}"));
    }

    let interpreter = first
        .trim_start_matches("#!")
        .split_whitespace()
        .last()
        .and_then(|word| word.rsplit('/').next())
        .unwrap_or("");
    if !COMPATIBLE_SHELLS.contains(&interpreter) {
        return HookChange::Incompatible;
    }
    HookChange::Write(format!("{first}\n{block}{rest}"))
}

/// Add generated files to `.gitignore`, or `None` if they are all there.
///
/// A rooted pattern, because a generated file sits at one known path and an
/// unrooted one could hide a person's file of the same name deeper in the tree.
pub fn gitignore_with(existing: Option<&str>, outputs: &[OutputSpec]) -> Option<String> {
    let existing = existing.unwrap_or("");
    let missing: Vec<String> = outputs
        .iter()
        .map(|output| format!("/{}", output.path.trim_start_matches("./")))
        .filter(|pattern| !existing.lines().any(|line| line.trim() == pattern))
        .collect();
    if missing.is_empty() {
        return None;
    }

    let mut text = existing.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str("# Rewritten by `autonet dev` when the network changes.\n");
    for pattern in missing {
        text.push_str(&pattern);
        text.push('\n');
    }
    Some(text)
}

/// The `autonet.toml` for a project.
///
/// Writes down only what a person might want to change. The command stays with
/// detection, so that a project keeps the flags a framework's next release needs
/// without anyone editing this file.
///
/// # Panics
///
/// Never in practice: every field is a string, a number or a list of those, which
/// TOML can always represent.
pub fn manifest_text(kind: Option<Kind>, port: Option<u16>, outputs: &[OutputSpec]) -> String {
    let mut text = String::from(
        "# Commit this file. It holds no address: `autonet dev` works that out each time.\n",
    );
    if let Some(kind) = kind {
        let _ = writeln!(
            text,
            "# Detected: {}. Set `command` to run something other than its usual dev server.",
            kind.label()
        );
    }
    // Above the serialised part: a TOML key after `[[output]]` would belong to it.
    if port.is_none() {
        text.push_str(NO_PORT_COMMENT);
    }
    let manifest = Manifest {
        port,
        output: outputs.to_vec(),
        ..Manifest::default()
    };
    text.push('\n');
    text.push_str(&toml::to_string(&manifest).expect("a manifest always serialises"));
    text
}

/// A short description of a format, for what `init` prints.
pub fn format_name(format: Format) -> &'static str {
    match format {
        Format::Env => "env",
        Format::Ts => "TypeScript",
        Format::Js => "JavaScript",
        Format::Json => "JSON",
        Format::Dart => "Dart",
        Format::Properties => "properties",
        Format::Xcconfig => "xcconfig",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(change: HookChange) -> String {
        match change {
            HookChange::Write(text) => text,
            other => panic!("expected a write, got {other:?}"),
        }
    }

    fn env_output() -> Vec<OutputSpec> {
        vec![OutputSpec {
            format: Format::Env,
            path: ".env.development.local".into(),
            prefix: Some("VITE_".into()),
        }]
    }

    #[test]
    fn a_missing_hook_is_created_as_a_shell_script() {
        let text = write(hook_with_check(None));
        assert!(text.starts_with("#!/bin/sh\n"));
        assert!(text.contains("autonet check --staged || exit 1"));
    }

    #[test]
    fn an_existing_hook_keeps_its_contents_and_runs_after_the_check() {
        let existing = "#!/bin/bash\nnpm test\nexit 0\n";
        let text = write(hook_with_check(Some(existing)));
        assert!(text.starts_with("#!/bin/bash\n# >>> autonet >>>"));
        assert!(text.ends_with("# <<< autonet <<<\nnpm test\nexit 0\n"));
    }

    #[test]
    fn a_hook_with_an_env_shebang_is_recognised_as_a_shell() {
        let text = write(hook_with_check(Some("#!/usr/bin/env bash\nmake lint\n")));
        assert!(text.contains("autonet check --staged"));
        assert!(text.contains("make lint"));
    }

    #[test]
    fn installing_twice_changes_nothing() {
        let once = write(hook_with_check(None));
        assert_eq!(hook_with_check(Some(&once)), HookChange::AlreadyInstalled);
    }

    #[test]
    fn a_hook_in_another_language_is_left_alone() {
        assert_eq!(
            hook_with_check(Some("#!/usr/bin/env python3\nprint('hi')\n")),
            HookChange::Incompatible
        );
    }

    #[test]
    fn a_hook_with_no_shebang_is_kept_and_given_one() {
        let text = write(hook_with_check(Some("make lint\n")));
        assert!(text.starts_with("#!/bin/sh\n"));
        assert!(text.ends_with("make lint\n"));
    }

    #[test]
    fn gitignore_gets_a_rooted_entry_and_keeps_what_was_there() {
        let text = gitignore_with(Some("node_modules"), &env_output()).unwrap();
        assert!(text.starts_with("node_modules\n\n# Rewritten"));
        assert!(text.ends_with("/.env.development.local\n"));
    }

    #[test]
    fn gitignore_is_created_when_there_is_none() {
        let text = gitignore_with(None, &env_output()).unwrap();
        assert!(text.starts_with("# Rewritten"));
    }

    #[test]
    fn gitignore_is_left_alone_when_the_entry_is_already_there() {
        let once = gitignore_with(None, &env_output()).unwrap();
        assert_eq!(gitignore_with(Some(&once), &env_output()), None);
    }

    #[test]
    fn the_manifest_parses_back_and_holds_no_address() {
        let text = manifest_text(Some(Kind::Vite), Some(5173), &env_output());
        assert!(text.contains("Detected: Vite."));
        assert_eq!(crate::ipscan::scan_text(&text), Vec::new());

        let manifest = Manifest::parse(&text).unwrap();
        assert_eq!(manifest.port, Some(5173));
        assert_eq!(manifest.output, env_output());
        assert_eq!(manifest.command, None, "the command stays with detection");
    }

    #[test]
    fn a_manifest_without_a_port_leaves_the_key_out() {
        let text = manifest_text(None, None, &env_output());
        assert!(!text.lines().any(|line| line.starts_with("port")));
        assert!(text.contains("# port = 3000"));
        assert_eq!(Manifest::parse(&text).expect("it parses").port, None);
    }
}
