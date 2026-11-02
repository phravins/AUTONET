//! Finding, stopping and marking the programs AutoNet starts.
//!
//! Everything here differs per operating system and none of it is about the
//! network, but architecture rule 2 keeps `cfg` out of the CLI, so it lives here.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What Windows tries when `PATHEXT` is unset, which is its own documented default.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The program to hand to [`Command::new`] for a name a person typed.
///
/// On Windows, `Command` only ever appends `.exe`, and `npm`, `npx`, `yarn` and
/// `pnpm` are all `.cmd` shims there. So `autonet run -- npm run dev` would report
/// "command not found" for a command that works in the same terminal. Everywhere
/// else the name goes through untouched, because the kernel's own lookup is
/// already right.
pub fn resolve_program(program: &str) -> OsString {
    if cfg!(windows) {
        if let Some(found) = find_program(program) {
            return found.into_os_string();
        }
    }
    OsString::from(program)
}

/// Where `program` would be found on `PATH`, if it would be.
pub fn find_program(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let directories: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let extensions = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| DEFAULT_PATHEXT.to_string())
    } else {
        String::new()
    };
    search(program, &directories, &extensions, Path::is_file)
}

/// The lookup itself, without the environment or the disk.
///
/// `extensions` is a `PATHEXT` value, and empty where the concept does not exist.
/// A name that already has a directory or an extension is left for the operating
/// system, since at that point the person has said exactly which file they mean.
fn search(
    program: &str,
    directories: &[PathBuf],
    extensions: &str,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if program.contains(['/', '\\']) {
        return None;
    }
    let names: Vec<String> = if extensions.is_empty() || Path::new(program).extension().is_some() {
        vec![program.to_string()]
    } else {
        extensions
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(|extension| format!("{program}{extension}"))
            .collect()
    };

    directories.iter().find_map(|directory| {
        names
            .iter()
            .map(|name| directory.join(name))
            .find(|candidate| exists(candidate))
    })
}

/// Ask a process and everything it started to stop, without waiting for it.
///
/// Needed because a signal to the process AutoNet started is not enough for a dev
/// server. `npm run dev` is npm holding a shell holding node, and npm exits on
/// SIGTERM without passing it on, which leaves node running with the port still
/// bound. So the whole tree is signalled at once, listed before any of it dies,
/// because an orphan no longer says whose child it was.
///
/// Best-effort: the processes may already be gone, and the caller follows up with
/// `Child::kill` if this was not enough.
pub fn request_stop(pid: u32) {
    let mut command = if cfg!(windows) {
        // Walks the tree itself. `/F` because Windows has no signal to ask with.
        let mut command = Command::new("taskkill");
        command.args(["/PID", &pid.to_string(), "/T", "/F"]);
        command
    } else {
        let mut command = Command::new("kill");
        command.arg("-TERM").arg(pid.to_string());
        command.args(descendants_of(pid).iter().map(ToString::to_string));
        command
    };
    let _ = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Every running process descended from `pid`, as far as `ps` can say.
fn descendants_of(pid: u32) -> Vec<u32> {
    Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|output| descendants(&String::from_utf8_lossy(&output.stdout), pid))
        .unwrap_or_default()
}

/// The descendants of `root` in a table of `pid ppid` lines.
fn descendants(table: &str, root: u32) -> Vec<u32> {
    let rows: Vec<(u32, u32)> = table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();

    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(parent) = pending.pop() {
        for (pid, ppid) in &rows {
            // Checked against `found` because a table read while processes come
            // and go is not guaranteed to be a tree.
            if *ppid == parent && *pid != root && !found.contains(pid) {
                found.push(*pid);
                pending.push(*pid);
            }
        }
    }
    found
}

/// Make a file runnable, for a Git hook.
///
/// A no-op on Windows, where Git for Windows runs hooks through its own shell
/// whatever their mode.
///
/// # Errors
///
/// Returns the underlying error if the file's mode cannot be read or set.
pub fn make_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        std::fs::set_permissions(path, permissions)
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(program: &str, extensions: &str, files: &[&str]) -> Option<String> {
        let directories = [PathBuf::from("first"), PathBuf::from("second")];
        search(program, &directories, extensions, |path| {
            files.iter().any(|file| Path::new(file) == path)
        })
        .map(|path| path.to_string_lossy().into_owned())
    }

    #[test]
    fn a_cmd_shim_is_found_when_only_the_bare_name_was_typed() {
        // The whole reason this exists: there is no npm.exe to find.
        assert_eq!(
            found("npm", ".COM;.EXE;.CMD", &["second/npm.CMD"]),
            Some("second/npm.CMD".to_string())
        );
    }

    #[test]
    fn extensions_are_tried_in_pathext_order() {
        assert_eq!(
            found("tool", ".EXE;.CMD", &["first/tool.CMD", "first/tool.EXE"]),
            Some("first/tool.EXE".to_string())
        );
    }

    #[test]
    fn an_earlier_directory_beats_a_preferred_extension() {
        // PATH order decides first, or a project's own shim would lose to a
        // system copy of the same tool.
        assert_eq!(
            found("tool", ".EXE;.CMD", &["first/tool.CMD", "second/tool.EXE"]),
            Some("first/tool.CMD".to_string())
        );
    }

    #[test]
    fn a_name_that_already_has_an_extension_is_taken_literally() {
        assert_eq!(
            found("npm.cmd", ".EXE;.CMD", &["first/npm.cmd"]),
            Some("first/npm.cmd".to_string())
        );
        assert_eq!(found("npm.cmd", ".CMD", &["first/npm.cmd.CMD"]), None);
    }

    #[test]
    fn a_path_is_left_for_the_operating_system() {
        assert_eq!(found("./npm", ".CMD", &["first/./npm.CMD"]), None);
        assert_eq!(found("bin\\npm", ".CMD", &["first/bin\\npm.CMD"]), None);
    }

    #[test]
    fn without_pathext_the_bare_name_is_what_is_looked_for() {
        assert_eq!(
            found("python3", "", &["second/python3"]),
            Some("second/python3".to_string())
        );
    }

    #[test]
    fn a_missing_program_is_reported_as_missing() {
        assert_eq!(found("nothing", ".EXE", &[]), None);
    }

    #[test]
    fn the_whole_tree_is_found_however_deep_it_goes() {
        // npm -> sh -> node -> esbuild, beside an unrelated process.
        let table = " 100 1\n 200 100\n 300 200\n 400 300\n 500 1\n";
        let mut found = descendants(table, 100);
        found.sort_unstable();
        assert_eq!(found, [200, 300, 400]);
    }

    #[test]
    fn a_process_with_no_children_has_no_descendants() {
        assert!(descendants(" 100 1\n", 100).is_empty());
    }

    #[test]
    fn a_table_that_is_not_a_tree_does_not_hang_the_walk() {
        assert_eq!(descendants(" 100 200\n 200 100\n", 100), [200]);
    }

    #[test]
    fn lines_that_are_not_two_numbers_are_ignored() {
        assert_eq!(descendants("garbage\n\n 200 100\n x 100\n", 100), [200]);
    }

    #[test]
    fn empty_entries_in_pathext_do_not_match_the_bare_name() {
        assert_eq!(found("tool", ";;.EXE;", &["first/tool"]), None);
    }
}
