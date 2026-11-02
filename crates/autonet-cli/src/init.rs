//! `autonet init` — set a project up once.
//!
//! Every change is computed before any is made, so a refusal partway through
//! leaves nothing half done, and `--dry-run` reports exactly what a real run would
//! do because they share everything up to the last step.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use autonet_core::project::{
    detect, notes, plan, ProjectFacts, Refusal, MANIFEST_NAME, NO_PORT_NOTE,
};
use autonet_core::scaffold::{gitignore_with, hook_with_check, manifest_text, HookChange};
use autonet_core::select::select;

use crate::autodetect::{read_facts, read_manifest};
use crate::cli::GlobalArgs;
use crate::commands::Context;
use crate::outputs;
use crate::CliError;

/// What to run from a hook AutoNet could not edit itself.
const HOOK_LINE: &str = "autonet check --staged";

/// One file `init` will write.
struct Planned {
    /// Absolute.
    path: PathBuf,
    /// Relative, or otherwise as a person would say it.
    label: String,
    contents: String,
    verb: &'static str,
    executable: bool,
}

/// Set up the project in the current directory.
pub fn init(ctx: &Context, args: &GlobalArgs, dry_run: bool) -> Result<(), CliError> {
    if args.json {
        return Err(CliError::Usage(
            "init has no --json form. Use --dry-run to see what it would change.".into(),
        ));
    }
    let theme = ctx.theme;
    let root = std::env::current_dir()
        .map_err(|error| CliError::Usage(format!("cannot read the current directory: {error}")))?;
    let facts = read_facts(&root)?;
    let manifest = read_manifest(&root)?;

    // The wildcard does not matter here: it is only ever passed through to flags
    // that `init` does not run, and the plan is otherwise the same either way.
    let dev_plan = plan(
        &facts,
        manifest.as_ref(),
        args.port.filter(|port| *port != 0),
        "0.0.0.0",
    )
    .map_err(|Refusal(message)| CliError::Usage(message))?;

    outputs::check_ownership(&root, &dev_plan.outputs)?;

    let kind = detect(&facts);
    if let Some(kind) = kind {
        println!("{} {}", theme.label("Detected"), theme.value(kind.label()));
    }

    let mut planned = Vec::new();

    if manifest.is_none() {
        planned.push(Planned {
            path: root.join(MANIFEST_NAME),
            label: MANIFEST_NAME.to_string(),
            contents: manifest_text(kind, dev_plan.port, &dev_plan.outputs),
            verb: "Create",
            executable: false,
        });
    }

    let gitignore_path = root.join(".gitignore");
    let gitignore = read_optional(&gitignore_path)?;
    if let Some(contents) = gitignore_with(gitignore.as_deref(), &dev_plan.outputs) {
        planned.push(Planned {
            path: gitignore_path,
            label: ".gitignore".to_string(),
            contents,
            verb: if gitignore.is_some() {
                "Update"
            } else {
                "Create"
            },
            executable: false,
        });
    }

    let hook_message = plan_hook(&root, &mut planned)?;

    // Listed with the rest, written through `outputs` below, because that write is
    // the atomic one and this file is the only one a running dev server may read.
    let state = ctx.snapshot()?;
    let selection = select(&state, &ctx.config.selection);
    let values = selection.selected.as_ref().map_or_else(
        || outputs::offline_values(dev_plan.port),
        |selected| outputs::current_values(selected, dev_plan.port),
    );

    for change in &planned {
        let verb = if dry_run {
            format!("Would {}", change.verb.to_lowercase())
        } else {
            change.verb.to_string()
        };
        println!("{} {}", theme.label(&verb), change.label);
        if !dry_run {
            apply(change)?;
        }
    }

    for output in &dev_plan.outputs {
        if dry_run {
            println!("{} {}", theme.label("Would write"), output.path);
            continue;
        }
        let changed = outputs::write(&root, output, &values)
            .map_err(|error| CliError::Usage(format!("cannot write {}: {error}", output.path)))?;
        if changed {
            println!("{} {}", theme.label("Write"), output.path);
        }
    }

    if let Some(message) = hook_message {
        println!("\n{message}");
    }

    let needs_port = dev_plan.port.is_none() && !dev_plan.outputs.is_empty();
    print_notes(ctx, kind, &facts, needs_port);

    if planned.is_empty() && !dry_run {
        println!("\nAlready set up. Run `autonet dev`.");
    } else if !dry_run {
        println!("\nSet up. Run `autonet dev`.");
    }
    Ok(())
}

fn print_notes(
    ctx: &Context,
    kind: Option<autonet_core::project::Kind>,
    facts: &ProjectFacts,
    needs_port: bool,
) {
    let mut notes = notes(kind, facts);
    if needs_port {
        notes.push(NO_PORT_NOTE.to_string());
    }
    if notes.is_empty() {
        return;
    }
    println!(
        "\n{}",
        ctx.theme.heading("AutoNet will not change these for you:")
    );
    for note in notes {
        println!("  {note}");
    }
}

fn apply(change: &Planned) -> Result<(), CliError> {
    let failed =
        |error: io::Error| CliError::Usage(format!("cannot write {}: {error}", change.label));
    if let Some(parent) = change.path.parent() {
        fs::create_dir_all(parent).map_err(failed)?;
    }
    fs::write(&change.path, &change.contents).map_err(failed)?;
    if change.executable {
        autonet_platform::make_executable(&change.path).map_err(failed)?;
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<String>, CliError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CliError::Usage(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

/// Add the pre-commit hook to `planned`, or return what the person has to do.
fn plan_hook(root: &Path, planned: &mut Vec<Planned>) -> Result<Option<String>, CliError> {
    let Some(hooks) = git_line(root, &["rev-parse", "--git-path", "hooks"]) else {
        return Ok(Some(
            "Not a Git repository, so no pre-commit hook was added. Run `autonet check` in CI."
                .to_string(),
        ));
    };

    // Husky and its like own this directory and may regenerate it, so an edit here
    // would disappear without a word. Saying what to add is the honest option.
    if git_line(root, &["config", "core.hooksPath"]).is_some_and(|path| !path.is_empty()) {
        return Ok(Some(format!(
            "core.hooksPath is set, so another tool manages hooks. Add this to its pre-commit:\n\n    {HOOK_LINE}"
        )));
    }

    // `rev-parse` answers relative to where it ran, and is absolute in a worktree.
    let path = root.join(hooks).join("pre-commit");
    let existing = read_optional(&path)?;
    match hook_with_check(existing.as_deref()) {
        HookChange::Write(contents) => {
            planned.push(Planned {
                path,
                label: "Git pre-commit hook".to_string(),
                contents,
                verb: if existing.is_some() { "Update" } else { "Create" },
                executable: true,
            });
            Ok(None)
        }
        HookChange::AlreadyInstalled => Ok(None),
        HookChange::Incompatible => Ok(Some(format!(
            "The pre-commit hook is not a shell script, so it was left alone. Have it run:\n\n    {HOOK_LINE}"
        ))),
    }
}

fn git_line(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
