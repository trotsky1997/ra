//! Native comby wrapper.
//!
//! This tool exposes common structural rewrite workflows as enum actions while
//! delegating matching and rewriting semantics to the system `comby` binary.

use crate::events::Event;
use crate::tool_ctx::ToolCtx;
use crate::tools::core::Tool;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const COMBY_BINARY: &str = "comby";
const DEFAULT_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 65_536;
const DEFAULT_STDERR_BYTES: usize = 32_000;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CombyAction {
    /// Apply match_template -> rewrite_template in place across matching files.
    Rewrite,
    /// Report matches without modifying files.
    Check,
    /// Show unified diff for the requested rewrite without modifying files.
    Diff,
}

impl CombyAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rewrite => "rewrite",
            Self::Check => "check",
            Self::Diff => "diff",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CombyParams {
    /// Which comby operation to run.
    pub action: CombyAction,
    /// Comby match template, e.g. `foo(:[arg])`.
    pub match_template: String,
    /// Comby rewrite template. Required for `rewrite` and `diff`.
    #[serde(default)]
    pub rewrite_template: Option<String>,
    /// File extensions to match, e.g. [".rs", ".py"]. Empty lets comby decide.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Root directory passed to comby with `-d`.
    #[serde(default)]
    pub directory: Option<String>,
    /// Language matcher passed with `-matcher`, e.g. "rust", "python", "generic".
    #[serde(default)]
    pub matcher: Option<String>,
    /// Only match files whose path matches this regex.
    #[serde(default)]
    pub include_files: Option<String>,
    /// Exclude files whose path matches this regex.
    #[serde(default)]
    pub exclude_files: Option<String>,
    /// Additional comby flags passed after Ra's action flags.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Working directory for the command. Relative `directory` is left as a comby argument.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Process timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Maximum bytes in Ra's returned JSON envelope. Set to 0 for unbounded.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

pub struct CombyTool;

#[async_trait]
impl Tool for CombyTool {
    fn name(&self) -> &str {
        COMBY_BINARY
    }

    fn description(&self) -> &str {
        "Run the native comby CLI for structural code rewriting. Supports \
         `rewrite` (in-place), `check` (match-only), and `diff` actions with \
         argv-safe process spawning. Returns a bounded JSON envelope with \
         stdout, stderr, exit status, truncation state, validation errors, \
         timeouts, and missing-comby guidance."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(CombyParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope(COMBY_BINARY);
        let params: CombyParams =
            serde_json::from_value(input).context("invalid params for comby")?;
        let action = params.action;
        let invocation = match CombyInvocation::from_params(params, &ctx.cwd).await {
            Ok(invocation) => invocation,
            Err(error) => return Ok(invalid_request_json(action, error)),
        };

        execute_comby(call_id, invocation, ctx).await
    }
}

#[derive(Debug, Clone)]
struct CombyInvocation {
    action: CombyAction,
    args: Vec<String>,
    cwd: PathBuf,
    timeout_ms: u64,
    max_output_bytes: usize,
}

impl CombyInvocation {
    async fn from_params(params: CombyParams, session_cwd: &Path) -> Result<Self> {
        if params.match_template.is_empty() {
            return Err(anyhow!("comby requires a non-empty match_template"));
        }

        let rewrite_template = non_empty(params.rewrite_template);
        if matches!(params.action, CombyAction::Rewrite | CombyAction::Diff)
            && rewrite_template.is_none()
        {
            return Err(anyhow!(
                "comby action `{}` requires rewrite_template",
                params.action.as_str()
            ));
        }

        let cwd = resolve_cwd(params.cwd, session_cwd);
        validate_cwd(&cwd).await?;

        let mut args = Vec::new();
        args.push(params.match_template);
        args.push(match params.action {
            CombyAction::Check => String::new(),
            CombyAction::Rewrite | CombyAction::Diff => {
                rewrite_template.expect("validated rewrite template")
            }
        });
        args.extend(params.extensions);

        if let Some(directory) = non_empty(params.directory) {
            args.push("-d".to_string());
            args.push(directory);
        }
        if let Some(matcher) = non_empty(params.matcher) {
            args.push("-matcher".to_string());
            args.push(matcher);
        }
        if let Some(include_files) = non_empty(params.include_files) {
            args.push("-include-files".to_string());
            args.push(include_files);
        }
        if let Some(exclude_files) = non_empty(params.exclude_files) {
            args.push("-exclude-files".to_string());
            args.push(exclude_files);
        }

        match params.action {
            CombyAction::Rewrite => args.push("-in-place".to_string()),
            CombyAction::Check => args.push("-match-only".to_string()),
            CombyAction::Diff => args.push("-diff".to_string()),
        }
        args.extend(params.extra_args);

        Ok(Self {
            action: params.action,
            args,
            cwd,
            timeout_ms: params.timeout_ms,
            max_output_bytes: params.max_output_bytes,
        })
    }

    fn command_json(&self) -> serde_json::Value {
        json!({
            "program": COMBY_BINARY,
            "args": self.args,
            "cwd": self.cwd,
        })
    }

    fn summary(&self) -> String {
        shell_words(COMBY_BINARY, &self.args)
    }
}

async fn execute_comby(
    call_id: &str,
    invocation: CombyInvocation,
    ctx: &ToolCtx,
) -> Result<String> {
    let comby = match which::which(COMBY_BINARY) {
        Ok(path) => path,
        Err(_) => return Ok(missing_comby_json(&invocation)),
    };

    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[comby] {}", invocation.summary()),
    });

    let output = match run_comby(&comby, &invocation).await {
        Ok(output) => output,
        Err(CombyRunError::Timeout) => return Ok(timeout_json(&invocation)),
        Err(CombyRunError::Other(error)) => return Err(error),
    };

    let exit_code = output.exit_code;
    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[exit={exit_code}]"),
    });

    format_comby_output(&invocation, output)
}

#[derive(Debug)]
enum CombyRunError {
    Timeout,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for CombyRunError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

#[derive(Debug, Clone)]
struct CombyOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

async fn run_comby(
    binary_path: &Path,
    invocation: &CombyInvocation,
) -> std::result::Result<CombyOutput, CombyRunError> {
    let child = Command::new(binary_path)
        .args(&invocation.args)
        .current_dir(&invocation.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "spawn {} in {}",
                invocation.summary(),
                invocation.cwd.display()
            )
        })?;

    let output_future = child.wait_with_output();
    let output =
        match tokio::time::timeout(Duration::from_millis(invocation.timeout_ms), output_future)
            .await
        {
            Ok(output) => output,
            Err(_) => return Err(CombyRunError::Timeout),
        }
        .context("wait comby")?;

    Ok(CombyOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn format_comby_output(invocation: &CombyInvocation, output: CombyOutput) -> Result<String> {
    let stdout = output.stdout;
    let mut stderr = output.stderr;
    let ok = output.exit_code == 0;
    let stderr_truncated = trim_to_char_budget(&mut stderr, DEFAULT_STDERR_BYTES);

    let mut base = json!({
        "ok": ok,
        "tool": COMBY_BINARY,
        "action": invocation.action.as_str(),
        "command": invocation.command_json(),
        "exit_code": output.exit_code,
        "stdout": stdout.clone(),
        "stderr": nullable_string(&stderr),
        "truncated": stderr_truncated,
    });

    if !ok {
        base["error"] = json!({
            "kind": "command_failed",
            "message": "comby exited with a non-zero status"
        });
    }

    bounded_output_json(base, &stdout, stderr, invocation.max_output_bytes)
}

fn invalid_request_json(action: CombyAction, error: anyhow::Error) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": COMBY_BINARY,
        "action": action.as_str(),
        "command": {
            "program": COMBY_BINARY,
            "args": [],
            "cwd": null
        },
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "invalid_request",
            "message": error.to_string()
        }
    }))
    .expect("invalid request JSON is serializable")
}

fn missing_comby_json(invocation: &CombyInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": COMBY_BINARY,
        "action": invocation.action.as_str(),
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "missing_comby",
            "message": "comby was not found on PATH, so the comby tool could not be run.",
            "install": [
                "Install comby from https://comby.dev or your system package manager.",
                "After comby is available on PATH, rerun this tool."
            ]
        }
    }))
    .expect("missing comby JSON is serializable")
}

fn timeout_json(invocation: &CombyInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": COMBY_BINARY,
        "action": invocation.action.as_str(),
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "timeout",
            "message": format!(
                "comby exceeded the configured timeout of {} ms",
                invocation.timeout_ms
            )
        }
    }))
    .expect("timeout JSON is serializable")
}

fn bounded_output_json(
    mut value: serde_json::Value,
    stdout: &str,
    stderr: String,
    max_output_bytes: usize,
) -> Result<String> {
    let mut rendered = serde_json::to_string_pretty(&value)?;
    if max_output_bytes == 0 || rendered.len() <= max_output_bytes {
        return Ok(rendered);
    }

    let mut low = 0;
    let mut high = stdout.chars().count();
    let mut best = 0;

    while low <= high {
        let mid = low + (high - low) / 2;
        value["stdout"] = json!(with_truncation_marker(&take_chars(stdout, mid)));
        value["stderr"] = nullable_string(&stderr);
        value["truncated"] = json!(true);
        let probe = serde_json::to_string_pretty(&value)?;
        if probe.len() <= max_output_bytes {
            best = mid;
            low = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }

    value["stdout"] = json!(with_truncation_marker(&take_chars(stdout, best)));
    rendered = serde_json::to_string_pretty(&value)?;
    if rendered.len() <= max_output_bytes || stderr.is_empty() {
        return Ok(rendered);
    }

    let mut stderr_trimmed = stderr;
    trim_to_char_budget(&mut stderr_trimmed, DEFAULT_STDERR_BYTES.min(1024));
    value["stderr"] = json!(stderr_trimmed);
    rendered = serde_json::to_string_pretty(&value)?;
    if rendered.len() <= max_output_bytes || best == 0 {
        return Ok(rendered);
    }

    value["stdout"] = json!(with_truncation_marker(""));
    value["truncated"] = json!(true);
    serde_json::to_string_pretty(&value).map_err(Into::into)
}

fn nullable_string(text: &str) -> serde_json::Value {
    if text.is_empty() {
        serde_json::Value::Null
    } else {
        json!(text)
    }
}

fn resolve_cwd(cwd: Option<String>, session_cwd: &Path) -> PathBuf {
    match non_empty(cwd) {
        Some(cwd) => {
            let path = PathBuf::from(cwd);
            if path.is_absolute() {
                path
            } else {
                session_cwd.join(path)
            }
        }
        None => session_cwd.to_path_buf(),
    }
}

async fn validate_cwd(cwd: &Path) -> Result<()> {
    let metadata = tokio::fs::metadata(cwd)
        .await
        .with_context(|| format!("read cwd {}", cwd.display()))?;
    if !metadata.is_dir() {
        return Err(anyhow!("cwd is not a directory: {}", cwd.display()));
    }
    Ok(())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn trim_to_char_budget(text: &mut String, max_bytes: usize) -> bool {
    if text.len() <= max_bytes {
        return false;
    }

    let mut end = 0;
    for (idx, ch) in text.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }

    text.truncate(end);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("[truncated]\n");
    true
}

fn take_chars(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

fn with_truncation_marker(text: &str) -> String {
    if text.is_empty() {
        "[truncated]\n".to_string()
    } else if text.ends_with('\n') {
        format!("{text}[truncated]\n")
    } else {
        format!("{text}\n[truncated]\n")
    }
}

fn shell_words(binary: &str, args: &[String]) -> String {
    std::iter::once(shell_word(binary))
        .chain(args.iter().map(|arg| shell_word(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_word(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '_' | '-' | '=' | ':'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invocation_builds_rewrite_argv() {
        let dir = tempfile::tempdir().unwrap();
        let invocation = CombyInvocation::from_params(
            CombyParams {
                action: CombyAction::Rewrite,
                match_template: "foo(:[x])".into(),
                rewrite_template: Some("bar(:[x])".into()),
                extensions: vec![".rs".into()],
                directory: Some("src".into()),
                matcher: Some("rust".into()),
                include_files: Some(".*\\.rs".into()),
                exclude_files: Some("target".into()),
                extra_args: vec!["-jobs".into(), "1".into()],
                cwd: None,
                timeout_ms: DEFAULT_TIMEOUT_MS,
                max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            },
            dir.path(),
        )
        .await
        .unwrap();

        assert_eq!(
            invocation.args,
            vec![
                "foo(:[x])",
                "bar(:[x])",
                ".rs",
                "-d",
                "src",
                "-matcher",
                "rust",
                "-include-files",
                ".*\\.rs",
                "-exclude-files",
                "target",
                "-in-place",
                "-jobs",
                "1"
            ]
        );
    }

    #[tokio::test]
    async fn check_does_not_require_rewrite_template() {
        let dir = tempfile::tempdir().unwrap();
        let invocation = CombyInvocation::from_params(
            CombyParams {
                action: CombyAction::Check,
                match_template: "foo(:[x])".into(),
                rewrite_template: None,
                extensions: vec![],
                directory: None,
                matcher: None,
                include_files: None,
                exclude_files: None,
                extra_args: vec![],
                cwd: None,
                timeout_ms: DEFAULT_TIMEOUT_MS,
                max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            },
            dir.path(),
        )
        .await
        .unwrap();

        assert_eq!(invocation.args, vec!["foo(:[x])", "", "-match-only"]);
    }

    #[tokio::test]
    async fn diff_requires_rewrite_template() {
        let dir = tempfile::tempdir().unwrap();
        let err = CombyInvocation::from_params(
            CombyParams {
                action: CombyAction::Diff,
                match_template: "foo(:[x])".into(),
                rewrite_template: None,
                extensions: vec![],
                directory: None,
                matcher: None,
                include_files: None,
                exclude_files: None,
                extra_args: vec![],
                cwd: None,
                timeout_ms: DEFAULT_TIMEOUT_MS,
                max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            },
            dir.path(),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("requires rewrite_template"));
    }
}
