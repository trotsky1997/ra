//! External-binary search tools: `grep` (ripgrep), `find` (fd), `ls` (eza).
//!
//! Each tool detects its binary on PATH at startup via a static probe. If
//! the binary is missing, `detect()` returns None and the tool registry
//! drops it with a one-line log; the agent's tool catalog stays valid
//! and the LLM never sees a tool it can't use.
//!
//! Output is captured combined stdout+stderr, truncated by the tool to
//! ~64 KiB so a stray `find /` doesn't blow up the model's context.

use crate::tool_ctx::ToolCtx;
use crate::tools::core::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use std::path::PathBuf;
use tokio::process::Command;

/// Soft cap on captured output (per tool call). Anything larger gets
/// truncated with a `[…N bytes truncated]` marker.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

fn locate(candidates: &[&str]) -> Option<PathBuf> {
    for name in candidates {
        if let Ok(p) = which::which(name) {
            return Some(p);
        }
    }
    None
}

async fn run_capture(mut cmd: Command, scope: &'static str) -> Result<String> {
    let _scope = crate::nemo_obs::tool_scope(scope);
    let out = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .context("spawn external binary")?;
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    if combined.len() > MAX_OUTPUT_BYTES {
        let cut = combined
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|i| *i < MAX_OUTPUT_BYTES)
            .last()
            .unwrap_or(MAX_OUTPUT_BYTES);
        let dropped = combined.len() - cut;
        combined.truncate(cut);
        combined.push_str(&format!("\n[… {dropped} bytes truncated]"));
    }
    if combined.is_empty() {
        // Surface exit code so the model can tell "no matches" from success.
        if let Some(code) = out.status.code() {
            if code != 0 {
                return Ok(format!("(no output, exit {code})"));
            }
        }
    }
    Ok(combined)
}

// ---------- GrepTool (ripgrep) --------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepParams {
    /// Pattern to search for. Interpreted as a regex by ripgrep unless
    /// `fixed_string=true`.
    pub pattern: String,
    /// Path or directory to search in. Defaults to the current working
    /// directory.
    #[serde(default)]
    pub path: Option<String>,
    /// Case-insensitive search (`-i`).
    #[serde(default)]
    pub case_insensitive: bool,
    /// Treat the pattern as a literal string, not a regex (`-F`).
    #[serde(default)]
    pub fixed_string: bool,
    /// Glob filter passed to `--glob` (e.g. `*.rs`).
    #[serde(default)]
    pub glob: Option<String>,
    /// File-type filter passed to `--type` (e.g. `rust`, `py`).
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
    /// Cap on lines printed per file (`--max-count`).
    #[serde(default)]
    pub max_count: Option<u32>,
    /// Show N lines of context around each match (`--context`).
    #[serde(default)]
    pub context: Option<u32>,
    /// List matching filenames only (`-l`).
    #[serde(default)]
    pub files_with_matches: bool,
}

pub struct GrepTool {
    bin: PathBuf,
}

impl GrepTool {
    pub fn detect() -> Option<Self> {
        Some(Self { bin: locate(&["rg"])? })
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search file contents using ripgrep (rg). Recursive, respects \
         .gitignore by default, fast on large repos. Use `pattern` plus \
         optional `path`, `glob`, `type`, `case_insensitive`, \
         `fixed_string`, `context`, `max_count`, `files_with_matches`."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(GrepParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<String> {
        let p: GrepParams =
            serde_json::from_value(input).context("invalid params for grep")?;

        let mut cmd = Command::new(&self.bin);
        cmd.arg("--color=never").arg("--line-number");
        if p.case_insensitive {
            cmd.arg("-i");
        }
        if p.fixed_string {
            cmd.arg("-F");
        }
        if p.files_with_matches {
            cmd.arg("-l");
        }
        if let Some(g) = &p.glob {
            cmd.arg("--glob").arg(g);
        }
        if let Some(t) = &p.file_type {
            cmd.arg("--type").arg(t);
        }
        if let Some(n) = p.max_count {
            cmd.arg("--max-count").arg(n.to_string());
        }
        if let Some(n) = p.context {
            cmd.arg("--context").arg(n.to_string());
        }
        cmd.arg("--").arg(&p.pattern);
        if let Some(path) = &p.path {
            cmd.arg(path);
        }

        run_capture(cmd, "grep").await
    }
}

// ---------- FindTool (fd) --------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindParams {
    /// Filename pattern (regex by default, glob with `glob=true`).
    /// Empty / omitted lists everything under `path`.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Directory to search in. Defaults to the current working directory.
    #[serde(default)]
    pub path: Option<String>,
    /// Treat `pattern` as a glob (`--glob`).
    #[serde(default)]
    pub glob: bool,
    /// Restrict to files (`f`), directories (`d`), symlinks (`l`), or
    /// executables (`x`). Maps to `--type`.
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
    /// File extension to filter on (e.g. `rs`).
    #[serde(default)]
    pub extension: Option<String>,
    /// Include hidden files / directories (`--hidden`).
    #[serde(default)]
    pub hidden: bool,
    /// Don't honour .gitignore (`--no-ignore`).
    #[serde(default)]
    pub no_ignore: bool,
    /// Cap on results (`--max-results`).
    #[serde(default)]
    pub max_results: Option<u32>,
}

pub struct FindTool {
    bin: PathBuf,
}

impl FindTool {
    pub fn detect() -> Option<Self> {
        // Debian ships fd as `fdfind` to avoid colliding with the kernel's `fd`.
        Some(Self { bin: locate(&["fd", "fdfind"])? })
    }
}

#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }
    fn description(&self) -> &str {
        "Find files / directories by name using fd. Honours .gitignore \
         by default. Use `pattern` (regex unless `glob=true`), optional \
         `path`, `type` (f/d/l/x), `extension`, `hidden`, `no_ignore`, \
         `max_results`."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(FindParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<String> {
        let p: FindParams =
            serde_json::from_value(input).context("invalid params for find")?;

        let mut cmd = Command::new(&self.bin);
        cmd.arg("--color=never");
        if p.glob {
            cmd.arg("--glob");
        }
        if p.hidden {
            cmd.arg("--hidden");
        }
        if p.no_ignore {
            cmd.arg("--no-ignore");
        }
        if let Some(t) = &p.file_type {
            cmd.arg("--type").arg(t);
        }
        if let Some(ext) = &p.extension {
            cmd.arg("--extension").arg(ext);
        }
        if let Some(n) = p.max_results {
            cmd.arg("--max-results").arg(n.to_string());
        }
        // fd's positional args are: PATTERN [PATH...]. Pass empty string
        // when listing-only so PATH still applies.
        cmd.arg(p.pattern.as_deref().unwrap_or(""));
        if let Some(path) = &p.path {
            cmd.arg(path);
        }

        run_capture(cmd, "find").await
    }
}

// ---------- LsTool (eza / exa) --------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LsParams {
    /// Path to list. Defaults to the current working directory.
    #[serde(default)]
    pub path: Option<String>,
    /// Show hidden entries (`-a`).
    #[serde(default)]
    pub all: bool,
    /// Long format with size + permissions (`-l`).
    #[serde(default)]
    pub long: bool,
    /// Render as a tree (`--tree`).
    #[serde(default)]
    pub tree: bool,
    /// Recurse depth (`--level`); only meaningful with `tree=true`.
    #[serde(default)]
    pub level: Option<u32>,
    /// Sort by modified time (`--sort=modified`).
    #[serde(default)]
    pub sort_modified: bool,
}

pub struct LsTool {
    bin: PathBuf,
}

impl LsTool {
    pub fn detect() -> Option<Self> {
        // eza is the actively-maintained successor; exa is the original.
        Some(Self { bin: locate(&["eza", "exa"])? })
    }
}

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List directory contents using eza (or exa). Optional `path`, \
         `all`, `long`, `tree`, `level`, `sort_modified`."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(LsParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<String> {
        let p: LsParams =
            serde_json::from_value(input).context("invalid params for ls")?;

        let mut cmd = Command::new(&self.bin);
        cmd.arg("--color=never");
        if p.all {
            cmd.arg("-a");
        }
        if p.long {
            cmd.arg("-l");
        }
        if p.tree {
            cmd.arg("--tree");
        }
        if let Some(lv) = p.level {
            cmd.arg("--level").arg(lv.to_string());
        }
        if p.sort_modified {
            cmd.arg("--sort=modified");
        }
        if let Some(path) = &p.path {
            cmd.arg(path);
        }

        run_capture(cmd, "ls").await
    }
}
