//! `isolate`-backed sandbox.
//!
//! Mirrors the Python backend's `init_exec_environment`,
//! `cleanup_exec_environment` and the `_exec` resource-limit handling:
//! boxes live under `/tmp/box/`, at most [`IsolateSandbox::MAX_CONCURRENT`]
//! of them run concurrently, and participant code is executed with the same
//! `isolate` CLI arguments the Python code used.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

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
pub trait Sandbox: Send + Sync {
    /// Host path of the box directory.
    fn box_dir(&self, box_id: &BoxId) -> PathBuf;

    /// Find a free box id and run `isolate --init` on it.
    /// Returns [`SandboxError::NotAvailable`] when the `isolate` binary
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

    /// Run `isolate --cleanup` and remove leftover box data. Best-effort.
    async fn cleanup_box(&self, box_id: &BoxId);
}

/// `isolate`-backed implementation of [`Sandbox`].
#[derive(Debug, Clone)]
pub struct IsolateSandbox {
    /// Resolved `isolate` binary, or `None` when it is not installed.
    binary: Option<PathBuf>,
    /// Directory holding the boxes (Python: `EXEC_PATH = /tmp/box/`).
    exec_path: PathBuf,
    /// Box id prefix (Python: `util.config.box_prefix_id()`, default `1`).
    box_prefix: String,
    /// Max concurrently running boxes.
    max_concurrent: usize,
}

impl Default for IsolateSandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl IsolateSandbox {
    /// Create a sandbox with the Python defaults (`/tmp/box/`, prefix `1`,
    /// max 3 concurrent boxes). Does not fail when `isolate` is missing —
    /// [`SandboxError::NotAvailable`] is returned lazily from [`Sandbox::init_box`].
    pub fn new() -> Self {
        Self {
            binary: find_binary("isolate"),
            exec_path: PathBuf::from("/tmp/box"),
            box_prefix: std::env::var("KSI_BOX_PREFIX").unwrap_or_else(|_| "1".into()),
            max_concurrent: std::env::var("KSI_MAX_CONCURRENT_EXEC")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(MAX_CONCURRENT_EXEC),
        }
    }

    /// Whether the `isolate` binary was found on `PATH`.
    pub fn is_available(&self) -> bool {
        self.binary.is_some()
    }

    /// Host path of the box directory (`/tmp/box/<id>`).
    pub fn box_dir(&self, box_id: &BoxId) -> PathBuf {
        self.exec_path.join(box_id.as_str())
    }

    /// Pick a free box id (Python: `find_free_box_id`).
    fn find_free_box_id(&self) -> Result<BoxId, SandboxError> {
        let existing: Vec<_> = std::fs::read_dir(&self.exec_path)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.file_name().to_string_lossy().starts_with(&self.box_prefix))
                    .collect()
            })
            .unwrap_or_default();

        if existing.len() >= self.max_concurrent {
            return Err(SandboxError::NoFreeBox);
        }

        loop {
            let candidate = format!("{}{:03}", self.box_prefix, rand::random::<u32>() % 1001);
            if !self.exec_path.join(&candidate).exists() {
                return Ok(BoxId(candidate));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn spawn(&self) -> Result<Command, SandboxError> {
        match &self.binary {
            Some(bin) => Ok(Command::new(bin)),
            None => Err(SandboxError::NotAvailable(
                "the `isolate` binary was not found on PATH; code execution is disabled".into(),
            )),
        }
    }
}

/// Search `PATH` for an executable, like the `which` command.
fn find_binary(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(md) = std::fs::metadata(&cand) {
                    if md.permissions().mode() & 0o111 != 0 {
                        return Some(cand);
                    }
                }
            }
            #[cfg(not(unix))]
            return Some(cand);
        }
    }
    None
}

impl Sandbox for IsolateSandbox {
    fn box_dir(&self, box_id: &BoxId) -> PathBuf {
        IsolateSandbox::box_dir(self, box_id)
    }

    async fn init_box(&self) -> Result<BoxId, SandboxError> {
        if !self.exec_path.exists() {
            std::fs::create_dir_all(&self.exec_path)?;
        }
        let box_id = self.find_free_box_id()?;

        let output = self
            .spawn()?
            .arg("-b")
            .arg(box_id.as_str())
            .arg("--init")
            .output()
            .await?;
        if !output.status.success() {
            return Err(SandboxError::IsolateError(format!(
                "isolate --init for box '{}' returned {}\n---- STDOUT ----\n{}\n---- STDERR ----\n{}",
                box_id,
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            )));
        }
        Ok(box_id)
    }

    async fn run_in_box(
        &self,
        box_id: &BoxId,
        cfg: &RunConfig,
        reporter: &mut Reporter,
    ) -> Result<RunOutcome, SandboxError> {
        let limits = &cfg.limits;
        let sandbox_dir = self.box_dir(box_id);

        // Mock /etc/passwd (Python does this in `_exec`).
        let etc_dir = sandbox_dir.join("etc");
        std::fs::create_dir_all(etc_dir.join("alternatives"))?;
        let uid: u32 = box_id
            .as_str()
            .parse::<u32>()
            .map(|n| 60000 + n)
            .unwrap_or(60000);
        std::fs::write(
            etc_dir.join("passwd"),
            format!("tester:x:{uid}:0:Tester:/:\n"),
        )?;

        let mut cmd = self.spawn()?;
        cmd.arg("-b")
            .arg(box_id.as_str())
            .arg(format!("--dir=/etc={}", etc_dir.display()))
            .arg("--dir=/etc/alternatives=/opt/etc/alternatives")
            .arg("--env=PATH")
            .arg("--env=LANG=en_US.UTF-8")
            .arg("-Mmeta")
            // NOTE: like the Python code, the raw parsed byte count is passed
            // to `-m` verbatim (isolate interprets it as KiB).
            .arg(format!("-m{}", limits.mem_bytes))
            .arg(format!("-w{}", limits.total_time_secs))
            .arg(format!("--fsize={}", limits.file_size_bytes / 1000))
            .arg(format!("-q{},{}", limits.blocks, limits.inodes));
        if let Some(cpu) = limits.cpu_time_secs {
            cmd.arg(format!("-t{cpu}"));
        }
        if let Some(stack) = limits.stack_bytes {
            cmd.arg(format!("-k{}", stack / 1000));
        }
        if let Some(procs) = limits.processes {
            cmd.arg(format!("-p{procs}"));
        }
        if limits.share_net {
            cmd.arg("--share-net");
        }
        cmd.arg("-c/box").arg("--run");
        for arg in &cfg.interpreter {
            cmd.arg(arg);
        }
        cmd.arg(&cfg.run_path);

        reporter.push(&format!(
            "Running sandbox: isolate {}\n",
            cmd.as_std()
                .get_args()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        ));

        let stdout_path = sandbox_dir.join("stdout");
        let stderr_path = sandbox_dir.join("stderr");
        let stdout_file = std::fs::File::create(&stdout_path)?;
        let stderr_file = std::fs::File::create(&stderr_path)?;
        let stdin_file = std::fs::File::open(&cfg.stdin_path)?;

        let mut child = cmd
            .stdin(stdin_file)
            .stdout(stdout_file)
            .stderr(stderr_file)
            .current_dir(&sandbox_dir)
            .spawn()?;

        let status = tokio::time::timeout(HARD_QUOTA_TIMEOUT, child.wait())
            .await
            .map_err(|_| {
                let _ = child.start_kill();
                SandboxError::Timeout
            })??;

        let return_code = status.code().unwrap_or(-1);
        reporter.push(&format!("Return code: {return_code}\n"));

        if return_code != 0 {
            if let Ok(stdout) = std::fs::read_to_string(&stdout_path) {
                reporter.push(&format!("Stdout: {stdout}\n"));
            }
            if let Ok(stderr) = std::fs::read_to_string(&stderr_path) {
                reporter.push(&format!("Stderr: {stderr}\n"));
            }
            // 1 = error inside the sandbox (participant's code failed);
            // >1 = isolate itself failed.
            if return_code != 1 {
                return Err(SandboxError::IsolateError(format!(
                    "isolate --run returned code {return_code}"
                )));
            }
        }

        Ok(RunOutcome {
            return_code,
            stdout_path,
            stderr_path,
        })
    }

    async fn cleanup_box(&self, box_id: &BoxId) {
        let sandbox_root = self.box_dir(box_id);
        if sandbox_root.is_dir() {
            if let Ok(mut cmd) = self.spawn() {
                let _ = cmd
                    .arg("-b")
                    .arg(box_id.as_str())
                    .arg("--cleanup")
                    .output()
                    .await;
            }
            // Best-effort removal of leftovers (mirrors Python).
            let _ = std::fs::remove_dir_all(&sandbox_root);
        }
    }
}

/// Convenience: the box content directory (`<box>/box`) that is visible
/// inside the sandbox as `/box`.
pub fn box_content_dir(sandbox_dir: &Path) -> PathBuf {
    sandbox_dir.join("box")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_units() {
        assert_eq!(parse_size("50M"), 50_000_000);
        assert_eq!(parse_size("1K"), 1000);
        assert_eq!(parse_size("2KiB"), 2048);
        assert_eq!(parse_size("100"), 100);
    }

    #[test]
    fn parse_timespan_units() {
        assert_eq!(parse_timespan("5s"), 5.0);
        assert_eq!(parse_timespan("2m"), 120.0);
        assert_eq!(parse_timespan("1h"), 3600.0);
        assert!((parse_timespan("1.5s") - 1.5).abs() < 1e-9);
    }

    #[test]
    fn missing_isolate_reports_not_available() {
        let sb = IsolateSandbox {
            binary: None,
            exec_path: PathBuf::from("/tmp/box"),
            box_prefix: "1".into(),
            max_concurrent: 3,
        };
        assert!(!sb.is_available());
    }
}
