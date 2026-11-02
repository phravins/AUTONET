//! `autonet check` — reject IP addresses written by hand into a project.
//!
//! The scanning itself is `autonet_core::ipscan`. This module decides which files
//! to give it and reports what comes back.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use autonet_core::ipscan::{
    is_checked_path, is_skipped_directory, scan_text, Finding, ALLOW_MARKER, MAX_FILE_BYTES,
};
use autonet_core::project::detect;
use serde_json::json;

use crate::autodetect::read_facts;
use crate::cli::GlobalArgs;
use crate::commands::to_json_line;
use crate::CliError;

/// One file's findings.
pub(crate) struct Report {
    /// Relative to the directory it was scanned from.
    pub path: String,
    /// What was found in it.
    pub findings: Vec<Finding>,
}

/// What a scan of a project came back with.
pub(crate) struct Scan {
    /// How many files were actually read.
    pub files_checked: usize,
    /// The files that had something in them.
    pub reports: Vec<Report>,
}

impl Scan {
    /// How many addresses were found in all.
    pub(crate) fn finding_count(&self) -> usize {
        self.reports
            .iter()
            .map(|report| report.findings.len())
            .sum()
    }
}

/// Report hardcoded addresses, and exit 1 if there are any.
pub fn check(args: &GlobalArgs, staged: bool) -> Result<(), CliError> {
    let root = std::env::current_dir()
        .map_err(|error| CliError::Usage(format!("cannot read the current directory: {error}")))?;
    let scan = if staged {
        scan_staged(&root)?
    } else {
        scan_project(&root)
    };

    if args.json {
        print!("{}", to_json_line(&document(&scan)));
    } else {
        print!("{}", render(&scan, &root));
    }

    if scan.reports.is_empty() {
        Ok(())
    } else {
        Err(CliError::Unhealthy)
    }
}

/// Scan the project at `root`, as `autonet check` does with no flags.
pub(crate) fn scan_project(root: &Path) -> Scan {
    let files = git_files(root).unwrap_or_else(|| walk(root));
    let mut scan = Scan {
        files_checked: 0,
        reports: Vec::new(),
    };
    for relative in files {
        if !is_checked_path(&relative) {
            continue;
        }
        let Some(text) = read_text(&root.join(&relative)) else {
            continue;
        };
        record(&mut scan, relative, &text);
    }
    scan
}

fn scan_staged(root: &Path) -> Result<Scan, CliError> {
    let names = git(
        root,
        &[
            "diff",
            "--cached",
            "--name-only",
            "--diff-filter=ACMR",
            "-z",
        ],
    )
    .ok_or_else(|| CliError::Usage("--staged needs a Git repository".to_string()))?;

    let mut scan = Scan {
        files_checked: 0,
        reports: Vec::new(),
    };
    for relative in split_nul(&names) {
        if !is_checked_path(&relative) {
            continue;
        }
        // The index and not the working tree. Otherwise an address removed from
        // the file but still staged would pass, and one added but not staged
        // would stop a commit that does not contain it.
        let Some(bytes) = git(root, &["show", &format!(":{relative}")]) else {
            continue;
        };
        let Some(text) = text_of(bytes) else {
            continue;
        };
        record(&mut scan, relative, &text);
    }
    Ok(scan)
}

fn record(scan: &mut Scan, path: String, text: &str) {
    scan.files_checked += 1;
    let findings = scan_text(text);
    if !findings.is_empty() {
        scan.reports.push(Report { path, findings });
    }
}

/// The project's files according to Git, if this is a repository.
///
/// Untracked files are included. A file not yet added is the likeliest place for
/// an address someone typed today, and this is what `autonet dev` warns from.
fn git_files(root: &Path) -> Option<Vec<String>> {
    let names = git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    Some(split_nul(&names))
}

/// Run Git, and give back its stdout if it succeeded.
fn git(root: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

/// Every file under `root`, outside dependencies and build output, as relative
/// paths with forward slashes.
fn walk(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut pending: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let Ok(entries) = fs::read_dir(root.join(&relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let path = relative.join(&name);
            // `file_type` does not follow links, so a symlink cycle cannot hang this.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !is_skipped_directory(&name.to_string_lossy()) {
                    pending.push(path);
                }
            } else if kind.is_file() {
                files.push(path.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files.sort();
    files
}

/// A file's contents, if it is text of a size a person might have typed into.
fn read_text(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    // One past the limit, to tell a file at the limit from one over it without a
    // separate `stat` that could disagree.
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    text_of(bytes)
}

fn text_of(bytes: Vec<u8>) -> Option<String> {
    if bytes.len() as u64 > MAX_FILE_BYTES || bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn document(scan: &Scan) -> serde_json::Value {
    let findings: Vec<_> = scan
        .reports
        .iter()
        .flat_map(|report| {
            report.findings.iter().map(move |finding| {
                json!({
                    "path": report.path,
                    "line": finding.line,
                    "column": finding.column,
                    "ip": finding.ip,
                })
            })
        })
        .collect();
    json!({
        "schema_version": autonet_core::SCHEMA_VERSION,
        "files_checked": scan.files_checked,
        "findings": findings,
    })
}

fn render(scan: &Scan, root: &Path) -> String {
    use std::fmt::Write as _;

    if scan.reports.is_empty() {
        return format!("No hardcoded addresses in {} files.\n", scan.files_checked);
    }

    let mut out = String::new();
    for report in &scan.reports {
        for finding in &report.findings {
            let _ = writeln!(
                out,
                "{}:{}:{}: {}",
                report.path, finding.line, finding.column, finding.ip
            );
        }
    }

    // Best-effort. The findings stand on their own, and a broken package.json
    // should not hide them.
    let kind = read_facts(root).ok().and_then(|facts| detect(&facts));
    let replacement = kind.map_or_else(
        || "the value `autonet dev` generates".to_string(),
        |kind| kind.usage().to_string(),
    );
    let _ = writeln!(
        out,
        "\n{} hardcoded {}. This stops working when the network changes.",
        scan.finding_count(),
        if scan.finding_count() == 1 {
            "address"
        } else {
            "addresses"
        },
    );
    let _ = writeln!(out, "Use instead: {replacement}");
    let _ = writeln!(
        out,
        "A real fixed address, such as a router's, can stay: add {ALLOW_MARKER} in a comment on that line."
    );
    out
}
