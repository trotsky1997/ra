//! Native wrappers for task runners and local workflow validators.
//!
//! These tools keep common test-first project loops out of shell strings while
//! preserving the underlying CLI semantics of mise, just, and wrkflw.

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

const DEFAULT_MAX_OUTPUT_BYTES: usize = 120_000;
const DEFAULT_STDERR_BYTES: usize = 32_000;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskWorkflowParams {
    /// Arguments passed to the command, excluding the binary name.
    ///
    /// Examples: `["run", "test"]` for mise, `["test"]` for just, or
    /// workflow validation arguments for wrkflw.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory for the command. Relative paths resolve against the
    /// session cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Optional process timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Maximum bytes in Ra's returned JSON envelope.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

pub struct MiseTool;

pub struct JustTool;

pub struct WrkflwTool;

#[async_trait]
impl Tool for MiseTool {
    fn name(&self) -> &str {
        "mise"
    }

    fn description(&self) -> &str {
        "Run the native mise CLI with argv-safe arguments for project task and \
         test loops, for example `{ \"args\": [\"run\", \"test\"] }`. \
         Returns a bounded JSON envelope and missing-mise guidance."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(TaskWorkflowParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        execute_task_workflow_tool("mise", call_id, input, ctx).await
    }
}

#[async_trait]
impl Tool for JustTool {
    fn name(&self) -> &str {
        "just"
    }

    fn description(&self) -> &str {
        "Run the native just command runner with argv-safe arguments for \
         project recipes and test-first loops, for example \
         `{ \"args\": [\"test\"] }`. Returns a bounded JSON envelope and \
         missing-just guidance."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(TaskWorkflowParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        execute_task_workflow_tool("just", call_id, input, ctx).await
    }
}

#[async_trait]
impl Tool for WrkflwTool {
    fn name(&self) -> &str {
        "wrkflw"
    }

    fn description(&self) -> &str {
        "Run the native wrkflw CLI with argv-safe arguments to validate and run \
         GitHub Actions workflows locally before review. Returns a bounded JSON \
         envelope and missing-wrkflw guidance."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(TaskWorkflowParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        execute_task_workflow_tool("wrkflw", call_id, input, ctx).await
    }
}

async fn execute_task_workflow_tool(
    binary: &'static str,
    call_id: &str,
    input: serde_json::Value,
    ctx: &ToolCtx,
) -> Result<String> {
    let _scope = crate::nemo_obs::tool_scope(binary);
    let params: TaskWorkflowParams =
        serde_json::from_value(input).with_context(|| format!("invalid params for {binary}"))?;
    let invocation = match TaskWorkflowInvocation::from_params(binary, params, &ctx.cwd).await {
        Ok(invocation) => invocation,
        Err(error) => return Ok(invalid_request_json(binary, error)),
    };

    execute_invocation(call_id, invocation, ctx).await
}

#[derive(Debug, Clone)]
struct TaskWorkflowInvocation {
    binary: &'static str,
    args: Vec<String>,
    cwd: PathBuf,
    timeout_ms: Option<u64>,
    max_output_bytes: usize,
}

impl TaskWorkflowInvocation {
    async fn from_params(
        binary: &'static str,
        params: TaskWorkflowParams,
        session_cwd: &Path,
    ) -> Result<Self> {
        let cwd = resolve_cwd(params.cwd, session_cwd);
        validate_cwd(&cwd).await?;

        Ok(Self {
            binary,
            args: params.args,
            cwd,
            timeout_ms: params.timeout_ms,
            max_output_bytes: params.max_output_bytes,
        })
    }

    fn command_json(&self) -> serde_json::Value {
        json!({
            "program": self.binary,
            "args": self.args,
            "cwd": self.cwd,
        })
    }

    fn summary(&self) -> String {
        shell_words(self.binary, &self.args)
    }

    fn missing_kind(&self) -> String {
        format!("missing_{}", self.binary)
    }
}

async fn execute_invocation(
    call_id: &str,
    invocation: TaskWorkflowInvocation,
    ctx: &ToolCtx,
) -> Result<String> {
    let binary_path = match which::which(invocation.binary) {
        Ok(path) => path,
        Err(_) => return Ok(missing_binary_json(&invocation)),
    };

    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[{}] {}", invocation.binary, invocation.summary()),
    });

    let output = match run_binary(&binary_path, &invocation).await {
        Ok(output) => output,
        Err(TaskWorkflowRunError::Timeout) => return Ok(timeout_json(&invocation)),
        Err(TaskWorkflowRunError::Other(error)) => return Err(error),
    };

    let exit_code = output.exit_code;
    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[exit={exit_code}]"),
    });

    format_output(&invocation, output)
}

#[derive(Debug)]
enum TaskWorkflowRunError {
    Timeout,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for TaskWorkflowRunError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

#[derive(Debug, Clone)]
struct TaskWorkflowOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

async fn run_binary(
    binary_path: &Path,
    invocation: &TaskWorkflowInvocation,
) -> std::result::Result<TaskWorkflowOutput, TaskWorkflowRunError> {
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
    let output = if let Some(timeout_ms) = invocation.timeout_ms {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), output_future).await {
            Ok(output) => output,
            Err(_) => return Err(TaskWorkflowRunError::Timeout),
        }
    } else {
        output_future.await
    }
    .with_context(|| format!("wait {}", invocation.binary))?;

    Ok(TaskWorkflowOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn format_output(
    invocation: &TaskWorkflowInvocation,
    output: TaskWorkflowOutput,
) -> Result<String> {
    let stdout = output.stdout;
    let mut stderr = output.stderr;
    let ok = output.exit_code == 0;
    let stderr_truncated = trim_to_char_budget(&mut stderr, DEFAULT_STDERR_BYTES);

    let mut base = json!({
        "ok": ok,
        "tool": invocation.binary,
        "command": invocation.command_json(),
        "exit_code": output.exit_code,
        "stdout": stdout.clone(),
        "stderr": nullable_string(&stderr),
        "truncated": stderr_truncated,
    });

    if !ok {
        base["error"] = json!({
            "kind": "command_failed",
            "message": format!("{} exited with a non-zero status", invocation.binary)
        });
    }

    bounded_output_json(base, &stdout, stderr, invocation.max_output_bytes)
}

fn invalid_request_json(binary: &'static str, error: anyhow::Error) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": binary,
        "command": {
            "program": binary,
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

fn missing_binary_json(invocation: &TaskWorkflowInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": invocation.binary,
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": invocation.missing_kind(),
            "message": format!(
                "{} was not found on PATH, so the {} tool could not be run.",
                invocation.binary,
                invocation.binary
            ),
            "install": install_guidance(invocation.binary)
        }
    }))
    .expect("missing binary JSON is serializable")
}

fn timeout_json(invocation: &TaskWorkflowInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": invocation.binary,
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "timeout",
            "message": format!(
                "{} exceeded the configured timeout of {} ms",
                invocation.binary,
                invocation.timeout_ms.unwrap_or_default()
            )
        }
    }))
    .expect("timeout JSON is serializable")
}

fn install_guidance(binary: &'static str) -> Vec<String> {
    match binary {
        "mise" => vec![
            "Install mise from https://mise.jdx.dev or your system package manager.".to_string(),
            "After mise is available on PATH, rerun this tool.".to_string(),
        ],
        "just" => vec![
            "Install just from https://just.systems or your system package manager.".to_string(),
            "After just is available on PATH, rerun this tool.".to_string(),
        ],
        "wrkflw" => vec![
            "Install wrkflw from https://github.com/bahdotsh/wrkflw or your system package manager.".to_string(),
            "After wrkflw is available on PATH, rerun this tool.".to_string(),
        ],
        _ => vec![format!("Install {binary} and ensure it is available on PATH.")],
    }
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

fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invocation_resolves_relative_cwd_against_session_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        tokio::fs::create_dir(&child).await.unwrap();

        let invocation = TaskWorkflowInvocation::from_params(
            "just",
            TaskWorkflowParams {
                args: vec!["test".into()],
                cwd: Some("child".into()),
                timeout_ms: Some(100),
                max_output_bytes: 1024,
            },
            dir.path(),
        )
        .await
        .unwrap();

        assert_eq!(invocation.cwd, child);
        assert_eq!(invocation.args, vec!["test"]);
        assert_eq!(invocation.timeout_ms, Some(100));
    }

    #[test]
    fn output_budget_preserves_valid_json_and_marks_truncated() {
        let invocation = TaskWorkflowInvocation {
            binary: "mise",
            args: vec!["run".into(), "test".into()],
            cwd: PathBuf::from("."),
            timeout_ms: None,
            max_output_bytes: 420,
        };
        let rendered = format_output(
            &invocation,
            TaskWorkflowOutput {
                exit_code: 0,
                stdout: "x".repeat(2_000),
                stderr: String::new(),
            },
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(value["truncated"], true);
        assert!(value["stdout"].as_str().unwrap().contains("[truncated]"));
    }
}
