//! Error type for sandbox operations.

use std::fmt;

/// Errors that can occur while preparing or running a sandbox box.
#[derive(Debug)]
pub enum SandboxError {
    /// The `isolate` binary is not installed / not executable.
    /// Callers must translate this into a 503-style response, not a crash.
    NotAvailable(String),
    /// The limit of concurrently running boxes was reached
    /// (mirrors Python's `ENoFreeBox`).
    NoFreeBox,
    /// `isolate` itself failed (bad return code on `--init`/`--run`).
    IsolateError(String),
    /// The merge script failed.
    MergeError(String),
    /// The check script failed (non-empty stderr).
    CheckError(String),
    /// The check/merge script is missing or a box file is absent.
    MissingFile(String),
    /// Box setup failed (missing shebang, unknown interpreter, …).
    Setup(String),
    /// Execution exceeded the hard quota.
    Timeout,
    /// The module's `programming` spec is malformed or its version is
    /// unsupported by this backend.
    UnsupportedVersion(String),
    /// Underlying I/O error.
    Io(std::io::Error),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SandboxError::NotAvailable(msg) => write!(f, "sandbox unavailable: {msg}"),
            SandboxError::NoFreeBox => {
                write!(f, "reached limit of concurrent sandboxed executions")
            }
            SandboxError::IsolateError(msg) => write!(f, "isolate error: {msg}"),
            SandboxError::MergeError(msg) => write!(f, "merge error: {msg}"),
            SandboxError::CheckError(msg) => write!(f, "check error: {msg}"),
            SandboxError::MissingFile(msg) => write!(f, "missing file: {msg}"),
            SandboxError::Setup(msg) => write!(f, "sandbox setup error: {msg}"),
            SandboxError::Timeout => write!(f, "execution timed out"),
            SandboxError::UnsupportedVersion(msg) => {
                write!(f, "unsupported programming version: {msg}")
            }
            SandboxError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for SandboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SandboxError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SandboxError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::NotFound {
            SandboxError::NotAvailable(format!("failed to spawn `isolate`: {e}"))
        } else {
            SandboxError::Io(e)
        }
    }
}
