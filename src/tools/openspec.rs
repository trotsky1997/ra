//! Native OpenSpec lifecycle tool.
//!
//! Ra already folds an *agent-own* spec-driven-development playbook into the
//! system prompt (see [`crate::openspec`]). That tells the agent how to drive
//! the upstream [OpenSpec](https://github.com/Fission-AI/OpenSpec) CLI, but
//! leaves the actual loop as free-form `bash` work: the agent has to remember
//! exact command syntax, hand-build `--json` invocations, dodge interactive
//! `init`/`archive` prompts, and parse failures out of opaque shell output.
//!
//! This tool turns that loop into a narrow, structured control surface. Each
//! [`OpenSpecAction`] maps to one OpenSpec concept (`status`, `instructions`,
//! `validate`, `archive`, …); the tool spawns the upstream `openspec` binary
//! with `Command::arg` (argv-safe, no shell), forces non-interactive flags on
//! every bootstrap/archive path so an unattended run can never hang, and
//! returns a bounded JSON envelope with exit status and stderr surfaced.
//!
//! Ra stays the *consumer* of the convention: this is an execution surface
//! over the upstream CLI, not a reimplementation of OpenSpec's schemas or
//! lifecycle semantics. The system-prompt catalog/playbook layer in
//! [`crate::openspec`] is unchanged; this tool complements it.

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
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const OPENSPEC_BINARY: &str = "openspec";
const DEFAULT_MAX_OUTPUT_BYTES: usize = 120_000;
const DEFAULT_STDERR_BYTES: usize = 32_000;

/// One OpenSpec lifecycle operation. Each variant maps to a single upstream
/// `openspec` subcommand. Read-only variants (`status`, `list`, `show`,
/// `instructions`, `validate`) never mutate the project; mutation variants
/// (`init`, `update`, `new_change`, `archive`) do.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OpenSpecAction {
    /// `openspec status --change <change> --json` — the apply-readiness state
    /// machine: each artifact's `status`, its `missingDeps`, and the
    /// `applyRequires` set that must be `done` before implementing.
    Status,
    /// `openspec list --json` — active changes (or specs with `specs: true`).
    List,
    /// `openspec show <item> --json` — one change or spec as JSON.
    Show,
    /// `openspec instructions <artifact|apply> --change <change> --json` — the
    /// per-step template, dependencies, and resolved output path.
    Instructions,
    /// `openspec validate <item> --strict --json` — machine-actionable
    /// validation errors. Strict mode is forced on.
    Validate,
    /// `openspec init --tools <tools> [path]` — scaffold `openspec/`
    /// non-interactively. `tools` defaults to `none`.
    Init,
    /// `openspec update [path]` — refresh instruction files in an existing
    /// project.
    Update,
    /// `openspec new change <name>` — create a new change directory.
    NewChange,
    /// `openspec archive <change> -y` — promote delta specs into `specs/` and
    /// move the change under `changes/archive/`. Requires an explicit
    /// `confirm_archive: true` because it is destructive to the change dir.
    Archive,
    /// `openspec workflow_state` is not a CLI command; it is a convenience
    /// summary derived from `status --json` (apply-ready / blocked / done).
    WorkflowState,
}

impl OpenSpecAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::List => "list",
            Self::Show => "show",
            Self::Instructions => "instructions",
            Self::Validate => "validate",
            Self::Init => "init",
            Self::Update => "update",
            Self::NewChange => "new_change",
            Self::Archive => "archive",
            Self::WorkflowState => "workflow_state",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenSpecParams {
    /// Which OpenSpec lifecycle operation to run.
    pub action: OpenSpecAction,
    /// Change name (kebab-case). Required by `status`, `instructions`,
    /// `new_change`, `archive`, and `workflow_state`; optional elsewhere.
    #[serde(default)]
    pub change: Option<String>,
    /// Item name for `show`/`validate` (a change name or spec id). When
    /// omitted, `validate` falls back to `change`.
    #[serde(default)]
    pub item: Option<String>,
    /// Artifact id for `instructions` (e.g. `proposal`, `design`, `specs`,
    /// `tasks`, or `apply`). Defaults to `apply`.
    #[serde(default)]
    pub artifact: Option<String>,
    /// `list`/`validate`: target specs instead of changes.
    #[serde(default)]
    pub specs: bool,
    /// `init`: comma-separated tool surfaces, `all`, or `none`. Defaults to
    /// `none` (scaffold only, no slash/skill generation). Bare `openspec
    /// init` prompts interactively, so a value is always passed.
    #[serde(default)]
    pub tools: Option<String>,
    /// `init`/`update`: target directory path. Defaults to the working dir.
    #[serde(default)]
    pub path: Option<String>,
    /// `new_change`: optional description recorded in the change README.
    #[serde(default)]
    pub description: Option<String>,
    /// `archive`: required confirmation. Archive is destructive to the change
    /// directory, so it only runs when this is explicitly `true`; the `-y`
    /// flag is then passed internally to skip the interactive prompt.
    #[serde(default)]
    pub confirm_archive: bool,
    /// `archive`: skip spec promotion (maps to `--skip-specs`), for tooling or
    /// doc-only changes.
    #[serde(default)]
    pub skip_specs: bool,
    /// Working directory for the spawned process and relative `path`
    /// resolution. Defaults to the session cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Optional process timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Maximum bytes in Ra's returned JSON envelope.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

pub struct OpenSpecTool;

#[async_trait]
impl Tool for OpenSpecTool {
    fn name(&self) -> &str {
        "openspec"
    }

    fn description(&self) -> &str {
        "Drive the agent-own OpenSpec spec-driven-development loop through the \
         upstream `openspec` CLI as structured actions: inspect state \
         (`status`, `list`, `show`, `instructions`, `workflow_state`), \
         `validate` strictly, and mutate non-interactively (`init`, `update`, \
         `new_change`, `archive`). All `openspec` calls request `--json` where \
         supported, never run interactively, and require explicit confirmation \
         for the destructive `archive`. Returns a bounded JSON envelope with \
         stdout, stderr, exit status, and structured missing-binary guidance. \
         Ra consumes the OpenSpec convention; it does not reimplement the CLI."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(OpenSpecParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("openspec");
        let params: OpenSpecParams =
            serde_json::from_value(input).context("invalid params for openspec")?;
        let action = params.action;
        let invocation = match OpenSpecInvocation::from_params(params, &ctx.cwd) {
            Ok(invocation) => invocation,
            Err(error) => return Ok(invalid_request_json(action, error)),
        };
        execute_openspec(call_id, invocation, ctx).await
    }
}

/// A fully-resolved `openspec` invocation: the argv (minus the binary), the
/// working directory, and output controls. `derive_workflow_state` marks the
/// `workflow_state` convenience action, which runs `status --json` and then
/// summarizes apply-readiness on top of the raw envelope.
#[derive(Debug, Clone)]
struct OpenSpecInvocation {
    action: OpenSpecAction,
    args: Vec<String>,
    cwd: PathBuf,
    timeout_ms: Option<u64>,
    max_output_bytes: usize,
    derive_workflow_state: bool,
}

impl OpenSpecInvocation {
    fn from_params(params: OpenSpecParams, session_cwd: &Path) -> Result<Self> {
        let action = params.action;
        let cwd = resolve_cwd(params.cwd, session_cwd);
        let derive_workflow_state = action == OpenSpecAction::WorkflowState;

        let args = match action {
            OpenSpecAction::Status | OpenSpecAction::WorkflowState => {
                let change = require_change(&params.change, action)?;
                vec![
                    "status".to_string(),
                    "--change".to_string(),
                    change,
                    "--json".to_string(),
                ]
            }
            OpenSpecAction::List => {
                let mut args = vec!["list".to_string()];
                if params.specs {
                    args.push("--specs".to_string());
                }
                args.push("--json".to_string());
                args
            }
            OpenSpecAction::Show => {
                let item = require_item(&params.item, &params.change, action)?;
                let mut args = vec!["show".to_string(), item, "--json".to_string()];
                args.push("--type".to_string());
                args.push(if params.specs { "spec" } else { "change" }.to_string());
                args
            }
            OpenSpecAction::Instructions => {
                let change = require_change(&params.change, action)?;
                let artifact = non_empty(params.artifact).unwrap_or_else(|| "apply".to_string());
                vec![
                    "instructions".to_string(),
                    artifact,
                    "--change".to_string(),
                    change,
                    "--json".to_string(),
                ]
            }
            OpenSpecAction::Validate => {
                let mut args = vec!["validate".to_string()];
                if let Ok(item) = require_item(&params.item, &params.change, action) {
                    args.push(item);
                    args.push("--type".to_string());
                    args.push(if params.specs { "spec" } else { "change" }.to_string());
                } else if params.specs {
                    args.push("--specs".to_string());
                } else {
                    args.push("--changes".to_string());
                }
                args.push("--strict".to_string());
                args.push("--json".to_string());
                args
            }
            OpenSpecAction::Init => {
                let tools = non_empty(params.tools).unwrap_or_else(|| "none".to_string());
                let mut args = vec!["init".to_string(), "--tools".to_string(), tools];
                if let Some(path) = non_empty(params.path) {
                    args.push(path);
                }
                args
            }
            OpenSpecAction::Update => {
                let mut args = vec!["update".to_string()];
                if let Some(path) = non_empty(params.path) {
                    args.push(path);
                }
                args
            }
            OpenSpecAction::NewChange => {
                let change = require_change(&params.change, action)?;
                validate_change_name(&change)?;
                let mut args = vec!["new".to_string(), "change".to_string(), change];
                if let Some(description) = non_empty(params.description) {
                    args.push("--description".to_string());
                    args.push(description);
                }
                args
            }
            OpenSpecAction::Archive => {
                let change = require_change(&params.change, action)?;
                if !params.confirm_archive {
                    return Err(anyhow!(
                        "archive is destructive to the change directory; pass \
                         confirm_archive: true to proceed (the -y flag is then \
                         supplied internally)"
                    ));
                }
                let mut args = vec!["archive".to_string(), change, "-y".to_string()];
                if params.skip_specs {
                    args.push("--skip-specs".to_string());
                }
                args
            }
        };

        Ok(Self {
            action,
            args,
            cwd,
            timeout_ms: params.timeout_ms,
            max_output_bytes: params.max_output_bytes,
            derive_workflow_state,
        })
    }

    fn command_json(&self) -> serde_json::Value {
        json!({
            "program": OPENSPEC_BINARY,
            "args": self.args,
            "display": shell_words(OPENSPEC_BINARY, &self.args),
        })
    }
}

fn require_change(change: &Option<String>, action: OpenSpecAction) -> Result<String> {
    non_empty(change.clone())
        .ok_or_else(|| anyhow!("openspec action `{}` requires `change`", action.as_str()))
}

fn require_item(
    item: &Option<String>,
    change: &Option<String>,
    action: OpenSpecAction,
) -> Result<String> {
    non_empty(item.clone())
        .or_else(|| non_empty(change.clone()))
        .ok_or_else(|| {
            anyhow!(
                "openspec action `{}` requires `item` (or `change`)",
                action.as_str()
            )
        })
}

/// Reject names that are not safe kebab-case change ids before they reach the
/// CLI. The CLI itself is argv-safe, but a stray leading dash would be parsed
/// as a flag, and path separators would escape the changes directory.
fn validate_change_name(name: &str) -> Result<()> {
    if name.starts_with('-') {
        return Err(anyhow!("change name must not start with `-`: {name:?}"));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(anyhow!(
            "change name must not contain path separators: {name:?}"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(anyhow!(
            "change name must be kebab-case (letters, digits, `-`, `_`, `.`): {name:?}"
        ));
    }
    Ok(())
}

async fn execute_openspec(
    call_id: &str,
    invocation: OpenSpecInvocation,
    ctx: &ToolCtx,
) -> Result<String> {
    let openspec = match find_openspec_binary() {
        Some(path) => path,
        None => return Ok(missing_openspec_json(&invocation)),
    };

    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!(
            "[openspec] {}",
            shell_words(OPENSPEC_BINARY, &invocation.args)
        ),
    });

    let output = match run_openspec(&openspec, &invocation).await {
        Ok(output) => output,
        Err(OpenSpecRunError::Timeout) => return Ok(timeout_json(&invocation)),
        Err(OpenSpecRunError::Other(error)) => return Err(error),
    };

    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[exit={}]", output.exit_code),
    });

    format_openspec_output(&invocation, output)
}

fn find_openspec_binary() -> Option<PathBuf> {
    which::which(OPENSPEC_BINARY).ok()
}

#[derive(Debug)]
enum OpenSpecRunError {
    Timeout,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for OpenSpecRunError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

async fn run_openspec(
    openspec: &Path,
    invocation: &OpenSpecInvocation,
) -> std::result::Result<OpenSpecOutput, OpenSpecRunError> {
    let mut child = Command::new(openspec)
        .args(&invocation.args)
        // `--no-color` keeps ANSI escapes out of captured stdout/stderr so the
        // JSON envelope stays clean and parseable.
        .arg("--no-color")
        .current_dir(&invocation.cwd)
        // Never let a subcommand block on stdin: an unattended run must not
        // hang waiting for interactive input.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "spawn `{}` in {}",
                shell_words(OPENSPEC_BINARY, &invocation.args),
                invocation.cwd.display()
            )
        })?;

    let mut stdout_handle = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("openspec stdout was not piped"))?;
    let mut stderr_handle = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("openspec stderr was not piped"))?;

    let stdout_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout_handle.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&bytes).into_owned())
    });
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr_handle.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&bytes).into_owned())
    });

    let wait_future = child.wait();
    let status = if let Some(timeout_ms) = invocation.timeout_ms {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), wait_future).await {
            Ok(status) => status,
            Err(_) => return Err(OpenSpecRunError::Timeout),
        }
    } else {
        wait_future.await
    }
    .context("wait openspec")?;

    let stdout = stdout_task
        .await
        .context("join openspec stdout reader")?
        .context("read openspec stdout")?;
    let stderr = stderr_task
        .await
        .context("join openspec stderr reader")?
        .context("read openspec stderr")?;

    Ok(OpenSpecOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

#[derive(Debug, Clone)]
struct OpenSpecOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Summarize apply-readiness from a `status --json` payload. Returns `None`
/// when the JSON does not match the expected shape, so a schema drift degrades
/// to "no derived summary" rather than a hard error.
fn derive_workflow_state(stdout: &str) -> Option<serde_json::Value> {
    let status: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let artifacts = status.get("artifacts")?.as_array()?;

    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    let mut done = Vec::new();
    for artifact in artifacts {
        let Some(id) = artifact.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        match artifact.get("status").and_then(|v| v.as_str()) {
            Some("ready") => ready.push(id.to_string()),
            Some("done") => done.push(id.to_string()),
            _ => blocked.push(id.to_string()),
        }
    }

    let apply_requires: Vec<String> = status
        .get("applyRequires")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let is_complete = status
        .get("isComplete")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Apply is ready when every artifact the change requires before applying
    // is `done`.
    let apply_ready =
        !apply_requires.is_empty() && apply_requires.iter().all(|req| done.contains(req));

    Some(json!({
        "changeName": status.get("changeName"),
        "isComplete": is_complete,
        "applyRequires": apply_requires,
        "applyReady": apply_ready,
        "ready": ready,
        "blocked": blocked,
        "done": done,
        "nextActions": next_actions(&ready, apply_ready, is_complete),
    }))
}

fn next_actions(ready: &[String], apply_ready: bool, is_complete: bool) -> Vec<String> {
    if is_complete {
        return vec![
            "All tasks complete; archive the change with confirm_archive: true.".to_string(),
        ];
    }
    let mut actions = Vec::new();
    for artifact in ready {
        actions.push(format!(
            "Write `{artifact}`: pull `instructions` (artifact: {artifact}) then create its output."
        ));
    }
    if apply_ready {
        actions.push(
            "Required artifacts are done; implement against `instructions` (artifact: apply)."
                .to_string(),
        );
    }
    if actions.is_empty() {
        actions.push("No artifact is ready; resolve missingDeps on blocked artifacts.".to_string());
    }
    actions
}

fn format_openspec_output(
    invocation: &OpenSpecInvocation,
    output: OpenSpecOutput,
) -> Result<String> {
    let stdout = output.stdout;
    let mut stderr = output.stderr;
    let ok = output.exit_code == 0;
    let stderr_truncated = trim_to_char_budget(&mut stderr, DEFAULT_STDERR_BYTES);

    let mut base = json!({
        "ok": ok,
        "tool": "openspec",
        "action": invocation.action.as_str(),
        "command": invocation.command_json(),
        "exit_code": output.exit_code,
        "stdout": stdout.clone(),
        "stderr": nullable_string(&stderr),
        "truncated": stderr_truncated,
    });

    if !ok {
        base["error"] = json!({
            "kind": "openspec_error",
            "message": "openspec exited with a non-zero status",
        });
    }

    if invocation.derive_workflow_state {
        if let Some(summary) = derive_workflow_state(&stdout) {
            base["workflow_state"] = summary;
        }
    }

    bounded_output_json(base, &stdout, stderr, invocation.max_output_bytes)
}

fn invalid_request_json(action: OpenSpecAction, error: anyhow::Error) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": "openspec",
        "action": action.as_str(),
        "command": { "program": OPENSPEC_BINARY, "args": [] },
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "invalid_request",
            "message": error.to_string(),
        }
    }))
    .expect("invalid request JSON is serializable")
}

fn missing_openspec_json(invocation: &OpenSpecInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": "openspec",
        "action": invocation.action.as_str(),
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "missing_openspec",
            "message": "openspec was not found on PATH, so the openspec tool could not be run.",
            "install": [
                "Install the OpenSpec CLI, e.g. `npm install -g @fission-ai/openspec`.",
                "After `openspec` is available on PATH, rerun this tool.",
            ]
        }
    }))
    .expect("missing openspec JSON is serializable")
}

fn timeout_json(invocation: &OpenSpecInvocation) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": "openspec",
        "action": invocation.action.as_str(),
        "command": invocation.command_json(),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
        "error": {
            "kind": "timeout",
            "message": format!(
                "openspec exceeded the configured timeout of {} ms",
                invocation.timeout_ms.unwrap_or_default()
            )
        }
    }))
    .expect("timeout JSON is serializable")
}

/// Shrink the envelope to fit `max_output_bytes` by truncating `stdout` (then
/// `stderr`) while keeping the JSON valid and flagging `truncated`. Mirrors the
/// jq/webfetch budgeting so behavior is consistent across native CLI tools.
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
    std::iter::once(binary.to_string())
        .chain(args.iter().map(|arg| shell_word(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_word(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
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

    fn params(action: OpenSpecAction) -> OpenSpecParams {
        OpenSpecParams {
            action,
            change: None,
            item: None,
            artifact: None,
            specs: false,
            tools: None,
            path: None,
            description: None,
            confirm_archive: false,
            skip_specs: false,
            cwd: None,
            timeout_ms: None,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    fn build(p: OpenSpecParams) -> OpenSpecInvocation {
        OpenSpecInvocation::from_params(p, Path::new("/work")).unwrap()
    }

    #[test]
    fn status_builds_json_argv() {
        let mut p = params(OpenSpecAction::Status);
        p.change = Some("add-thing".into());
        let inv = build(p);
        assert_eq!(inv.args, vec!["status", "--change", "add-thing", "--json"]);
        assert_eq!(inv.cwd, Path::new("/work"));
    }

    #[test]
    fn list_specs_maps_flag() {
        let mut p = params(OpenSpecAction::List);
        p.specs = true;
        assert_eq!(build(p).args, vec!["list", "--specs", "--json"]);
    }

    #[test]
    fn instructions_defaults_artifact_to_apply() {
        let mut p = params(OpenSpecAction::Instructions);
        p.change = Some("c".into());
        assert_eq!(
            build(p).args,
            vec!["instructions", "apply", "--change", "c", "--json"]
        );
    }

    #[test]
    fn instructions_uses_explicit_artifact() {
        let mut p = params(OpenSpecAction::Instructions);
        p.change = Some("c".into());
        p.artifact = Some("proposal".into());
        assert_eq!(
            build(p).args,
            vec!["instructions", "proposal", "--change", "c", "--json"]
        );
    }

    #[test]
    fn validate_change_is_always_strict() {
        let mut p = params(OpenSpecAction::Validate);
        p.item = Some("add-thing".into());
        assert_eq!(
            build(p).args,
            vec![
                "validate",
                "add-thing",
                "--type",
                "change",
                "--strict",
                "--json"
            ]
        );
    }

    #[test]
    fn validate_without_item_targets_all_changes() {
        let p = params(OpenSpecAction::Validate);
        assert_eq!(
            build(p).args,
            vec!["validate", "--changes", "--strict", "--json"]
        );
    }

    #[test]
    fn validate_specs_targets_all_specs() {
        let mut p = params(OpenSpecAction::Validate);
        p.specs = true;
        assert_eq!(
            build(p).args,
            vec!["validate", "--specs", "--strict", "--json"]
        );
    }

    #[test]
    fn init_defaults_to_tools_none_non_interactive() {
        let p = params(OpenSpecAction::Init);
        assert_eq!(build(p).args, vec!["init", "--tools", "none"]);
    }

    #[test]
    fn init_honors_explicit_tools_and_path() {
        let mut p = params(OpenSpecAction::Init);
        p.tools = Some("claude".into());
        p.path = Some("sub/dir".into());
        assert_eq!(build(p).args, vec!["init", "--tools", "claude", "sub/dir"]);
    }

    #[test]
    fn new_change_appends_description() {
        let mut p = params(OpenSpecAction::NewChange);
        p.change = Some("add-thing".into());
        p.description = Some("does a thing".into());
        assert_eq!(
            build(p).args,
            vec![
                "new",
                "change",
                "add-thing",
                "--description",
                "does a thing"
            ]
        );
    }

    #[test]
    fn archive_requires_confirmation() {
        let mut p = params(OpenSpecAction::Archive);
        p.change = Some("add-thing".into());
        let err = OpenSpecInvocation::from_params(p, Path::new("/work")).unwrap_err();
        assert!(err.to_string().contains("confirm_archive"));
    }

    #[test]
    fn archive_with_confirmation_passes_yes() {
        let mut p = params(OpenSpecAction::Archive);
        p.change = Some("add-thing".into());
        p.confirm_archive = true;
        p.skip_specs = true;
        assert_eq!(
            build(p).args,
            vec!["archive", "add-thing", "-y", "--skip-specs"]
        );
    }

    #[test]
    fn status_requires_change() {
        let err = OpenSpecInvocation::from_params(params(OpenSpecAction::Status), Path::new("/w"))
            .unwrap_err();
        assert!(err.to_string().contains("requires `change`"));
    }

    #[test]
    fn new_change_rejects_unsafe_names() {
        for bad in ["-flag", "a/b", "weird name", "x;y"] {
            let mut p = params(OpenSpecAction::NewChange);
            p.change = Some(bad.into());
            assert!(
                OpenSpecInvocation::from_params(p, Path::new("/w")).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn relative_cwd_resolves_against_session_cwd() {
        let mut p = params(OpenSpecAction::List);
        p.cwd = Some("proj".into());
        assert_eq!(build(p).cwd, Path::new("/work/proj"));
    }

    #[test]
    fn workflow_state_runs_status_and_flags_derivation() {
        let mut p = params(OpenSpecAction::WorkflowState);
        p.change = Some("c".into());
        let inv = build(p);
        assert_eq!(inv.args, vec!["status", "--change", "c", "--json"]);
        assert!(inv.derive_workflow_state);
    }

    #[test]
    fn workflow_state_summary_classifies_artifacts() {
        let status = r#"{
            "changeName": "add-thing",
            "isComplete": false,
            "applyRequires": ["tasks"],
            "artifacts": [
                {"id": "proposal", "status": "done"},
                {"id": "design", "status": "ready"},
                {"id": "tasks", "status": "blocked"}
            ]
        }"#;
        let summary = derive_workflow_state(status).unwrap();
        assert_eq!(summary["ready"], json!(["design"]));
        assert_eq!(summary["done"], json!(["proposal"]));
        assert_eq!(summary["blocked"], json!(["tasks"]));
        assert_eq!(summary["applyReady"], json!(false));
    }

    #[test]
    fn workflow_state_apply_ready_when_requires_done() {
        let status = r#"{
            "changeName": "c",
            "isComplete": false,
            "applyRequires": ["tasks"],
            "artifacts": [{"id": "tasks", "status": "done"}]
        }"#;
        let summary = derive_workflow_state(status).unwrap();
        assert_eq!(summary["applyReady"], json!(true));
    }

    #[test]
    fn workflow_state_summary_none_on_bad_json() {
        assert!(derive_workflow_state("not json").is_none());
        assert!(derive_workflow_state("{}").is_none());
    }

    #[test]
    fn output_envelope_marks_non_zero_exit() {
        let inv = build(params(OpenSpecAction::List));
        let rendered = format_openspec_output(
            &inv,
            OpenSpecOutput {
                exit_code: 1,
                stdout: String::new(),
                stderr: "boom".into(),
            },
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["exit_code"], json!(1));
        assert_eq!(value["error"]["kind"], "openspec_error");
        assert_eq!(value["stderr"], json!("boom"));
    }

    #[test]
    fn output_is_bounded_and_valid_json() {
        let mut inv = build(params(OpenSpecAction::List));
        inv.max_output_bytes = 500;
        let rendered = format_openspec_output(
            &inv,
            OpenSpecOutput {
                exit_code: 0,
                stdout: "x".repeat(5_000),
                stderr: String::new(),
            },
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["truncated"], json!(true));
        assert!(value["stdout"].as_str().unwrap().contains("[truncated]"));
        assert!(rendered.len() <= 500, "len={}", rendered.len());
    }

    #[test]
    fn missing_binary_is_structured() {
        let inv = build(params(OpenSpecAction::Status).tap(|p| p.change = Some("c".into())));
        let rendered = missing_openspec_json(&inv);
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["error"]["kind"], "missing_openspec");
        assert!(value["error"]["install"][0]
            .as_str()
            .unwrap()
            .contains("openspec"));
    }

    #[test]
    fn invalid_request_is_structured() {
        let rendered = invalid_request_json(
            OpenSpecAction::Archive,
            anyhow!("archive needs confirm_archive: true"),
        );
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["action"], "archive");
        assert_eq!(value["error"]["kind"], "invalid_request");
    }

    // Tiny helper so a single-field tweak reads inline in a test.
    trait Tap: Sized {
        fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
            f(&mut self);
            self
        }
    }
    impl Tap for OpenSpecParams {}
}
