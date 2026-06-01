//! Native jq wrapper.
//!
//! This tool keeps common JSON filtering out of shell pipelines while still
//! delegating jq semantics to the system `jq` binary.

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
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const DEFAULT_MAX_OUTPUT_BYTES: usize = 100_000;
const DEFAULT_STDERR_BYTES: usize = 32_000;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct JqParams {
    /// jq filter to execute, for example `.items[] | .name`.
    pub filter: String,
    /// Inline JSON text to pass to jq on stdin. Mutually exclusive with `path`.
    #[serde(default)]
    pub input: Option<String>,
    /// JSON file path to read and pass to jq on stdin. Mutually exclusive with
    /// `input`; relative paths resolve against `cwd` or the session cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Working directory for relative `path` resolution and jq execution.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Map to jq `-r`.
    #[serde(default)]
    pub raw_output: bool,
    /// Map to jq `-c`.
    #[serde(default)]
    pub compact_output: bool,
    /// Map to jq `-S`.
    #[serde(default)]
    pub sort_keys: bool,
    /// Optional jq process timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Maximum bytes in Ra's returned JSON envelope.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

pub struct JqTool;

#[async_trait]
impl Tool for JqTool {
    fn name(&self) -> &str {
        "jq"
    }

    fn description(&self) -> &str {
        "Run a jq filter against inline JSON text or one JSON file using argv-safe \
         process spawning and stdin. Returns a bounded JSON envelope with stdout, \
         stderr, exit status, truncation state, and structured missing-jq guidance."
    }

    fn schema(&self) -> serde_json::Value {
        jq_schema()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("jq");
        let params: JqParams = serde_json::from_value(input).context("invalid params for jq")?;
        let submitted_filter = params.filter.trim().to_string();
        let invocation = match JqInvocation::from_params(params, &ctx.cwd).await {
            Ok(invocation) => invocation,
            Err(error) => return Ok(invalid_request_json(Some(&submitted_filter), error)),
        };

        execute_jq(call_id, invocation, ctx).await
    }
}

fn jq_schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schema_for!(JqParams)).unwrap();
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "oneOf".to_string(),
            json!([
                {
                    "required": ["input"],
                    "not": { "required": ["path"] }
                },
                {
                    "required": ["path"],
                    "not": { "required": ["input"] }
                }
            ]),
        );
    }
    schema
}

#[derive(Debug, Clone)]
struct JqInvocation {
    filter: String,
    args: Vec<String>,
    stdin: Vec<u8>,
    cwd: PathBuf,
    timeout_ms: Option<u64>,
    max_output_bytes: usize,
}

impl JqInvocation {
    async fn from_params(params: JqParams, session_cwd: &Path) -> Result<Self> {
        let filter = params.filter.trim().to_string();
        if filter.is_empty() {
            return Err(anyhow!("jq requires a non-empty filter"));
        }

        let input = non_empty(params.input);
        let path = non_empty(params.path);
        match (input.is_some(), path.is_some()) {
            (true, true) => {
                return Err(anyhow!(
                    "jq requires exactly one input source: provide either input or path, not both"
                ));
            }
            (false, false) => {
                return Err(anyhow!(
                    "jq requires exactly one input source: provide input or path"
                ));
            }
            _ => {}
        }

        let cwd = resolve_cwd(params.cwd, session_cwd);
        validate_cwd(&cwd).await?;

        let stdin = if let Some(input) = input {
            input.into_bytes()
        } else {
            let path = path.expect("path is present after source validation");
            let resolved = resolve_path(&path, &cwd);
            tokio::fs::read(&resolved)
                .await
                .with_context(|| format!("read jq input {}", resolved.display()))?
        };

        let mut args = Vec::new();
        if params.raw_output {
            args.push("-r".to_string());
        }
        if params.compact_output {
            args.push("-c".to_string());
        }
        if params.sort_keys {
            args.push("-S".to_string());
        }
        args.push(filter.clone());

        Ok(Self {
            filter,
            args,
            stdin,
            cwd,
            timeout_ms: params.timeout_ms,
            max_output_bytes: params.max_output_bytes,
        })
    }

    fn command_json(&self) -> serde_json::Value {
        json!({
            "program": "jq",
            "args": self.args,
        })
    }
}

async fn execute_jq(call_id: &str, invocation: JqInvocation, ctx: &ToolCtx) -> Result<String> {
    let jq = match find_jq_binary() {
        Some(path) => path,
        None => return Ok(missing_jq_json(&invocation)),
    };

    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[jq] jq {}", shell_words(&invocation.args)),
    });

    let output = match run_jq(&jq, &invocation).await {
        Ok(output) => output,
        Err(JqRunError::Timeout) => {
            return Ok(timeout_json(&invocation));
        }
        Err(JqRunError::Other(error)) => return Err(error),
    };

    let exit_code = output.exit_code;
    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[exit={exit_code}]"),
    });

    format_jq_output(&invocation, output)
}

fn find_jq_binary() -> Option<PathBuf> {
    which::which("jq").ok()
}

#[derive(Debug)]
enum JqRunError {
    Timeout,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for JqRunError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

async fn run_jq(jq: &Path, invocation: &JqInvocation) -> std::result::Result<JqOutput, JqRunError> {
    let mut child = Command::new(jq)
        .args(&invocation.args)
        .current_dir(&invocation.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn jq in {}", invocation.cwd.display()))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("jq stdin was not piped"))?;
    let input = invocation.stdin.clone();
    let stdin_task = tokio::spawn(async move {
        match stdin.write_all(&input).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error),
        }
        match stdin.shutdown().await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            Err(error) => Err(error),
        }
    });

    let output_future = child.wait_with_output();
    let output = if let Some(timeout_ms) = invocation.timeout_ms {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), output_future).await {
            Ok(output) => output,
            Err(_) => return Err(JqRunError::Timeout),
        }
    } else {
        output_future.await
    }
    .context("wait jq")?;

    stdin_task
        .await
        .context("join jq stdin writer")?
        .context("write jq stdin")?;

    Ok(JqOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[derive(Debug, Clone)]
struct JqOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn format_jq_output(invocation: &JqInvocation, output: JqOutput) -> Result<String> {
    let stdout = output.stdout;
    let mut stderr = output.stderr;
    let ok = output.exit_code == 0;
    let stderr_truncated = trim_to_char_budget(&mut stderr, DEFAULT_STDERR_BYTES);

    let mut base = json!({
        "ok": ok,
        "tool": "jq",
        "filter": invocation.filter,
        "command": invocation.command_json(),
        "exit_code": output.exit_code,
        "stdout": stdout.clone(),
        "stderr": nullable_string(&stderr),
        "truncated": stderr_truncated,
    });

    if !ok {
        base["error"] = json!({
            "kind": "jq_error",
            "message": "jq exited with a non-zero status"
        });
    }

    bounded_output_json(
        base,
        &stdout,
        stderr,
        invocation.max_output_bytes,
        stderr_truncated,
    )
}

fn invalid_request_json(filter: Option<&str>, error: anyhow::Error) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": "jq",
        "filter": filter.filter(|filter| !filter.is_empty()),
        "command": {
            "program": "jq",
            "args": []
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

fn missing_jq_json(invocation: &JqInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": "jq",
        "filter": invocation.filter,
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "missing_jq",
            "message": "jq was not found on PATH, so the jq tool could not be run.",
            "install": [
                "Install jq using your system package manager.",
                "After jq is available on PATH, rerun this tool."
            ]
        }
    }))
    .expect("missing jq JSON is serializable")
}

fn timeout_json(invocation: &JqInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": "jq",
        "filter": invocation.filter,
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "timeout",
            "message": format!(
                "jq exceeded the configured timeout of {} ms",
                invocation.timeout_ms.unwrap_or_default()
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
    _already_truncated: bool,
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

fn resolve_path(path: &str, cwd: &Path) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
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

fn shell_words(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_word(arg))
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

    fn base_params() -> JqParams {
        JqParams {
            filter: ".name".into(),
            input: Some("{\"name\":\"Ada\"}".into()),
            path: None,
            cwd: None,
            raw_output: false,
            compact_output: false,
            sort_keys: false,
            timeout_ms: None,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    #[tokio::test]
    async fn invocation_maps_output_flags_to_argv() {
        let dir = tempfile::tempdir().unwrap();
        let mut params = base_params();
        params.raw_output = true;
        params.compact_output = true;
        params.sort_keys = true;

        let invocation = JqInvocation::from_params(params, dir.path()).await.unwrap();

        assert_eq!(invocation.args, vec!["-r", "-c", "-S", ".name"]);
        assert_eq!(invocation.stdin, br#"{"name":"Ada"}"#);
    }

    #[tokio::test]
    async fn invocation_requires_exactly_one_input_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut none = base_params();
        none.input = None;
        assert!(JqInvocation::from_params(none, dir.path())
            .await
            .unwrap_err()
            .to_string()
            .contains("exactly one input source"));

        let mut both = base_params();
        both.path = Some("data.json".into());
        assert!(JqInvocation::from_params(both, dir.path())
            .await
            .unwrap_err()
            .to_string()
            .contains("not both"));
    }

    #[test]
    fn output_budget_preserves_valid_json_and_marks_truncated() {
        let invocation = JqInvocation {
            filter: ".".into(),
            args: vec![".".into()],
            stdin: b"{}".to_vec(),
            cwd: PathBuf::from("."),
            timeout_ms: None,
            max_output_bytes: 420,
        };
        let rendered = format_jq_output(
            &invocation,
            JqOutput {
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

    #[test]
    fn schema_requires_one_input_source() {
        let schema = jq_schema();
        let one_of = schema["oneOf"].as_array().unwrap();

        assert_eq!(one_of.len(), 2);
        assert_eq!(one_of[0]["required"], json!(["input"]));
        assert_eq!(one_of[1]["required"], json!(["path"]));
    }
}
