//! Pure-Rust search tools: `grep`, `find`, `ls`.
//!
//! All three are implemented without any external binary dependency so Ra
//! works as a true single binary. The implementations use:
//!
//! - `ignore::WalkBuilder` for gitignore-aware directory traversal (grep/find/ls)
//! - `regex` crate for pattern matching (grep/find)
//! - `globset` for glob patterns (grep --glob, find --glob)
//!
//! Output is truncated to ~64 KiB so a stray wide search doesn't blow up
//! the model's context.

use crate::tool_ctx::ToolCtx;
use crate::tools::core::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::RegexBuilder;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

fn truncate(mut s: String) -> String {
    if s.len() > MAX_OUTPUT_BYTES {
        let cut = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|i| *i < MAX_OUTPUT_BYTES)
            .last()
            .unwrap_or(MAX_OUTPUT_BYTES);
        let dropped = s.len() - cut;
        s.truncate(cut);
        let _ = write!(s, "\n[… {dropped} bytes truncated]");
    }
    s
}

fn resolve_path(path: Option<&str>) -> PathBuf {
    match path {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

// ---------- GrepTool ---------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepParams {
    /// Pattern to search for (regex unless `fixed_string=true`).
    pub pattern: String,
    /// Path or directory to search in. Defaults to cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Case-insensitive search.
    #[serde(default)]
    pub case_insensitive: bool,
    /// Treat the pattern as a literal string, not a regex.
    #[serde(default)]
    pub fixed_string: bool,
    /// Glob filter (e.g. `*.rs`). Only files matching this glob are searched.
    #[serde(default)]
    pub glob: Option<String>,
    /// File-type filter (e.g. `rust` → `*.rs`, `py` → `*.py`).
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
    /// Cap on lines printed per file.
    #[serde(default)]
    pub max_count: Option<u32>,
    /// Show N lines of context around each match.
    #[serde(default)]
    pub context: Option<u32>,
    /// List matching filenames only.
    #[serde(default)]
    pub files_with_matches: bool,
}

pub struct GrepTool;

impl GrepTool {
    pub fn detect() -> Option<Self> {
        Some(Self)
    }
}

/// Map a file-type name to a glob extension pattern.
fn type_to_glob(t: &str) -> Option<&'static str> {
    match t {
        "rust" | "rs" => Some("*.rs"),
        "py" | "python" => Some("*.py"),
        "js" | "javascript" => Some("*.js"),
        "ts" | "typescript" => Some("*.ts"),
        "go" => Some("*.go"),
        "c" => Some("*.c"),
        "cpp" | "cxx" => Some("*.cpp"),
        "java" => Some("*.java"),
        "rb" | "ruby" => Some("*.rb"),
        "sh" | "bash" => Some("*.sh"),
        "toml" => Some("*.toml"),
        "yaml" | "yml" => Some("*.yaml"),
        "json" => Some("*.json"),
        "md" | "markdown" => Some("*.md"),
        "html" => Some("*.html"),
        "css" => Some("*.css"),
        _ => None,
    }
}

fn build_glob_set(globs: &[&str]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for g in globs {
        builder.add(Glob::new(g).with_context(|| format!("invalid glob: {g}"))?);
    }
    builder.build().context("build globset")
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search file contents using built-in regex engine. Recursive, respects \
         .gitignore by default. Use `pattern` plus optional `path`, `glob`, \
         `type`, `case_insensitive`, `fixed_string`, `context`, `max_count`, \
         `files_with_matches`."
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
        let _scope = crate::nemo_obs::tool_scope("grep");
        let p: GrepParams = serde_json::from_value(input).context("invalid params for grep")?;

        let pattern = if p.fixed_string {
            regex::escape(&p.pattern)
        } else {
            p.pattern.clone()
        };
        let re = RegexBuilder::new(&pattern)
            .case_insensitive(p.case_insensitive)
            .build()
            .with_context(|| format!("invalid regex: {}", p.pattern))?;

        // Build glob filter
        let mut glob_patterns: Vec<&str> = Vec::new();
        let glob_str;
        if let Some(g) = &p.glob {
            glob_str = g.clone();
            glob_patterns.push(&glob_str);
        }
        let type_glob;
        if let Some(t) = &p.file_type {
            if let Some(g) = type_to_glob(t) {
                type_glob = g.to_string();
                glob_patterns.push(&type_glob);
            }
        }
        let glob_set = if glob_patterns.is_empty() {
            None
        } else {
            Some(build_glob_set(&glob_patterns)?)
        };

        let root = resolve_path(p.path.as_deref());
        let context_lines = p.context.unwrap_or(0) as usize;
        let max_count = p.max_count.map(|n| n as usize);

        let mut out = String::new();
        let mut total_matches: usize = 0;

        let walker = WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.file_type().map(|t| !t.is_file()).unwrap_or(true) {
                continue;
            }
            let path = entry.path();

            // Apply glob filter against filename
            if let Some(gs) = &glob_set {
                let fname = path.file_name().unwrap_or_default();
                if !gs.is_match(fname) {
                    continue;
                }
            }

            let content = match fs::read(path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            // Skip binary files
            if content.contains(&0u8) {
                continue;
            }
            let text = String::from_utf8_lossy(&content);
            let lines: Vec<&str> = text.lines().collect();

            let mut file_matches = 0usize;
            let mut printed_lines: std::collections::HashSet<usize> = std::collections::HashSet::new();

            for (i, line) in lines.iter().enumerate() {
                if re.is_match(line) {
                    file_matches += 1;
                    if let Some(mc) = max_count {
                        if file_matches > mc {
                            break;
                        }
                    }
                    total_matches += 1;

                    if p.files_with_matches {
                        let _ = writeln!(out, "{}", path.display());
                        break;
                    }

                    // Context range
                    let start = i.saturating_sub(context_lines);
                    let end = (i + context_lines + 1).min(lines.len());
                    for j in start..end {
                        if printed_lines.insert(j) {
                            let sep = if j == i { ":" } else { "-" };
                            let _ = writeln!(out, "{}:{}{}", path.display(), j + 1, sep);
                            let _ = writeln!(out, "{}", lines[j]);
                        }
                    }
                    if out.len() > MAX_OUTPUT_BYTES {
                        return Ok(truncate(out));
                    }
                }
            }
            let _ = total_matches; // suppress unused warning
        }

        if out.is_empty() {
            return Ok("(no matches)".into());
        }
        Ok(truncate(out))
    }
}

// ---------- FindTool ---------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindParams {
    /// Filename pattern (regex by default, glob with `glob=true`).
    /// Empty / omitted lists everything under `path`.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Directory to search in. Defaults to cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Treat `pattern` as a glob.
    #[serde(default)]
    pub glob: bool,
    /// Restrict to `f` (files), `d` (directories), `l` (symlinks), `x` (executables).
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
    /// File extension to filter on (e.g. `rs`).
    #[serde(default)]
    pub extension: Option<String>,
    /// Include hidden files / directories.
    #[serde(default)]
    pub hidden: bool,
    /// Don't honour .gitignore.
    #[serde(default)]
    pub no_ignore: bool,
    /// Cap on results.
    #[serde(default)]
    pub max_results: Option<u32>,
}

pub struct FindTool;

impl FindTool {
    pub fn detect() -> Option<Self> {
        Some(Self)
    }
}

#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }
    fn description(&self) -> &str {
        "Find files / directories by name. Honours .gitignore by default. \
         Use `pattern` (regex unless `glob=true`), optional `path`, \
         `type` (f/d/l/x), `extension`, `hidden`, `no_ignore`, `max_results`."
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
        let _scope = crate::nemo_obs::tool_scope("find");
        let p: FindParams = serde_json::from_value(input).context("invalid params for find")?;

        let root = resolve_path(p.path.as_deref());
        let max_results = p.max_results.map(|n| n as usize);

        // Compile pattern
        enum PatternMatcher {
            None,
            Regex(regex::Regex),
            Glob(GlobSet),
        }
        let matcher = match &p.pattern {
            None => PatternMatcher::None,
            Some(s) if s.is_empty() => PatternMatcher::None,
            Some(pat) if p.glob => {
                let gs = build_glob_set(&[pat])?;
                PatternMatcher::Glob(gs)
            }
            Some(pat) => {
                let re = regex::Regex::new(pat)
                    .with_context(|| format!("invalid regex: {pat}"))?;
                PatternMatcher::Regex(re)
            }
        };

        let walker = WalkBuilder::new(&root)
            .hidden(!p.hidden)
            .git_ignore(!p.no_ignore)
            .git_global(!p.no_ignore)
            .git_exclude(!p.no_ignore)
            .build();

        let mut out = String::new();
        let mut count = 0usize;

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let ft = entry.file_type();
            // Type filter
            if let Some(type_filter) = &p.file_type {
                let ok = match type_filter.as_str() {
                    "f" => ft.as_ref().map(|t| t.is_file()).unwrap_or(false),
                    "d" => ft.as_ref().map(|t| t.is_dir()).unwrap_or(false),
                    "l" => ft.as_ref().map(|t| t.is_symlink()).unwrap_or(false),
                    "x" => {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            ft.as_ref().map(|t| t.is_file()).unwrap_or(false)
                                && fs::metadata(entry.path())
                                    .map(|m| m.permissions().mode() & 0o111 != 0)
                                    .unwrap_or(false)
                        }
                        #[cfg(not(unix))]
                        { ft.as_ref().map(|t| t.is_file()).unwrap_or(false) }
                    }
                    _ => true,
                };
                if !ok {
                    continue;
                }
            }

            let path = entry.path();

            // Extension filter
            if let Some(ext) = &p.extension {
                let file_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if file_ext != ext.trim_start_matches('.') {
                    continue;
                }
            }

            // Pattern filter (against filename only)
            let fname = path.file_name().unwrap_or_default().to_string_lossy();
            let matches = match &matcher {
                PatternMatcher::None => true,
                PatternMatcher::Regex(re) => re.is_match(&fname),
                PatternMatcher::Glob(gs) => gs.is_match(fname.as_ref()),
            };
            if !matches {
                continue;
            }

            let _ = writeln!(out, "{}", path.display());
            count += 1;
            if let Some(max) = max_results {
                if count >= max {
                    break;
                }
            }
            if out.len() > MAX_OUTPUT_BYTES {
                return Ok(truncate(out));
            }
        }

        if out.is_empty() {
            return Ok("(no results)".into());
        }
        Ok(truncate(out))
    }
}

// ---------- LsTool -----------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LsParams {
    /// Path to list. Defaults to cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Show hidden entries.
    #[serde(default)]
    pub all: bool,
    /// Long format with size + permissions.
    #[serde(default)]
    pub long: bool,
    /// Render as a tree.
    #[serde(default)]
    pub tree: bool,
    /// Recurse depth (only meaningful with `tree=true`).
    #[serde(default)]
    pub level: Option<u32>,
    /// Sort by modified time (newest first).
    #[serde(default)]
    pub sort_modified: bool,
}

pub struct LsTool;

impl LsTool {
    pub fn detect() -> Option<Self> {
        Some(Self)
    }
}

struct DirEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
    modified: u64,
    #[cfg(unix)]
    mode: u32,
}

fn read_dir_entries(dir: &Path, show_hidden: bool) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return entries,
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let meta = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode()
        };
        entries.push(DirEntry {
            path: e.path(),
            name,
            is_dir: meta.is_dir(),
            size: meta.len(),
            modified,
            #[cfg(unix)]
            mode,
        });
    }
    entries
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(unix)]
fn format_mode(mode: u32, is_dir: bool) -> String {
    let d = if is_dir { 'd' } else { '-' };
    let bits = [
        (0o400, 'r'), (0o200, 'w'), (0o100, 'x'),
        (0o040, 'r'), (0o020, 'w'), (0o010, 'x'),
        (0o004, 'r'), (0o002, 'w'), (0o001, 'x'),
    ];
    let perms: String = bits.iter().map(|(b, c)| if mode & b != 0 { *c } else { '-' }).collect();
    format!("{d}{perms}")
}

fn ls_flat(dir: &Path, params: &LsParams, out: &mut String) {
    let mut entries = read_dir_entries(dir, params.all);
    if params.sort_modified {
        entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    } else {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
    }
    for e in &entries {
        if params.long {
            #[cfg(unix)]
            let mode_str = format_mode(e.mode, e.is_dir);
            #[cfg(not(unix))]
            let mode_str = if e.is_dir { "d---------".to_string() } else { "----------".to_string() };
            let size_str = if e.is_dir { "     -".to_string() } else { format!("{:>6}", format_size(e.size)) };
            let suffix = if e.is_dir { "/" } else { "" };
            let _ = writeln!(out, "{mode_str} {size_str}  {}{suffix}", e.name);
        } else {
            let suffix = if e.is_dir { "/" } else { "" };
            let _ = writeln!(out, "{}{suffix}", e.name);
        }
    }
}

fn ls_tree(dir: &Path, params: &LsParams, prefix: &str, depth: u32, max_depth: u32, out: &mut String) {
    let mut entries = read_dir_entries(dir, params.all);
    if params.sort_modified {
        entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    } else {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
    }
    let len = entries.len();
    for (i, e) in entries.iter().enumerate() {
        let is_last = i + 1 == len;
        let connector = if is_last { "└── " } else { "├── " };
        let suffix = if e.is_dir { "/" } else { "" };
        let _ = writeln!(out, "{prefix}{connector}{}{suffix}", e.name);
        if e.is_dir && depth < max_depth {
            let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
            ls_tree(&e.path, params, &new_prefix, depth + 1, max_depth, out);
        }
        if out.len() > MAX_OUTPUT_BYTES {
            return;
        }
    }
}

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List directory contents. Optional `path`, `all`, `long`, `tree`, \
         `level`, `sort_modified`."
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
        let _scope = crate::nemo_obs::tool_scope("ls");
        let p: LsParams = serde_json::from_value(input).context("invalid params for ls")?;

        let dir = resolve_path(p.path.as_deref());
        let mut out = String::new();

        if p.tree {
            let max_depth = p.level.unwrap_or(3);
            let root_name = dir.file_name().unwrap_or(dir.as_os_str()).to_string_lossy();
            let _ = writeln!(out, "{}/", root_name);
            ls_tree(&dir, &p, "", 0, max_depth, &mut out);
        } else {
            ls_flat(&dir, &p, &mut out);
        }

        if out.is_empty() {
            return Ok("(empty directory)".into());
        }
        Ok(truncate(out))
    }
}
