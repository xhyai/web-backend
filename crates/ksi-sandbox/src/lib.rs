//! Code-execution sandbox built on top of the `isolate` binary.
//!
//! This is a port of the Python backend's `util/programming.py`: it finds a
//! free box id under `/tmp/box/`, enforces a limit on concurrently running
//! boxes, executes merged participant code inside the sandbox with resource
//! limits, runs the module's check script against the produced output, and
//! cleans the box up afterwards.
//!
//! If the `isolate` binary is not installed, every operation fails with
//! [`SandboxError::NotAvailable`] instead of crashing — callers map that to
//! an HTTP 503.

pub mod error;
pub mod isolate;
pub mod programming;
pub mod reporter;

pub use error::SandboxError;
pub use isolate::{parse_size, parse_timespan, BoxId, IsolateSandbox, RunConfig, RunLimits, RunOutcome, Sandbox};
pub use programming::{
    evaluate, run_code, EvalOutcome, ExecOutcome, ProgramSpec,
    RunContext, RunType,
};
pub use reporter::Reporter;
