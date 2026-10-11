//! Shared sandbox types: box IDs, resource limits, run configuration,
//! and the [`Sandbox`] trait implemented by sandbox backends.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::SandboxError;
use crate::reporter::Reporter;

/// Maximum concurrently running boxes (Python: `MAX_CONCURRENT_EXEC = 3`).
pub const MAX_CONCURRENT_EXEC: usize = 3;

/// Hard timeout after which the sandboxed subprocess is killed
/// (Python: `HARD_QUOTA_TIMEOUT = 3600`).
pub const HARD_QUOTA_TIMEOUT: Duration = Duration::from_secs(3600);

// Default quotas for the sandbox (Python: QUOTA_MEM etc.).
pub const DEFAULT_MEM: &str = "50M";
pub const DEFAULT_WALL_TIME: &str = "5s";
pub const DEFAULT_FILE_SIZE: &str = "50M";
pub const DEFAULT_BLOCKS: u64 = 100;
pub const DEFAULT_INODES: u64 = 100;

/// Identifier of an initialized sandbox box, e.g. `1007`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BoxId(pub String);

impl BoxId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BoxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Resource limits for a single sandboxed run, parsed from the module's
/// `programming.limits` JSON object (with Python's defaults filled in).
#[derive(Debug, Clone)]
pub struct RunLimits {
    /// Memory limit as a raw byte count, passed to `isolate -m` verbatim
    /// (Python passes `str(parse_size(mem))`).
    pub mem_bytes: u64,
    /// Wall-clock limit in seconds (`-w`).
    pub total_time_secs: f64,
    /// Max file size in bytes (`--fsize` gets `bytes // 1000`).
    pub file_size_bytes: u64,
    /// Disk quota blocks (`-q`).
    pub blocks: u64,
    /// Disk quota inodes (`-q`).
    pub inodes: u64,
    /// CPU time limit in seconds (`-t`, optional).
    pub cpu_time_secs: Option<f64>,
    /// Stack size in bytes (`-k` gets `bytes // 1000`, optional).
    pub stack_bytes: Option<u64>,
    /// Max processes (`-p`, optional).
    pub processes: Option<u64>,
    /// Whether the sandbox shares the host network (`--share-net`).
    pub share_net: bool,
}

impl RunLimits {
    /// Parse limits from the module's `programming.limits` object,
    /// applying the same defaults as the Python code.
    pub fn from_map(map: &serde_json::Map<String, serde_json::Value>) -> Self {
        let get_str = |k: &str| map.get(k).and_then(|v| v.as_str());
        let get_num = |k: &str| map.get(k).and_then(|v| v.as_u64());
        Self {
            mem_bytes: get_str("mem")
                .map(parse_size)
                .unwrap_or_else(|| parse_size(DEFAULT_MEM)),
            total_time_secs: get_str("total_time")
                .map(parse_timespan)
                .unwrap_or_else(|| parse_timespan(DEFAULT_WALL_TIME)),
            file_size_bytes: get_str("file_size")
                .map(parse_size)
                .unwrap_or_else(|| parse_size(DEFAULT_FILE_SIZE)),
            blocks: get_num("blocks").unwrap_or(DEFAULT_BLOCKS),
            inodes: get_num("inodes").unwrap_or(DEFAULT_INODES),
            cpu_time_secs: get_str("cpu_time").map(parse_timespan),
            stack_bytes: get_str("stack").map(parse_size),
            processes: get_num("processes"),
            share_net: map.get("net").and_then(|v| v.as_str()) == Some("share"),
        }
    }
}

/// Parse a human-friendly size like `50M` into bytes (SI units, like
/// Python's `humanfriendly.parse_size`).
pub fn parse_size(s: &str) -> u64 {
    let s = s.trim().to_ascii_uppercase().replace(' ', "");
    let units: &[(&str, u64)] = &[
        ("KIB", 1024),
        ("MIB", 1024 * 1024),
        ("GIB", 1024 * 1024 * 1024),
        ("TIB", 1024 * 1024 * 1024 * 1024),
        ("KB", 1000),
        ("MB", 1000 * 1000),
        ("GB", 1000 * 1000 * 1000),
        ("TB", 1000 * 1000 * 1000 * 1000),
        ("K", 1000),
        ("M", 1000 * 1000),
        ("G", 1000 * 1000 * 1000),
        ("T", 1000 * 1000 * 1000 * 1000),
        ("B", 1),
    ];
    for (suffix, mult) in units {
        if let Some(num) = s.strip_suffix(suffix) {
            if let Ok(n) = num.parse::<f64>() {
                return (n * *mult as f64) as u64;
            }
        }
    }
    s.parse::<f64>().map(|n| n as u64).unwrap_or(0)
}

/// Parse a human-friendly timespan like `5s` or `2m` into seconds
/// (like Python's `humanfriendly.parse_timespan`).
pub fn parse_timespan(s: &str) -> f64 {
    let s = s.trim().to_ascii_lowercase().replace(' ', "");
    let units: &[(&str, f64)] = &[
        ("seconds", 1.0),
        ("second", 1.0),
        ("secs", 1.0),
        ("sec", 1.0),
        ("minutes", 60.0),
        ("minute", 60.0),
        ("mins", 60.0),
        ("min", 60.0),
        ("hours", 3600.0),
        ("hour", 3600.0),
        ("days", 86400.0),
        ("day", 86400.0),
        ("weeks", 604800.0),
        ("week", 604800.0),
        ("s", 1.0),
        ("m", 60.0),
        ("h", 3600.0),
        ("d", 86400.0),
        ("w", 604800.0),
    ];
    for (suffix, mult) in units {
        if let Some(num) = s.strip_suffix(suffix) {
            if let Ok(n) = num.parse::<f64>() {
                return n * mult;
            }
        }
    }
    s.parse::<f64>().unwrap_or(0.0)
}

/// Configuration for one sandboxed run (`_exec` in Python).
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Resource limits for the run.
    pub limits: RunLimits,
    /// Raw `programming.limits` map (used by the bwrap backend for
    /// limits that have no `RunLimits` equivalent).
    pub limits_extra: serde_json::Map<String, serde_json::Value>,
    /// Path to the stdin file (module's `programming.stdin`).
    pub stdin_path: PathBuf,
    /// In-box path of the merged participant program (Python: `/box/run`).
    pub run_path: String,
    /// Shebang interpreter of the merged program, e.g. `["/usr/bin/python3", "--", "-B"]`.
    pub interpreter: Vec<String>,
}

/// Outcome of a sandboxed run.
#[derive(Debug)]
pub struct RunOutcome {
    /// Process return code (1 means the participant's code failed;
    /// anything else is an isolate error and becomes [`SandboxError::IsolateError`]).
    pub return_code: i32,
    /// Host path of the captured stdout file.
    pub stdout_path: PathBuf,
    /// Host path of the captured stderr file.
    pub stderr_path: PathBuf,
}

/// Sandbox lifecycle: initialize a box, run code in it, clean it up.
///
/// Uses native `async fn` in the trait (rather than `async_trait`) since the
/// toolchain supports it; the lint below is silenced deliberately.
#[allow(async_fn_in_trait)]
pub trait Sandbox: Send + Sync {
    /// Host path of the box directory.
    fn box_dir(&self, box_id: &BoxId) -> PathBuf;

    /// Find a free box id and initialize it.
    /// Returns [`SandboxError::NotAvailable`] when the sandbox backend
    /// is missing and [`SandboxError::NoFreeBox`] when the concurrency
    /// limit is reached.
    async fn init_box(&self) -> Result<BoxId, SandboxError>;

    /// Run the merged program inside an initialized box with resource limits.
    async fn run_in_box(
        &self,
        box_id: &BoxId,
        cfg: &RunConfig,
        reporter: &mut Reporter,
    ) -> Result<RunOutcome, SandboxError>;

    /// Remove the box and its data. Best-effort.
    async fn cleanup_box(&self, box_id: &BoxId);
}

/// Convenience: the box content directory (`<box>/box`) that is visible
/// inside the sandbox as `/box`.
pub fn box_content_dir(sandbox_dir: &Path) -> PathBuf {
    sandbox_dir.join("box")
}
