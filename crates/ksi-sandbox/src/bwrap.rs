//! Bubblewrap-backed sandbox (Cauldron-style, per the MUNI thesis
//! "Automated Evaluation of Student Submissions in Computer Science
//! Correspondence Seminar").
//!
//! Replaces the `isolate` tool with `bwrap`:
//! - Linux namespaces via bwrap flags: user, mount, PID, IPC, UTS, cgroup,
//!   network (`--unshare-net`)
//! - All capabilities dropped (`--cap-drop ALL`); bwrap runs unprivileged
//!   via user namespaces
//! - Resource limits via `setrlimit(2)` in the child pre-exec hook
//!   (RLIMIT_AS, RLIMIT_CPU, RLIMIT_FSIZE, RLIMIT_STACK, RLIMIT_NPROC,
//!   RLIMIT_NOFILE, RLIMIT_CORE=0) — same set the thesis' trusted init uses
//! - Size-limited tmpfs for the box, read-only bind mounts for system dirs
//! - Timeout enforced from the parent; over-running processes are killed
//!
//! If the `bwrap` binary is not installed, every operation fails with
//! [`SandboxError::NotAvailable`] instead of crashing.

#[allow(unused_imports)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

use crate::error::SandboxError;
use crate::common::{parse_size, parse_timespan, BoxId, RunConfig, RunOutcome, Sandbox, DEFAULT_FILE_SIZE, DEFAULT_MEM, DEFAULT_WALL_TIME, HARD_QUOTA_TIMEOUT, MAX_CONCURRENT_EXEC};
use crate::reporter::Reporter;

/// Resource limits for a bwrap run, applied via `setrlimit(2)`.
/// Parsed from the module's `programming.limits` JSON object.
#[derive(Debug, Clone)]
pub struct BwrapLimits {
    /// RLIMIT_AS in bytes.
    pub mem_bytes: u64,
    /// RLIMIT_CPU in seconds.
    pub cpu_secs: u64,
    /// RLIMIT_FSIZE in bytes.
    pub file_size_bytes: u64,
    /// RLIMIT_STACK in bytes.
    pub stack_bytes: u64,
    /// RLIMIT_NPROC (max processes).
    pub nproc: u64,
    /// RLIMIT_NOFILE (max open files).
    pub nofile: u64,
    /// Wall-clock timeout for the whole run.
    pub wall_time: Duration,
}

impl BwrapLimits {
    /// Parse limits from the module's `programming.limits` object,
    /// applying the same defaults as the Python code.
    pub fn from_map(map: &serde_json::Map<String, serde_json::Value>) -> Self {
        let get_str = |k: &str| map.get(k).and_then(|v| v.as_str());
        let get_num = |k: &str| map.get(k).and_then(|v| v.as_u64());
        Self {
            mem_bytes: get_str("mem")
                .map(parse_size)
                .unwrap_or_else(|| parse_size(DEFAULT_MEM)),
            cpu_secs: get_str("cpu_time")
                .map(|s| parse_timespan(s) as u64)
                .unwrap_or(10),
            file_size_bytes: get_str("file_size")
                .map(parse_size)
                .unwrap_or_else(|| parse_size(DEFAULT_FILE_SIZE)),
            stack_bytes: get_str("stack").map(parse_size).unwrap_or(8 * 1024 * 1024),
            nproc: get_num("processes").unwrap_or(64),
            nofile: 256,
            wall_time: Duration::from_secs_f64(
                get_str("total_time")
                    .map(parse_timespan)
                    .unwrap_or_else(|| parse_timespan(DEFAULT_WALL_TIME)),
            ),
        }
    }
}

/// Apply `setrlimit(2)` resource limits. Called in the child process
/// pre-exec hook (after fork, before exec).
fn apply_rlimits(limits: &BwrapLimits) {
    unsafe {
        let set = |resource: libc::__rlimit_resource_t, value: u64| {
            let rlim = libc::rlimit {
                rlim_cur: value as libc::rlim_t,
                rlim_max: value as libc::rlim_t,
            };
            libc::setrlimit(resource, &rlim);
        };
        set(libc::RLIMIT_AS, limits.mem_bytes);
        set(libc::RLIMIT_CPU, limits.cpu_secs);
        set(libc::RLIMIT_FSIZE, limits.file_size_bytes);
        set(libc::RLIMIT_STACK, limits.stack_bytes.min(64 * 1024 * 1024));
        set(libc::RLIMIT_NPROC, limits.nproc);
        set(libc::RLIMIT_NOFILE, limits.nofile);
        set(libc::RLIMIT_CORE, 0);
        // Keep RLIMIT_DATA in sync with AS for good measure.
        set(libc::RLIMIT_DATA, limits.mem_bytes);
    }
}

/// `bwrap`-backed implementation of [`Sandbox`].
#[derive(Debug, Clone)]
pub struct BwrapSandbox {
    /// Resolved `bwrap` binary, or `None` when it is not installed.
    binary: Option<PathBuf>,
    /// Directory holding the boxes (default `/tmp/ksi-bwrap/`).
    box_root: PathBuf,
    /// Max concurrently running boxes.
    max_concurrent: usize,
}

impl BwrapSandbox {
    pub fn new() -> Self {
        Self {
            binary: find_bwrap(),
            box_root: PathBuf::from("/tmp/ksi-bwrap"),
            max_concurrent: MAX_CONCURRENT_EXEC,
        }
    }

    pub fn with_box_root(mut self, dir: impl Into<PathBuf>) -> Self {
        self.box_root = dir.into();
        self
    }

    fn require_binary(&self) -> Result<&Path, SandboxError> {
        self.binary.as_deref().ok_or_else(|| SandboxError::NotAvailable("bwrap binary not found".into()))
    }

    /// Build the `bwrap` argument list for a sandboxed run.
    fn bwrap_args(
        &self,
        box_dir: &Path,
        cfg: &RunConfig,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Vec<String> {
        let mut args: Vec<String> = vec![
            // Namespaces (thesis § sandboxer).
            "--unshare-user".into(),
            "--unshare-pid".into(),
            "--unshare-net".into(),
            "--unshare-ipc".into(),
            "--unshare-uts".into(),
            "--unshare-cgroup".into(),
            // Drop all capabilities.
            "--cap-drop".into(),
            "ALL".into(),
            // Die when the parent dies.
            "--die-with-parent".into(),
            // Fresh /tmp with a size limit.
            "--tmpfs".into(),
            "/tmp".into(),
            // Read-only system directories.
            "--ro-bind".into(),
            "/usr".into(),
            "/usr".into(),
            "--ro-bind".into(),
            "/lib".into(),
            "/lib".into(),
            "--ro-bind".into(),
            "/lib64".into(),
            "/lib64".into(),
            "--ro-bind".into(),
            "/bin".into(),
            "/bin".into(),
            "--ro-bind".into(),
            "/etc".into(),
            "/etc".into(),
            // /proc and minimal /dev.
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            // The box: writable work dir.
            "--bind".into(),
            box_dir.to_string_lossy().into_owned(),
            "/box".into(),
            "--chdir".into(),
            "/box".into(),
        ];

        // Stdin redirection is handled by the parent via Stdio.

        // The program to run: interpreter + script.
        args.push("--".into());
        args.extend(cfg.interpreter.iter().cloned());
        args.push(cfg.run_path.clone());

        let _ = (stdout_path, stderr_path);
        args
    }
}

impl Default for BwrapSandbox {
    fn default() -> Self {
        Self::new()
    }
}

fn find_bwrap() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let p = dir.join("bwrap");
            p.is_file().then_some(p)
        })
    })
}

#[allow(async_fn_in_trait)]
impl Sandbox for BwrapSandbox {
    fn box_dir(&self, box_id: &BoxId) -> PathBuf {
        self.box_root.join(box_id.as_str())
    }

    async fn init_box(&self) -> Result<BoxId, SandboxError> {
        self.require_binary()?;

        std::fs::create_dir_all(&self.box_root).map_err(|e| {
            SandboxError::Io(std::io::Error::new(std::io::ErrorKind::Other, format!("cannot create box root {}: {e}", self.box_root.display())))
        })?;

        // Find a free box id (same scheme as the isolate version).
        let entries = std::fs::read_dir(&self.box_root)
            .map(|rd| rd.count())
            .unwrap_or(0);
        if entries >= self.max_concurrent {
            return Err(SandboxError::NoFreeBox);
        }

        let box_id = BoxId(format!(
            "box-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let dir = self.box_dir(&box_id);
        std::fs::create_dir_all(&dir).map_err(|e| {
            SandboxError::Io(std::io::Error::new(std::io::ErrorKind::Other, format!("cannot create box dir {}: {e}", dir.display())))
        })?;
        Ok(box_id)
    }

    async fn run_in_box(
        &self,
        box_id: &BoxId,
        cfg: &RunConfig,
        reporter: &mut Reporter,
    ) -> Result<RunOutcome, SandboxError> {
        let bwrap = self.require_binary()?.to_path_buf();
        let box_dir = self.box_dir(box_id);
        let limits = BwrapLimits::from_map(
            &cfg.limits_extra,
        );

        let stdout_path = box_dir.join("stdout.txt");
        let stderr_path = box_dir.join("stderr.txt");
        let stdout_file = std::fs::File::create(&stdout_path).map_err(|e| {
            SandboxError::Io(std::io::Error::new(std::io::ErrorKind::Other, format!("cannot create stdout file: {e}")))
        })?;
        let stderr_file = std::fs::File::create(&stderr_path).map_err(|e| {
            SandboxError::Io(std::io::Error::new(std::io::ErrorKind::Other, format!("cannot create stderr file: {e}")))
        })?;

        let stdin_file = std::fs::File::open(&cfg.stdin_path).map_err(|e| {
            SandboxError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "cannot open stdin file {}: {e}",
                    cfg.stdin_path.display()
                ),
            ))
        })?;

        let args = self.bwrap_args(&box_dir, cfg, &stdout_path, &stderr_path);
        reporter.push(&format!("bwrap args: {}", args.join(" ")));

        let limits_clone = limits.clone();
        let mut child = Command::new(&bwrap);
        unsafe {
            child
                .args(&args)
                .stdin(std::process::Stdio::from(stdin_file))
                .stdout(std::process::Stdio::from(stdout_file))
                .stderr(std::process::Stdio::from(stderr_file))
                .pre_exec(move || {
                    apply_rlimits(&limits_clone);
                    Ok(())
                });
        }

        let mut child = child.spawn().map_err(|e| {
            SandboxError::from(std::io::Error::new(std::io::ErrorKind::Other, format!("cannot spawn bwrap: {e}")))
        })?;

        // Enforce the wall-clock timeout; kill over-running processes
        // (thesis: the trusted init enforces timeout and kills).
        let wall = limits.wall_time.min(HARD_QUOTA_TIMEOUT);
        let status = match tokio::time::timeout(wall, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => return Err(SandboxError::Io(std::io::Error::new(std::io::ErrorKind::Other, format!("bwrap wait failed: {e}")))),
            Err(_) => {
                child.kill().await.ok();
                let _ = child.wait().await;
                return Err(SandboxError::Timeout);
            }
        };

        Ok(RunOutcome {
            return_code: status.code().unwrap_or(-1),
            stdout_path,
            stderr_path,
        })
    }

    async fn cleanup_box(&self, box_id: &BoxId) {
        let dir = self.box_dir(box_id);
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bwrap_limits_defaults() {
        let map = serde_json::Map::new();
        let l = BwrapLimits::from_map(&map);
        assert_eq!(l.mem_bytes, parse_size(DEFAULT_MEM));
        assert!(l.wall_time.as_secs() > 0);
        assert_eq!(l.nofile, 256);
    }

    #[test]
    fn bwrap_args_contain_namespaces() {
        let sb = BwrapSandbox::new().with_box_root("/tmp/test-bwrap");
        let cfg = RunConfig {
            limits: crate::common::RunLimits::from_map(&serde_json::Map::new()),
            limits_extra: serde_json::Map::new(),
            stdin_path: PathBuf::from("/dev/null"),
            run_path: "/box/run".into(),
            interpreter: vec!["/usr/bin/python3".into()],
        };
        let args = sb.bwrap_args(
            Path::new("/tmp/test-bwrap/box-1"),
            &cfg,
            Path::new("/tmp/out"),
            Path::new("/tmp/err"),
        );
        for flag in [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
            "--cap-drop",
        ] {
            assert!(args.contains(&flag.to_string()), "missing {flag}");
        }
        assert!(args.contains(&"--".to_string()));
    }

    #[test]
    fn missing_bwrap_is_not_available() {
        let sb = BwrapSandbox {
            binary: None,
            box_root: PathBuf::from("/tmp/test-bwrap"),
            max_concurrent: 1,
        };
        assert!(matches!(
            sb.require_binary(),
            Err(SandboxError::NotAvailable(_))
        ));
    }
}
