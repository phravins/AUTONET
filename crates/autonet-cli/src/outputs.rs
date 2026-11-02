//! Write the generated address files into a project.
//!
//! The one place AutoNet writes to a project's own files, and limited to the
//! paths the project's plan names. See `docs/adr/0004-project-integration.md`.

use std::fs;
use std::io;
use std::path::Path;

use autonet_core::generate::{is_generated, render, OutputSpec, Values};
use autonet_core::select::SelectedAddress;

use crate::state::{replace_file, temporary_path};
use crate::url::network_url;
use crate::CliError;

/// What a generated file says about the current selection.
pub(crate) fn current_values(selected: &SelectedAddress, port: Option<u16>) -> Values {
    Values {
        ip: selected.ip.to_string(),
        host: selected.url_host(),
        url: port.map(|port| network_url(selected, port, None)),
        port,
        online: true,
    }
}

/// What a generated file says when nothing has ever been selectable.
///
/// Loopback and marked offline, rather than no file: source code imports this, so
/// a missing file is a build failure where an offline flag is a running app. It
/// does work on this machine, and a phone was not going to reach anything anyway.
pub(crate) fn offline_values(port: Option<u16>) -> Values {
    Values {
        ip: "127.0.0.1".to_string(),
        host: "127.0.0.1".to_string(),
        url: port.map(|port| format!("{}://127.0.0.1:{port}", crate::url::SCHEME)),
        port,
        online: false,
    }
}

/// Refuse to go on if any output is a file AutoNet did not write.
///
/// Run over all of them before any is written, so a refusal changes nothing.
/// `.env.development.local` is a name people use for their own secrets, and
/// replacing one would lose them with no way to get them back.
///
/// # Errors
///
/// Returns [`CliError::Usage`] naming the file and what to do about it.
pub(crate) fn check_ownership(root: &Path, outputs: &[OutputSpec]) -> Result<(), CliError> {
    for output in outputs {
        let contents = match fs::read(root.join(&output.path)) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(CliError::Usage(format!(
                    "cannot read {}: {error}",
                    output.path
                )))
            }
        };
        if !is_generated(&String::from_utf8_lossy(&contents)) {
            return Err(CliError::Usage(format!(
                "{path} already exists and AutoNet did not write it, so it will not be overwritten.\n\
                 Move what is in it somewhere else, or choose another path in autonet.toml:\n\n    \
                 [[output]]\n    format = \"...\"\n    path = \"...\"",
                path = output.path
            )));
        }
    }
    Ok(())
}

/// Write one output atomically, and say whether its contents changed.
///
/// An unchanged file is left alone. Its modification time is what a dev server's
/// watcher reacts to, and touching it would reload the app for nothing.
///
/// # Errors
///
/// Returns the underlying error if the file or its directory cannot be written.
pub(crate) fn write(root: &Path, output: &OutputSpec, values: &Values) -> io::Result<bool> {
    let path = root.join(&output.path);
    let contents = render(output, values);
    if fs::read_to_string(&path).is_ok_and(|existing| existing == contents) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    replace_file(&path, &temporary_path(&path, std::process::id()), &contents)?;
    Ok(true)
}
