//! Code-execution sandbox built on Bubblewrap (`bwrap`), Cauldron-style.
//!
//! This is a port of the Python backend's `util/programming.py`, with the
//! sandboxing approach taken from the MUNI thesis "Automated Evaluation of
//! Student Submissions in Computer Science Correspondence Seminar"
//! (Cauldron): instead of the `isolate` tool it uses `bwrap` with Linux
//! namespaces, dropped capabilities, `setrlimit(2)` resource limits and
//! size-limited tmpfs.
//!
//! If the `bwrap` binary is not installed, every operation fails with
//! [`SandboxError::NotAvailable`] instead of crashing — callers map that to
//! an HTTP 503.

pub mod bwrap;
pub mod common;
pub mod error;
pub mod programming;
pub mod reporter;

pub use bwrap::{BwrapLimits, BwrapSandbox};
pub use common::{parse_size, parse_timespan, BoxId, RunConfig, RunLimits, RunOutcome, Sandbox};
pub use error::SandboxError;
pub use programming::{
    evaluate, run_code, EvalOutcome, ExecOutcome, ProgramSpec,
    RunContext, RunType,
};
pub use reporter::Reporter;
