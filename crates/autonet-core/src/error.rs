//! Error types for the AutoNet core.

use thiserror::Error;

/// Errors produced by the core model, configuration, and selection engine.
///
/// The core performs no I/O against the operating system, so these errors are
/// always about *data* — malformed configuration, an unparsable value, or a
/// network state in which no address can legitimately be selected.
#[derive(Debug, Error)]
pub enum CoreError {
    /// No address survived the selection engine's filters.
    ///
    /// This is a normal, expected outcome (an unplugged laptop, or a machine
    /// with nothing but loopback), which is why it carries a human-readable
    /// explanation rather than being a panic.
    #[error("no usable address found: {reason}")]
    NoAddressFound {
        /// Why nothing was selectable, phrased for a developer reading a terminal.
        reason: String,
    },

    /// A configuration file could not be parsed.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// A configuration file could not be read from disk.
    #[error("could not read configuration file {path}: {source}")]
    ConfigIo {
        /// The path that could not be read.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// A value could not be parsed into the type it names.
    #[error("could not parse {kind} from {value:?}")]
    Parse {
        /// What we were trying to build (for example `"IpNetwork"`).
        kind: &'static str,
        /// The offending input.
        value: String,
    },
}

/// Convenience alias for results produced by the core.
pub type Result<T> = std::result::Result<T, CoreError>;
