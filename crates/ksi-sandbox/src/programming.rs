//! Programming-module evaluation pipeline.
//!
//! Port of the Python backend's `util.programming.evaluate` / `run` / `_run`:
//! merges participant code with the module's merge script, executes it inside
//! an [`IsolateSandbox`] box with resource limits, splits the sandbox stdout
//! on the randomized meta tag, runs the check script against the output
//! (evaluations only), and stores the box contents permanently.

use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use tokio::process::Command;

use crate::error::SandboxError;
use crate::isolate::{
    box_content_dir, RunConfig, RunLimits, RunOutcome, Sandbox,
};
use crate::reporter::Reporter;

/// Default meta tag; randomized per run (Python: `DEFAULT_META_TAG`).
pub const DEFAULT_META_TAG: &str = "#KSI_META_OUTPUT_0a859a#";
/// Max bytes of program output kept (Python: `OUTPUT_MAX_LEN = 5000`).
pub const OUTPUT_MAX_LEN: usize = 5000;
/// Name of the optional JSON result file written by check scripts.
pub const RESULT_FILE: &str = "eval.out";
/// Name of the file recording which run a stored box belongs to.
pub const SOURCE_FILE: &str = "source";

/// Which kind of run this is (Python's `run_type` = `'eval'` / `'exec'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunType {
    Eval,
    Exec,
}

impl RunType {
    fn as_str(self) -> &'static str {
        match self {
            RunType::Eval => "eval",
            RunType::Exec => "exec",
        }
    }

    fn store_label(self) -> &'static str {
        match self {
            RunType::Eval => "evaluation",
            RunType::Exec => "execution",
        }
    }
}

/// Parsed `programming` section of a module's `data` JSON.
#[derive(Debug, Clone)]
pub struct ProgramSpec {
    pub default_code: String,
    pub merge_script: PathBuf,
    pub stdin: PathBuf,
    pub check_script: PathBuf,
    pub limits: RunLimits,
}

impl ProgramSpec {
    /// Parse from the module's `data` JSON value. Fails with
    /// [`SandboxError::UnsupportedVersion`] when the version is missing or
    /// its major number is < 2 (mirrors the Python version gate).
    pub fn from_data(data: &serde_json::Value) -> Result<Self, SandboxError> {
        let prog = data
            .get("programming")
            .and_then(|v| v.as_object())
            .ok_or_else(|| {
                SandboxError::UnsupportedVersion("missing `programming` section".into())
            })?;

        let version = prog.get("version").and_then(|v| v.as_str()).unwrap_or("");
        let major: u32 = version.split('.').next().and_then(|p| p.parse().ok()).unwrap_or(0);
        if major < 2 {
            return Err(SandboxError::UnsupportedVersion(format!(
                "unsupported programming version `{version}`"
            )));
        }

        let get_path = |key: &str| -> Result<PathBuf, SandboxError> {
            prog.get(key)
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .ok_or_else(|| SandboxError::MissingFile(format!("`programming.{key}` missing")))
        };

        let limits = prog
            .get("limits")
            .and_then(|v| v.as_object())
            .map(RunLimits::from_map)
            .unwrap_or_else(|| RunLimits::from_map(&serde_json::Map::new()));

        Ok(Self {
            default_code: prog
                .get("default_code")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            merge_script: get_path("merge_script")?,
            stdin: get_path("stdin")?,
            check_script: get_path("check_script")?,
            limits,
        })
    }
}

/// Context identifying a single evaluation / execution run.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub module_id: i64,
    pub user_id: i64,
    /// Evaluation id (`RunType::Eval`) or code-execution id (`RunType::Exec`).
    pub run_id: i64,
    pub run_type: RunType,
    /// Backend's module library directory (`data/module_lib/`).
    pub module_lib_path: PathBuf,
    /// Directory under which finished boxes are stored (`data/exec/`).
    pub store_base: PathBuf,
}

/// Outcome of a full evaluation (merge + run + check script).
#[derive(Debug)]
pub struct EvalOutcome {
    /// `"ok"`, `"nok"` or `"error"`.
    pub result: String,
    pub message: Option<String>,
    pub stdout: String,
    pub score: Option<f64>,
    pub actions: Vec<String>,
    pub cheat: bool,
}

/// Outcome of a bare code run (`/runCode`), without a check script.
#[derive(Debug)]
pub struct ExecOutcome {
    pub stdout: String,
    pub cheat: bool,
}

/// Evaluate participant code: merge, execute in the sandbox, run the check
/// script, store the box. Mirrors Python's `evaluate()`.
pub async fn evaluate<S: Sandbox>(
    sandbox: &S,
    spec: &ProgramSpec,
    ctx: &RunContext,
    code: &str,
    reporter: &mut Reporter,
) -> Result<EvalOutcome, SandboxError> {
    let box_id = sandbox.init_box().await.map_err(|e| {
        if matches!(e, SandboxError::NoFreeBox) {
            reporter.push("Reached limit of concurrent tasks!\n");
        }
        e
    })?;

    let mut isolate_err = false;
    let run_res = async {
        let res = run_merged(sandbox, &box_id, spec, ctx, code, reporter).await;
        match res {
            Ok(r) => Ok(Some(r)),
            Err(SandboxError::IsolateError(_)) => {
                isolate_err = true;
                Err(SandboxError::IsolateError("isolate failed".into()))
            }
            Err(e) => Err(e),
        }
    }
    .await;

    // Store the box unless isolate itself blew up (mirrors Python's finally).
    let store_res = if !isolate_err {
        store_exec(
            sandbox,
            &box_id,
            ctx,
            reporter,
        )
        .await
    } else {
        Ok(())
    };

    sandbox.cleanup_box(&box_id).await;
    store_res?;

    let res = run_res?.expect("run_merged returned Ok(None)");

    if res.cheat {
        tracing::warn!(
            scope = "CHEAT",
            user_id = ctx.user_id,
            module_id = ctx.module_id,
            eval_id = ctx.run_id,
            "tried to cheat with evaluation"
        );
    }

    let (result, message, score, actions, stdout) = if res.return_code == 0 {
        let sandbox_dir = sandbox_box_dir(sandbox, &box_id);
        let check = check_output(
            &sandbox_dir,
            &spec.check_script,
            &sandbox_dir.join("output"),
            &ctx.module_lib_path,
            ctx.user_id,
            reporter,
        )
        .await?;
        let ok = check.success;
        let score = check.score.filter(|_| ok);
        (
            if ok { "ok" } else { "nok" },
            check.message,
            score,
            check.actions,
            res.stdout,
        )
    } else {
        (
            "nok",
            Some(
                "Tvůj kód se nepodařilo spustit, oprav si chyby!".to_string(),
            ),
            None,
            Vec::new(),
            res.stdout,
        )
    };

    // Python: score = check score when within [0, max_points], else max_points
    // when ok, else 0. max_points is applied by the caller; here we just pass
    // the check score through.
    Ok(EvalOutcome {
        result: result.to_string(),
        message,
        stdout,
        score,
        actions,
        cheat: res.cheat,
    })
}

/// Run participant code without a check script. Mirrors Python's `run()`.
pub async fn run_code<S: Sandbox>(
    sandbox: &S,
    spec: &ProgramSpec,
    ctx: &RunContext,
    code: &str,
    reporter: &mut Reporter,
) -> Result<ExecOutcome, SandboxError> {
    let box_id = sandbox.init_box().await.map_err(|e| {
        if matches!(e, SandboxError::NoFreeBox) {
            reporter.push(&format!("{e}\n"));
        }
        e
    })?;

    let mut isolate_err = false;
    let run_res = async {
        match run_merged(sandbox, &box_id, spec, ctx, code, reporter).await {
            Ok(r) => Ok(r),
            Err(SandboxError::IsolateError(_)) => {
                isolate_err = true;
                Err(SandboxError::IsolateError("isolate failed".into()))
            }
            Err(e) => Err(e),
        }
    }
    .await;

    if !isolate_err {
        store_exec(sandbox, &box_id, ctx, reporter).await?;
    }
    sandbox.cleanup_box(&box_id).await;

    let res = run_res?;

    if res.cheat {
        tracing::warn!(
            scope = "CHEAT",
            user_id = ctx.user_id,
            module_id = ctx.module_id,
            exec_id = ctx.run_id,
            "tried to cheat with code execution"
        );
    }

    Ok(ExecOutcome {
        stdout: res.stdout,
        cheat: res.cheat,
    })
}

fn sandbox_box_dir<S: Sandbox>(sandbox: &S, box_id: &crate::isolate::BoxId) -> PathBuf {
    sandbox.box_dir(box_id)
}

/// Internal result of the merge + sandboxed run stage.
struct MergedRun {
    return_code: i32,
    stdout: String,
    cheat: bool,
}

/// Merge participant code and execute it in the box. Mirrors Python's `_run`.
async fn run_merged<S: Sandbox>(
    sandbox: &S,
    box_id: &crate::isolate::BoxId,
    spec: &ProgramSpec,
    ctx: &RunContext,
    code: &str,
    reporter: &mut Reporter,
) -> Result<MergedRun, SandboxError> {
    let sandbox_dir = sandbox_box_dir(sandbox, box_id);
    let raw_code = sandbox_dir.join("raw");
    let merged_code = box_content_dir(&sandbox_dir).join("run");
    let merged_backup = sandbox_dir.join(".run.backup");

    reporter.push(&format!("Saving raw code into {}...\n", raw_code.display()));
    tokio::fs::write(&raw_code, code).await?;

    reporter.push("Merging code\n");
    merge_code(
        &sandbox_dir,
        &spec.merge_script,
        &raw_code,
        &merged_code,
        &ctx.module_lib_path,
        ctx.user_id,
        ctx.run_type,
        reporter,
    )
    .await?;

    reporter.push("Backing-up run file\n");
    tokio::fs::copy(&merged_code, &merged_backup).await?;

    reporter.push("Making files read-only-once\n");
    let (interpreter, meta_tag) = make_read_only_once(&sandbox_dir, reporter).await?;

    reporter.push(&format!("Adjusting meta tag to {meta_tag}\n"));
    reporter.push("Adding honeypot\n");
    let check_cheating = add_honeypot(&sandbox_dir, reporter)?;

    reporter.push("Executing code\n");
    let cfg = RunConfig {
        limits: spec.limits.clone(),
        stdin_path: spec.stdin.clone(),
        run_path: "/box/run".to_string(),
        interpreter,
    };
    let outcome: RunOutcome = sandbox.run_in_box(box_id, &cfg, reporter).await?;
    reporter.push("Execution done\n");

    if !merged_code.exists() {
        reporter.push("Restoring run file from backup\n");
        tokio::fs::rename(&merged_backup, &merged_code).await?;
    } else {
        reporter.push("Deleting backup run file\n");
        let _ = tokio::fs::remove_file(&merged_backup).await;
    }

    let (cheating_detected, message) = check_cheating.await;
    reporter.push(&message);
    if cheating_detected {
        reporter.push("Honeypot was triggered\n");
    }

    let output_path = sandbox_dir.join("output");
    let secret_path = sandbox_dir.join("secret");
    split_stdout(&outcome.stdout_path, &output_path, &secret_path, &meta_tag).await?;

    // Read at most OUTPUT_MAX_LEN bytes of output (plus stderr on failure).
    let stdout = read_capped(&output_path, OUTPUT_MAX_LEN).await?;
    let stdout = if outcome.return_code == 0 {
        stdout
    } else {
        let stderr = read_capped(&outcome.stderr_path, OUTPUT_MAX_LEN).await?;
        format!("{stdout}\n{stderr}")
    };
    let stdout = if stdout.len() >= OUTPUT_MAX_LEN {
        format!("{stdout}\nOutput too long, stripped!\n")
    } else {
        stdout
    };

    Ok(MergedRun {
        return_code: outcome.return_code,
        stdout,
        cheat: cheating_detected,
    })
}

/// Run the module's merge script. Mirrors Python's `_merge`.
#[allow(clippy::too_many_arguments)]
async fn merge_code(
    wd: &Path,
    merge_script: &Path,
    code: &Path,
    code_merged: &Path,
    module_lib: &Path,
    user_id: i64,
    run_type: RunType,
    reporter: &mut Reporter,
) -> Result<(), SandboxError> {
    let abs = |p: &Path| {
        std::path::absolute(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .into_owned()
    };
    let stdout_path = wd.join("merge.stdout");
    let stderr_path = wd.join("merge.stderr");

    reporter.push(&format!(
        "Merging code to {} (cmd: {} {} {} {} {} {})\n",
        code_merged.display(),
        abs(merge_script),
        abs(code),
        abs(code_merged),
        abs(module_lib),
        user_id,
        run_type.as_str()
    ));
    reporter.push(&format!(" * stdout: {}\n", stdout_path.display()));
    reporter.push(&format!(" * stderr: {}\n", stderr_path.display()));

    let stdout_file = std::fs::File::create(&stdout_path)?;
    let stderr_file = std::fs::File::create(&stderr_path)?;
    let status = Command::new(abs(merge_script))
        .arg(abs(code))
        .arg(abs(code_merged))
        .arg(abs(module_lib))
        .arg(user_id.to_string())
        .arg(run_type.as_str())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .current_dir(wd)
        .status()
        .await?;

    if !status.success() {
        reporter.push("\nError: Merge script exited with nonzero return code!\n");
        reporter.push("Stderr:\n");
        if let Ok(stderr) = tokio::fs::read_to_string(&stderr_path).await {
            reporter.push(&stderr);
        }
        return Err(SandboxError::MergeError(
            "Merge script exited with nonzero return code!".into(),
        ));
    }

    if !code_merged.exists() {
        reporter.push("\nError: merge script did not create merged file!\n");
        return Err(SandboxError::MissingFile(
            "Merge script did not create merged file!".into(),
        ));
    }

    // Add the executable flag to the merged code.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(code_merged).await?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        tokio::fs::set_permissions(code_merged, perms).await?;
    }

    Ok(())
}

/// Make the run script delete itself on execution and randomize the meta tag.
/// Returns the shebang interpreter and the meta tag to split stdout on.
/// Mirrors Python's `_box_make_read_only_once`.
async fn make_read_only_once(
    sandbox_dir: &Path,
    reporter: &mut Reporter,
) -> Result<(Vec<String>, String), SandboxError> {
    let content_dir = box_content_dir(sandbox_dir);
    let run_file = content_dir.join("run");

    let content = tokio::fs::read_to_string(&run_file).await?;
    let first_line = content.lines().next().unwrap_or("");
    let mut interpreter: Vec<String> = first_line
        .strip_prefix("#!")
        .map(|s| s.trim().split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    if interpreter.is_empty() {
        return Err(SandboxError::Setup("missing shebang in run script".into()));
    }

    let meta_tag = randomize_meta_tag(&run_file).await?.unwrap_or_else(|| DEFAULT_META_TAG.to_string());

    let last = interpreter.last().cloned().unwrap_or_default();
    if last.contains("python") {
        arm_python_self_destruct(&run_file).await?;
        interpreter.extend(["--".to_string(), "-B".to_string()]);
    } else if last.contains("sh") {
        let content = tokio::fs::read_to_string(&run_file).await?;
        tokio::fs::write(&run_file, format!("rm \"$0\"\n{content}")).await?;
    } else {
        return Err(SandboxError::Setup(format!("unknown interpreter {interpreter:?}")));
    }

    // Arm every other module .py file too, except participant/shared ones.
    let mut entries = tokio::fs::read_dir(&content_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.extension().and_then(|e| e.to_str()) == Some("py")
            && !name.starts_with("participant")
            && !name.ends_with("_shared.py")
        {
            let _ = arm_python_self_destruct(&path).await;
        }
    }

    let _ = reporter;
    Ok((interpreter, meta_tag))
}

/// Replace the default meta tag with a random one; returns the new tag.
async fn randomize_meta_tag(file: &Path) -> Result<Option<String>, SandboxError> {
    let content = tokio::fs::read_to_string(file).await?;
    if !content.contains(DEFAULT_META_TAG) {
        return Ok(None);
    }
    let hex: String = (0..32).map(|_| format!("{:02x}", rand::random::<u8>())).collect();
    let meta_tag = format!("#KSI_META_OUTPUT_{hex}#");
    let content = content.replace(DEFAULT_META_TAG, &meta_tag);
    tokio::fs::write(file, content).await?;
    Ok(Some(meta_tag))
}

/// Prepend `unlink(__file__)` so the script deletes itself when executed.
async fn arm_python_self_destruct(file: &Path) -> Result<(), SandboxError> {
    let content = tokio::fs::read_to_string(file).await?;
    tokio::fs::write(file, format!("from os import unlink\nunlink(__file__)\n{content}")).await?;
    Ok(())
}

/// Honeypot: a file the participant must never read. Returns an async
/// checker yielding `(cheating_detected, message)`.
/// Mirrors Python's `_box_add_honeypot`.
fn add_honeypot(
    sandbox_dir: &Path,
    reporter: &mut Reporter,
) -> Result<impl std::future::Future<Output = (bool, String)> + Send, SandboxError> {
    // Kept obfuscated in source, like the Python original.
    let honeypot_name: String = "txt.noitulos_neddih".chars().rev().collect();
    let msg: String = "eranimes murotazinagro nesalhan lyb inedavdop o sukop otneT"
        .chars()
        .rev()
        .collect();
    let honeypot = box_content_dir(sandbox_dir).join(&honeypot_name);
    if honeypot.exists() {
        return Err(SandboxError::Setup("Honeypot file already exists!".into()));
    }

    let cheating_detected = Arc::new(AtomicBool::new(false));

    #[cfg(unix)]
    let access_time_supported = {
        use std::os::unix::fs::MetadataExt;
        std::fs::write(&honeypot, b"")?;
        let before = std::fs::metadata(&honeypot)?.atime_nsec();
        std::thread::sleep(Duration::from_millis(10));
        // `touch`: bump atime/mtime.
        let now = std::time::SystemTime::now();
        let _ = filetime_set(&honeypot, now);
        let after = std::fs::metadata(&honeypot)?.atime_nsec();
        before != after
    };
    #[cfg(not(unix))]
    let access_time_supported = false;

    enum Honeypot {
        #[allow(dead_code)]
        File { path: PathBuf, atime: i64 },
        Pipe { path: PathBuf, writer: Option<tokio::task::JoinHandle<()>> },
    }

    let state = if access_time_supported {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            reporter.push("Honeypot uses classic file\n");
            std::fs::write(&honeypot, msg.as_bytes())?;
            let atime = std::fs::metadata(&honeypot)?.atime_nsec();
            std::thread::sleep(Duration::from_millis(10));
            Honeypot::File { path: honeypot.clone(), atime }
        }
        #[cfg(not(unix))]
        {
            unreachable!()
        }
    } else {
        reporter.push("Honeypot uses pipe\n");
        let _ = std::fs::remove_file(&honeypot);
        mkfifo(&honeypot)?;
        let flag = cheating_detected.clone();
        let path = honeypot.clone();
        let msg = msg.clone();
        // Blocking writer: `open` blocks until the participant opens the
        // fifo for reading (mirrors the Python multiprocessing writer).
        let writer = tokio::task::spawn_blocking(move || {
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
                use std::io::Write;
                let mut f = f;
                let _ = f.write_all(&msg.as_bytes()[..1]);
                flag.store(true, Ordering::SeqCst);
                let _ = f.write_all(&msg.as_bytes()[1..]);
            }
        });
        std::thread::sleep(Duration::from_millis(10));
        Honeypot::Pipe {
            path: honeypot.clone(),
            writer: Some(writer),
        }
    };

    let flag = cheating_detected.clone();
    Ok(async move {
        match state {
            Honeypot::File { path, atime } => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    std::thread::sleep(Duration::from_millis(10));
                    let now = std::fs::metadata(&path).map(|m| m.atime_nsec()).unwrap_or(atime);
                    let cheated = now != atime;
                    flag.store(cheated, Ordering::SeqCst);
                    let _ = std::fs::remove_file(&path);
                    (
                        cheated,
                        format!("Honeypot check: cheat={cheated} access_time_diff={}\n", now - atime),
                    )
                }
                #[cfg(not(unix))]
                {
                    let _ = std::fs::remove_file(&path);
                    (false, "Honeypot check: unsupported platform\n".to_string())
                }
            }
            Honeypot::Pipe { path, mut writer } => {
                let (cheated, message) = if flag.load(Ordering::SeqCst) {
                    (true, "Honeypot check: the cheating was already triggered\n".to_string())
                } else if let Some(handle) = writer.take() {
                    if !handle.is_finished() {
                        handle.abort();
                        (
                            false,
                            format!(
                                "Honeypot check: the file was not read fully cheat={}\n",
                                flag.load(Ordering::SeqCst)
                            ),
                        )
                    } else {
                        flag.store(true, Ordering::SeqCst);
                        (
                            true,
                            "Honeypot check: the file was read fully cheat=True\n".to_string(),
                        )
                    }
                } else {
                    (false, "Honeypot check: no writer\n".to_string())
                };
                let _ = std::fs::remove_file(&path);
                (cheated, message)
            }
        }
    })
}

/// Set atime+mtime of a file to `time` (portable `touch`).
#[cfg(unix)]
fn filetime_set(path: &Path, time: std::time::SystemTime) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::OpenOptions::new().write(true).open(path)?;
    let tv = libc_timeval(time);
    let times = [tv, tv];
    let ret = unsafe { libc_utimensat(f.as_raw_fd(), times.as_ptr()) };
    if ret == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

#[cfg(unix)]
#[repr(C)]
#[derive(Clone, Copy)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[cfg(unix)]
fn libc_timeval(t: std::time::SystemTime) -> Timespec {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    Timespec {
        tv_sec: d.as_secs() as i64,
        tv_nsec: d.subsec_nanos() as i64,
    }
}

#[cfg(unix)]
unsafe fn libc_utimensat(fd: std::os::unix::io::RawFd, times: *const Timespec) -> i32 {
    extern "C" {
        fn futimens(fd: std::os::unix::io::RawFd, times: *const Timespec) -> i32;
    }
    futimens(fd, times)
}

/// Create a named pipe (fifo) at `path`.
#[cfg(unix)]
fn mkfifo(path: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad fifo path")
    })?;
    let ret = unsafe {
        extern "C" {
            fn mkfifo(pathname: *const i8, mode: u32) -> i32;
        }
        mkfifo(c.as_ptr(), 0o600)
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn mkfifo(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "fifo honeypot not supported on this platform",
    ))
}

/// Split sandbox stdout on the meta tag: lines before the tag go to `output`,
/// lines after it go to `secret` (mirrors Python's post-processing in `_exec`).
async fn split_stdout(
    stdout_path: &Path,
    output_path: &Path,
    secret_path: &Path,
    meta_tag: &str,
) -> Result<(), SandboxError> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let f = tokio::fs::File::open(stdout_path).await?;
    let mut lines = tokio::io::BufReader::new(f).lines();
    let mut out = tokio::fs::File::create(output_path).await?;
    let mut secret = tokio::fs::File::create(secret_path).await?;
    let mut meta_found = false;
    while let Some(line) = lines.next_line().await? {
        if line.contains(meta_tag) {
            meta_found = true;
        } else if meta_found {
            secret.write_all(line.as_bytes()).await?;
            secret.write_all(b"\n").await?;
        } else {
            out.write_all(line.as_bytes()).await?;
            out.write_all(b"\n").await?;
        }
    }
    Ok(())
}

/// Read at most `max` bytes from a file as a lossy string.
async fn read_capped(path: &Path, max: usize) -> Result<String, SandboxError> {
    use tokio::io::AsyncReadExt;
    let f = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return Ok(String::new()),
    };
    let mut buf = Vec::new();
    f.take(max as u64 + 1).read_to_end(&mut buf).await?;
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if s.len() > max {
        s.truncate(max);
    }
    Ok(s)
}

/// Result of running the check script.
struct CheckResult {
    success: bool,
    actions: Vec<String>,
    message: Option<String>,
    score: Option<f64>,
}

/// Run the check script against the sandbox output. Mirrors Python's `_check`.
async fn check_output(
    sandbox_dir: &Path,
    check_script: &Path,
    sandbox_stdout: &Path,
    module_lib: &Path,
    user_id: i64,
    reporter: &mut Reporter,
) -> Result<CheckResult, SandboxError> {
    let abs = |p: &Path| {
        std::path::absolute(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .into_owned()
    };
    let stdout_path = sandbox_dir.join("check.stdout");
    let stderr_path = sandbox_dir.join("check.stderr");

    reporter.push(&format!(
        "Checking output (cmd: {} {} {} {} {})\n",
        abs(check_script),
        abs(sandbox_dir),
        abs(sandbox_stdout),
        abs(module_lib),
        user_id
    ));
    reporter.push(&format!(" * stdout: {}\n", stdout_path.display()));
    reporter.push(&format!(" * stderr: {}\n", stderr_path.display()));

    let stdout_file = std::fs::File::create(&stdout_path)?;
    let stderr_file = std::fs::File::create(&stderr_path)?;
    let status = Command::new(abs(check_script))
        .arg(abs(sandbox_dir))
        .arg(abs(sandbox_stdout))
        .arg(abs(module_lib))
        .arg(user_id.to_string())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .current_dir(sandbox_dir)
        .status()
        .await?;

    let mut actions = Vec::new();
    if let Ok(content) = tokio::fs::read_to_string(&stdout_path).await {
        for line in content.lines() {
            if line.starts_with("action ") {
                actions.push(line.trim().to_string());
            }
        }
    }

    let stderr_len = tokio::fs::metadata(&stderr_path).await.map(|m| m.len()).unwrap_or(0);
    if stderr_len > 0 {
        reporter.push("Check script returned nonempty stderr:\n");
        if let Ok(stderr) = tokio::fs::read_to_string(&stderr_path).await {
            reporter.push(&stderr);
        }
        return Err(SandboxError::CheckError(
            "Check script returned non-empty stderr!".into(),
        ));
    }

    let mut message = None;
    let mut score = None;
    let result_path = sandbox_dir.join(RESULT_FILE);
    if result_path.is_file() {
        if let Ok(data) = tokio::fs::read_to_string(&result_path).await {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                message = json.get("message").and_then(|v| v.as_str()).map(str::to_string);
                score = json
                    .get("score")
                    .and_then(|v| v.as_f64())
                    .map(|s| (s * 10.0).round() / 10.0);
            }
        }
    }

    Ok(CheckResult {
        success: status.success(),
        actions,
        message,
        score,
    })
}

/// Permanently store the box contents under
/// `<store_base>/module_<id>/user_<id>/`, like Python's `store_exec`.
async fn store_exec<S: Sandbox>(
    _sandbox: &S,
    box_id: &crate::isolate::BoxId,
    ctx: &RunContext,
    reporter: &mut Reporter,
) -> Result<(), SandboxError> {
    let src = PathBuf::from("/tmp/box").join(box_id.as_str());
    let dst = ctx
        .store_base
        .join(format!("module_{}", ctx.module_id))
        .join(format!("user_{}", ctx.user_id));

    let src_c = src.clone();
    let dst_c = dst.clone();
    tokio::task::spawn_blocking(move || copy_box(&src_c, &dst_c))
        .await
        .map_err(|e| SandboxError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))??;

    let source = format!("{}\n{}\n", ctx.run_type.store_label(), ctx.run_id);
    tokio::fs::write(dst.join(SOURCE_FILE), source).await?;
    let _ = reporter;
    Ok(())
}

/// Recursive copy of a box directory, skipping sandbox-internal entries
/// (mirrors Python's `shutil.copytree(..., ignore=…)`).
fn copy_box(src: &Path, dst: &Path) -> std::io::Result<()> {
    const IGNORED_DIRS: &[&str] = &["tmp", "root", "etc", "__pycache__"];
    if dst.is_dir() {
        std::fs::remove_dir_all(dst)?;
    }
    std::fs::create_dir_all(dst)?;
    copy_dir_filtered(src, dst, IGNORED_DIRS)
}

fn copy_dir_filtered(src: &Path, dst: &Path, ignored: &[&str]) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if ignored.contains(&name.as_ref()) {
            continue;
        }
        if name.ends_with(".pyc") || name == "hidden_solution.txt" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(name.as_ref());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            std::fs::create_dir_all(&to)?;
            copy_dir_filtered(&from, &to, ignored)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_rejects_old_version() {
        let data = serde_json::json!({
            "programming": {
                "version": "1.0a",
                "default_code": "",
                "merge_script": "m",
                "stdin": "s",
                "check_script": "c",
            }
        });
        assert!(matches!(
            ProgramSpec::from_data(&data),
            Err(SandboxError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn spec_accepts_v2() {
        let data = serde_json::json!({
            "programming": {
                "version": "2.1",
                "default_code": "print(1)",
                "merge_script": "merge.py",
                "stdin": "stdin.txt",
                "check_script": "check.py",
                "limits": {"mem": "100M", "processes": 8, "net": "share"},
            }
        });
        let spec = ProgramSpec::from_data(&data).unwrap();
        assert_eq!(spec.default_code, "print(1)");
        assert_eq!(spec.limits.mem_bytes, 100_000_000);
        assert_eq!(spec.limits.processes, Some(8));
        assert!(spec.limits.share_net);
        // defaults still apply
        assert_eq!(spec.limits.blocks, 100);
    }
}
