//! Native mergiraf wrapper.
//!
//! This tool exposes the common syntax-aware merge operations without routing
//! path arguments through a shell.

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
use tokio::process::Command;

const DEFAULT_MAX_OUTPUT_BYTES: usize = 100_000;
const DEFAULT_STDERR_BYTES: usize = 32_000;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergirafAction {
    Merge,
    Solve,
    Languages,
}

impl MergirafAction {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Solve => "solve",
            Self::Languages => "languages",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MergirafParams {
    /// Action to run: `merge`, `solve`, or `languages`.
    pub action: MergirafAction,
    /// Base file for `merge`.
    #[serde(default)]
    pub base: Option<String>,
    /// Ours/current file for `merge`; mergiraf may update this file in place.
    #[serde(default)]
    pub ours: Option<String>,
    /// Theirs/other file for `merge`.
    #[serde(default)]
    pub theirs: Option<String>,
    /// File with existing conflict markers for `solve`.
    #[serde(default)]
    pub file: Option<String>,
    /// Optional language override passed as `--language <language>` for `merge`.
    #[serde(default)]
    pub language: Option<String>,
    /// Map to `--compact` for `merge`.
    #[serde(default)]
    pub compact: bool,
    /// Map to `--allow-parse-errors` for `merge`.
    #[serde(default)]
    pub allow_parse_errors: bool,
    /// Working directory for the command. Relative paths resolve against the
    /// session cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Maximum bytes in Ra's returned JSON envelope.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

pub struct MergirafTool;

#[async_trait]
impl Tool for MergirafTool {
    fn name(&self) -> &str {
        "mergiraf"
    }

    fn description(&self) -> &str {
        "Run native mergiraf actions with argv-safe arguments: merge three files, \
         solve conflict markers in one file, or list supported languages. Returns \
         a bounded JSON envelope and structured missing-mergiraf guidance."
    }

    fn schema(&self) -> serde_json::Value {
        mergiraf_schema()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("mergiraf");
        let params: MergirafParams =
            serde_json::from_value(input).context("invalid params for mergiraf")?;
        let submitted_action = params.action.as_str();
        let invocation = match MergirafInvocation::from_params(params, &ctx.cwd).await {
            Ok(invocation) => invocation,
            Err(error) => return Ok(invalid_request_json(submitted_action, error)),
        };

        execute_mergiraf(call_id, invocation, ctx).await
    }
}

fn mergiraf_schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schema_for!(MergirafParams)).unwrap();
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "oneOf".to_string(),
            json!([
                {
                    "properties": { "action": { "const": "merge" } },
                    "required": ["action", "base", "ours", "theirs"]
                },
                {
                    "properties": { "action": { "const": "solve" } },
                    "required": ["action", "file"]
                },
                {
                    "properties": { "action": { "const": "languages" } },
                    "required": ["action"]
                }
            ]),
        );
    }
    schema
}

#[derive(Debug, Clone)]
struct MergirafInvocation {
    action: &'static str,
    args: Vec<String>,
    cwd: PathBuf,
    max_output_bytes: usize,
}

impl MergirafInvocation {
    async fn from_params(params: MergirafParams, session_cwd: &Path) -> Result<Self> {
        let action = params.action.as_str();
        let max_output_bytes = params.max_output_bytes;
        let cwd = resolve_cwd(params.cwd.clone(), session_cwd);
        validate_cwd(&cwd).await?;

        let args = match params.action {
            MergirafAction::Merge => merge_args(&params)?,
            MergirafAction::Solve => solve_args(&params)?,
            MergirafAction::Languages => languages_args(),
        };

        Ok(Self {
            action,
            args,
            cwd,
            max_output_bytes,
        })
    }

    fn command_json(&self) -> serde_json::Value {
        json!({
            "program": "mergiraf",
            "args": self.args,
            "cwd": self.cwd,
        })
    }

    fn summary(&self) -> String {
        shell_words("mergiraf", &self.args)
    }
}

fn merge_args(params: &MergirafParams) -> Result<Vec<String>> {
    let base = required_path(&params.base, "base", "merge")?;
    let ours = required_path(&params.ours, "ours", "merge")?;
    let theirs = required_path(&params.theirs, "theirs", "merge")?;

    let mut args = vec!["merge".to_string(), base, ours, theirs];
    if let Some(language) = non_empty(params.language.clone()) {
        args.push("--language".to_string());
        args.push(language);
    }
    if params.compact {
        args.push("--compact".to_string());
    }
    if params.allow_parse_errors {
        args.push("--allow-parse-errors".to_string());
    }
    Ok(args)
}

fn solve_args(params: &MergirafParams) -> Result<Vec<String>> {
    Ok(vec![
        "solve".to_string(),
        required_path(&params.file, "file", "solve")?,
    ])
}

fn languages_args() -> Vec<String> {
    vec!["languages".to_string(), "--gitattributes".to_string()]
}

fn required_path(value: &Option<String>, field: &str, action: &str) -> Result<String> {
    non_empty(value.clone()).ok_or_else(|| anyhow!("mergiraf {action} requires `{field}`"))
}

async fn execute_mergiraf(
    call_id: &str,
    invocation: MergirafInvocation,
    ctx: &ToolCtx,
) -> Result<String> {
    let binary = match which::which("mergiraf") {
        Ok(path) => path,
        Err(_) => return Ok(missing_mergiraf_json(&invocation)),
    };

    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[mergiraf] {}", invocation.summary()),
    });

    let output = run_mergiraf(&binary, &invocation).await?;
    let exit_code = output.exit_code;
    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[exit={exit_code}]"),
    });

    format_output(&invocation, output)
}

#[derive(Debug, Clone)]
struct MergirafOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

async fn run_mergiraf(binary: &Path, invocation: &MergirafInvocation) -> Result<MergirafOutput> {
    let output = Command::new(binary)
        .args(&invocation.args)
        .current_dir(&invocation.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| {
            format!(
                "spawn {} in {}",
                invocation.summary(),
                invocation.cwd.display()
            )
        })?;

    Ok(MergirafOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn format_output(invocation: &MergirafInvocation, output: MergirafOutput) -> Result<String> {
    let stdout = output.stdout;
    let mut stderr = output.stderr;
    let ok = output.exit_code == 0;
    let stderr_truncated = trim_to_char_budget(&mut stderr, DEFAULT_STDERR_BYTES);

    let mut base = json!({
        "ok": ok,
        "tool": "mergiraf",
        "action": invocation.action,
        "command": invocation.command_json(),
        "exit_code": output.exit_code,
        "stdout": stdout.clone(),
        "stderr": nullable_string(&stderr),
        "truncated": stderr_truncated,
    });

    if !ok {
        base["error"] = json!({
            "kind": "mergiraf_error",
            "message": "mergiraf exited with a non-zero status"
        });
    }

    bounded_output_json(base, &stdout, stderr, invocation.max_output_bytes)
}

fn invalid_request_json(action: &str, error: anyhow::Error) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": "mergiraf",
        "action": action,
        "command": {
            "program": "mergiraf",
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

fn missing_mergiraf_json(invocation: &MergirafInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": "mergiraf",
        "action": invocation.action,
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "missing_mergiraf",
            "message": "mergiraf was not found on PATH, so the mergiraf tool could not be run.",
            "install_hint": "Install mergiraf, for example with `cargo install mergiraf`, and ensure it is available on PATH."
        }
    }))
    .expect("missing mergiraf JSON is serializable")
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

    fn merge_params() -> MergirafParams {
        MergirafParams {
            action: MergirafAction::Merge,
            base: Some("base.rs".into()),
            ours: Some("ours.rs".into()),
            theirs: Some("theirs.rs".into()),
            file: None,
            language: None,
            compact: false,
            allow_parse_errors: false,
            cwd: None,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    #[tokio::test]
    async fn merge_invocation_maps_optional_flags_to_argv() {
        let dir = tempfile::tempdir().unwrap();
        let mut params = merge_params();
        params.language = Some("rust".into());
        params.compact = true;
        params.allow_parse_errors = true;

        let invocation = MergirafInvocation::from_params(params, dir.path())
            .await
            .unwrap();

        assert_eq!(
            invocation.args,
            vec![
                "merge",
                "base.rs",
                "ours.rs",
                "theirs.rs",
                "--language",
                "rust",
                "--compact",
                "--allow-parse-errors"
            ]
        );
    }

    #[tokio::test]
    async fn merge_invocation_requires_three_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut params = merge_params();
        params.theirs = None;

        assert!(MergirafInvocation::from_params(params, dir.path())
            .await
            .unwrap_err()
            .to_string()
            .contains("requires `theirs`"));
    }

    #[test]
    fn output_budget_preserves_valid_json_and_marks_truncated() {
        let invocation = MergirafInvocation {
            action: "languages",
            args: vec!["languages".into(), "--gitattributes".into()],
            cwd: PathBuf::from("."),
            max_output_bytes: 450,
        };
        let rendered = format_output(
            &invocation,
            MergirafOutput {
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
    fn schema_declares_action_specific_required_fields() {
        let schema = mergiraf_schema();
        let one_of = schema["oneOf"].as_array().unwrap();

        assert!(one_of.iter().any(|entry| {
            entry["properties"]["action"]["const"] == "merge"
                && entry["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|field| field == "theirs")
        }));
        assert!(one_of.iter().any(|entry| {
            entry["properties"]["action"]["const"] == "solve"
                && entry["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|field| field == "file")
        }));
        assert!(one_of
            .iter()
            .any(|entry| entry["properties"]["action"]["const"] == "languages"));
    }
}
