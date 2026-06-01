//! Extended built-in tools for structured search, discovery, listing, fuzzy
//! filtering, and controlled patch application.

use crate::tool_ctx::{FileChange, FileChangeDecision, ToolCtx};
use crate::tools::core::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{DirEntry as IgnoreDirEntry, WalkBuilder};
use regex::{Regex, RegexBuilder};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::UNIX_EPOCH;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const DEFAULT_LIMIT: usize = 500;
const DEFAULT_MATCH_LIMIT: usize = 200;
const DEFAULT_RECURSIVE_DEPTH: usize = 3;
const DEFAULT_SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];

fn resolve_path(path: Option<&str>) -> PathBuf {
    match path {
        Some(path) if !path.trim().is_empty() => PathBuf::from(path),
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

fn json_pretty<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).context("serialize tool output")
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn relative_to<'a>(path: &'a Path, root: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

fn display_path(path: &Path, root: &Path) -> String {
    if root.is_file() {
        if let Some(parent) = root.parent() {
            if let Ok(rel) = path.strip_prefix(parent) {
                if !rel.as_os_str().is_empty() {
                    return normalize_path(rel);
                }
            }
        }
    }

    if let Ok(rel) = path.strip_prefix(root) {
        if !rel.as_os_str().is_empty() {
            return normalize_path(rel);
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = path.strip_prefix(cwd) {
            if !rel.as_os_str().is_empty() {
                return normalize_path(rel);
            }
        }
    }

    normalize_path(path)
}

fn entry_is_default_skip_dir(entry: &IgnoreDirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .map(|name| DEFAULT_SKIP_DIRS.contains(&name))
        .unwrap_or(false)
}

fn walk_builder(
    root: &Path,
    include_hidden: bool,
    no_ignore: bool,
    max_depth: Option<usize>,
) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!include_hidden)
        .ignore(!no_ignore)
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore);
    if let Some(max_depth) = max_depth {
        builder.max_depth(Some(max_depth));
    }
    builder.filter_entry(move |entry| no_ignore || !entry_is_default_skip_dir(entry));
    builder
}

fn build_glob_set(pattern: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(pattern).with_context(|| format!("invalid glob: {pattern}"))?);
    builder.build().context("build glob matcher")
}

#[derive(Debug)]
struct PathGlob {
    set: GlobSet,
    basename_set: Option<GlobSet>,
}

impl PathGlob {
    fn new(pattern: impl Into<String>) -> Result<Self> {
        let pattern = pattern.into();
        let basename_set = if pattern.contains('/') || pattern.contains('\\') {
            None
        } else {
            Some(build_glob_set(&pattern)?)
        };
        let set = build_glob_set(&pattern)?;
        Ok(Self { set, basename_set })
    }

    fn is_match(&self, path: &Path, root: &Path) -> bool {
        let match_root = if root.is_file() {
            root.parent().unwrap_or(root)
        } else {
            root
        };
        let rel = normalize_path(relative_to(path, match_root));
        if self.set.is_match(&rel) {
            return true;
        }
        if let Some(basename_set) = &self.basename_set {
            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                return basename_set.is_match(name);
            }
        }
        false
    }
}

fn type_to_glob(file_type: &str) -> Result<&'static str> {
    match file_type {
        "rust" | "rs" => Ok("*.rs"),
        "py" | "python" => Ok("*.py"),
        "js" | "javascript" => Ok("*.js"),
        "jsx" => Ok("*.jsx"),
        "ts" | "typescript" => Ok("*.ts"),
        "tsx" => Ok("*.tsx"),
        "go" => Ok("*.go"),
        "c" => Ok("*.c"),
        "cpp" | "cxx" => Ok("*.cpp"),
        "java" => Ok("*.java"),
        "rb" | "ruby" => Ok("*.rb"),
        "sh" | "bash" => Ok("*.sh"),
        "toml" => Ok("*.toml"),
        "yaml" | "yml" => Ok("*.yml"),
        "json" => Ok("*.json"),
        "md" | "markdown" => Ok("*.md"),
        "html" => Ok("*.html"),
        "css" => Ok("*.css"),
        other => anyhow::bail!("unknown file type filter: {other}"),
    }
}

fn path_matches_all_globs(path: &Path, root: &Path, globs: &[PathGlob]) -> bool {
    globs.iter().all(|glob| glob.is_match(path, root))
}

fn file_kind(path: &Path, ft: Option<std::fs::FileType>) -> &'static str {
    match ft {
        Some(ft) if ft.is_dir() => "dir",
        Some(ft) if ft.is_file() => "file",
        Some(ft) if ft.is_symlink() => "symlink",
        _ => {
            if path.is_dir() {
                "dir"
            } else if path.is_file() {
                "file"
            } else {
                "other"
            }
        }
    }
}

fn matches_kind_filter(kind: &str, filter: &Option<String>) -> Result<bool> {
    let Some(filter) = filter else {
        return Ok(true);
    };
    match filter.as_str() {
        "any" | "a" => Ok(true),
        "file" | "f" => Ok(kind == "file"),
        "dir" | "directory" | "d" => Ok(kind == "dir"),
        "symlink" | "link" | "l" => Ok(kind == "symlink"),
        other => anyhow::bail!("unknown kind filter: {other}"),
    }
}

fn validate_kind_filter(filter: &Option<String>) -> Result<()> {
    let Some(filter) = filter else {
        return Ok(());
    };
    match filter.as_str() {
        "any" | "a" | "file" | "f" | "dir" | "directory" | "d" | "symlink" | "link" | "l" => Ok(()),
        other => anyhow::bail!("unknown kind filter: {other}"),
    }
}

// ---------- grep -----------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepParams {
    /// Pattern to search for. Treated as a regex unless `fixed_string` is true.
    pub pattern: String,
    /// File or directory to search. Defaults to the agent's cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Glob filter such as `*.rs` or `src/**/*.ts`.
    #[serde(default)]
    pub glob: Option<String>,
    /// File type filter such as `rust`, `python`, `json`, or `markdown`.
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
    /// Case-insensitive search.
    #[serde(default)]
    pub case_insensitive: bool,
    /// Treat `pattern` as a literal string instead of a regex.
    #[serde(default)]
    pub fixed_string: bool,
    /// Include hidden files and directories.
    #[serde(default)]
    pub include_hidden: bool,
    /// Disable .gitignore/.ignore/default build-directory filtering.
    #[serde(default)]
    pub no_ignore: bool,
    /// Maximum number of matches to return.
    #[serde(default, alias = "limit")]
    pub max_matches: Option<usize>,
}

#[derive(Debug, Serialize)]
struct GrepMatch {
    path: String,
    line: usize,
    text: String,
}

#[derive(Debug, Serialize)]
struct GrepOutput {
    matches: Vec<GrepMatch>,
    truncated: bool,
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents with structured, bounded output. Recursive by \
         default, skips hidden/gitignored/build directories unless requested, \
         and supports regex or fixed-string matching plus glob/type filters."
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
        let params: GrepParams =
            serde_json::from_value(input).context("invalid params for grep")?;
        let pattern = if params.fixed_string {
            regex::escape(&params.pattern)
        } else {
            params.pattern.clone()
        };
        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(params.case_insensitive)
            .build()
            .with_context(|| format!("invalid regex: {}", params.pattern))?;

        let root = resolve_path(params.path.as_deref());
        let glob_filters = grep_globs(&params)?;
        let limit = params.max_matches.unwrap_or(DEFAULT_MATCH_LIMIT);
        let mut matches = Vec::new();
        let mut truncated = false;

        if root.is_file() {
            truncated = grep_file(&root, &root, &regex, &glob_filters, limit, &mut matches)?;
        } else {
            let walker = walk_builder(&root, params.include_hidden, params.no_ignore, None).build();
            for entry in walker {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };
                if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    continue;
                }
                if grep_file(
                    entry.path(),
                    &root,
                    &regex,
                    &glob_filters,
                    limit,
                    &mut matches,
                )? {
                    truncated = true;
                    break;
                }
            }
        }

        Ok(json_pretty(&GrepOutput { matches, truncated })?)
    }
}

fn grep_globs(params: &GrepParams) -> Result<Vec<PathGlob>> {
    let mut globs = Vec::new();
    if let Some(glob) = &params.glob {
        globs.push(PathGlob::new(glob)?);
    }
    if let Some(file_type) = &params.file_type {
        globs.push(PathGlob::new(type_to_glob(file_type)?)?);
    }
    Ok(globs)
}

fn grep_file(
    path: &Path,
    root: &Path,
    regex: &Regex,
    glob_filters: &[PathGlob],
    limit: usize,
    matches: &mut Vec<GrepMatch>,
) -> Result<bool> {
    if matches.len() >= limit {
        return Ok(true);
    }
    if !path_matches_all_globs(path, root, glob_filters) {
        return Ok(false);
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(false),
    };
    if bytes.contains(&0) {
        return Ok(false);
    }
    let text = String::from_utf8_lossy(&bytes);
    for (idx, line) in text.lines().enumerate() {
        if regex.is_match(line) {
            matches.push(GrepMatch {
                path: display_path(path, root),
                line: idx + 1,
                text: line.to_string(),
            });
            if matches.len() >= limit {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// ---------- glob -----------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GlobParams {
    /// Glob pattern to match. Defaults to `**/*`.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Root directory to search. Defaults to the agent's cwd.
    #[serde(default, alias = "root")]
    pub path: Option<String>,
    /// Restrict results to `file`/`f`, `dir`/`d`, `symlink`/`l`, or `any`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Maximum traversal depth relative to `path`.
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Include hidden files and directories.
    #[serde(default)]
    pub include_hidden: bool,
    /// Disable .gitignore/.ignore/default build-directory filtering.
    #[serde(default)]
    pub no_ignore: bool,
    /// Maximum number of paths to return.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct GlobOutput {
    paths: Vec<String>,
    truncated: bool,
}

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Discover files or directories with glob semantics. Skips hidden, \
         gitignored, and common build directories by default. `*.rs` matches \
         nested Rust files by basename, while slash-containing patterns match \
         paths relative to the root."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(GlobParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("glob");
        let params: GlobParams =
            serde_json::from_value(input).context("invalid params for glob")?;
        let root = resolve_path(params.path.as_deref());
        let matcher = PathGlob::new(params.pattern.as_deref().unwrap_or("**/*"))?;
        validate_kind_filter(&params.kind)?;
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
        let mut paths = Vec::new();
        let mut truncated = false;

        let walker = walk_builder(
            &root,
            params.include_hidden,
            params.no_ignore,
            params.max_depth,
        )
        .build();
        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if entry.depth() == 0 {
                continue;
            }
            let kind = file_kind(entry.path(), entry.file_type());
            if !matches_kind_filter(kind, &params.kind)? {
                continue;
            }
            if !matcher.is_match(entry.path(), &root) {
                continue;
            }
            paths.push(display_path(entry.path(), &root));
            if paths.len() >= limit {
                truncated = true;
                break;
            }
        }

        Ok(json_pretty(&GlobOutput { paths, truncated })?)
    }
}

// ---------- ls -------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LsParams {
    /// Directory to list. Defaults to the agent's cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Recurse into children.
    #[serde(default)]
    pub recursive: bool,
    /// Maximum traversal depth when recursive. Defaults to 3.
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Include hidden files and directories.
    #[serde(default)]
    pub include_hidden: bool,
    /// Disable .gitignore/.ignore/default build-directory filtering.
    #[serde(default)]
    pub no_ignore: bool,
    /// Maximum number of entries to return.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct LsEntry {
    path: String,
    name: String,
    kind: String,
    size: Option<u64>,
    modified_unix: Option<u64>,
}

#[derive(Debug, Serialize)]
struct LsOutput {
    entries: Vec<LsEntry>,
    truncated: bool,
}

pub struct LsTool;

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "List directory entries as structured data. Non-recursive by default; \
         skips hidden, gitignored, and common build directories unless \
         requested."
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
        let params: LsParams = serde_json::from_value(input).context("invalid params for ls")?;
        let root = resolve_path(params.path.as_deref());
        let max_depth = if params.recursive {
            Some(params.max_depth.unwrap_or(DEFAULT_RECURSIVE_DEPTH))
        } else {
            Some(1)
        };
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
        let mut entries = Vec::new();
        let mut truncated = false;

        let walker =
            walk_builder(&root, params.include_hidden, params.no_ignore, max_depth).build();
        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if entry.depth() == 0 {
                continue;
            }
            let metadata = entry.metadata().ok();
            let kind = file_kind(entry.path(), entry.file_type()).to_string();
            let modified_unix = metadata
                .as_ref()
                .and_then(|meta| meta.modified().ok())
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs());
            entries.push(LsEntry {
                path: display_path(entry.path(), &root),
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
                size: metadata
                    .as_ref()
                    .filter(|meta| meta.is_file())
                    .map(|meta| meta.len()),
                modified_unix,
            });
            if entries.len() >= limit {
                truncated = true;
                break;
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(json_pretty(&LsOutput { entries, truncated })?)
    }
}

// ---------- fuzzy ----------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FuzzyParams {
    /// Candidate strings to rank or filter.
    pub candidates: Vec<String>,
    /// Fuzzy query.
    pub query: String,
    /// Maximum number of matches to return.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Preserve input order instead of sorting by score.
    #[serde(default)]
    pub no_sort: bool,
    /// Require a case-insensitive substring match instead of fuzzy subsequence.
    #[serde(default)]
    pub exact: bool,
}

#[derive(Debug, Serialize)]
struct FuzzyMatch {
    rank: usize,
    value: String,
    score: i64,
}

#[derive(Debug, Serialize)]
struct FuzzyOutput {
    backend: &'static str,
    matches: Vec<FuzzyMatch>,
}

pub struct FuzzyTool;

#[async_trait]
impl Tool for FuzzyTool {
    fn name(&self) -> &str {
        "fuzzy"
    }

    fn description(&self) -> &str {
        "Non-interactive fuzzy candidate filtering. Returns ranked matches \
         without opening a TTY UI; use this instead of interactive fzf."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(FuzzyParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("fuzzy");
        let params: FuzzyParams =
            serde_json::from_value(input).context("invalid params for fuzzy")?;
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
        let mut scored: Vec<(usize, String, i64)> = params
            .candidates
            .into_iter()
            .enumerate()
            .filter_map(|(idx, candidate)| {
                fuzzy_score(&candidate, &params.query, params.exact)
                    .map(|score| (idx, candidate, score))
            })
            .collect();

        if params.no_sort {
            scored.sort_by_key(|(idx, _, _)| *idx);
        } else {
            scored.sort_by(|a, b| match b.2.cmp(&a.2) {
                Ordering::Equal => a.1.cmp(&b.1),
                order => order,
            });
        }

        let matches = scored
            .into_iter()
            .take(limit)
            .enumerate()
            .map(|(rank, (_, value, score))| FuzzyMatch {
                rank: rank + 1,
                value,
                score,
            })
            .collect();
        Ok(json_pretty(&FuzzyOutput {
            backend: "builtin",
            matches,
        })?)
    }
}

fn fuzzy_score(candidate: &str, query: &str, exact: bool) -> Option<i64> {
    let candidate_lc = candidate.to_lowercase();
    let query_lc = query.to_lowercase();
    if query_lc.is_empty() {
        return Some(0);
    }
    if exact {
        return candidate_lc
            .find(&query_lc)
            .map(|idx| 10_000 - idx as i64 - candidate_lc.len() as i64);
    }

    let mut positions = Vec::new();
    let mut cursor = 0usize;
    for q in query_lc.chars() {
        let tail = &candidate_lc[cursor..];
        let found = tail.find(q)?;
        let absolute = cursor + found;
        positions.push(absolute);
        cursor = absolute + q.len_utf8();
    }

    let start = *positions.first().unwrap_or(&0) as i64;
    let span = (*positions.last().unwrap_or(&0) - *positions.first().unwrap_or(&0) + 1) as i64;
    let contiguous_bonus = positions
        .windows(2)
        .filter(|pair| pair[1] == pair[0] + 1)
        .count() as i64
        * 50;
    let boundary_bonus = positions
        .iter()
        .filter(|pos| {
            if **pos == 0 {
                return true;
            }
            candidate_lc
                .as_bytes()
                .get(pos.saturating_sub(1))
                .map(|b| matches!(b, b'/' | b'_' | b'-' | b'.' | b' '))
                .unwrap_or(false)
        })
        .count() as i64
        * 25;

    Some(10_000 + contiguous_bonus + boundary_bonus - span * 10 - start - candidate_lc.len() as i64)
}

// ---------- apply_patch ----------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyPatchParams {
    /// Unified diff text to apply.
    pub patch: String,
    /// Only validate the patch with `git apply --check`; do not write files.
    #[serde(default)]
    pub check_only: bool,
    /// Apply the patch in reverse.
    #[serde(default)]
    pub reverse: bool,
    /// Strip this many leading path components (`git apply -pN`).
    #[serde(default)]
    pub strip: Option<u8>,
    /// Prepend a safe relative directory to all patched filenames.
    #[serde(default)]
    pub directory: Option<String>,
    /// Working directory for `git apply`. Defaults to the process cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Ignore whitespace in context lines.
    #[serde(default)]
    pub ignore_whitespace: bool,
    /// Whitespace handling: nowarn, warn, fix, error, or error-all.
    #[serde(default)]
    pub whitespace: Option<String>,
    /// Infer hunk line counts from the patch.
    #[serde(default)]
    pub recount: bool,
    /// Allow zero-context unified diffs.
    #[serde(default)]
    pub unidiff_zero: bool,
}

pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a unified diff using a controlled `git apply` subset. The \
         tool validates with `git apply --check` before writing and supports \
         check_only, reverse, strip, directory, cwd, ignore_whitespace, \
         whitespace, recount, and unidiff_zero."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(ApplyPatchParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("apply_patch");
        let params: ApplyPatchParams =
            serde_json::from_value(input).context("invalid params for apply_patch")?;
        validate_apply_patch_params(&params)?;

        let check_output = run_git_apply(&params, true)
            .await
            .context("patch check failed")?;
        if params.check_only {
            return Ok(if check_output.trim().is_empty() {
                "patch applies cleanly".to_string()
            } else {
                check_output
            });
        }

        if let Some(approver) = &ctx.file_approver {
            match approver
                .approve_file_change(FileChange {
                    call_id: call_id.to_string(),
                    tool_name: "apply_patch".to_string(),
                    path: "(patch)".to_string(),
                    old_content: None,
                    new_content: params.patch.clone(),
                    diff: params.patch.clone(),
                    summary: "apply patch".to_string(),
                })
                .await?
            {
                FileChangeDecision::Accept => {}
                FileChangeDecision::Reject => {
                    return Err(anyhow::anyhow!("file change rejected by user"));
                }
            }
        }

        let apply_output = run_git_apply(&params, false).await?;
        Ok(if apply_output.trim().is_empty() {
            "applied patch".to_string()
        } else {
            apply_output
        })
    }
}

fn validate_apply_patch_params(params: &ApplyPatchParams) -> Result<()> {
    if params.patch.trim().is_empty() {
        anyhow::bail!("patch must not be empty");
    }
    if let Some(directory) = &params.directory {
        validate_safe_relative_path(directory, "directory")?;
    }
    if let Some(cwd) = &params.cwd {
        let cwd_path = Path::new(cwd);
        if cwd.trim().is_empty() {
            anyhow::bail!("cwd must not be empty");
        }
        if !cwd_path.exists() {
            anyhow::bail!("cwd does not exist: {cwd}");
        }
        if !cwd_path.is_dir() {
            anyhow::bail!("cwd must be a directory: {cwd}");
        }
    }
    if let Some(whitespace) = &params.whitespace {
        match whitespace.as_str() {
            "nowarn" | "warn" | "fix" | "error" | "error-all" => {}
            other => anyhow::bail!("invalid whitespace mode: {other}"),
        }
    }
    Ok(())
}

fn validate_safe_relative_path(path: &str, name: &str) -> Result<()> {
    let path = Path::new(path);
    if path.is_absolute() {
        anyhow::bail!("{name} must be relative");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        anyhow::bail!("{name} must not contain '..'");
    }
    Ok(())
}

fn git_apply_args(params: &ApplyPatchParams, check: bool) -> Vec<String> {
    let mut args = vec!["apply".to_string()];
    if check {
        args.push("--check".to_string());
    }
    if params.reverse {
        args.push("--reverse".to_string());
    }
    if let Some(strip) = params.strip {
        args.push(format!("-p{strip}"));
    }
    if let Some(directory) = &params.directory {
        args.push(format!("--directory={directory}"));
    }
    if params.ignore_whitespace {
        args.push("--ignore-whitespace".to_string());
    }
    if let Some(whitespace) = &params.whitespace {
        args.push(format!("--whitespace={whitespace}"));
    }
    if params.recount {
        args.push("--recount".to_string());
    }
    if params.unidiff_zero {
        args.push("--unidiff-zero".to_string());
    }
    args
}

async fn run_git_apply(params: &ApplyPatchParams, check: bool) -> Result<String> {
    let args = git_apply_args(params, check);
    let mut command = Command::new("git");
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &params.cwd {
        command.current_dir(cwd);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn `git {}`", args.join(" ")))?;
    let mut stdin = child.stdin.take().context("open git apply stdin")?;
    stdin
        .write_all(params.patch.as_bytes())
        .await
        .context("write patch to git apply stdin")?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .context("wait for git apply")?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if !output.status.success() {
        let mut rendered = String::new();
        let _ = writeln!(
            rendered,
            "`git {}` exited with status {}",
            args.join(" "),
            output.status.code().unwrap_or(-1)
        );
        rendered.push_str(&combined);
        anyhow::bail!("{rendered}");
    }
    Ok(combined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_glob_matches_nested_paths() {
        let matcher = PathGlob::new("*.rs").unwrap();
        assert!(matcher.is_match(Path::new("src/main.rs"), Path::new(".")));
        assert!(!matcher.is_match(Path::new("src/main.py"), Path::new(".")));
    }

    #[test]
    fn fuzzy_prefers_tighter_match() {
        let main = fuzzy_score("src/main.rs", "main", false).unwrap();
        let markdown = fuzzy_score("README.md", "main", false);
        assert!(markdown.is_none());
        assert!(main > 0);
    }
}
