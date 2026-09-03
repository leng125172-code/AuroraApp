//! Error boundary for the host build tool.

use std::path::PathBuf;

/// An actionable failure produced by `aurora-build`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BuildError {
    /// A filesystem operation failed.
    #[error("{operation} `{path}`: {source}")]
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Original operating-system error.
        source: std::io::Error,
    },

    /// A JSON document could not be parsed.
    #[error("parse JSON `{path}`: {source}")]
    Json {
        /// Affected document.
        path: PathBuf,
        /// Original parser error.
        source: serde_json::Error,
    },

    /// A subprocess could not be started.
    #[error("start `{program}`: {source}")]
    StartProcess {
        /// Executable name.
        program: String,
        /// Original operating-system error.
        source: std::io::Error,
    },

    /// A subprocess returned a non-zero exit code.
    #[error("command failed ({status}): {program}")]
    ProcessFailed {
        /// Executable and arguments.
        program: String,
        /// Portable status description.
        status: String,
    },

    /// Contract or build policy validation failed.
    #[error("{0}")]
    Validation(String),
}

/// Result type used inside the build tool.
pub(crate) type BuildResult<T> = Result<T, BuildError>;
