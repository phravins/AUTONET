//! `autonet` — print the address other devices on your network can reach.
//!
//! The binary is deliberately thin. It parses flags, layers configuration,
//! asks `autonet-platform` for a snapshot, asks `autonet-core` a question about
//! it, and renders the answer. Every decision worth arguing about lives in the
//! core, where it is covered by fixture tests that no amount of Wi-Fi switching
//! can perturb.

#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::must_use_candidate)]
// "AutoNet" is a product name, not code.
#![allow(clippy::doc_markdown)]

mod cli;
mod render;
mod run;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::render::Theme;
use crate::run::Context;

/// Exit codes, which are part of the CLI's contract with shell scripts.
mod exit {
    /// Nothing usable could be selected. Distinct from a real failure: the
    /// machine may simply be offline, which is an answer, not a malfunction.
    pub const NO_ADDRESS: u8 = 1;
    /// AutoNet could not do its job — the OS could not be queried, or the
    /// configuration is invalid.
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
}

impl CliError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::NoAddress(_) => exit::NO_ADDRESS,
            Self::Platform(_) | Self::Config(_) | Self::Usage(_) => exit::FAILED,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::NoAddress(reason) => format!("no usable address: {reason}"),
            Self::Platform(error) => error.to_string(),
            Self::Config(error) => error.to_string(),
            Self::Usage(message) => message.clone(),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Diagnostics go to stderr so that `autonet ip` keeps stdout clean
            // for the one value it exists to produce.
            eprintln!("autonet: {}", error.message());
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
        // JSON is consumed by programs, never read off a terminal, so colour is
        // switched off for it unconditionally rather than left to the tty check.
        theme: if cli.global.json {
            Theme::plain()
        } else {
            Theme::detect()
        },
    };

    match cli.command() {
        Command::Status => run::status(&ctx, &cli.global),
        Command::Ip => run::ip(&ctx, &cli.global),
        Command::Interfaces => run::interfaces(&ctx, &cli.global),
        Command::Routes => run::routes(&ctx, &cli.global),
    }
}
