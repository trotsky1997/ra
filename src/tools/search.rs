//! External-binary search tools: `grep` (ripgrep), `find` (fd), `ls` (eza).
//!
//! Each tool detects its binary on PATH at startup via a static probe. If
//! the binary is missing, `detect()` returns None and the tool registry
//! drops it with a one-line log; the agent's tool catalog stays valid
//! and the LLM never sees a tool it can't use.
//!
//! When [`RtkRewriter`](crate::tools::RtkRewriter) is configured on the
//! ToolCtx, each tool first asks RTK to rewrite the shell-equivalent of
//! the command it's about to run (`rg ...`, `fd ...`, `eza ...`). If
//! RTK has a recipe (almost always for these binaries), the rewritten
//! command runs through `/bin/sh -c` and we get RTK's compressed
//! output; otherwise we exec the binary directly via argv.
//!
//! Output is captured combined stdout+stderr, truncated by the tool to
//! ~64 KiB so a stray `find /` doesn't blow up the model's context.

use crate::events::Event;
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

/// Run a tool by either asking RTK to rewrite its shell-form first, or
/// executing the argv directly. `argv[0]` is the binary path; the rest
/// are arguments. `scope` is the ATOF scope label.
///
/// `call_id` is used solely so the `[rtk] ...` notice can be threaded
/// onto the broadcast bus next to the tool's own ToolCallUpdate stream.
async fn run_with_rtk(
    argv: Vec<String>,
    ctx: &ToolCtx,
    call_id: &str,
    scope: &'static str,
) -> Result<String> {
    let _scope = crate::nemo_obs::tool_scope(scope);

    let rewritten = if ctx.rtk.is_active() {
        // RTK matches commands by their canonical name. Pass the basename
        // (so `/usr/bin/rg` looks like `rg`) and normalise the Debian
        // alias `fdfind` → `fd` so RTK's recipe table catches it.
        let probe = canonical_probe(&argv);
        ctx.rtk.rewrite(&probe).await
    } else {
        None
    };

    let out = match rewritten {
        Some(rewritten) => {
            let _ = ctx.events.send(Event::ToolCallUpdate {
                id: call_id.to_string(),
                chunk: format!("[rtk] {} → {}", canonical_probe(&argv), rewritten),
            });
            tokio::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(&rewritten)
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .with_context(|| format!("spawn `{rewritten}`"))?
        }
        None => {
            let mut cmd = Command::new(&argv[0]);
            for a in &argv[1..] {
                cmd.arg(a);
            }
            cmd.stdin(std::process::Stdio::null())
                .output()
                .await
                .context("spawn external binary")?
        }
    };
    finish_capture(out)
}

/// Build the shell-form string that RTK's rewrite table looks up. Strips
/// the binary's directory (`/usr/bin/rg` → `rg`) and aliases the Debian
/// `fdfind` back to its upstream name `fd` so RTK matches.
fn canonical_probe(argv: &[String]) -> String {
    if argv.is_empty() {
        return String::new();
    }
    let mut bin = std::path::Path::new(&argv[0])
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| argv[0].clone());
    if bin == "fdfind" {
        bin = "fd".into();
    }
    let mut argv = argv.to_vec();
    argv[0] = bin;
    quote_argv(&argv)
}

fn finish_capture(out: std::process::Output) -> Result<String> {
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
        if let Some(code) = out.status.code() {
            if code != 0 {
                return Ok(format!("(no output, exit {code})"));
            }
        }
    }
    Ok(combined)
}

/// Quote an argv list back into a single shell-safe command string for
/// RTK to look up by canonical form.
fn quote_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty()
                || a.chars().any(|c| {
                    c.is_whitespace()
                        || matches!(c, '"' | '\'' | '\\' | '$' | '`' | '*' | '?' | '|' | '&' | ';' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '#' | '!')
                })
            {
                let escaped = a.replace('\'', "'\\''");
                format!("'{escaped}'")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let p: GrepParams =
            serde_json::from_value(input).context("invalid params for grep")?;

        let mut argv: Vec<String> = vec![
            self.bin.display().to_string(),
            "--color=never".into(),
            "--line-number".into(),
        ];
        if p.case_insensitive {
            argv.push("-i".into());
        }
        if p.fixed_string {
            argv.push("-F".into());
        }
        if p.files_with_matches {
            argv.push("-l".into());
        }
        if let Some(g) = &p.glob {
            argv.push("--glob".into());
            argv.push(g.clone());
        }
        if let Some(t) = &p.file_type {
            argv.push("--type".into());
            argv.push(t.clone());
        }
        if let Some(n) = p.max_count {
            argv.push("--max-count".into());
            argv.push(n.to_string());
        }
        if let Some(n) = p.context {
            argv.push("--context".into());
            argv.push(n.to_string());
        }
        argv.push("--".into());
        argv.push(p.pattern.clone());
        if let Some(path) = &p.path {
            argv.push(path.clone());
        }

        run_with_rtk(argv, ctx, call_id, "grep").await
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
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let p: FindParams =
            serde_json::from_value(input).context("invalid params for find")?;

        let mut argv: Vec<String> = vec![
            self.bin.display().to_string(),
            "--color=never".into(),
        ];
        if p.glob {
            argv.push("--glob".into());
        }
        if p.hidden {
            argv.push("--hidden".into());
        }
        if p.no_ignore {
            argv.push("--no-ignore".into());
        }
        if let Some(t) = &p.file_type {
            argv.push("--type".into());
            argv.push(t.clone());
        }
        if let Some(ext) = &p.extension {
            argv.push("--extension".into());
            argv.push(ext.clone());
        }
        if let Some(n) = p.max_results {
            argv.push("--max-results".into());
            argv.push(n.to_string());
        }
        // fd's positional args are: PATTERN [PATH...]. Pass empty string
        // when listing-only so PATH still applies.
        argv.push(p.pattern.clone().unwrap_or_default());
        if let Some(path) = &p.path {
            argv.push(path.clone());
        }

        run_with_rtk(argv, ctx, call_id, "find").await
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
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let p: LsParams =
            serde_json::from_value(input).context("invalid params for ls")?;

        let mut argv: Vec<String> = vec![
            self.bin.display().to_string(),
            "--color=never".into(),
        ];
        if p.all {
            argv.push("-a".into());
        }
        if p.long {
            argv.push("-l".into());
        }
        if p.tree {
            argv.push("--tree".into());
        }
        if let Some(lv) = p.level {
            argv.push("--level".into());
            argv.push(lv.to_string());
        }
        if p.sort_modified {
            argv.push("--sort=modified".into());
        }
        if let Some(path) = &p.path {
            argv.push(path.clone());
        }

        run_with_rtk(argv, ctx, call_id, "ls").await
    }
}
