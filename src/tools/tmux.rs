//! Native tmux wrappers for persistent terminal sessions.
//!
//! These tools keep session/window/pane targeting typed while delegating the
//! terminal multiplexing behavior to tmux. All tmux invocations are built as
//! argv arrays. The only shell string is `tmux_run.command`, because the
//! command intentionally executes inside an interactive tmux pane.

use crate::events::Event;
use crate::tool_ctx::ToolCtx;
use crate::tools::core::Tool;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use regex::Regex;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;

const TMUX_BINARY: &str = "tmux";
const DEFAULT_WINDOW: &str = "main";
const DEFAULT_MAX_OUTPUT_BYTES: usize = 100_000;
const DEFAULT_LISTEN_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_LISTEN_POLL_MS: u64 = 500;
const DEFAULT_RUN_WAIT_TIMEOUT_MS: u64 = 30_000;
const RUN_WAIT_CAPTURE_START: i64 = -2_000;
const RA_SESSION_PREFIX: &str = "ra__";
const EXIT_MARKER_PREFIX: &str = "__RA_TMUX_EXIT:";
const EXIT_MARKER_SUFFIX: &str = "__";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TmuxEventKind {
    OutputUpdate,
    OutputMatch,
    ProgramExit,
    ProgramOutput,
    Hook,
    Sleep,
}

impl TmuxEventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::OutputUpdate => "output_update",
            Self::OutputMatch => "output_match",
            Self::ProgramExit => "program_exit",
            Self::ProgramOutput => "program_output",
            Self::Hook => "hook",
            Self::Sleep => "sleep",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxRunParams {
    /// Logical session name. Ra maps this to a tmux session named
    /// `ra__{session}`.
    pub session: String,
    /// Shell command to run inside the tmux pane.
    pub command: String,
    /// Window name. Defaults to `main`.
    #[serde(default)]
    pub window: Option<String>,
    /// Optional pane index/id within the window.
    #[serde(default)]
    pub pane: Option<String>,
    /// Wait for the command to finish and return captured pane output.
    #[serde(default)]
    pub wait: bool,
    /// Blocking wait timeout in milliseconds. Defaults to 30000.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Maximum bytes returned in captured stdout fields.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxSendParams {
    /// Logical session name. Ra maps this to a tmux session named
    /// `ra__{session}`.
    pub session: String,
    /// Window name. Defaults to `main`.
    #[serde(default)]
    pub window: Option<String>,
    /// Optional pane index/id within the window.
    #[serde(default)]
    pub pane: Option<String>,
    /// Text or tmux key name(s) to send.
    pub keys: String,
    /// Append Enter after sending `keys`.
    #[serde(default)]
    pub enter: bool,
    /// Send `keys` as literal text. Set false for tmux key names such as
    /// `C-c`, `Escape`, or `Up`.
    #[serde(default = "default_true")]
    pub literal: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxCaptureParams {
    /// Logical session name. Ra maps this to a tmux session named
    /// `ra__{session}`.
    pub session: String,
    /// Window name. Defaults to `main`.
    #[serde(default)]
    pub window: Option<String>,
    /// Optional pane index/id within the window.
    #[serde(default)]
    pub pane: Option<String>,
    /// Start line for `tmux capture-pane -S`, e.g. `-50` for recent history.
    #[serde(default)]
    pub start_line: Option<i64>,
    /// End line for `tmux capture-pane -E`.
    #[serde(default)]
    pub end_line: Option<i64>,
    /// Maximum bytes returned in captured stdout fields.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxKillParams {
    /// Kill every Ra-owned `ra__*` tmux session.
    #[serde(default)]
    pub all: bool,
    /// Logical session name. Required unless `all` is true.
    #[serde(default)]
    pub session: Option<String>,
    /// Window name. When set without `pane`, kills the window.
    #[serde(default)]
    pub window: Option<String>,
    /// Optional pane index/id. When set, kills the pane.
    #[serde(default)]
    pub pane: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxListenParams {
    /// Logical session name. Ra maps this to a tmux session named
    /// `ra__{session}`.
    pub session: String,
    /// Window name. Defaults to `main`.
    #[serde(default)]
    pub window: Option<String>,
    /// Optional pane index/id within the window.
    #[serde(default)]
    pub pane: Option<String>,
    /// Optional substring or regex to wait for in captured pane output.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Interpret `pattern` as a regex.
    #[serde(default)]
    pub regex: bool,
    /// Event to listen for. Defaults to `output_match` when `pattern` is set,
    /// otherwise `output_update`.
    #[serde(default)]
    pub event: Option<TmuxEventKind>,
    /// Optional hook label returned when `event` is `hook`.
    #[serde(default)]
    pub hook: Option<String>,
    /// Start line for each `tmux capture-pane -S`.
    #[serde(default)]
    pub start_line: Option<i64>,
    /// End line for each `tmux capture-pane -E`.
    #[serde(default)]
    pub end_line: Option<i64>,
    /// Listen timeout in milliseconds. Defaults to 30000.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Poll interval in milliseconds. Defaults to 500.
    #[serde(default)]
    pub poll_ms: Option<u64>,
    /// Maximum bytes returned in captured stdout fields.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxWaitParams {
    /// Event to wait for.
    pub event: TmuxEventKind,
    /// Logical session name. Required unless `event` is `sleep`.
    #[serde(default)]
    pub session: Option<String>,
    /// Shell command used by `program_exit` and `program_output`.
    #[serde(default)]
    pub command: Option<String>,
    /// Window name. Defaults to `main`.
    #[serde(default)]
    pub window: Option<String>,
    /// Optional pane index/id within the window.
    #[serde(default)]
    pub pane: Option<String>,
    /// Optional substring or regex expression for output and hook events.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Interpret `pattern` as a regex.
    #[serde(default)]
    pub regex: bool,
    /// Optional hook label returned when `event` is `hook`.
    #[serde(default)]
    pub hook: Option<String>,
    /// Start line for each `tmux capture-pane -S`.
    #[serde(default)]
    pub start_line: Option<i64>,
    /// End line for each `tmux capture-pane -E`.
    #[serde(default)]
    pub end_line: Option<i64>,
    /// Required wait timeout in milliseconds.
    pub timeout_ms: u64,
    /// Sleep duration in milliseconds when `event` is `sleep`.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Poll interval in milliseconds. Defaults to 500.
    #[serde(default)]
    pub poll_ms: Option<u64>,
    /// Maximum bytes returned in captured stdout fields.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

pub struct TmuxRunTool;
pub struct TmuxSendTool;
pub struct TmuxCaptureTool;
pub struct TmuxKillTool;
pub struct TmuxListenTool;
pub struct TmuxWaitTool;

#[async_trait]
impl Tool for TmuxRunTool {
    fn name(&self) -> &str {
        "tmux_run"
    }

    fn description(&self) -> &str {
        "Run a shell command inside a Ra-owned tmux session/window. Creates \
         or reuses `ra__{session}`; use `wait:false` for persistent commands \
         and `wait:true` to block until completion and return captured output."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(TmuxRunParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("tmux_run");
        let params: TmuxRunParams =
            serde_json::from_value(input).context("invalid params for tmux_run")?;
        execute_tmux_run(call_id, params, ctx).await
    }
}

#[async_trait]
impl Tool for TmuxSendTool {
    fn name(&self) -> &str {
        "tmux_send"
    }

    fn description(&self) -> &str {
        "Send literal input or tmux key names to a pane in a Ra-owned tmux \
         session. Use `literal:false` for tmux key names and `enter:true` to \
         append Enter."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(TmuxSendParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("tmux_send");
        let params: TmuxSendParams =
            serde_json::from_value(input).context("invalid params for tmux_send")?;
        execute_tmux_send(call_id, params, ctx).await
    }
}

#[async_trait]
impl Tool for TmuxCaptureTool {
    fn name(&self) -> &str {
        "tmux_capture"
    }

    fn description(&self) -> &str {
        "Capture visible content or scrollback from a pane in a Ra-owned tmux \
         session. Supports `start_line`/`end_line` bounds matching \
         `tmux capture-pane -S/-E`."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(TmuxCaptureParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("tmux_capture");
        let params: TmuxCaptureParams =
            serde_json::from_value(input).context("invalid params for tmux_capture")?;
        execute_tmux_capture(call_id, params, ctx).await
    }
}

#[async_trait]
impl Tool for TmuxKillTool {
    fn name(&self) -> &str {
        "tmux_kill"
    }

    fn description(&self) -> &str {
        "Kill a Ra-owned tmux session, window, pane, or all `ra__*` sessions. \
         This tool never targets non-namespaced tmux sessions."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(TmuxKillParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("tmux_kill");
        let params: TmuxKillParams =
            serde_json::from_value(input).context("invalid params for tmux_kill")?;
        execute_tmux_kill(call_id, params, ctx).await
    }
}

#[async_trait]
impl Tool for TmuxListenTool {
    fn name(&self) -> &str {
        "tmux_listen"
    }

    fn description(&self) -> &str {
        "Poll a tmux pane until captured output changes or an optional \
         substring/regex appears. Returns the latest bounded capture and \
         timeout state; it does not create a daemonized stream."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(TmuxListenParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("tmux_listen");
        let params: TmuxListenParams =
            serde_json::from_value(input).context("invalid params for tmux_listen")?;
        execute_tmux_listen(call_id, params, ctx).await
    }
}

#[async_trait]
impl Tool for TmuxWaitTool {
    fn name(&self) -> &str {
        "tmux_wait"
    }

    fn description(&self) -> &str {
        "Block until a tmux wait event occurs or `timeout_ms` expires. Supports \
         output_update, output_match, program_exit, program_output, hook, and \
         sleep using the same substring/regex expression semantics as \
         tmux_listen."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(TmuxWaitParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("tmux_wait");
        let params: TmuxWaitParams =
            serde_json::from_value(input).context("invalid params for tmux_wait")?;
        execute_tmux_wait(call_id, params, ctx).await
    }
}

async fn execute_tmux_run(call_id: &str, params: TmuxRunParams, ctx: &ToolCtx) -> Result<String> {
    if params.command.trim().is_empty() {
        return Err(anyhow!("tmux_run requires a non-empty command"));
    }
    let target = TmuxTarget::new(
        &params.session,
        params.window.as_deref(),
        params.pane.as_deref(),
    )?;
    let tmux = match find_tmux_binary() {
        Some(tmux) => tmux,
        None => return Ok(missing_tmux_json("tmux_run", &["run command".to_string()])),
    };

    if params.wait {
        run_blocking(call_id, &tmux, &target, &params, ctx).await
    } else {
        run_nonblocking(call_id, &tmux, &target, &params, ctx).await
    }
}

async fn execute_tmux_send(call_id: &str, params: TmuxSendParams, ctx: &ToolCtx) -> Result<String> {
    if params.keys.is_empty() && !params.enter {
        return Err(anyhow!("tmux_send requires keys or enter=true"));
    }
    let target = TmuxTarget::new(
        &params.session,
        params.window.as_deref(),
        params.pane.as_deref(),
    )?;
    let tmux = match find_tmux_binary() {
        Some(tmux) => tmux,
        None => {
            return Ok(missing_tmux_json(
                "tmux_send",
                &send_args(&target, &params),
            ))
        }
    };

    let mut last = None;
    if !params.keys.is_empty() {
        let args = send_args(&target, &params);
        emit_invocation(ctx, call_id, &args);
        let output = run_tmux(&tmux, &args).await?;
        let exit_code = output.exit_code;
        emit_exit(ctx, call_id, exit_code);
        if exit_code != 0 {
            return process_response(
                "tmux_send",
                &args,
                &target,
                output,
                DEFAULT_MAX_OUTPUT_BYTES,
            );
        }
        last = Some((args, output));
    }

    if params.enter {
        let args = vec![
            "send-keys".to_string(),
            "-t".to_string(),
            target.target.clone(),
            "Enter".to_string(),
        ];
        emit_invocation(ctx, call_id, &args);
        let output = run_tmux(&tmux, &args).await?;
        let exit_code = output.exit_code;
        emit_exit(ctx, call_id, exit_code);
        if exit_code != 0 {
            return process_response(
                "tmux_send",
                &args,
                &target,
                output,
                DEFAULT_MAX_OUTPUT_BYTES,
            );
        }
        last = Some((args, output));
    }

    let (args, output) = last.expect("tmux_send executes at least one send-keys command");
    process_response(
        "tmux_send",
        &args,
        &target,
        output,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
}

async fn execute_tmux_capture(
    call_id: &str,
    params: TmuxCaptureParams,
    ctx: &ToolCtx,
) -> Result<String> {
    let target = TmuxTarget::new(
        &params.session,
        params.window.as_deref(),
        params.pane.as_deref(),
    )?;
    let args = capture_args(&target, params.start_line, params.end_line);
    let tmux = match find_tmux_binary() {
        Some(tmux) => tmux,
        None => return Ok(missing_tmux_json("tmux_capture", &args)),
    };

    emit_invocation(ctx, call_id, &args);
    let output = run_tmux(&tmux, &args).await?;
    emit_exit(ctx, call_id, output.exit_code);
    process_response(
        "tmux_capture",
        &args,
        &target,
        output,
        params.max_output_bytes,
    )
}

async fn execute_tmux_kill(call_id: &str, params: TmuxKillParams, ctx: &ToolCtx) -> Result<String> {
    let tmux = match find_tmux_binary() {
        Some(tmux) => tmux,
        None => {
            return Ok(missing_tmux_json(
                "tmux_kill",
                &["kill-session".to_string()],
            ))
        }
    };

    if params.all {
        return kill_all_ra_sessions(call_id, &tmux, ctx).await;
    }

    let session = params
        .session
        .as_deref()
        .ok_or_else(|| anyhow!("tmux_kill requires session unless all=true"))?;
    let target = TmuxTarget::new(session, params.window.as_deref(), params.pane.as_deref())?;
    let args = if params.pane.is_some() {
        vec![
            "kill-pane".to_string(),
            "-t".to_string(),
            target.target.clone(),
        ]
    } else if params.window.is_some() {
        vec![
            "kill-window".to_string(),
            "-t".to_string(),
            target.target.clone(),
        ]
    } else {
        vec![
            "kill-session".to_string(),
            "-t".to_string(),
            target.session.clone(),
        ]
    };

    emit_invocation(ctx, call_id, &args);
    let output = run_tmux(&tmux, &args).await?;
    emit_exit(ctx, call_id, output.exit_code);
    process_response(
        "tmux_kill",
        &args,
        &target,
        output,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
}

async fn execute_tmux_listen(
    call_id: &str,
    params: TmuxListenParams,
    ctx: &ToolCtx,
) -> Result<String> {
    let target = TmuxTarget::new(
        &params.session,
        params.window.as_deref(),
        params.pane.as_deref(),
    )?;
    let args = capture_args(&target, params.start_line, params.end_line);
    let tmux = match find_tmux_binary() {
        Some(tmux) => tmux,
        None => return Ok(missing_tmux_json("tmux_listen", &args)),
    };
    let event = ListenEvent::from_listen(&params)?;
    let timeout = Duration::from_millis(params.timeout_ms.unwrap_or(DEFAULT_LISTEN_TIMEOUT_MS));
    let poll = Duration::from_millis(params.poll_ms.unwrap_or(DEFAULT_LISTEN_POLL_MS).max(10));

    let initial =
        capture_for_listen(call_id, &tmux, &args, &target, ctx, params.max_output_bytes).await?;
    if initial.exit_code != 0 {
        return process_response(
            "tmux_listen",
            &args,
            &target,
            initial,
            params.max_output_bytes,
        );
    }

    let initial_eval = event.evaluate(&initial.stdout, &initial.stdout);
    if initial_eval.triggered {
        return listen_response(ListenResponse {
            args: &args,
            target: &target,
            event: &event,
            stdout: initial.stdout,
            delta: String::new(),
            eval: initial_eval,
            timed_out: false,
            max_output_bytes: params.max_output_bytes,
        });
    }

    let started = tokio::time::Instant::now();
    let mut latest = initial.stdout.clone();
    while started.elapsed() < timeout {
        let remaining = timeout.saturating_sub(started.elapsed());
        tokio::time::sleep(poll.min(remaining)).await;
        let output =
            capture_for_listen(call_id, &tmux, &args, &target, ctx, params.max_output_bytes)
                .await?;
        if output.exit_code != 0 {
            return process_response(
                "tmux_listen",
                &args,
                &target,
                output,
                params.max_output_bytes,
            );
        }
        latest = output.stdout;
        let eval = event.evaluate(&initial.stdout, &latest);
        if eval.triggered {
            let delta = text_delta(&initial.stdout, &latest);
            return listen_response(ListenResponse {
                args: &args,
                target: &target,
                event: &event,
                stdout: latest,
                delta,
                eval,
                timed_out: false,
                max_output_bytes: params.max_output_bytes,
            });
        }
    }

    let delta = text_delta(&initial.stdout, &latest);
    let eval = event.evaluate(&initial.stdout, &latest);
    listen_response(ListenResponse {
        args: &args,
        target: &target,
        event: &event,
        stdout: latest,
        delta,
        eval,
        timed_out: true,
        max_output_bytes: params.max_output_bytes,
    })
}

async fn execute_tmux_wait(call_id: &str, params: TmuxWaitParams, ctx: &ToolCtx) -> Result<String> {
    match params.event {
        TmuxEventKind::Sleep => execute_tmux_wait_sleep(params).await,
        TmuxEventKind::OutputUpdate | TmuxEventKind::OutputMatch | TmuxEventKind::Hook => {
            execute_tmux_wait_pane(call_id, params, ctx).await
        }
        TmuxEventKind::ProgramExit | TmuxEventKind::ProgramOutput => {
            execute_tmux_wait_program(call_id, params, ctx).await
        }
    }
}

async fn execute_tmux_wait_sleep(params: TmuxWaitParams) -> Result<String> {
    let duration_ms = params
        .duration_ms
        .ok_or_else(|| anyhow!("tmux_wait sleep requires duration_ms"))?;
    let timeout = Duration::from_millis(params.timeout_ms);
    let duration = Duration::from_millis(duration_ms);
    let started = tokio::time::Instant::now();
    let timed_out = duration > timeout;
    tokio::time::sleep(duration.min(timeout)).await;
    wait_response(WaitResponse {
        args: &[],
        target: None,
        event: &WaitEvent::new(TmuxEventKind::Sleep, None, false, None)?,
        stdout: String::new(),
        delta: String::new(),
        eval: EventEvaluation {
            changed: false,
            matched: false,
            triggered: !timed_out,
        },
        timed_out,
        exit_code: 0,
        command_exit_code: None,
        capture_exit_code: None,
        stderr: String::new(),
        elapsed_ms: elapsed_millis(started.elapsed()),
        max_output_bytes: params.max_output_bytes,
    })
}

async fn execute_tmux_wait_pane(
    call_id: &str,
    params: TmuxWaitParams,
    ctx: &ToolCtx,
) -> Result<String> {
    let target = wait_target(&params)?;
    let args = capture_args(&target, params.start_line, params.end_line);
    let tmux = match find_tmux_binary() {
        Some(tmux) => tmux,
        None => return Ok(missing_tmux_json("tmux_wait", &args)),
    };
    let event = WaitEvent::from_wait(&params)?;
    let timeout = Duration::from_millis(params.timeout_ms);
    let poll = Duration::from_millis(params.poll_ms.unwrap_or(DEFAULT_LISTEN_POLL_MS).max(10));

    let initial =
        capture_for_listen(call_id, &tmux, &args, &target, ctx, params.max_output_bytes).await?;
    if initial.exit_code != 0 {
        return process_response(
            "tmux_wait",
            &args,
            &target,
            initial,
            params.max_output_bytes,
        );
    }

    let started = tokio::time::Instant::now();
    let mut latest = initial.stdout.clone();
    loop {
        let eval = event.evaluate(&initial.stdout, &latest);
        if eval.triggered || started.elapsed() >= timeout {
            let delta = text_delta(&initial.stdout, &latest);
            return wait_response(WaitResponse {
                args: &args,
                target: Some(&target),
                event: &event,
                stdout: latest,
                delta,
                eval,
                timed_out: !eval.triggered,
                exit_code: 0,
                command_exit_code: None,
                capture_exit_code: None,
                stderr: String::new(),
                elapsed_ms: elapsed_millis(started.elapsed()),
                max_output_bytes: params.max_output_bytes,
            });
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        tokio::time::sleep(poll.min(remaining)).await;
        let output =
            capture_for_listen(call_id, &tmux, &args, &target, ctx, params.max_output_bytes)
                .await?;
        if output.exit_code != 0 {
            return process_response("tmux_wait", &args, &target, output, params.max_output_bytes);
        }
        latest = output.stdout;
    }
}

async fn execute_tmux_wait_program(
    call_id: &str,
    params: TmuxWaitParams,
    ctx: &ToolCtx,
) -> Result<String> {
    let command = params
        .command
        .as_deref()
        .ok_or_else(|| anyhow!("tmux_wait {:?} requires command", params.event))?;
    if command.trim().is_empty() {
        return Err(anyhow!(
            "tmux_wait {:?} requires a non-empty command",
            params.event
        ));
    }
    let target = wait_target(&params)?;
    let tmux = match find_tmux_binary() {
        Some(tmux) => tmux,
        None => {
            return Ok(missing_tmux_json(
                "tmux_wait",
                &["respawn-pane".to_string(), "-t".to_string(), target.target],
            ))
        }
    };
    let event = WaitEvent::from_wait(&params)?;

    if let Some(failure) = ensure_session_window(&tmux, &target, &ctx.cwd).await? {
        let args = failure.args.clone();
        return process_response(
            "tmux_wait",
            &args,
            &target,
            failure,
            params.max_output_bytes,
        );
    }

    let tempdir = tempfile::tempdir().context("create tmux wait script directory")?;
    let script_path = tempdir.path().join("ra-tmux-wait.sh");
    let token = format!("ra_tmux_wait_{}", unique_token());
    let script = wait_script(command, &token);
    tokio::fs::write(&script_path, script)
        .await
        .with_context(|| format!("write {}", script_path.display()))?;

    let shell_command = format!("/bin/sh {}", shell_word(&script_path.to_string_lossy()));
    let start_args = vec![
        "respawn-pane".to_string(),
        "-k".to_string(),
        "-t".to_string(),
        target.target.clone(),
        "-c".to_string(),
        ctx.cwd.to_string_lossy().to_string(),
        shell_command,
    ];
    emit_invocation(ctx, call_id, &start_args);
    let start = run_tmux(&tmux, &start_args).await?;
    emit_exit(ctx, call_id, start.exit_code);
    if start.exit_code != 0 {
        return process_response(
            "tmux_wait",
            &start_args,
            &target,
            start,
            params.max_output_bytes,
        );
    }

    let capture_start = params.start_line.or(Some(RUN_WAIT_CAPTURE_START));
    let capture_args = capture_args(&target, capture_start, params.end_line);
    let timeout = Duration::from_millis(params.timeout_ms);
    let poll = Duration::from_millis(params.poll_ms.unwrap_or(DEFAULT_LISTEN_POLL_MS).max(10));
    let started = tokio::time::Instant::now();

    loop {
        let capture = capture_for_listen(
            call_id,
            &tmux,
            &capture_args,
            &target,
            ctx,
            params.max_output_bytes,
        )
        .await?;
        let capture_exit_code = capture.exit_code;
        if capture.exit_code != 0 {
            return process_response(
                "tmux_wait",
                &capture_args,
                &target,
                capture,
                params.max_output_bytes,
            );
        }

        let (stdout, command_exit_code) = strip_exit_marker(&capture.stdout, &token);
        let eval = event.evaluate_program_output(&stdout, command_exit_code);
        let terminal =
            eval.triggered || command_exit_code.is_some() || started.elapsed() >= timeout;
        if terminal {
            let timed_out = !eval.triggered && command_exit_code.is_none();
            let exit_code = if timed_out { -1 } else { 0 };
            let stderr = if timed_out {
                format!("timed out after {} ms", timeout.as_millis())
            } else {
                capture.stderr
            };
            return wait_response(WaitResponse {
                args: &capture_args,
                target: Some(&target),
                event: &event,
                stdout,
                delta: String::new(),
                eval,
                timed_out,
                exit_code,
                command_exit_code,
                capture_exit_code: Some(capture_exit_code),
                stderr,
                elapsed_ms: elapsed_millis(started.elapsed()),
                max_output_bytes: params.max_output_bytes,
            });
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        tokio::time::sleep(poll.min(remaining)).await;
    }
}

async fn run_nonblocking(
    call_id: &str,
    tmux: &Path,
    target: &TmuxTarget,
    params: &TmuxRunParams,
    ctx: &ToolCtx,
) -> Result<String> {
    let (args, output) = if !session_exists(tmux, target).await? {
        let args = new_session_args(target, Some(&params.command), &ctx.cwd);
        emit_invocation(ctx, call_id, &args);
        let output = run_tmux(tmux, &args).await?;
        emit_exit(ctx, call_id, output.exit_code);
        (args, output)
    } else if !window_exists(tmux, target).await? {
        let args = new_window_args(target, Some(&params.command), &ctx.cwd);
        emit_invocation(ctx, call_id, &args);
        let output = run_tmux(tmux, &args).await?;
        emit_exit(ctx, call_id, output.exit_code);
        (args, output)
    } else {
        let send = TmuxSendParams {
            session: params.session.clone(),
            window: params.window.clone(),
            pane: params.pane.clone(),
            keys: params.command.clone(),
            enter: true,
            literal: true,
        };
        let args = send_args(target, &send);
        emit_invocation(ctx, call_id, &args);
        let first = run_tmux(tmux, &args).await?;
        emit_exit(ctx, call_id, first.exit_code);
        if first.exit_code != 0 {
            return process_response("tmux_run", &args, target, first, params.max_output_bytes);
        }
        let enter_args = vec![
            "send-keys".to_string(),
            "-t".to_string(),
            target.target.clone(),
            "Enter".to_string(),
        ];
        emit_invocation(ctx, call_id, &enter_args);
        let output = run_tmux(tmux, &enter_args).await?;
        emit_exit(ctx, call_id, output.exit_code);
        (enter_args, output)
    };

    process_response("tmux_run", &args, target, output, params.max_output_bytes)
}

async fn run_blocking(
    call_id: &str,
    tmux: &Path,
    target: &TmuxTarget,
    params: &TmuxRunParams,
    ctx: &ToolCtx,
) -> Result<String> {
    if let Some(failure) = ensure_session_window(tmux, target, &ctx.cwd).await? {
        let args = failure.args.clone();
        return process_response("tmux_run", &args, target, failure, params.max_output_bytes);
    }

    let tempdir = tempfile::tempdir().context("create tmux wait script directory")?;
    let script_path = tempdir.path().join("ra-tmux-run.sh");
    let token = format!("ra_tmux_{}", unique_token());
    let script = wait_script(&params.command, &token);
    tokio::fs::write(&script_path, script)
        .await
        .with_context(|| format!("write {}", script_path.display()))?;

    let shell_command = format!("/bin/sh {}", shell_word(&script_path.to_string_lossy()));
    let args = vec![
        "respawn-pane".to_string(),
        "-k".to_string(),
        "-t".to_string(),
        target.target.clone(),
        "-c".to_string(),
        ctx.cwd.to_string_lossy().to_string(),
        shell_command,
    ];
    emit_invocation(ctx, call_id, &args);
    let output = run_tmux(tmux, &args).await?;
    emit_exit(ctx, call_id, output.exit_code);
    if output.exit_code != 0 {
        return process_response("tmux_run", &args, target, output, params.max_output_bytes);
    }

    let wait_args = vec!["wait-for".to_string(), token.clone()];
    emit_invocation(ctx, call_id, &wait_args);
    let timeout = Duration::from_millis(params.timeout_ms.unwrap_or(DEFAULT_RUN_WAIT_TIMEOUT_MS));
    let wait = run_tmux_timeout(tmux, &wait_args, timeout).await?;
    emit_exit(ctx, call_id, wait.output.exit_code);

    let capture_args = capture_args(target, Some(RUN_WAIT_CAPTURE_START), None);
    let capture = run_tmux(tmux, &capture_args).await?;
    let (stdout, command_exit_code) = strip_exit_marker(&capture.stdout, &token);
    let ok = !wait.timed_out
        && wait.output.exit_code == 0
        && capture.exit_code == 0
        && command_exit_code == Some(0);
    let (stdout, stdout_truncated) = trim_to_byte_budget(stdout, params.max_output_bytes);
    let stderr = join_non_empty(&[wait.output.stderr, capture.stderr]);
    let (stderr, stderr_truncated) = trim_to_byte_budget(stderr, DEFAULT_MAX_OUTPUT_BYTES);

    serde_json::to_string_pretty(&json!({
        "ok": ok,
        "tool": "tmux_run",
        "target": target,
        "command": {
            "program": TMUX_BINARY,
            "args": args,
        "display": shell_words(TMUX_BINARY, &args),
        },
        "wait": true,
        "timed_out": wait.timed_out,
        "exit_code": wait.output.exit_code,
        "command_exit_code": command_exit_code,
        "capture_exit_code": capture.exit_code,
        "stdout": stdout,
        "stderr": if stderr.is_empty() { serde_json::Value::Null } else { json!(stderr) },
        "truncated": stdout_truncated || stderr_truncated,
    }))
    .context("serialize tmux_run output")
}

async fn ensure_session_window(
    tmux: &Path,
    target: &TmuxTarget,
    cwd: &Path,
) -> Result<Option<TmuxOutput>> {
    if !session_exists(tmux, target).await? {
        let args = new_session_args(target, None, cwd);
        let output = run_tmux(tmux, &args).await?;
        if output.exit_code != 0 {
            return Ok(Some(output));
        }
        return Ok(None);
    }

    if !window_exists(tmux, target).await? {
        let args = new_window_args(target, None, cwd);
        let output = run_tmux(tmux, &args).await?;
        if output.exit_code != 0 {
            return Ok(Some(output));
        }
    }
    Ok(None)
}

async fn session_exists(tmux: &Path, target: &TmuxTarget) -> Result<bool> {
    let args = vec![
        "has-session".to_string(),
        "-t".to_string(),
        target.session.clone(),
    ];
    let output = run_tmux(tmux, &args).await?;
    Ok(output.exit_code == 0)
}

async fn window_exists(tmux: &Path, target: &TmuxTarget) -> Result<bool> {
    let args = vec![
        "list-windows".to_string(),
        "-t".to_string(),
        target.session.clone(),
        "-F".to_string(),
        "#{window_name}".to_string(),
    ];
    let output = run_tmux(tmux, &args).await?;
    if output.exit_code != 0 {
        return Ok(false);
    }
    Ok(output.stdout.lines().any(|line| line == target.window))
}

async fn kill_all_ra_sessions(call_id: &str, tmux: &Path, ctx: &ToolCtx) -> Result<String> {
    let list_args = vec![
        "list-sessions".to_string(),
        "-F".to_string(),
        "#{session_name}".to_string(),
    ];
    emit_invocation(ctx, call_id, &list_args);
    let list_output = run_tmux(tmux, &list_args).await?;
    emit_exit(ctx, call_id, list_output.exit_code);
    if list_output.exit_code != 0 {
        return serde_json::to_string_pretty(&json!({
            "ok": true,
            "tool": "tmux_kill",
            "all": true,
            "killed": [],
            "command": command_metadata(&list_args),
            "exit_code": list_output.exit_code,
            "stdout": "",
            "stderr": if list_output.stderr.is_empty() {
                serde_json::Value::Null
            } else {
                json!(list_output.stderr)
            },
            "truncated": false,
        }))
        .context("serialize tmux_kill all output");
    }

    let mut killed = Vec::new();
    let mut failures = Vec::new();
    for session in list_output
        .stdout
        .lines()
        .filter(|line| line.starts_with(RA_SESSION_PREFIX))
    {
        let args = vec![
            "kill-session".to_string(),
            "-t".to_string(),
            session.to_string(),
        ];
        emit_invocation(ctx, call_id, &args);
        let output = run_tmux(tmux, &args).await?;
        emit_exit(ctx, call_id, output.exit_code);
        if output.exit_code == 0 {
            killed.push(session.to_string());
        } else {
            failures.push(json!({
                "session": session,
                "exit_code": output.exit_code,
                "stderr": output.stderr,
            }));
        }
    }

    serde_json::to_string_pretty(&json!({
        "ok": failures.is_empty(),
        "tool": "tmux_kill",
        "all": true,
        "killed": killed,
        "failures": failures,
        "command": command_metadata(&list_args),
        "exit_code": if failures.is_empty() { 0 } else { 1 },
        "stdout": "",
        "stderr": null,
        "truncated": false,
    }))
    .context("serialize tmux_kill all output")
}

async fn capture_for_listen(
    call_id: &str,
    tmux: &Path,
    args: &[String],
    _target: &TmuxTarget,
    ctx: &ToolCtx,
    _max_output_bytes: usize,
) -> Result<TmuxOutput> {
    emit_invocation(ctx, call_id, args);
    let output = run_tmux(tmux, args).await?;
    emit_exit(ctx, call_id, output.exit_code);
    Ok(output)
}

fn new_session_args(target: &TmuxTarget, command: Option<&str>, cwd: &Path) -> Vec<String> {
    let mut args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        target.session.clone(),
        "-n".to_string(),
        target.window.clone(),
        "-c".to_string(),
        cwd.to_string_lossy().to_string(),
    ];
    if let Some(command) = command {
        args.push(command.to_string());
    }
    args
}

fn new_window_args(target: &TmuxTarget, command: Option<&str>, cwd: &Path) -> Vec<String> {
    let mut args = vec![
        "new-window".to_string(),
        "-d".to_string(),
        "-t".to_string(),
        target.session.clone(),
        "-n".to_string(),
        target.window.clone(),
        "-c".to_string(),
        cwd.to_string_lossy().to_string(),
    ];
    if let Some(command) = command {
        args.push(command.to_string());
    }
    args
}

fn send_args(target: &TmuxTarget, params: &TmuxSendParams) -> Vec<String> {
    let mut args = vec![
        "send-keys".to_string(),
        "-t".to_string(),
        target.target.clone(),
    ];
    if params.literal {
        args.push("-l".to_string());
        // `--` prevents tmux from interpreting a leading `-` in the keys as a flag.
        args.push("--".to_string());
        args.push(params.keys.clone());
    } else {
        args.extend(
            params
                .keys
                .split_whitespace()
                .filter(|key| !key.is_empty())
                .map(ToString::to_string),
        );
    }
    args
}

fn capture_args(
    target: &TmuxTarget,
    start_line: Option<i64>,
    end_line: Option<i64>,
) -> Vec<String> {
    let mut args = vec![
        "capture-pane".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        target.target.clone(),
    ];
    if let Some(start_line) = start_line {
        args.push("-S".to_string());
        args.push(start_line.to_string());
    }
    if let Some(end_line) = end_line {
        args.push("-E".to_string());
        args.push(end_line.to_string());
    }
    args
}

fn wait_script(command: &str, token: &str) -> String {
    format!(
        "#!/bin/sh\n\
         /bin/sh -c {} 2>&1\n\
         status=$?\n\
         printf '\\n{EXIT_MARKER_PREFIX}{}:{EXIT_MARKER_SUFFIX}%s{EXIT_MARKER_SUFFIX}\\n' \"$status\"\n\
         tmux wait-for -S {}\n\
         exec /bin/sh\n",
        shell_word(command),
        token,
        shell_word(token)
    )
}

async fn run_tmux(tmux: &Path, args: &[String]) -> Result<TmuxOutput> {
    let output = Command::new(tmux)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("spawn `{}`", shell_words(TMUX_BINARY, args)))?;
    Ok(TmuxOutput {
        args: args.to_vec(),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

async fn run_tmux_timeout(
    tmux: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<TimedTmuxOutput> {
    let child = Command::new(tmux)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn `{}`", shell_words(TMUX_BINARY, args)))?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(output) => {
            let output =
                output.with_context(|| format!("wait `{}`", shell_words(TMUX_BINARY, args)))?;
            Ok(TimedTmuxOutput {
                timed_out: false,
                output: TmuxOutput {
                    args: args.to_vec(),
                    exit_code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                },
            })
        }
        Err(_) => Ok(TimedTmuxOutput {
            timed_out: true,
            output: TmuxOutput {
                args: args.to_vec(),
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("timed out after {} ms", timeout.as_millis()),
            },
        }),
    }
}

fn process_response(
    tool: &str,
    args: &[String],
    target: &TmuxTarget,
    output: TmuxOutput,
    max_output_bytes: usize,
) -> Result<String> {
    let (stdout, stdout_truncated) = trim_to_byte_budget(output.stdout, max_output_bytes);
    let (stderr, stderr_truncated) = trim_to_byte_budget(output.stderr, DEFAULT_MAX_OUTPUT_BYTES);
    serde_json::to_string_pretty(&json!({
        "ok": output.exit_code == 0,
        "tool": tool,
        "target": target,
        "command": command_metadata(args),
        "exit_code": output.exit_code,
        "stdout": stdout,
        "stderr": if stderr.is_empty() { serde_json::Value::Null } else { json!(stderr) },
        "truncated": stdout_truncated || stderr_truncated,
    }))
    .context("serialize tmux tool output")
}

struct ListenResponse<'a> {
    args: &'a [String],
    target: &'a TmuxTarget,
    event: &'a ListenEvent,
    stdout: String,
    delta: String,
    eval: EventEvaluation,
    timed_out: bool,
    max_output_bytes: usize,
}

fn listen_response(r: ListenResponse<'_>) -> Result<String> {
    let ListenResponse {
        args,
        target,
        event,
        stdout,
        delta,
        eval,
        timed_out,
        max_output_bytes,
    } = r;
    let (stdout, stdout_truncated) = trim_to_byte_budget(stdout, max_output_bytes);
    let (delta, delta_truncated) = trim_to_byte_budget(delta, max_output_bytes);
    serde_json::to_string_pretty(&json!({
        "ok": !timed_out,
        "tool": "tmux_listen",
        "target": target,
        "command": command_metadata(args),
        "event": event.metadata(),
        "changed": eval.changed,
        "matched": eval.matched,
        "triggered": eval.triggered,
        "timed_out": timed_out,
        "exit_code": 0,
        "stdout": stdout,
        "delta": delta,
        "stderr": null,
        "truncated": stdout_truncated || delta_truncated,
    }))
    .context("serialize tmux_listen output")
}

struct WaitResponse<'a> {
    args: &'a [String],
    target: Option<&'a TmuxTarget>,
    event: &'a WaitEvent,
    stdout: String,
    delta: String,
    eval: EventEvaluation,
    timed_out: bool,
    exit_code: i32,
    command_exit_code: Option<i32>,
    capture_exit_code: Option<i32>,
    stderr: String,
    elapsed_ms: u128,
    max_output_bytes: usize,
}

fn wait_response(response: WaitResponse<'_>) -> Result<String> {
    let WaitResponse {
        args,
        target,
        event,
        stdout,
        delta,
        eval,
        timed_out,
        exit_code,
        command_exit_code,
        capture_exit_code,
        stderr,
        elapsed_ms,
        max_output_bytes,
    } = response;
    let (stdout, stdout_truncated) = trim_to_byte_budget(stdout, max_output_bytes);
    let (delta, delta_truncated) = trim_to_byte_budget(delta, max_output_bytes);
    let (stderr, stderr_truncated) = trim_to_byte_budget(stderr, DEFAULT_MAX_OUTPUT_BYTES);

    serde_json::to_string_pretty(&json!({
        "ok": eval.triggered && !timed_out,
        "tool": "tmux_wait",
        "target": target,
        "command": command_metadata(args),
        "event": event.metadata(),
        "changed": eval.changed,
        "matched": eval.matched,
        "triggered": eval.triggered,
        "timed_out": timed_out,
        "exit_code": exit_code,
        "command_exit_code": command_exit_code,
        "capture_exit_code": capture_exit_code,
        "elapsed_ms": elapsed_ms,
        "stdout": stdout,
        "delta": delta,
        "stderr": if stderr.is_empty() { serde_json::Value::Null } else { json!(stderr) },
        "truncated": stdout_truncated || delta_truncated || stderr_truncated,
    }))
    .context("serialize tmux_wait output")
}

fn missing_tmux_json(tool: &str, args: &[String]) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": tool,
        "error": {
            "kind": "missing_tmux",
            "message": "tmux was not found on PATH, so the tmux tool could not be run.",
            "install": [
                "Install tmux with your system package manager, for example: apt install tmux, brew install tmux, or dnf install tmux.",
                "After tmux is available on PATH, rerun the tool."
            ]
        },
        "command": command_metadata(args),
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false,
    }))
    .expect("missing tmux JSON is serializable")
}

fn command_metadata(args: &[String]) -> serde_json::Value {
    json!({
        "program": TMUX_BINARY,
        "args": args,
        "display": shell_words(TMUX_BINARY, args),
    })
}

fn emit_invocation(ctx: &ToolCtx, call_id: &str, args: &[String]) {
    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[tmux] {}", shell_words(TMUX_BINARY, args)),
    });
}

fn emit_exit(ctx: &ToolCtx, call_id: &str, exit_code: i32) {
    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[exit={exit_code}]"),
    });
}

fn find_tmux_binary() -> Option<PathBuf> {
    which::which(TMUX_BINARY).ok()
}

#[derive(Debug, Clone, Serialize)]
struct TmuxTarget {
    logical_session: String,
    session: String,
    window: String,
    pane: Option<String>,
    target: String,
}

impl TmuxTarget {
    fn new(session: &str, window: Option<&str>, pane: Option<&str>) -> Result<Self> {
        let logical_session = validate_name("session", session, true, false)?;
        let session = format!("{RA_SESSION_PREFIX}{logical_session}");
        let window = match window {
            // `.` is the pane separator in tmux target syntax (session:window.pane),
            // so window names must not contain it to avoid ambiguous targets.
            Some(window) => validate_name("window", window, false, false)?,
            None => DEFAULT_WINDOW.to_string(),
        };
        let pane = pane
            .map(|pane| validate_name("pane", pane, true, true))
            .transpose()?;
        let target = match &pane {
            Some(pane) => format!("{session}:{window}.{pane}"),
            None => format!("{session}:{window}"),
        };
        Ok(Self {
            logical_session,
            session,
            window,
            pane,
            target,
        })
    }
}

#[derive(Debug)]
struct TmuxOutput {
    args: Vec<String>,
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
struct TimedTmuxOutput {
    timed_out: bool,
    output: TmuxOutput,
}

enum OutputMatcher {
    None,
    Substring(String),
    Regex(Regex),
}

impl OutputMatcher {
    fn new(pattern: Option<&str>, regex: bool) -> Result<Self> {
        let Some(pattern) = pattern else {
            return Ok(Self::None);
        };
        if pattern.is_empty() {
            return Ok(Self::None);
        }
        if regex {
            Ok(Self::Regex(Regex::new(pattern).with_context(|| {
                format!("invalid tmux_listen regex: {pattern}")
            })?))
        } else {
            Ok(Self::Substring(pattern.to_string()))
        }
    }

    fn matches(&self, text: &str) -> bool {
        match self {
            Self::None => false,
            Self::Substring(pattern) => text.contains(pattern),
            Self::Regex(regex) => regex.is_match(text),
        }
    }

    fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

struct ListenEvent {
    inner: WaitEvent,
}

impl ListenEvent {
    fn from_listen(params: &TmuxListenParams) -> Result<Self> {
        let kind = params.event.unwrap_or_else(|| {
            if params
                .pattern
                .as_deref()
                .is_some_and(|pattern| !pattern.is_empty())
            {
                TmuxEventKind::OutputMatch
            } else {
                TmuxEventKind::OutputUpdate
            }
        });
        match kind {
            TmuxEventKind::OutputUpdate | TmuxEventKind::OutputMatch | TmuxEventKind::Hook => {}
            TmuxEventKind::ProgramExit | TmuxEventKind::ProgramOutput | TmuxEventKind::Sleep => {
                return Err(anyhow!(
                    "tmux_listen supports output_update, output_match, and hook events"
                ));
            }
        }
        Ok(Self {
            inner: WaitEvent::new(
                kind,
                params.pattern.as_deref(),
                params.regex,
                params.hook.as_deref(),
            )?,
        })
    }

    fn evaluate(&self, initial: &str, latest: &str) -> EventEvaluation {
        self.inner.evaluate_pane(initial, latest)
    }

    fn metadata(&self) -> serde_json::Value {
        self.inner.metadata()
    }
}

struct WaitEvent {
    kind: TmuxEventKind,
    matcher: OutputMatcher,
    hook: Option<String>,
}

impl WaitEvent {
    fn from_wait(params: &TmuxWaitParams) -> Result<Self> {
        Self::new(
            params.event,
            params.pattern.as_deref(),
            params.regex,
            params.hook.as_deref(),
        )
    }

    fn new(
        kind: TmuxEventKind,
        pattern: Option<&str>,
        regex: bool,
        hook: Option<&str>,
    ) -> Result<Self> {
        let matcher = OutputMatcher::new(pattern, regex)?;
        match kind {
            TmuxEventKind::OutputMatch | TmuxEventKind::Hook => {
                if matcher.is_none() {
                    return Err(anyhow!("tmux {} event requires pattern", kind.as_str()));
                }
            }
            TmuxEventKind::ProgramOutput => {}
            TmuxEventKind::OutputUpdate | TmuxEventKind::ProgramExit | TmuxEventKind::Sleep => {}
        }
        Ok(Self {
            kind,
            matcher,
            hook: hook.map(ToString::to_string),
        })
    }

    fn evaluate(&self, initial: &str, latest: &str) -> EventEvaluation {
        self.evaluate_pane(initial, latest)
    }

    fn evaluate_pane(&self, initial: &str, latest: &str) -> EventEvaluation {
        let changed = latest != initial;
        let matched = self.matcher.matches(latest);
        let triggered = match self.kind {
            TmuxEventKind::OutputUpdate => changed,
            TmuxEventKind::OutputMatch | TmuxEventKind::Hook => matched,
            TmuxEventKind::ProgramOutput => {
                if self.matcher.is_none() {
                    changed && !latest.is_empty()
                } else {
                    matched
                }
            }
            TmuxEventKind::ProgramExit | TmuxEventKind::Sleep => false,
        };
        EventEvaluation {
            changed,
            matched,
            triggered,
        }
    }

    fn evaluate_program_output(
        &self,
        stdout: &str,
        command_exit_code: Option<i32>,
    ) -> EventEvaluation {
        let matched = self.matcher.matches(stdout);
        let triggered = match self.kind {
            TmuxEventKind::ProgramExit => command_exit_code.is_some(),
            TmuxEventKind::ProgramOutput => {
                if self.matcher.is_none() {
                    !stdout.is_empty()
                } else {
                    matched
                }
            }
            _ => self.evaluate_pane("", stdout).triggered,
        };
        EventEvaluation {
            changed: !stdout.is_empty(),
            matched,
            triggered,
        }
    }

    fn metadata(&self) -> serde_json::Value {
        json!({
            "kind": self.kind.as_str(),
            "hook": self.hook,
            "matcher": self.matcher.metadata(),
        })
    }
}

#[derive(Clone, Copy)]
struct EventEvaluation {
    changed: bool,
    matched: bool,
    triggered: bool,
}

impl OutputMatcher {
    fn metadata(&self) -> serde_json::Value {
        match self {
            Self::None => json!({ "type": "none", "pattern": null }),
            Self::Substring(pattern) => json!({ "type": "substring", "pattern": pattern }),
            Self::Regex(regex) => json!({ "type": "regex", "pattern": regex.as_str() }),
        }
    }
}

fn wait_target(params: &TmuxWaitParams) -> Result<TmuxTarget> {
    let session = params
        .session
        .as_deref()
        .ok_or_else(|| anyhow!("tmux_wait {} requires session", params.event.as_str()))?;
    TmuxTarget::new(session, params.window.as_deref(), params.pane.as_deref())
}

fn elapsed_millis(duration: Duration) -> u128 {
    duration.as_millis()
}

fn validate_name(field: &str, value: &str, allow_dot: bool, allow_percent: bool) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("tmux {field} must not be empty"));
    }
    if value.len() > 80 {
        return Err(anyhow!("tmux {field} must be at most 80 bytes"));
    }
    let valid = value.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(ch, '_' | '-')
            || (allow_dot && ch == '.')
            || (allow_percent && ch == '%')
    });
    if !valid {
        let mut allowed = "ASCII letters, digits, `_`, `-`".to_string();
        if allow_dot {
            allowed.push_str(", `.`");
        }
        if allow_percent {
            allowed.push_str(", `%`");
        }
        return Err(anyhow!("tmux {field} may only contain {allowed}"));
    }
    Ok(value.to_string())
}

fn strip_exit_marker(text: &str, token: &str) -> (String, Option<i32>) {
    // Marker format: __RA_TMUX_EXIT:<token>:__<exit_code>__
    // The token makes the marker unique per call so command output cannot forge it.
    let prefix = format!("{EXIT_MARKER_PREFIX}{token}:{EXIT_MARKER_SUFFIX}");
    let mut exit_code = None;
    let mut kept = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end_matches('\r').trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            if let Some(code) = rest.strip_suffix(EXIT_MARKER_SUFFIX) {
                exit_code = code.parse::<i32>().ok();
                continue;
            }
        }
        kept.push(line);
    }
    let mut out = kept.join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    (out, exit_code)
}

fn trim_to_byte_budget(mut text: String, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text, false);
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
    (text, true)
}

fn text_delta(initial: &str, latest: &str) -> String {
    latest
        .strip_prefix(initial)
        .map(|s| s.to_string())
        .unwrap_or_else(|| latest.to_string())
}

fn join_non_empty(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("")
}

fn shell_words(binary: &str, args: &[String]) -> String {
    std::iter::once(binary.to_string())
        .chain(args.iter().map(|arg| shell_word(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_word(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '_' | '-' | '=' | ':' | '%'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}_{}", std::process::id(), nanos)
}

fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_namespaces_logical_session() {
        let target = TmuxTarget::new("dev", Some("tests"), Some("0")).unwrap();

        assert_eq!(target.logical_session, "dev");
        assert_eq!(target.session, "ra__dev");
        assert_eq!(target.window, "tests");
        assert_eq!(target.target, "ra__dev:tests.0");
    }

    #[test]
    fn target_rejects_ambiguous_names() {
        assert!(TmuxTarget::new("dev:other", None, None).is_err());
        assert!(TmuxTarget::new("dev", Some("bad.window:name"), None).is_err());
        assert!(TmuxTarget::new("dev", None, Some("%1")).is_ok());
    }

    #[test]
    fn run_params_default_to_nonblocking_main_window() {
        let params: TmuxRunParams =
            serde_json::from_value(json!({ "session": "dev", "command": "echo ok" })).unwrap();
        let target = TmuxTarget::new(&params.session, params.window.as_deref(), None).unwrap();

        assert!(!params.wait);
        assert_eq!(target.target, "ra__dev:main");
        assert_eq!(params.max_output_bytes, DEFAULT_MAX_OUTPUT_BYTES);
    }

    #[test]
    fn send_literal_builds_argv_without_shell() {
        let target = TmuxTarget::new("dev", None, None).unwrap();
        let params = TmuxSendParams {
            session: "dev".into(),
            window: None,
            pane: None,
            keys: "hello; rm -rf /".into(),
            enter: false,
            literal: true,
        };

        assert_eq!(
            send_args(&target, &params),
            vec!["send-keys", "-t", "ra__dev:main", "-l", "--", "hello; rm -rf /"]
        );
    }

    #[test]
    fn capture_builds_line_bounds() {
        let target = TmuxTarget::new("dev", Some("logs"), None).unwrap();

        assert_eq!(
            capture_args(&target, Some(-50), Some(-1)),
            vec![
                "capture-pane",
                "-p",
                "-t",
                "ra__dev:logs",
                "-S",
                "-50",
                "-E",
                "-1"
            ]
        );
    }

    #[test]
    fn missing_tmux_response_is_structured_json() {
        let rendered = missing_tmux_json("tmux_capture", &["capture-pane".into()]);
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["error"]["kind"], "missing_tmux");
        assert!(value["error"]["install"][0]
            .as_str()
            .unwrap()
            .contains("Install tmux"));
    }

    #[test]
    fn exit_marker_is_removed_and_parsed() {
        let (text, code) = strip_exit_marker("one\n__RA_TMUX_EXIT:mytoken:__7__\ntwo\n", "mytoken");

        assert_eq!(text, "one\ntwo\n");
        assert_eq!(code, Some(7));
    }

    #[test]
    fn exit_marker_wrong_token_is_not_stripped() {
        let input = "one\n__RA_TMUX_EXIT:othertoken:__7__\ntwo\n";
        let (text, code) = strip_exit_marker(input, "mytoken");

        assert_eq!(text, input);
        assert_eq!(code, None);
    }

    #[test]
    fn wait_script_shell_quotes_command() {
        let script = wait_script("printf 'hi'; exit 7", "token");

        assert!(script.contains("/bin/sh -c 'printf '\\''hi'\\''; exit 7' 2>&1"));
        assert!(script.contains("__RA_TMUX_EXIT:token:__"));
        assert!(script.contains("tmux wait-for -S token"));
    }
}
