//! Content processing for task deploys.
//!
//! Faithful port of `util/admin/taskDeploy.py`'s `process_task` pipeline:
//! `task.json` metadata, assignment/solution markdown → HTML (via the
//! `pandoc` binary, same flags `pypandoc` used), icon copying including
//! auto-generated `correcting.svg`/`done.svg` badge variants, data
//! directory copies, and per-module processing.
//!
//! Pure helpers (custom-tag formatting, pseudocode/collapse rewriting,
//! the prerequisities expression parser, icon variant generation) are
//! unit-tested below; the pandoc/git/filesystem/DB parts run in the
//! background deploy worker spawned from `POST /admin/atasks/{id}/deploy`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::future::Future;

use chrono::{Local, NaiveDateTime, TimeZone};
use serde_json::Value;
use sqlx::SqlitePool;

/// Paths used by the deploy pipeline. Overridable via environment variables
/// so integration tests can point the deploy at throwaway directories.
#[derive(Debug, Clone)]
pub struct DeployPaths {
    /// Git checkout of the seminar repository (`data/seminar/`).
    pub seminar: PathBuf,
    /// Backend data directory (`data/`).
    pub data_dir: PathBuf,
    /// Directory holding the `ksi-*` lock files (`/var/lock`).
    pub lock_dir: PathBuf,
}

impl DeployPaths {
    pub fn from_env() -> Self {
        Self {
            seminar: std::env::var("KSI_GIT_SEMINAR_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("data/seminar/")),
            data_dir: std::env::var("KSI_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("data")),
            lock_dir: std::env::var("KSI_LOCK_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/var/lock")),
        }
    }

    pub fn deploy_log(&self) -> PathBuf {
        self.data_dir.join("deploy.log")
    }

    pub fn task_new_lock(&self) -> PathBuf {
        self.lock_dir.join("ksi-task-new")
    }

    pub fn task_deploy_lock(&self) -> PathBuf {
        self.lock_dir.join("ksi-task-deploy")
    }

    pub fn task_merge_lock(&self) -> PathBuf {
        self.lock_dir.join("ksi-task-merge")
    }

    pub fn wave_diff_lock(&self) -> PathBuf {
        self.lock_dir.join("ksi-wave-diff")
    }
}

pub fn backend_url() -> String {
    std::env::var("BACKEND_URL")
        .unwrap_or_else(|_| "https://rest.ksi.fi.muni.cz".to_string())
}

/// Append-only deploy log (`data/deploy.log` in Python).
#[derive(Debug, Clone)]
pub struct DeployLog {
    path: PathBuf,
}

impl DeployLog {
    pub fn create(path: &Path, task_id: i64) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, format!("{task_id}\n"))?;
        Ok(Self { path: path.to_path_buf() })
    }

    pub fn log(&self, text: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{text}");
        }
    }
}

/// Mutable state threaded through the deploy (mirrors Python's module-level
/// `eval_public` global).
pub struct DeployCtx<'a> {
    pub db: &'a SqlitePool,
    pub paths: DeployPaths,
    pub backend_url: String,
    pub log: DeployLog,
    pub eval_public: bool,
    pub mangled_datadir: String,
    pub mangled_soldir: String,
}

#[derive(Debug)]
pub struct DeployFailure(pub String);

impl std::fmt::Display for DeployFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DeployFailure {}

pub type DeployResult<T> = Result<T, DeployFailure>;

fn fail(msg: impl Into<String>) -> DeployFailure {
    DeployFailure(msg.into())
}

// ---------------------------------------------------------------------------
// pandoc
// ---------------------------------------------------------------------------

/// `pypandoc.convert_text(source, 'html5', format='markdown+smart',
/// extra_args=['--mathjax', '--email-obfuscation=none', '--wrap=none'])`
/// — shelled out to the `pandoc` binary with identical flags.
pub fn parse_pandoc(source: &str) -> DeployResult<String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = std::process::Command::new("pandoc")
        .args([
            "-f",
            "markdown+smart",
            "-t",
            "html5",
            "--mathjax",
            "--email-obfuscation=none",
            "--wrap=none",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(format!("cannot run pandoc: {e}")))?;

    child
        .stdin
        .as_mut()
        .ok_or_else(|| fail("pandoc stdin unavailable"))?
        .write_all(source.as_bytes())
        .map_err(|e| fail(format!("cannot write to pandoc stdin: {e}")))?;

    let out = child
        .wait_with_output()
        .map_err(|e| fail(format!("cannot read pandoc output: {e}")))?;
    if !out.status.success() {
        return Err(fail(format!(
            "pandoc failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    String::from_utf8(out.stdout).map_err(|e| fail(format!("pandoc output is not UTF-8: {e}")))
}

// ---------------------------------------------------------------------------
// HTML/text helpers (mirrors the `parse_*`/regex helpers in taskDeploy.py)
// ---------------------------------------------------------------------------

/// Replaces custom `<ksi-*>` tags with `<div class="ksi-custom ...">`.
pub fn format_custom_tags(source: &str) -> String {
    let mut out = source.to_string();
    for tag in ["ksi-tip"] {
        out = replace_tag_open_ci(&out, tag, &format!("<div class=\"ksi-custom {tag}\""));
        out = replace_tag_close_ci(&out, tag, "</div>");
    }
    out
}

fn replace_tag_open_ci(haystack: &str, tag: &str, replacement_open: &str) -> String {
    // Case-insensitive `<tag ...>` -> `<div class="...` keeping attributes.
    let lower = haystack.to_lowercase();
    let needle = format!("<{tag}");
    let mut result = String::with_capacity(haystack.len());
    let mut rest = haystack;
    let mut rest_lower = lower.as_str();
    while let Some(pos) = rest_lower.find(&needle) {
        // Ensure it is really a tag open (next char is whitespace, '>' or '/').
        let after = rest_lower[pos + needle.len()..].chars().next();
        let is_tag = match after {
            Some('>') | Some('/') => true,
            Some(c) => c.is_whitespace(),
            None => false,
        };
        if !is_tag {
            result.push_str(&rest[..pos + 1]);
            rest = &rest[pos + 1..];
            rest_lower = &rest_lower[pos + 1..];
            continue;
        }
        result.push_str(&rest[..pos]);
        result.push_str(replacement_open);
        rest = &rest[pos + needle.len()..];
        rest_lower = &rest_lower[pos + needle.len()..];
    }
    result.push_str(rest);
    result
}

fn replace_tag_close_ci(haystack: &str, tag: &str, replacement: &str) -> String {
    let lower = haystack.to_lowercase();
    let needle = format!("</{tag}>");
    let mut result = String::with_capacity(haystack.len());
    let mut rest = haystack;
    let mut rest_lower = lower.as_str();
    while let Some(pos) = rest_lower.find(&needle) {
        result.push_str(&rest[..pos]);
        result.push_str(replacement);
        rest = &rest[pos + needle.len()..];
        rest_lower = &rest_lower[pos + needle.len()..];
    }
    result.push_str(rest);
    result
}

/// `<h2>` → `<h3>`, `<h3>` → `<h4>`, … (mirrors `replace_h`).
pub fn replace_h(source: &str) -> String {
    source
        .replace("<h4", "<h5")
        .replace("</h4>", "</h5>")
        .replace("<h3", "<h4")
        .replace("</h3>", "</h4>")
        .replace("<h2", "<h3")
        .replace("</h2>", "</h3>")
        .replace("<h1", "<h3")
        .replace("</h1>", "</h3>")
}

/// Adds `class='table table-striped'` to every `<table>`.
pub fn add_table_class(source: &str) -> String {
    source.replace("<table>", "<table class='table table-striped'>")
}

/// Rewrites `../data/`, `data/` and `data_solution/` links to backend URLs
/// (mirrors `change_links`).
pub fn change_links(task_id: i64, mangled_datadir: &str, mangled_soldir: &str, backend_url: &str, source: &str) -> String {
    let mut res = source.to_string();
    let replacements = [
        (
            "data_solution/",
            format!("{backend_url}/taskContent/{task_id}/{mangled_soldir}/"),
        ),
        (
            "data/",
            format!("{backend_url}/taskContent/{task_id}/{mangled_datadir}/"),
        ),
    ];
    for (fragment, url) in replacements {
        for prefix in ["(../", "(", "\""] {
            res = res.replace(&format!("{prefix}{fragment}"), &format!("{prefix}{url}"));
        }
    }
    res
}

fn bold_keywords(source: &str, keywords: &[&str]) -> String {
    let mut out = source.to_string();
    for kw in keywords {
        out = out.replace(kw, &format!("**{kw}**"));
    }
    out
}

/// Rewrites one `<ksi-pseudocode>…</ksi-pseudocode>` block (mirrors
/// `one_ksi_pseudocode`).
fn one_ksi_pseudocode(inner: &str) -> String {
    let mut source = inner.to_string();
    source = bold_keywords(
        &source,
        &[
            "function", "funkce", "procedure", "Vstup", "Výstup", "then", "return", "else",
        ],
    );
    // Keyword must be followed by a space.
    for kw in ["if", "while", "for"] {
        source = source.replace(&format!("{kw} "), &format!("**{kw}** "));
    }
    // `do` must be preceded by a space.
    source = source.replace(" do", " **do**");
    // `od` / `fi` on their own line (optional trailing backslash).
    let mut lines: Vec<String> = source.lines().map(|l| l.to_string()).collect();
    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        let indent_len = line.len() - trimmed.len();
        let indent = &line[..indent_len];
        for kw in ["od", "fi"] {
            if trimmed.starts_with(kw) {
                let rest = &trimmed[kw.len()..];
                let tail = rest.trim();
                if tail.is_empty() || tail == "\\" {
                    *line = format!("{indent}**{kw}**{rest}");
                    break;
                }
            }
        }
    }
    source = lines.join("\n");
    // NOTE: the `<ksi-pseudocode>` → padded-div rewriting is done by the
    // caller `ksi_pseudocode` (Python's `one_ksi_pseudocode` does it on the
    // whole regex match including the tags).
    // Force pandoc to put every line in its own paragraph.
    source = source.replace('\n', "\n\n");
    source = source.replace('\t', "&emsp;").replace("    ", "&emsp;");
    source
}

/// Replaces `<ksi-pseudocode>` blocks with HTML (mirrors `ksi_pseudocode`).
pub fn ksi_pseudocode(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("<ksi-pseudocode>") {
        // Require the newline variant like Python's regex `<ksi-pseudocode>\n((.|\n)*?)</ksi-pseudocode>`.
        let after_open = start + "<ksi-pseudocode>".len();
        let Some(end) = rest[after_open..].find("</ksi-pseudocode>") else {
            break;
        };
        let end = after_open + end;
        let inner_full = &rest[start..end + "</ksi-pseudocode>".len()];
        if !inner_full.starts_with("<ksi-pseudocode>\n") {
            out.push_str(&rest[..after_open]);
            rest = &rest[after_open..];
            continue;
        }
        let inner = &rest[after_open..end];
        out.push_str(&rest[..start]);
        // Python's one_ksi_pseudocode receives the whole match including the
        // tags and rewrites them into the padded div; replicate that here.
        out.push_str("<div style='padding-left:20px'>\n");
        out.push_str(&one_ksi_pseudocode(inner));
        out.push_str("</div>");
        rest = &rest[end + "</ksi-pseudocode>".len()..];
    }
    out.push_str(rest);
    out
}

/// Replaces `<ksi-collapse title="…">` with nested divs (mirrors
/// `ksi_collapse`); ids are shared across assignment + modules via `counter`.
pub fn ksi_collapse(source: &str, counter: &mut u64) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("<ksi-collapse title=\"") {
        let title_start = start + "<ksi-collapse title=\"".len();
        let Some(title_end) = rest[title_start..].find("\">") else {
            break;
        };
        let title_end = title_start + title_end;
        let title = &rest[title_start..title_end];
        *counter += 1;
        let id = *counter;
        out.push_str(&rest[..start]);
        out.push_str(&format!(
            "\n<div class=\"panel panel-ksi panel-group\">\n<div class=\"panel-heading panel-heading-ksi\">\
             <h4 class=\"panel-title\"><a data-toggle=\"collapse\" href=\"#collapse{id}\">{title}</a></h4></div>\n\
             <div id=\"collapse{id}\" class=\"panel-collapse collapse\">\n<div class=\"panel-body\">"
        ));
        rest = &rest[title_end + 2..];
    }
    out.push_str(rest);
    out.replace("</ksi-collapse>", "</div></div></div>")
}

pub fn parse_simple_text(
    task_id: i64,
    mangled_datadir: &str,
    mangled_soldir: &str,
    backend_url: &str,
    text: &str,
    collapse_counter: &mut u64,
) -> DeployResult<String> {
    let html = parse_pandoc(&ksi_collapse(
        &ksi_pseudocode(&format_custom_tags(text)),
        collapse_counter,
    ))?;
    Ok(add_table_class(&change_links(
        task_id,
        mangled_datadir,
        mangled_soldir,
        backend_url,
        &replace_h(&html),
    )))
}

/// Extract `<p>…</p>` / `<h1 …>…</h1>` inner text from pandoc HTML lines.
fn extract_p(html: &str) -> Option<String> {
    let start = html.find("<p>")? + 3;
    let end = html[start..].find("</p>")?;
    Some(html[start..start + end].to_string())
}

fn extract_h1(html: &str) -> Option<String> {
    let h1 = html.find("<h1")?;
    let gt = html[h1..].find('>')? + h1 + 1;
    let end = html[gt..].find("</h1>")?;
    Some(html[gt..gt + end].to_string())
}

// ---------------------------------------------------------------------------
// Prerequisities expression parser (mirrors `parse_prereq_text` +
// `parse_prereq_logic` + cycle check in taskDeploy.py)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum PrereqToken {
    Ref(String),
    And,
    Or,
}

fn tokenize_prereq(text: &str) -> Result<Vec<PrereqToken>, DeployFailure> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '&' && chars.get(i + 1) == Some(&'&') {
            tokens.push(PrereqToken::And);
            i += 2;
            continue;
        }
        if c == '|' && chars.get(i + 1) == Some(&'|') {
            tokens.push(PrereqToken::Or);
            i += 2;
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            tokens.push(PrereqToken::Ref(chars[start..i].iter().collect()));
            continue;
        }
        if text[i..].starts_with("uloha_") {
            let mut j = i + "uloha_".len();
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j == i + "uloha_".len() || chars.get(j) != Some(&'_') {
                return Err(fail(format!("invalid task reference near '{}'", &text[i..])));
            }
            j += 1;
            let word_start = j;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j == word_start {
                return Err(fail(format!("invalid task reference near '{}'", &text[i..])));
            }
            tokens.push(PrereqToken::Ref(chars[i..j].iter().collect()));
            i = j;
            continue;
        }
        return Err(fail(format!("cannot parse prerequisities near '{}'", &text[i..])));
    }
    Ok(tokens)
}

#[derive(Debug, Clone)]
enum PrereqExpr {
    Ref(String),
    And(Box<PrereqExpr>, Box<PrereqExpr>),
    Or(Box<PrereqExpr>, Box<PrereqExpr>),
}

struct PrereqParser {
    tokens: Vec<PrereqToken>,
    pos: usize,
}

impl PrereqParser {
    fn parse(text: &str) -> Result<PrereqExpr, DeployFailure> {
        let tokens = tokenize_prereq(text)?;
        if tokens.is_empty() {
            return Err(fail("empty prerequisities expression"));
        }
        let mut p = Self { tokens, pos: 0 };
        let expr = p.parse_or()?;
        if p.pos != p.tokens.len() {
            return Err(fail("trailing tokens in prerequisities expression"));
        }
        Ok(expr)
    }

    fn peek(&self) -> Option<&PrereqToken> {
        self.tokens.get(self.pos)
    }

    fn parse_or(&mut self) -> Result<PrereqExpr, DeployFailure> {
        let mut left = self.parse_and()?;
        while self.peek() == Some(&PrereqToken::Or) {
            self.pos += 1;
            let right = self.parse_and()?;
            left = PrereqExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<PrereqExpr, DeployFailure> {
        let mut left = self.parse_atom()?;
        while self.peek() == Some(&PrereqToken::And) {
            self.pos += 1;
            let right = self.parse_atom()?;
            left = PrereqExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_atom(&mut self) -> Result<PrereqExpr, DeployFailure> {
        match self.tokens.get(self.pos) {
            Some(PrereqToken::Ref(r)) => {
                self.pos += 1;
                Ok(PrereqExpr::Ref(r.clone()))
            }
            other => Err(fail(format!("expected task reference, found {other:?}"))),
        }
    }
}

/// Resolve a textual task reference to a task id (numeric id, or lookup by
/// git directory name within the year's waves).
async fn resolve_prereq_task(db: &SqlitePool, reference: &str, year: i64) -> DeployResult<i64> {
    if let Ok(id) = reference.parse::<i64>() {
        return Ok(id);
    }
    let task_id: Option<i64> = sqlx::query_scalar(
        "SELECT t.id FROM tasks t JOIN waves w ON w.id = t.wave
         WHERE w.year = ? AND t.git_path LIKE ?",
    )
    .bind(year)
    .bind(format!("%/{reference}"))
    .fetch_optional(db)
    .await
    .map_err(|e| fail(format!("prerequisity task lookup failed: {e}")))?;
    task_id.ok_or_else(|| fail(format!("prerequisity task '{reference}' not found")))
}

/// Build/update the `prerequisities` tree for `expr` under `prereq_id`.
async fn parse_prereq_logic(
    db: &SqlitePool,
    expr: &PrereqExpr,
    prereq_id: i64,
    year: i64,
    log: &DeployLog,
) -> DeployResult<()> {
    match expr {
        PrereqExpr::Ref(reference) => {
            let task_id = resolve_prereq_task(db, reference, year).await?;
            sqlx::query("UPDATE prerequisities SET type = 'ATOMIC', task = ? WHERE id = ?")
                .bind(task_id)
                .bind(prereq_id)
                .execute(db)
                .await
                .map_err(|e| fail(format!("cannot update prerequisity: {e}")))?;
            // Delete potential subtree of children.
            sqlx::query("DELETE FROM prerequisities WHERE parent = ?")
                .bind(prereq_id)
                .execute(db)
                .await
                .map_err(|e| fail(format!("cannot delete prerequisity children: {e}")))?;
        }
        PrereqExpr::And(left, right) | PrereqExpr::Or(left, right) => {
            let typ = match expr {
                PrereqExpr::And(_, _) => "AND",
                _ => "OR",
            };
            sqlx::query("UPDATE prerequisities SET type = ?, task = NULL WHERE id = ?")
                .bind(typ)
                .bind(prereq_id)
                .execute(db)
                .await
                .map_err(|e| fail(format!("cannot update prerequisity: {e}")))?;

            let mut children: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM prerequisities WHERE parent = ? ORDER BY id",
            )
            .bind(prereq_id)
            .fetch_all(db)
            .await
            .map_err(|e| fail(format!("cannot load prerequisity children: {e}")))?;

            while children.len() < 2 {
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO prerequisities (type, parent, task) VALUES ('ATOMIC', ?, NULL) RETURNING id",
                )
                .bind(prereq_id)
                .fetch_one(db)
                .await
                .map_err(|e| fail(format!("cannot create prerequisity child: {e}")))?;
                children.push(id);
            }
            while children.len() > 2 {
                let extra = children.remove(2);
                sqlx::query("DELETE FROM prerequisities WHERE id = ?")
                    .bind(extra)
                    .execute(db)
                    .await
                    .map_err(|e| fail(format!("cannot delete prerequisity child: {e}")))?;
            }

            // Box::pin for the recursive async calls.
            parse_prereq_logic_boxed(db, left, children[0], year, log).await?;
            parse_prereq_logic_boxed(db, right, children[1], year, log).await?;
        }
    }
    Ok(())
}

fn parse_prereq_logic_boxed<'a>(
    db: &'a SqlitePool,
    expr: &'a PrereqExpr,
    prereq_id: i64,
    year: i64,
    log: &'a DeployLog,
) -> Pin<Box<dyn Future<Output = DeployResult<()>> + Send + 'a>> {
    Box::pin(parse_prereq_logic(db, expr, prereq_id, year, log))
}

/// Recursive cycle check (mirrors `_collect_task_dependencies_from_prereq`).
fn collect_deps<'a>(
    db: &'a SqlitePool,
    prereq_id: i64,
    task_id: i64,
    visited: HashSet<i64>,
) -> Pin<Box<dyn Future<Output = DeployResult<Option<String>>> + Send + 'a>> {
    Box::pin(async move {
        let row: Option<(String, Option<i64>)> =
            sqlx::query_as("SELECT type, task FROM prerequisities WHERE id = ?")
                .bind(prereq_id)
                .fetch_optional(db)
                .await
                .map_err(|e| fail(format!("prerequisity lookup failed: {e}")))?;
        let Some((typ, ptask)) = row else {
            return Ok(None);
        };
        if typ == "ATOMIC" {
            if let Some(pt) = ptask {
                if pt == task_id {
                    return Ok(Some(format!("úloha {task_id} závisí sama na sobě")));
                }
                if visited.contains(&pt) {
                    return Ok(Some(format!("cyklická závislost zahrnující úlohu {pt}")));
                }
                let mut visited = visited;
                visited.insert(pt);
                let parent: Option<i64> = sqlx::query_scalar(
                    "SELECT prerequisite FROM tasks WHERE id = ?",
                )
                .bind(pt)
                .fetch_optional(db)
                .await
                .map_err(|e| fail(format!("task lookup failed: {e}")))?
                .flatten();
                if let Some(proot) = parent {
                    if let Some(cycle) = collect_deps(db, proot, task_id, visited).await? {
                        return Ok(Some(cycle));
                    }
                }
            }
        } else {
            let children: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM prerequisities WHERE parent = ?",
            )
            .bind(prereq_id)
            .fetch_all(db)
            .await
            .map_err(|e| fail(format!("prerequisity children lookup failed: {e}")))?;
            for child in children {
                if let Some(cycle) = collect_deps(db, child, task_id, visited.clone()).await? {
                    return Ok(Some(cycle));
                }
            }
        }
        Ok(None)
    })
}

// ---------------------------------------------------------------------------
// process_meta
// ---------------------------------------------------------------------------

/// Try to parse a deadline string like Python's `dateutil.parser.parse`.
fn parse_deadline(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    const FORMATS: &[&str] = &[
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d",
        "%d.%m.%Y %H:%M:%S",
        "%d.%m.%Y %H:%M",
        "%d.%m.%Y",
    ];
    for fmt in FORMATS {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt);
        }
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, fmt) {
            if let Some(dt) = d.and_hms_opt(0, 0, 0) {
                return Some(dt);
            }
        }
    }
    None
}

/// Python's `local2UTC`: interpret the naive time as server-local, convert to UTC.
fn local_to_utc(local: NaiveDateTime) -> NaiveDateTime {
    Local
        .from_local_datetime(&local)
        .single()
        .map(|dt| dt.naive_utc())
        .unwrap_or(local)
}

async fn process_meta(ctx: &mut DeployCtx<'_>, task_id: i64, filename: &Path) -> DeployResult<()> {
    ctx.log.log(&format!("Processing meta {}", filename.display()));
    let raw = std::fs::read_to_string(filename)
        // Python opens with utf-8-sig; strip a BOM if present.
        .map_err(|e| fail(format!("cannot read {}: {e}", filename.display())))?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let data: Value =
        serde_json::from_str(raw).map_err(|e| fail(format!("cannot parse task.json: {e}")))?;

    let author = data
        .get("author")
        .and_then(Value::as_i64)
        .ok_or_else(|| fail("task.json: missing 'author'"))?;
    let co_author = data.get("co_author").and_then(Value::as_i64);

    let time_deadline = if let Some(s) = data.get("date_deadline").and_then(Value::as_str) {
        let parsed = parse_deadline(s)
            .ok_or_else(|| fail(format!("cannot parse date_deadline '{s}'")))?;
        let at_end_of_day = parsed
            .date()
            .and_hms_opt(23, 59, 59)
            .ok_or_else(|| fail("invalid date_deadline"))?;
        local_to_utc(at_end_of_day)
    } else if let Some(s) = data.get("time_deadline").and_then(Value::as_str) {
        parse_deadline(s).ok_or_else(|| fail("cannot parse time_deadline"))?
    } else {
        return Err(fail("task.json: missing deadline"));
    };

    let picture_base: Option<String> = match data.get("icon_ref") {
        Some(Value::Null) | None => None,
        Some(v) => {
            let icon_ref = v.as_i64().ok_or_else(|| fail("task.json: icon_ref must be an integer"))?;
            let other_base: Option<Option<String>> = sqlx::query_scalar(
                "SELECT picture_base FROM tasks WHERE id = ?",
            )
            .bind(icon_ref)
            .fetch_optional(ctx.db)
            .await
            .map_err(|e| fail(format!("icon_ref task lookup failed: {e}")))?;
            match other_base {
                Some(Some(base)) if !base.is_empty() => Some(base),
                _ => Some(format!("/taskContent/{icon_ref}/icon/")),
            }
        }
    };

    sqlx::query(
        "UPDATE tasks SET author = ?, co_author = ?, time_deadline = ?, picture_base = ? WHERE id = ?",
    )
    .bind(author)
    .bind(co_author)
    .bind(time_deadline)
    .bind(picture_base)
    .bind(task_id)
    .execute(ctx.db)
    .await
    .map_err(|e| fail(format!("cannot update task meta: {e}")))?;

    // Prerequisities.
    match data.get("prerequisities") {
        None | Some(Value::Null) => {
            sqlx::query("UPDATE tasks SET prerequisite = NULL WHERE id = ?")
                .bind(task_id)
                .execute(ctx.db)
                .await
                .map_err(|e| fail(format!("cannot clear prerequisity: {e}")))?;
        }
        Some(v) => {
            let text = if let Some(n) = v.as_i64() {
                n.to_string()
            } else if let Some(s) = v.as_str() {
                s.to_string()
            } else {
                return Err(fail("task.json: prerequisities must be a string or integer"));
            };

            let current: Option<i64> = sqlx::query_scalar("SELECT prerequisite FROM tasks WHERE id = ?")
                .bind(task_id)
                .fetch_one(ctx.db)
                .await
                .map_err(|e| fail(format!("task lookup failed: {e}")))?;

            let root_id: i64 = match current {
                Some(id) => {
                    let exists: Option<i64> =
                        sqlx::query_scalar("SELECT id FROM prerequisities WHERE id = ?")
                            .bind(id)
                            .fetch_optional(ctx.db)
                            .await
                            .map_err(|e| fail(format!("prerequisity lookup failed: {e}")))?;
                    match exists {
                        Some(id) => id,
                        None => {
                            sqlx::query("UPDATE tasks SET prerequisite = NULL WHERE id = ?")
                                .bind(task_id)
                                .execute(ctx.db)
                                .await
                                .map_err(|e| fail(format!("cannot clear prerequisity: {e}")))?;
                            create_prereq_root(ctx.db).await?
                        }
                    }
                }
                None => create_prereq_root(ctx.db).await?,
            };

            let wave_year: i64 = sqlx::query_scalar(
                "SELECT w.year FROM waves w JOIN tasks t ON t.wave = w.id WHERE t.id = ?",
            )
            .bind(task_id)
            .fetch_one(ctx.db)
            .await
            .map_err(|e| fail(format!("wave lookup failed: {e}")))?;

            let expr = PrereqParser::parse(&text)?;
            ctx.log.log(&format!("Parsing {expr:?}"));
            parse_prereq_logic_boxed(ctx.db, &expr, root_id, wave_year, &ctx.log).await?;

            if let Some(cycle) = collect_deps(ctx.db, root_id, task_id, HashSet::new()).await? {
                ctx.log.log(&format!(
                    "WARNING: Cyklická prerekvizita detekována pro úlohu {task_id}: {cycle}"
                ));
                return Err(fail(format!("Cyklická prerekvizita detekována: {cycle}")));
            }

            sqlx::query("UPDATE tasks SET prerequisite = ? WHERE id = ?")
                .bind(root_id)
                .bind(task_id)
                .execute(ctx.db)
                .await
                .map_err(|e| fail(format!("cannot set prerequisity: {e}")))?;
        }
    }
    Ok(())
}

async fn create_prereq_root(db: &SqlitePool) -> DeployResult<i64> {
    sqlx::query_scalar("INSERT INTO prerequisities (type, parent, task) VALUES ('ATOMIC', NULL, NULL) RETURNING id")
        .fetch_one(db)
        .await
        .map_err(|e| fail(format!("cannot create prerequisity root: {e}")))
}

// ---------------------------------------------------------------------------
// process_assignment / process_solution
// ---------------------------------------------------------------------------

async fn process_assignment(ctx: &mut DeployCtx<'_>, task_id: i64, filename: &Path) -> DeployResult<()> {
    let data = std::fs::read_to_string(filename)
        .map_err(|e| fail(format!("cannot read {}: {e}", filename.display())))?;
    let mut collapse_counter = 0u64;
    let parsed = parse_pandoc(&ksi_collapse(
        &ksi_pseudocode(&format_custom_tags(&data)),
        &mut collapse_counter,
    ))?;
    let mut lines: Vec<&str> = parsed.split('\n').collect();

    let intro = lines
        .first()
        .and_then(|l| extract_p(l))
        .unwrap_or_else(|| "Intro ulohy nenalezeno".to_string());
    if lines.first().and_then(|l| extract_p(l)).is_some() {
        lines.remove(0);
    }

    let title = lines
        .first()
        .and_then(|l| extract_h1(l))
        .unwrap_or_else(|| "Nazev ulohy nenalezen".to_string());
    if lines.first().and_then(|l| extract_h1(l)).is_some() {
        lines.remove(0);
    }

    let body = add_table_class(&change_links(
        task_id,
        &ctx.mangled_datadir,
        &ctx.mangled_soldir,
        &ctx.backend_url,
        &replace_h(&lines.join("\n")),
    ));

    sqlx::query("UPDATE tasks SET intro = ?, title = ?, body = ? WHERE id = ?")
        .bind(intro)
        .bind(title)
        .bind(body)
        .bind(task_id)
        .execute(ctx.db)
        .await
        .map_err(|e| fail(format!("cannot update assignment: {e}")))?;
    Ok(())
}

async fn process_solution(ctx: &mut DeployCtx<'_>, task_id: i64, filename: &Path) -> DeployResult<()> {
    let solution = if filename.is_file() {
        let data = std::fs::read_to_string(filename)
            .map_err(|e| fail(format!("cannot read {}: {e}", filename.display())))?;
        let mut collapse_counter = 0u64;
        Some(
            parse_simple_text(
                task_id,
                &ctx.mangled_datadir,
                &ctx.mangled_soldir,
                &ctx.backend_url,
                &data,
                &mut collapse_counter,
            )?,
        )
    } else {
        None
    };
    sqlx::query("UPDATE tasks SET solution = ? WHERE id = ?")
        .bind(solution)
        .bind(task_id)
        .execute(ctx.db)
        .await
        .map_err(|e| fail(format!("cannot update solution: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// copy_icons + generate_icon_variants
// ---------------------------------------------------------------------------

/// Copy `base.svg` / `correcting.svg` / `locked.svg` / `done.svg` from the
/// repository, then auto-generate missing badge variants from `base.svg`.
pub fn copy_icons(task_id: i64, source: &Path, data_dir: &Path) -> DeployResult<Vec<String>> {
    let target = data_dir.join("task-content").join(task_id.to_string()).join("icon");
    std::fs::create_dir_all(&target)
        .map_err(|e| fail(format!("cannot create {}: {e}", target.display())))?;
    for name in ["base.svg", "correcting.svg", "locked.svg", "done.svg"] {
        let src = source.join(name);
        if src.is_file() {
            let dst = target.join(name);
            // shutil.copy2 semantics; ignore SameFileError.
            if src != dst {
                std::fs::copy(&src, &dst)
                    .map_err(|e| fail(format!("cannot copy {}: {e}", src.display())))?;
            }
        }
    }
    Ok(generate_icon_variants(source, &target))
}

/// Badge groups hidden in `base.svg` (`<g id="…">`) that each produce one
/// icon variant by un-hiding the badge.
///
/// Auto-generates missing icon variants from `base.svg` badge groups:
/// explicitly provided files always win, malformed input is skipped with a
/// warning. Returns the names of the generated files.
pub fn generate_icon_variants(source: &Path, target: &Path) -> Vec<String> {
    const BADGES: &[(&str, &str)] = &[
        ("correcting", "correcting.svg"), // orange badge with arrow (submitted)
        ("done", "done.svg"),             // green badge with checkmark (corrected)
    ];

    let mut generated = Vec::new();
    let base_file = source.join("base.svg");
    let Ok(base) = std::fs::read_to_string(&base_file) else {
        return generated;
    };

    for (badge_id, variant_file) in BADGES {
        if source.join(variant_file).is_file() {
            continue; // organizer-provided variant wins
        }
        let Some(svg) = unhide_badge(&base, badge_id) else {
            tracing::warn!(
                "{} has no badge group id='{badge_id}', skipping {variant_file}",
                base_file.display()
            );
            continue;
        };
        let svg = if svg.trim_start().starts_with("<?xml") {
            svg
        } else {
            format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n{svg}")
        };
        let target_file = target.join(variant_file);
        match std::fs::write(&target_file, svg) {
            Ok(()) => generated.push(variant_file.to_string()),
            Err(e) => tracing::warn!("cannot write {}: {e}", target_file.display()),
        }
    }
    generated
}

/// String-based SVG manipulation: find the `<g … id="{badge_id}" …>` tag and
/// drop its `display="none"` attribute, un-hiding the badge. Returns `None`
/// when the badge group cannot be located (malformed input → skipped).
fn unhide_badge(svg: &str, badge_id: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("id={quote}{badge_id}{quote}");
        let mut search_from = 0;
        while let Some(rel) = svg[search_from..].find(&needle) {
            let id_pos = search_from + rel;
            // The containing tag must start with `<g`.
            let tag_start = svg[..id_pos].rfind('<')?;
            if !svg[tag_start..].starts_with("<g")
                || !matches!(
                    svg[tag_start + 2..].chars().next(),
                    Some(c) if c.is_whitespace() || c == '>'
                )
            {
                search_from = id_pos + needle.len();
                continue;
            }
            let tag_end_rel = svg[id_pos..].find('>')?;
            let tag_end = id_pos + tag_end_rel;
            let mut tag = svg[tag_start..=tag_end].to_string();
            // Drop display="none" / display='none' (with one surrounding space).
            let mut cleaned = tag.clone();
            for q in ['"', '\''] {
                let attr = format!("display={q}none{q}");
                if let Some(pos) = cleaned.find(&attr) {
                    let mut end = pos + attr.len();
                    let mut start = pos;
                    if cleaned[..pos].ends_with(' ') {
                        start = pos - 1;
                    } else if cleaned[end..].starts_with(' ') {
                        end += 1;
                    }
                    cleaned.replace_range(start..end, "");
                    break;
                }
            }
            tag = cleaned;
            let mut out = String::with_capacity(svg.len());
            out.push_str(&svg[..tag_start]);
            out.push_str(&tag);
            out.push_str(&svg[tag_end + 1..]);
            return Some(out);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// mangled_dirname / copy_data
// ---------------------------------------------------------------------------

/// Find or create the single `prefix*` directory (mirrors `mangled_dirname`).
pub fn mangled_dirname(base_directory: &Path, prefix: &str) -> DeployResult<String> {
    let mut matches = Vec::new();
    if base_directory.is_dir() {
        for entry in std::fs::read_dir(base_directory)
            .map_err(|e| fail(format!("cannot list {}: {e}", base_directory.display())))?
        {
            let entry = entry.map_err(|e| fail(format!("cannot list {}: {e}", base_directory.display())))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(prefix) {
                matches.push(name);
            }
        }
    }
    if let Some(first) = matches.first() {
        if matches.len() != 1 {
            return Err(fail(format!(
                "multiple directories {}/{}*",
                base_directory.display(),
                prefix
            )));
        }
        if !base_directory.join(first).is_dir() {
            return Err(fail(format!("not a directory: {first}")));
        }
        return Ok(first.clone());
    }
    let suffix: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(16)
        .collect();
    let name = format!("{prefix}{suffix}");
    std::fs::create_dir_all(base_directory.join(&name))
        .map_err(|e| fail(format!("cannot create {}: {e}", base_directory.join(&name).display())))?;
    Ok(name)
}

/// Copy a whole data directory (mirrors `copy_data`).
pub fn copy_data(source: &Path, target: &Path) -> DeployResult<()> {
    if !target.exists() {
        std::fs::create_dir_all(target)
            .map_err(|e| fail(format!("cannot create {}: {e}", target.display())))?;
    }
    if target.exists() {
        std::fs::remove_dir_all(target)
            .map_err(|e| fail(format!("cannot remove {}: {e}", target.display())))?;
    }
    if source.is_dir() {
        copy_dir_all(source, target)?;
    }
    Ok(())
}

/// Recursively copy a directory (used by `copy_data` and task creation).
pub fn copy_dir_all(source: &Path, target: &Path) -> DeployResult<()> {
    std::fs::create_dir_all(target)
        .map_err(|e| fail(format!("cannot create {}: {e}", target.display())))?;
    for entry in std::fs::read_dir(source)
        .map_err(|e| fail(format!("cannot list {}: {e}", source.display())))?
    {
        let entry = entry.map_err(|e| fail(format!("cannot list {}: {e}", source.display())))?;
        let dst = target.join(entry.file_name());
        let ftype = entry
            .file_type()
            .map_err(|e| fail(format!("cannot stat {}: {e}", entry.path().display())))?;
        if ftype.is_dir() {
            copy_dir_all(&entry.path(), &dst)?;
        } else {
            std::fs::copy(entry.path(), &dst)
                .map_err(|e| fail(format!("cannot copy {}: {e}", entry.path().display())))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// process_modules
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct ModuleRow {
    id: i64,
}

async fn process_modules(ctx: &mut DeployCtx<'_>, task_id: i64, git_path: &Path) -> DeployResult<()> {
    let mut modules: Vec<ModuleRow> = sqlx::query_as(
        "SELECT id FROM modules WHERE task = ? ORDER BY \"order\"",
    )
    .bind(task_id)
    .fetch_all(ctx.db)
    .await
    .map_err(|e| fail(format!("cannot load modules: {e}")))?;

    let mut i: usize = 0;
    while git_path.join(format!("module{}", i + 1)).is_dir() {
        let module_id = if i < modules.len() {
            let m = modules[i].id;
            sqlx::query("UPDATE modules SET \"order\" = ? WHERE id = ?")
                .bind(i as i64)
                .bind(m)
                .execute(ctx.db)
                .await
                .map_err(|e| fail(format!("cannot update module order: {e}")))?;
            m
        } else {
            let new_id: i64 = sqlx::query_scalar(
                "INSERT INTO modules (task, type, name, \"order\", submit_ratelimit, max_points, autocorrect, bonus, custom)
                 VALUES (?, 'general', '', ?, NULL, 0, 0, 0, 0) RETURNING id",
            )
            .bind(task_id)
            .bind(i as i64)
            .fetch_one(ctx.db)
            .await
            .map_err(|e| fail(format!("cannot create module: {e}")))?;
            modules.push(ModuleRow { id: new_id });
            new_id
        };

        ctx.log.log(&format!("Processing module{}", i + 1));
        process_module(ctx, task_id, module_id, &git_path.join(format!("module{}", i + 1))).await?;
        i += 1;
    }

    if i == 0 {
        // No module -> no public evaluation.
        ctx.eval_public = false;
    }

    // Delete surplus modules.
    while i < modules.len() {
        let m = modules[i].id;
        sqlx::query("DELETE FROM modules WHERE id = ?")
            .bind(m)
            .execute(ctx.db)
            .await
            .map_err(|e| fail(format!("cannot delete module: {e}")))?;
        i += 1;
    }
    Ok(())
}

async fn process_module(
    ctx: &mut DeployCtx<'_>,
    task_id: i64,
    module_id: i64,
    module_path: &Path,
) -> DeployResult<()> {
    let specific = process_module_json(ctx, module_id, &module_path.join("module.json")).await?;

    // Copy the whole module directory into data/modules/<id>.
    ctx.log.log("Copying module data");
    let target_path = ctx.paths.data_dir.join("modules").join(module_id.to_string());
    if target_path.is_dir() {
        std::fs::remove_dir_all(&target_path)
            .map_err(|e| fail(format!("cannot remove {}: {e}", target_path.display())))?;
    }
    copy_dir_all(module_path, &target_path)?;

    let custom = target_path.join("module-gen").is_file();
    sqlx::query("UPDATE modules SET custom = ? WHERE id = ?")
        .bind(custom)
        .bind(module_id)
        .execute(ctx.db)
        .await
        .map_err(|e| fail(format!("cannot update module custom flag: {e}")))?;

    process_module_md(ctx, task_id, module_id, &module_path.join("module.md"), &specific, module_path).await
}

async fn process_module_json(
    ctx: &mut DeployCtx<'_>,
    module_id: i64,
    filename: &Path,
) -> DeployResult<Value> {
    ctx.log.log("Processing module json");
    let raw = std::fs::read_to_string(filename)
        .map(|s| s.strip_prefix('\u{feff}').unwrap_or(&s).to_string())
        .map_err(|e| fail(format!("cannot read {}: {e}", filename.display())))?;
    let data: Value =
        serde_json::from_str(&raw).map_err(|e| fail(format!("cannot parse module.json: {e}")))?;

    let typ = data
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("module.json: missing 'type'"))?;
    let db_type = match typ {
        "text" => "text",
        "general" => "general",
        "programming" => "programming",
        "quiz" => "quiz",
        "sortable" => "sortable",
        _ => "general",
    };
    let specific = data.get(typ).cloned().unwrap_or(Value::Object(Default::default()));

    let max_points = data
        .get("max_points")
        .and_then(Value::as_f64)
        .ok_or_else(|| fail("module.json: missing 'max_points'"))?;
    let autocorrect = data
        .get("autocorrect")
        .and_then(Value::as_bool)
        .ok_or_else(|| fail("module.json: missing 'autocorrect'"))?;
    let bonus = data.get("bonus").and_then(Value::as_bool).unwrap_or(false);
    let mut action = data
        .get("action")
        .map(|v| {
            if v.is_string() {
                v.as_str().unwrap_or("").to_string()
            } else {
                serde_json::to_string_pretty(v).unwrap_or_default()
            }
        })
        .unwrap_or_default();
    let _ = &mut action;
    let submit_ratelimit = data.get("submit_ratelimit").and_then(Value::as_i64);

    sqlx::query(
        "UPDATE modules SET type = ?, max_points = ?, autocorrect = ?, bonus = ?, action = ?, submit_ratelimit = ? WHERE id = ?",
    )
    .bind(db_type)
    .bind(max_points)
    .bind(autocorrect)
    .bind(bonus)
    .bind(action)
    .bind(submit_ratelimit)
    .bind(module_id)
    .execute(ctx.db)
    .await
    .map_err(|e| fail(format!("cannot update module json fields: {e}")))?;

    if !autocorrect {
        ctx.eval_public = false;
    }
    Ok(specific)
}

async fn process_module_md(
    ctx: &mut DeployCtx<'_>,
    task_id: i64,
    module_id: i64,
    filename: &Path,
    specific: &Value,
    module_path: &Path,
) -> DeployResult<()> {
    ctx.log.log("Processing module md");
    let raw = std::fs::read_to_string(filename)
        .map_err(|e| fail(format!("cannot read {}: {e}", filename.display())))?;
    let mut data: Vec<String> = raw.lines().map(|l| l.to_string()).collect();

    // Module name on the zeroth line.
    let name = if let Some(first) = data.first() {
        match first.find("# ") {
            Some(pos) => {
                let heading = first[pos..].to_string();
                data.remove(0);
                let html = parse_pandoc(&heading)?;
                extract_h1(&html).unwrap_or_else(|| "Nazev modulu nenalezen".to_string())
            }
            None => "Nazev modulu nenalezen".to_string(),
        }
    } else {
        "Nazev modulu nenalezen".to_string()
    };

    let module_type: String = sqlx::query_scalar("SELECT type FROM modules WHERE id = ?")
        .bind(module_id)
        .fetch_one(ctx.db)
        .await
        .map_err(|e| fail(format!("cannot load module type: {e}")))?;

    let mut collapse_counter = 0u64;
    let remaining: Vec<String> = match module_type.as_str() {
        "general" => {
            ctx.log.log("Processing general module");
            sqlx::query("UPDATE modules SET data = '{}' WHERE id = ?")
                .bind(module_id)
                .execute(ctx.db)
                .await
                .map_err(|e| fail(format!("cannot update module data: {e}")))?;
            data
        }
        "programming" => process_module_programming(ctx, module_id, data, specific, module_path).await?,
        "quiz" => {
            process_module_quiz(ctx, task_id, module_id, data, &mut collapse_counter).await?
        }
        "sortable" => process_module_sortable(ctx, module_id, data).await?,
        "text" => {
            process_module_text(ctx, task_id, module_id, data, module_path, &mut collapse_counter).await?
        }
        _ => {
            sqlx::query("UPDATE modules SET description = 'Neznamy typ modulu' WHERE id = ?")
                .bind(module_id)
                .execute(ctx.db)
                .await
                .map_err(|e| fail(format!("cannot update module: {e}")))?;
            data
        }
    };

    ctx.log.log("Processing body");
    let description = parse_simple_text(
        task_id,
        &ctx.mangled_datadir,
        &ctx.mangled_soldir,
        &ctx.backend_url,
        &remaining.join("\n"),
        &mut collapse_counter,
    )?;

    sqlx::query("UPDATE modules SET name = ?, description = ? WHERE id = ?")
        .bind(name)
        .bind(description)
        .bind(module_id)
        .execute(ctx.db)
        .await
        .map_err(|e| fail(format!("cannot update module name/description: {e}")))?;
    Ok(())
}

async fn process_module_programming(
    ctx: &DeployCtx<'_>,
    module_id: i64,
    lines: Vec<String>,
    specific: &Value,
    source_path: &Path,
) -> DeployResult<Vec<String>> {
    ctx.log.log("Processing programming module");
    let mut line = 0;
    while line < lines.len() && !lines[line].starts_with("```~python") {
        line += 1;
    }
    if line == lines.len() {
        return Ok(lines);
    }
    let mut end = line + 1;
    while end < lines.len() && !lines[end].starts_with("```") {
        end += 1;
    }
    let code = lines[line + 1..end.min(lines.len())].join("\n");

    let target_path = ctx.paths.data_dir.join("modules").join(module_id.to_string());
    let mut programming = serde_json::json!({
        "default_code": code,
        "merge_script": target_path.join("merge").to_string_lossy(),
        "stdin": target_path.join("stdin.txt").to_string_lossy(),
        "check_script": target_path.join("eval").to_string_lossy(),
    });
    if let Some(version) = specific.get("version") {
        programming["version"] = version.clone();
    }
    // NOTE: Python checks `os.path.join(source_path, "/stdin.txt")`; with the
    // leading slash this is always the absolute `/stdin.txt`, so the empty
    // stdin file is effectively always created. Kept faithful.
    if !Path::new("/stdin.txt").is_file() {
        std::fs::write(target_path.join("stdin.txt"), "")
            .map_err(|e| fail(format!("cannot create stdin.txt: {e}")))?;
    }
    if source_path.join("post").is_file() {
        programming["post_trigger_script"] =
            Value::String(target_path.join("post").to_string_lossy().into_owned());
    }
    if let Some(limits) = specific.get("limits") {
        programming["limits"] = limits.clone();
    }
    let data = serde_json::to_string(&serde_json::json!({ "programming": programming }),)
        .map_err(|e| fail(format!("cannot serialize module data: {e}")))?;
    sqlx::query("UPDATE modules SET data = ? WHERE id = ?")
        .bind(data)
        .bind(module_id)
        .execute(ctx.db)
        .await
        .map_err(|e| fail(format!("cannot update module data: {e}")))?;
    Ok(lines[..line].to_vec())
}

/// Match `^##(.*?) \(([rc])\)` (non-greedy): returns (question, 'r'|'c').
fn match_quiz_head(line: &str) -> Option<(&str, char)> {
    let rest = line.strip_prefix("##")?;
    // Find first " (r)" or " (c)".
    let mut i = 0;
    let bytes = rest.as_bytes();
    while i + 4 <= bytes.len() {
        if bytes[i] == b' '
            && bytes[i + 1] == b'('
            && (bytes[i + 2] == b'r' || bytes[i + 2] == b'c')
            && bytes[i + 3] == b')'
        {
            return Some((rest[..i].trim_end(), bytes[i + 2] as char));
        }
        i += 1;
    }
    None
}

/// Match `^~\s*(.*?)\s*(\*|-)(\s|-)*$` against `line + " -"`.
/// Returns (content, is_correct).
fn match_quiz_option(line: &str) -> Option<(String, bool)> {
    let probe = format!("{line} -");
    let rest = probe.strip_prefix('~')?;
    let rest = rest.trim_start();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'*' || bytes[i] == b'-' {
            if bytes[i + 1..].iter().all(|b| *b == b' ' || *b == b'\t' || *b == b'-') {
                return Some((rest[..i].trim_end().to_string(), bytes[i] == b'*'));
            }
        }
        i += 1;
    }
    None
}

/// Match `^~\s*(.*)` — returns the rest of the line.
fn match_tilde_line(line: &str) -> Option<&str> {
    line.strip_prefix('~').map(|r| r.trim_start())
}

async fn process_module_quiz(
    ctx: &mut DeployCtx<'_>,
    task_id: i64,
    module_id: i64,
    lines: Vec<String>,
    collapse_counter: &mut u64,
) -> DeployResult<Vec<String>> {
    ctx.log.log("Processing quiz module");
    let mut quiz_data = Vec::new();
    let mut line: usize = 0;
    let mut text_end: usize = 0;
    let mut text_end_set = false;

    while line < lines.len() {
        while line < lines.len() && match_quiz_head(&lines[line]).is_none() {
            line += 1;
        }
        if !text_end_set {
            text_end = line;
            text_end_set = true;
        }
        if line == lines.len() {
            break;
        }
        let (head_text, head_kind) = match_quiz_head(&lines[line]).unwrap();
        let question_html = parse_pandoc(head_text)?;
        let question_text = extract_p(question_html.trim())
            .ok_or_else(|| fail("cannot parse quiz question heading"))?;
        let qtype = if head_kind == 'r' { "radio" } else { "checkbox" };

        line += 1;
        let mut end = line;
        while end < lines.len() && !lines[end].starts_with('~') {
            end += 1;
        }
        let qtext = parse_simple_text(
            task_id,
            &ctx.mangled_datadir,
            &ctx.mangled_soldir,
            &ctx.backend_url,
            &lines[line..end].join("\n"),
            collapse_counter,
        )?;

        line = end;
        let mut options = Vec::new();
        let mut correct = Vec::new();
        while line < lines.len() {
            let Some((content, is_correct)) = match_quiz_option(&lines[line]) else {
                break;
            };
            let parsed = parse_pandoc(&content)?
                .replace("<p>", "")
                .replace("</p>", "")
                .replace('\n', "");
            options.push(parsed);
            if is_correct {
                correct.push(options.len() - 1);
            }
            line += 1;
        }

        quiz_data.push(serde_json::json!({
            "question": question_text,
            "type": qtype,
            "text": qtext,
            "options": options,
            "correct": correct,
        }));
    }

    let data = serde_json::to_string(&serde_json::json!({ "quiz": quiz_data }))
        .map_err(|e| fail(format!("cannot serialize quiz data: {e}")))?;
    sqlx::query("UPDATE modules SET data = ? WHERE id = ?")
        .bind(data)
        .bind(module_id)
        .execute(ctx.db)
        .await
        .map_err(|e| fail(format!("cannot update module data: {e}")))?;
    Ok(lines[..text_end.min(lines.len())].to_vec())
}

fn get_sortable_offset(text: &str) -> i64 {
    if text.starts_with("if ")
        || text.starts_with("Vstup: ")
        || text.starts_with("while ")
        || text.starts_with("for ")
        || text.starts_with("def ")
    {
        1
    } else if text == "fi" || text == "od" || text.starts_with("return ") {
        -1
    } else {
        0
    }
}

/// Parse the `(a1,b2,…)` correct-order lines with a small scanner.
fn parse_sortable_correct_items(line: &str) -> Option<Vec<String>> {
    let rest = line.trim_start().strip_prefix("((")?;
    let mut items = Vec::new();
    let mut pos = 0;
    let bytes = rest.as_bytes();
    loop {
        if pos >= bytes.len() {
            return None;
        }
        let c = bytes[pos] as char;
        if c != 'a' && c != 'b' {
            return None;
        }
        let start = pos;
        pos += 1;
        let digits_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos == digits_start {
            return None;
        }
        items.push(rest[start..pos].to_string());
        if rest[pos..].starts_with("))") {
            return Some(items);
        }
        if bytes[pos] == b',' {
            pos += 1;
            continue;
        }
        return None;
    }
}

async fn process_module_sortable(
    ctx: &DeployCtx<'_>,
    module_id: i64,
    lines: Vec<String>,
) -> DeployResult<Vec<String>> {
    ctx.log.log("Processing sortable module");
    let mut fixed = Vec::new();
    let mut movable = Vec::new();
    let mut correct: Vec<Vec<String>> = Vec::new();

    let mut line: usize = 0;
    while line < lines.len() && !lines[line].starts_with('~') {
        line += 1;
    }
    let text_end = line;

    // Fixed part.
    while line < lines.len() {
        let Some(content) = match_tilde_line(&lines[line]) else {
            break;
        };
        let parsed = parse_pandoc(content)?
            .replace("<p>", "")
            .replace("</p>", "")
            .replace('\n', "");
        fixed.push(serde_json::json!({
            "content": parsed,
            "offset": get_sortable_offset(&parsed),
        }));
        line += 1;
    }

    // Blank line between fixed and movable parts.
    line += 1;

    // Movable part.
    while line < lines.len() {
        let Some(content) = match_tilde_line(&lines[line]) else {
            break;
        };
        let parsed = parse_pandoc(content)?
            .replace("<p>", "")
            .replace("</p>", "")
            .replace('\n', "");
        movable.push(serde_json::json!({
            "content": parsed,
            "offset": get_sortable_offset(&parsed),
        }));
        line += 1;
    }

    // Correct orders.
    while line < lines.len() {
        if let Some(items) = parse_sortable_correct_items(&lines[line]) {
            correct.push(items);
        }
        line += 1;
    }

    let data = serde_json::to_string(&serde_json::json!({
        "sortable": { "fixed": fixed, "movable": movable, "correct": correct },
    }))
    .map_err(|e| fail(format!("cannot serialize sortable data: {e}")))?;
    sqlx::query("UPDATE modules SET data = ? WHERE id = ?")
        .bind(data)
        .bind(module_id)
        .execute(ctx.db)
        .await
        .map_err(|e| fail(format!("cannot update module data: {e}")))?;
    Ok(lines[..text_end.min(lines.len())].to_vec())
}

/// Match `^~\s*(.*?)\s*(\*\*(.*?)\*\*)?\s*$` → (question, Option<diff>).
fn match_text_input(line: &str) -> Option<(&str, Option<&str>)> {
    let rest = line.strip_prefix('~')?.trim();
    let rest = rest.trim_end();
    if rest.ends_with("**") && rest.len() > 4 {
        let inner = &rest[..rest.len() - 2];
        if let Some(open) = inner.rfind("**") {
            let question = inner[..open].trim_end();
            let diff = inner[open + 2..].trim();
            return Some((question, Some(diff)));
        }
    }
    Some((rest, None))
}

async fn process_module_text(
    ctx: &mut DeployCtx<'_>,
    task_id: i64,
    module_id: i64,
    lines: Vec<String>,
    module_path: &Path,
    collapse_counter: &mut u64,
) -> DeployResult<Vec<String>> {
    ctx.log.log("Processing text module");
    let mut line: usize = 0;
    while line < lines.len() && !lines[line].starts_with('~') {
        line += 1;
    }
    let text_end = line;
    if line >= lines.len() {
        let data = serde_json::to_string(&serde_json::json!({ "text": { "inputs": 0 } }))
            .map_err(|e| fail(format!("cannot serialize text data: {e}")))?;
        sqlx::query("UPDATE modules SET data = ? WHERE id = ?")
            .bind(data)
            .bind(module_id)
            .execute(ctx.db)
            .await
            .map_err(|e| fail(format!("cannot update module data: {e}")))?;
        return Ok(lines);
    }

    let mut inputs_cnt: i64 = 0;
    let mut diff: Vec<String> = Vec::new();
    let mut questions = Vec::new();
    while line < lines.len() {
        let Some((q, d)) = match_text_input(&lines[line]) else {
            break;
        };
        let parsed = parse_simple_text(
            task_id,
            &ctx.mangled_datadir,
            &ctx.mangled_soldir,
            &ctx.backend_url,
            q,
            collapse_counter,
        )?
        .replace("<p>", "")
        .replace("</p>", "");
        questions.push(parsed);
        inputs_cnt += 1;
        match d {
            Some(s) => diff.push(s.to_string()),
            None => {
                if !diff.is_empty() {
                    diff.push(String::new());
                }
            }
        }
        line += 1;
    }

    let mut text_data = serde_json::json!({ "inputs": inputs_cnt, "questions": questions });
    if !diff.is_empty() {
        text_data["diff"] = Value::Array(diff.into_iter().map(Value::String).collect());
    } else {
        // Copy the eval script.
        let target_path = ctx.paths.data_dir.join("modules").join(module_id.to_string());
        std::fs::create_dir_all(&target_path)
            .map_err(|e| fail(format!("cannot create {}: {e}", target_path.display())))?;
        std::fs::copy(module_path.join("eval"), target_path.join("eval"))
            .map_err(|e| fail(format!("cannot copy eval script: {e}")))?;
        text_data["eval_script"] =
            Value::String(target_path.join("eval").to_string_lossy().into_owned());
    }
    let data = serde_json::to_string(&serde_json::json!({ "text": text_data }))
        .map_err(|e| fail(format!("cannot serialize text data: {e}")))?;
    sqlx::query("UPDATE modules SET data = ? WHERE id = ?")
        .bind(data)
        .bind(module_id)
        .execute(ctx.db)
        .await
        .map_err(|e| fail(format!("cannot update module data: {e}")))?;
    Ok(lines[..text_end.min(lines.len())].to_vec())
}

// ---------------------------------------------------------------------------
// process_task (top level)
// ---------------------------------------------------------------------------

/// Process the whole task from the git checkout (mirrors `process_task`).
pub async fn process_task(ctx: &mut DeployCtx<'_>, task_id: i64, git_task_path: &Path) -> DeployResult<()> {
    let data_path = ctx.paths.data_dir.join("task-content").join(task_id.to_string());

    process_meta(ctx, task_id, &git_task_path.join("task.json")).await?;

    // Remove the non-mangled path.
    let to_remove = data_path.join("zadani");
    if to_remove.is_dir() {
        ctx.log.log(&format!("Removing old {}...", to_remove.display()));
        std::fs::remove_dir_all(&to_remove)
            .map_err(|e| fail(format!("cannot remove {}: {e}", to_remove.display())))?;
    }

    ctx.mangled_datadir = mangled_dirname(&data_path, "zadani_")?;
    ctx.mangled_soldir = mangled_dirname(&data_path, "reseni_")?;

    ctx.log.log("Processing assignment");
    process_assignment(ctx, task_id, &git_task_path.join("assignment.md")).await?;

    ctx.log.log("Processing solution");
    process_solution(ctx, task_id, &git_task_path.join("solution.md")).await?;

    ctx.log.log("Processing icons & data");
    let icons_source = git_task_path.join("icons");
    let generated = copy_icons(task_id, &icons_source, &ctx.paths.data_dir)?;
    if !generated.is_empty() {
        ctx.log.log(&format!("Generated icon variants: {}", generated.join(", ")));
    }
    copy_data(
        &git_task_path.join("data"),
        &data_path.join(&ctx.mangled_datadir),
    )?;
    copy_data(
        &git_task_path.join("data_solution"),
        &data_path.join(&ctx.mangled_soldir),
    )?;

    ctx.log.log("Processing modules");
    process_modules(ctx, task_id, git_task_path).await?;

    ctx.log.log("Task processing done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudocode_bolds_keywords() {
        let src = "<ksi-pseudocode>\nfunction foo\nif x then\n  return 1\nfi\n</ksi-pseudocode>";
        let out = ksi_pseudocode(src);
        assert!(out.contains("**function**"), "{out}");
        assert!(out.contains("**if**"), "{out}");
        assert!(out.contains("**then**"), "{out}");
        assert!(out.contains("**return**"), "{out}");
        assert!(out.contains("**fi**"), "{out}");
        assert!(out.contains("<div style='padding-left:20px'>"), "{out}");
        assert!(!out.contains("ksi-pseudocode"), "{out}");
    }

    #[test]
    fn pseudocode_requires_newline_after_open_tag() {
        // Python's regex only matches `<ksi-pseudocode>\n…`.
        let src = "<ksi-pseudocode>if x</ksi-pseudocode>";
        assert_eq!(ksi_pseudocode(src), src);
    }

    #[test]
    fn collapse_ids_increment() {
        let mut counter = 0u64;
        let out = ksi_collapse(
            "<ksi-collapse title=\"A\">x</ksi-collapse><ksi-collapse title=\"B\">y</ksi-collapse>",
            &mut counter,
        );
        assert_eq!(counter, 2);
        assert!(out.contains("#collapse1"), "{out}");
        assert!(out.contains("#collapse2"), "{out}");
        assert!(out.contains("</div></div></div>"), "{out}");
    }

    #[test]
    fn custom_tags_rewritten() {
        let out = format_custom_tags("<KSI-TIP class=\"x\">hi</ksi-tip>");
        assert!(out.contains("<div class=\"ksi-custom ksi-tip\" class=\"x\">"), "{out}");
        assert!(out.contains("</div>"), "{out}");
    }

    #[test]
    fn headings_demoted() {
        assert_eq!(replace_h("<h2>x</h2>"), "<h3>x</h3>");
        assert_eq!(replace_h("<h1 class=\"a\">x</h1>"), "<h3 class=\"a\">x</h3>");
    }

    #[test]
    fn links_rewritten() {
        let out = change_links(42, "zadani_abc", "reseni_def", "https://b.example", "(data/a.png)");
        assert!(out.contains("(https://b.example/taskContent/42/zadani_abc/a.png)"), "{out}");
        let out = change_links(42, "zadani_abc", "reseni_def", "https://b.example", "\"data_solution/b\"");
        assert!(out.contains("\"https://b.example/taskContent/42/reseni_def/b\""), "{out}");
    }

    #[test]
    fn prereq_parses_precedence() {
        // && binds tighter than ||.
        let expr = PrereqParser::parse("1 || 2 && 3").unwrap();
        match expr {
            PrereqExpr::Or(left, right) => {
                assert!(matches!(*left, PrereqExpr::Ref(_)));
                assert!(matches!(*right, PrereqExpr::And(_, _)));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn prereq_parses_dir_names() {
        let expr = PrereqParser::parse("uloha_1_foo && 7").unwrap();
        assert!(matches!(expr, PrereqExpr::And(_, _)));
    }

    #[test]
    fn prereq_rejects_garbage() {
        assert!(PrereqParser::parse("1 &&& 2").is_err());
        assert!(PrereqParser::parse("").is_err());
    }

    #[test]
    fn quiz_head_matched() {
        // Python's `^##(.*?) \(([rc])\)` keeps the space after `##`.
        assert_eq!(match_quiz_head("## Kolik je 1+1? (r)"), Some((" Kolik je 1+1?", 'r')));
        assert_eq!(match_quiz_head("## Vyber (c) moznosti"), Some((" Vyber", 'c')));
        assert_eq!(match_quiz_head("# Neni otazka"), None);
    }

    #[test]
    fn quiz_option_matched() {
        // Note: the matcher appends " -" like Python does.
        assert_eq!(
            match_quiz_option("~ ano *"),
            Some(("ano".to_string(), true))
        );
        assert_eq!(
            match_quiz_option("~ ne -"),
            Some(("ne".to_string(), false))
        );
        assert_eq!(
            match_quiz_option("~ mozna"),
            Some(("mozna".to_string(), false))
        );
        assert_eq!(match_quiz_option("bez vlnovky"), None);
    }

    #[test]
    fn sortable_correct_parsed() {
        assert_eq!(
            parse_sortable_correct_items("((a1,b2,a3))"),
            Some(vec!["a1".to_string(), "b2".to_string(), "a3".to_string()])
        );
        assert_eq!(parse_sortable_correct_items("((a1))"), Some(vec!["a1".to_string()]));
        assert_eq!(parse_sortable_correct_items("((a))"), None);
        assert_eq!(parse_sortable_correct_items("nonsense"), None);
    }

    #[test]
    fn sortable_offset_rules() {
        assert_eq!(get_sortable_offset("if x"), 1);
        assert_eq!(get_sortable_offset("Vstup: n"), 1);
        assert_eq!(get_sortable_offset("fi"), -1);
        assert_eq!(get_sortable_offset("return 0"), -1);
        assert_eq!(get_sortable_offset("x = 1"), 0);
    }

    #[test]
    fn text_input_matched() {
        assert_eq!(match_text_input("~ Odpovez **spravne**"), Some(("Odpovez", Some("spravne"))));
        assert_eq!(match_text_input("~ Odpovez"), Some(("Odpovez", None)));
        assert_eq!(match_text_input("bez"), None);
    }

    #[test]
    fn icon_variants_generated_from_badge() {
        let dir = std::env::temp_dir().join(format!("ksi-icon-test-{}", std::process::id()));
        let src = dir.join("src");
        let dst = dir.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10"/><g id="correcting" display="none"><circle r="2"/></g><g id="done" display="none"><path d="M0 0h1"/></g></svg>"#;
        std::fs::write(src.join("base.svg"), svg).unwrap();

        let generated = generate_icon_variants(&src, &dst);
        assert_eq!(generated, vec!["correcting.svg".to_string(), "done.svg".to_string()]);

        let correcting = std::fs::read_to_string(dst.join("correcting.svg")).unwrap();
        assert!(correcting.starts_with("<?xml"), "{correcting}");
        // Only this variant's badge is un-hidden; the other stays hidden
        // (mirrors Python parsing base.svg fresh per variant).
        assert!(correcting.contains("<g id=\"correcting\">"), "{correcting}");
        assert!(!correcting.contains("<g id=\"correcting\" display=\"none\">"), "{correcting}");
        assert!(correcting.contains("<g id=\"done\" display=\"none\">"), "{correcting}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn icon_variants_explicit_files_win() {
        let dir = std::env::temp_dir().join(format!("ksi-icon-test2-{}", std::process::id()));
        let src = dir.join("src");
        let dst = dir.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("base.svg"), "<svg><g id=\"done\"></g></svg>").unwrap();
        std::fs::write(src.join("done.svg"), "<svg>explicit</svg>").unwrap();

        let generated = generate_icon_variants(&src, &dst);
        assert!(generated.is_empty());
        assert!(!dst.join("done.svg").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn icon_variants_missing_badge_skipped() {
        let dir = std::env::temp_dir().join(format!("ksi-icon-test3-{}", std::process::id()));
        let src = dir.join("src");
        let dst = dir.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("base.svg"), "<svg><g id=\"nope\"></g></svg>").unwrap();

        let generated = generate_icon_variants(&src, &dst);
        assert!(generated.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mangled_dirname_reuses_existing() {
        let dir = std::env::temp_dir().join(format!("ksi-mangle-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("zadani_abc123")).unwrap();
        let name = mangled_dirname(&dir, "zadani_").unwrap();
        assert_eq!(name, "zadani_abc123");
        let name2 = mangled_dirname(&dir, "reseni_").unwrap();
        assert!(name2.starts_with("reseni_") && name2.len() == "reseni_".len() + 16);
        assert!(dir.join(&name2).is_dir());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deadline_parsing() {
        let dt = parse_deadline("2026-11-08").unwrap();
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2026-11-08");
        assert!(parse_deadline("8.11.2026 23:59").is_some());
        assert!(parse_deadline("garbage").is_none());
    }
}
