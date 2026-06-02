//! Native sd wrapper.
//!
//! This tool keeps fast regex/literal replacements out of shell strings while
//! delegating replacement semantics to the system `sd` binary.

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

const SD_BINARY: &str = "sd";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 32_768;
const DEFAULT_STDERR_BYTES: usize = 32_000;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SdParams {
    /// Find pattern, interpreted as a regex unless `string_mode` is true.
    pub find: String,
    /// Replacement string. Capture references such as `$1` are interpreted by sd.
    pub replace: String,
    /// Explicit file paths to rewrite. Must not be empty; stdin mode is not supported.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Treat `find` as a literal string, not a regex.
    #[serde(default)]
    pub string_mode: bool,
    /// Additional sd flags passed before the find/replace positionals.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Working directory for resolving relative paths.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Process timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Maximum bytes in Ra's returned JSON envelope. Set to 0 for unbounded.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

pub struct SdTool;

#[async_trait]
impl Tool for SdTool {
    fn name(&self) -> &str {
        SD_BINARY
    }

    fn description(&self) -> &str {
        "Run the native sd CLI for regex or literal find/replace across \
         explicit file paths with argv-safe process spawning. Stdin mode is \
         intentionally unsupported: provide at least one path. Returns a \
         bounded JSON envelope with stdout, stderr, exit status, truncation \
         state, validation errors, timeouts, and missing-sd guidance."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(SdParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope(SD_BINARY);
        let params: SdParams = serde_json::from_value(input).context("invalid params for sd")?;
        let invocation = match SdInvocation::from_params(params, &ctx.cwd).await {
            Ok(invocation) => invocation,
            Err(error) => return Ok(invalid_request_json(error)),
        };

        execute_sd(call_id, invocation, ctx).await
    }
}

#[derive(Debug, Clone)]
struct SdInvocation {
    args: Vec<String>,
    cwd: PathBuf,
    timeout_ms: u64,
    max_output_bytes: usize,
}

impl SdInvocation {
    async fn from_params(params: SdParams, session_cwd: &Path) -> Result<Self> {
        if params.find.is_empty() {
            return Err(anyhow!("sd requires a non-empty find pattern"));
        }
        if params.paths.is_empty() {
            return Err(anyhow!(
                "sd requires at least one path; stdin mode is not supported"
            ));
        }

        let cwd = resolve_cwd(params.cwd, session_cwd);
        validate_cwd(&cwd).await?;

        let mut args = Vec::new();
        if params.string_mode {
            args.push("--fixed-strings".to_string());
        }
        args.extend(params.extra_args);
        args.push("--".to_string());
        args.push(params.find);
        args.push(params.replace);
        args.extend(params.paths);

        Ok(Self {
            args,
            cwd,
            timeout_ms: params.timeout_ms,
            max_output_bytes: params.max_output_bytes,
        })
    }

    fn command_json(&self) -> serde_json::Value {
        json!({
            "program": SD_BINARY,
            "args": self.args,
            "cwd": self.cwd,
        })
    }

    fn summary(&self) -> String {
        shell_words(SD_BINARY, &self.args)
    }
}

async fn execute_sd(call_id: &str, invocation: SdInvocation, ctx: &ToolCtx) -> Result<String> {
    let sd = match which::which(SD_BINARY) {
        Ok(path) => path,
        Err(_) => return Ok(missing_sd_json(&invocation)),
    };

    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[sd] {}", invocation.summary()),
    });

    let output = match run_sd(&sd, &invocation).await {
        Ok(output) => output,
        Err(SdRunError::Timeout) => return Ok(timeout_json(&invocation)),
        Err(SdRunError::Other(error)) => return Err(error),
    };

    let exit_code = output.exit_code;
    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[exit={exit_code}]"),
    });

    format_sd_output(&invocation, output)
}

#[derive(Debug)]
enum SdRunError {
    Timeout,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for SdRunError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

#[derive(Debug, Clone)]
struct SdOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

async fn run_sd(
    binary_path: &Path,
    invocation: &SdInvocation,
) -> std::result::Result<SdOutput, SdRunError> {
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
            Err(_) => return Err(SdRunError::Timeout),
        }
        .context("wait sd")?;

    Ok(SdOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn format_sd_output(invocation: &SdInvocation, output: SdOutput) -> Result<String> {
    let stdout = output.stdout;
    let mut stderr = output.stderr;
    let ok = output.exit_code == 0;
    let stderr_truncated = trim_to_char_budget(&mut stderr, DEFAULT_STDERR_BYTES);

    let mut base = json!({
        "ok": ok,
        "tool": SD_BINARY,
        "command": invocation.command_json(),
        "exit_code": output.exit_code,
        "stdout": stdout.clone(),
        "stderr": nullable_string(&stderr),
        "truncated": stderr_truncated,
    });

    if !ok {
        base["error"] = json!({
            "kind": "command_failed",
            "message": "sd exited with a non-zero status"
        });
    }

    bounded_output_json(base, &stdout, stderr, invocation.max_output_bytes)
}

fn invalid_request_json(error: anyhow::Error) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": SD_BINARY,
        "command": {
            "program": SD_BINARY,
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

fn missing_sd_json(invocation: &SdInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": SD_BINARY,
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "missing_sd",
            "message": "sd was not found on PATH, so the sd tool could not be run.",
            "install": [
                "Install sd using your system package manager or `cargo install sd`.",
                "After sd is available on PATH, rerun this tool."
            ]
        }
    }))
    .expect("missing sd JSON is serializable")
}

fn timeout_json(invocation: &SdInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": SD_BINARY,
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "timeout",
            "message": format!("sd exceeded the configured timeout of {} ms", invocation.timeout_ms)
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
    async fn invocation_builds_sd_argv() {
        let dir = tempfile::tempdir().unwrap();
        let invocation = SdInvocation::from_params(
            SdParams {
                find: "foo".into(),
                replace: "bar".into(),
                paths: vec!["src/main.rs".into()],
                string_mode: true,
                extra_args: vec!["--flags".into(), "i".into()],
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
                "--fixed-strings",
                "--flags",
                "i",
                "--",
                "foo",
                "bar",
                "src/main.rs"
            ]
        );
    }

    #[tokio::test]
    async fn invocation_rejects_empty_paths() {
        let dir = tempfile::tempdir().unwrap();
        let err = SdInvocation::from_params(
            SdParams {
                find: "foo".into(),
                replace: "bar".into(),
                paths: vec![],
                string_mode: false,
                extra_args: vec![],
                cwd: None,
                timeout_ms: DEFAULT_TIMEOUT_MS,
                max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            },
            dir.path(),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("at least one path"));
    }
}
