//! The `autonet` command-line entry point.

#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::doc_markdown)]

mod advertise;
mod cli;
mod commands;
mod doctor;
mod port;
mod qr;
mod render;
mod signal;
mod spawn;
mod url;
mod watch;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::commands::Context;
use crate::render::Theme;

/// CLI exit codes.
mod exit {
    /// No address was selectable.
    pub const NO_ADDRESS: u8 = 1;
    /// A command failed.
    pub const FAILED: u8 = 2;
}

/// Anything that stops a command from completing.
enum CliError {
    /// The selection engine found nothing, and explained why.
    NoAddress(String),
    /// The operating system could not be queried.
    Platform(autonet_platform::PlatformError),
    /// Configuration could not be loaded.
    Config(autonet_core::CoreError),
    /// The command was asked for something that does not exist.
    Usage(String),
    /// A child launched by `run` exited non-zero. Not an AutoNet failure.
    ChildExit(u8),
    /// `doctor` completed and something failed. The checklist is the message.
    Unhealthy,
    /// A streaming command ended for an ordinary reason: the reader closed the
    /// pipe, or the user pressed Ctrl-C.
    ///
    /// Shaped as an error because it travels back up through the same `?` as
    /// the real ones, but it is not one — exit code zero, nothing on stderr.
    /// `autonet watch --json | head -3` is a success, not a broken pipe, and
    /// interrupting a command whose only way to finish is to be interrupted is
    /// not a failure either.
    Stopped,
}

impl CliError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Stopped => 0,
            Self::NoAddress(_) | Self::Unhealthy => exit::NO_ADDRESS,
            Self::Platform(_) | Self::Config(_) | Self::Usage(_) => exit::FAILED,
            Self::ChildExit(code) => *code,
        }
    }

    /// The line to print on stderr, if there is one.
    ///
    /// `ChildExit` deliberately has none. `autonet run -- make test` failing
    /// its tests is the tests failing, and prefixing that with `autonet:`
    /// would blame the launcher for the launched program's verdict.
    ///
    /// `Unhealthy` has none for the same reason: the checklist has already
    /// named every row that failed and said what each one means, and
    /// `autonet: a check failed` on top of that is noise.
    fn message(&self) -> Option<String> {
        match self {
            Self::NoAddress(reason) => Some(format!("no usable address: {reason}")),
            Self::Platform(error) => Some(error.to_string()),
            Self::Config(error) => Some(error.to_string()),
            Self::Usage(message) => Some(message.clone()),
            Self::ChildExit(_) | Self::Unhealthy | Self::Stopped => None,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Keep command output machine-readable.
            if let Some(message) = error.message() {
                eprintln!("autonet: {message}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}

fn run(cli: &Cli) -> Result<(), CliError> {
    let config = cli.global.config().map_err(CliError::Config)?;
    let provider = autonet_platform::provider().map_err(CliError::Platform)?;

    let ctx = Context {
        provider,
        config,
        // JSON never includes terminal styling.
        theme: if cli.global.json {
            Theme::plain()
        } else {
            Theme::detect()
        },
    };

    match cli.command() {
        Command::Status => commands::status(&ctx, &cli.global),
        Command::Ip => commands::ip(&ctx, &cli.global),
        Command::Interfaces => commands::interfaces(&ctx, &cli.global),
        Command::Routes => commands::routes(&ctx, &cli.global),
        Command::Run { command } => spawn::run(&ctx, &cli.global, &command),
        Command::Watch => watch::watch(&ctx, &cli.global),
        Command::Doctor => commands::doctor(&ctx, &cli.global),
        Command::Advertise => advertise::advertise(&ctx, &cli.global),
    }
}
