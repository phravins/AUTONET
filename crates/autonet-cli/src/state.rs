//! The optional state file: the current answer, on disk, for a program that
//! wants to notice a network change without being restarted.
//!
//! `AUTONET_IP`, `AUTONET_HOST` and `AUTONET_URL` are a launch-time snapshot
//! and cannot be anything else — the environment of a running process cannot be
//! rewritten from outside it. `docs/adr/0001-network-change-during-autonet-run.md`
//! accepted that and named a state file as the way out for the programs that
//! do care: a file AutoNet keeps current, which the program re-reads on its own
//! schedule. This module is that file.
//!
//! Two rules govern it, and everything here follows from them.
//!
//! **A reader never sees a half-written file.** Each update is written to a
//! sibling temporary and renamed over the target, which is atomic on POSIX and
//! on Windows. A reader either gets the previous complete document or the next
//! one.
//!
//! **The file exists only while AutoNet is keeping it accurate.** It is removed
//! when the run ends, and removed again if an update ever fails. A stale file
//! that still looks current is worse than no file: absence is the one signal a
//! reader cannot misinterpret.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::commands::to_json_line;
use crate::CliError;

/// A path AutoNet is keeping a current selection document at.
pub(crate) struct StateFile {
    /// Where readers look. Absolute, because it is handed to a child process
    /// that may not share this working directory for the whole of its life.
    path: PathBuf,
    /// Where each update is assembled before it is renamed into place.
    temporary: PathBuf,
}

impl StateFile {
    /// Prepare `path` to be written, creating its directory if it is missing.
    ///
    /// Directories are created because the natural place for this file is a
    /// per-project one — `.autonet/current.json` — and requiring `mkdir` first
    /// would be a step that exists only to be forgotten. Nothing is created
    /// that the given path does not name.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Usage`] if the path cannot be made absolute or its
    /// directory cannot be created.
    pub(crate) fn create(path: &Path) -> Result<Self, CliError> {
        // `absolute` rather than `canonicalize`: the file does not exist yet,
        // so there is nothing to canonicalise, and on Windows canonicalising
        // would hand the child a `\\?\` path for no gain.
        let path = std::path::absolute(path).map_err(|error| failed("resolve", path, &error))?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| failed("create", parent, &error))?;
        }

        Ok(Self {
            temporary: temporary_path(&path, std::process::id()),
            path,
        })
    }

    /// Where readers look.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Replace the file's contents with `document`, atomically.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] if the temporary cannot be written
    /// or the rename fails.
    pub(crate) fn write(&self, document: &serde_json::Value) -> io::Result<()> {
        // One line, terminated, exactly as `--json` emits it: a reader that
        // already parses AutoNet's output does not need a second shape for the
        // same document.
        fs::write(&self.temporary, to_json_line(document))?;

        if let Err(error) = fs::rename(&self.temporary, &self.path) {
            // Otherwise a rename that fails for a durable reason — a
            // cross-device target, a read-only directory — leaves a
            // half-named file next to the one the user asked for on every
            // subsequent change.
            let _ = fs::remove_file(&self.temporary);
            return Err(error);
        }

        Ok(())
    }

    /// Take the file away, because it is no longer being kept current.
    ///
    /// Best-effort and deliberately silent. It is called on the way out of
    /// `run`, including the paths where something has already gone wrong, and
    /// "could not delete a file that may not exist" is not a second failure
    /// worth reporting over the first.
    pub(crate) fn remove(&self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.temporary);
    }
}

/// The sibling path an update is assembled at before being renamed into place.
///
/// A sibling, so the rename stays inside one filesystem — a temporary in
/// `/tmp` would make the rename a cross-device copy, which is not atomic and
/// on Linux is not a rename at all. Stamped with the process id so that two
/// AutoNet processes pointed at one path cannot overwrite each other's partial
/// writes; the final rename still means last-writer-wins, which is the same
/// answer either of them would give.
fn temporary_path(path: &Path, pid: u32) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| OsString::from("autonet-state"), OsString::from);
    name.push(format!(".{pid}.tmp"));
    path.with_file_name(name)
}

/// A path-shaped failure, named so the user can see which path it was.
fn failed(verb: &str, path: &Path, error: &io::Error) -> CliError {
    CliError::Usage(format!(
        "cannot {verb} the state file path {}: {error}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicU32, Ordering};

    use autonet_core::model::{AddressScope, Family, InterfaceKind};
    use autonet_core::select::SelectedAddress;
    use serde_json::Value;

    use super::*;
    use crate::commands::selection_document;

    /// A directory of this test's own, under the system temporary directory.
    ///
    /// Rolled by hand rather than with a crate: one counter and one `remove`
    /// is less to carry than a dependency that only the tests would use.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "autonet-state-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&dir).expect("a scratch directory");
            Self(dir)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn selected() -> SelectedAddress {
        SelectedAddress {
            ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
            family: Family::V4,
            prefix_len: 24,
            scope: AddressScope::Private,
            interface: "wlan0".into(),
            interface_index: 3,
            interface_kind: InterfaceKind::Wireless,
            gateway: None,
            score: 1000,
        }
    }

    /// [`StateFile::create`], panicking with the CLI's own message.
    ///
    /// `CliError` carries no `Debug` — it is a user-facing error whose only
    /// rendering is the line it prints on stderr — so `expect` cannot be used
    /// on it directly.
    fn create(path: &Path) -> StateFile {
        StateFile::create(path)
            .unwrap_or_else(|error| panic!("{}", error.message().unwrap_or_default()))
    }

    fn read(path: &Path) -> Value {
        let text = fs::read_to_string(path).expect("the state file should exist");
        assert!(text.ends_with('\n'), "one document per line, terminated");
        serde_json::from_str(&text).expect("the state file should be one JSON document")
    }

    #[test]
    fn the_document_is_the_one_status_json_emits() {
        // The whole point of the file is that a program can read the same
        // answer from disk that it would get from the CLI. Two shapes for one
        // question would be a bug the day either of them changed.
        let document = selection_document(
            "linux-netlink",
            Some(1_700_000_000),
            Some(&selected()),
            None,
            Some(3000),
        );

        assert_eq!(document["schema_version"], json_number(1));
        assert_eq!(document["platform"], "linux-netlink");
        assert_eq!(document["captured_at"], json_number(1_700_000_000));
        assert_eq!(document["selected"]["ip"], "192.168.1.20");
        assert_eq!(document["selected"]["interface"], "wlan0");
        assert_eq!(document["urls"]["network"], "http://192.168.1.20:3000");
        assert_eq!(document["urls"]["local"], "http://127.0.0.1:3000");
        assert!(
            document.get("error").is_none(),
            "no error while there is an address"
        );
    }

    #[test]
    fn losing_the_address_is_recorded_with_the_selectors_own_reason() {
        // A reader that sees `selected: null` and no reason has to guess
        // between "no network" and "AutoNet is confused", which are different
        // problems with different fixes.
        let document = selection_document(
            "linux-netlink",
            Some(1),
            None,
            Some("no interfaces reported any addresses"),
            Some(3000),
        );

        assert_eq!(document["selected"], Value::Null);
        assert_eq!(document["error"], "no interfaces reported any addresses");
        assert!(
            document.get("urls").is_none(),
            "no address, so no URL to offer"
        );
    }

    #[test]
    fn a_url_needs_a_port_here_exactly_as_it_does_in_the_environment() {
        let document = selection_document("linux-netlink", Some(1), Some(&selected()), None, None);
        assert!(document.get("urls").is_none());
        assert_eq!(document["selected"]["ip"], "192.168.1.20");
    }

    #[test]
    fn a_write_lands_as_one_complete_document() {
        let scratch = Scratch::new();
        let file = create(&scratch.join("current.json"));

        file.write(&selection_document(
            "linux-netlink",
            Some(7),
            Some(&selected()),
            None,
            Some(80),
        ))
        .expect("the write should succeed");

        assert_eq!(read(file.path())["selected"]["ip"], "192.168.1.20");
    }

    #[test]
    fn the_second_write_replaces_the_first_and_leaves_nothing_behind() {
        // The file is rewritten on every change for as long as the child runs.
        // A temporary left next to it each time would litter the user's
        // project directory in proportion to how much their Wi-Fi moved.
        let scratch = Scratch::new();
        let file = create(&scratch.join("current.json"));

        file.write(&selection_document(
            "linux-netlink",
            Some(1),
            Some(&selected()),
            None,
            None,
        ))
        .expect("the first write");
        file.write(&selection_document(
            "linux-netlink",
            Some(2),
            None,
            Some("cable unplugged"),
            None,
        ))
        .expect("the second write");

        let document = read(file.path());
        assert_eq!(document["captured_at"], json_number(2));
        assert_eq!(document["error"], "cable unplugged");

        let left: Vec<_> = fs::read_dir(&scratch.0)
            .expect("the scratch directory")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert_eq!(
            left,
            ["current.json"],
            "the temporary should not survive a write"
        );
    }

    #[test]
    fn a_missing_directory_is_created_rather_than_refused() {
        // `.autonet/current.json` is the shape the documentation recommends,
        // and it names a directory that will not exist the first time.
        let scratch = Scratch::new();
        let file = create(&scratch.join(".autonet").join("current.json"));

        file.write(&selection_document(
            "linux-netlink",
            Some(1),
            Some(&selected()),
            None,
            None,
        ))
        .expect("the write should succeed");
        assert!(file.path().exists());
    }

    #[test]
    fn removing_takes_the_file_away_so_a_reader_sees_absence_not_staleness() {
        let scratch = Scratch::new();
        let file = create(&scratch.join("current.json"));
        file.write(&selection_document(
            "linux-netlink",
            Some(1),
            Some(&selected()),
            None,
            None,
        ))
        .expect("the write");

        file.remove();

        assert!(
            !file.path().exists(),
            "the contract is that absence means 'not being updated'"
        );
    }

    #[test]
    fn removing_a_file_that_was_never_written_is_not_a_failure() {
        // `run` removes on every exit path, including the ones where the first
        // write never happened. Panicking there would turn a reported failure
        // into a crash on top of it.
        let scratch = Scratch::new();
        let file = create(&scratch.join("current.json"));
        file.remove();
        file.remove();
    }

    #[test]
    fn the_path_handed_to_the_child_is_absolute() {
        // The child inherits this directory but may not stay in it. A relative
        // path would resolve differently the moment it did not.
        let scratch = Scratch::new();
        let file = create(&scratch.join("current.json"));
        assert!(file.path().is_absolute());
    }

    #[test]
    fn the_temporary_is_a_sibling_of_the_target() {
        // Not a nicety: a temporary anywhere else makes the rename a
        // cross-device move, which is a copy, which is not atomic.
        let target = Path::new("/srv/app/.autonet/current.json");
        let temporary = temporary_path(target, 4242);

        assert_eq!(temporary.parent(), target.parent());
        assert_eq!(
            temporary.file_name().and_then(|name| name.to_str()),
            Some("current.json.4242.tmp")
        );
    }

    #[test]
    fn two_processes_on_one_path_do_not_share_a_temporary() {
        let target = Path::new("/srv/app/current.json");
        assert_ne!(temporary_path(target, 1), temporary_path(target, 2));
    }

    fn json_number(value: u64) -> Value {
        Value::from(value)
    }
}
