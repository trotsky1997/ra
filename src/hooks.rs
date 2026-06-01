//! Lifecycle hooks: spawn external processes at key turn-loop boundaries.
//!
//! Wire format follows Claude Code's hooks spec
//! (<https://code.claude.com/docs/en/hooks.md>) so existing Claude Code
//! hook scripts run unmodified.
//!
//! On stdin the hook receives the common Claude Code payload:
//! ```json
//! {
//!   "session_id": "...",
//!   "cwd": "...",
//!   "transcript_path": "...",
//!   "hook_event_name": "PreToolUse" | "PostToolUse"
//!                    | "UserPromptSubmit" | "Stop",
//!   "tool_name":   "...",         // tool events only
//!   "tool_input":  { ... },       // tool events only
//!   "tool_response": "...",       // PostToolUse only
//!   "prompt": "..."               // UserPromptSubmit only
//! }
//! ```
//!
//! On stdout the hook may print JSON. We honour the subset that maps onto
//! Ra's existing turn loop:
//!   - `continue: false` + `stopReason` — universal kill switch
//!   - `decision: "block"` + `reason`   — block the current event
//!     (UserPromptSubmit, PostToolUse, Stop)
//!   - `hookSpecificOutput.permissionDecision: "deny"` (PreToolUse)
//!   - `hookSpecificOutput.additionalContext` — extra system reminder
//!     injected next to the tool result / prompt
//!   - `systemMessage`, `suppressOutput` — operator-visible only
//!
//! Exit codes:
//!   - 0     → parse stdout JSON
//!   - 2     → block (stderr fed back as reason)
//!   - other → non-blocking error (stderr logged)
//!
//! Each hook is matched by regex against:
//!   - PreToolUse / PostToolUse: the tool name
//!   - UserPromptSubmit: the user message text
//!   - Stop: always matches
//!
//! `run_async = true` (or TOML `async = true`) fires-and-forgets the hook.

use crate::config::Hook;
use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// One of the four hook event kinds we currently fire. Names match the
/// Claude Code spec verbatim — they appear on the wire as
/// `hook_event_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    Stop,
}

impl HookEvent {
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::Stop => "Stop",
        }
    }
}

/// JSON payload sent to the hook's stdin. Mirrors Claude Code's common
/// fields; `serde(skip_serializing_if = "Option::is_none")` keeps the
/// shape per-event-tight without us having to model 4 distinct structs.
#[derive(Debug, Serialize)]
pub struct HookInput<'a> {
    pub hook_event_name: &'static str,
    pub session_id: Option<&'a str>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// PreToolUse / PostToolUse: the tool that's about to run / just ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<&'a str>,
    /// PreToolUse / PostToolUse: tool input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<&'a serde_json::Value>,
    /// PostToolUse: tool output (text).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<&'a str>,
    /// UserPromptSubmit: the raw user text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<&'a str>,
}

/// JSON payload returned by the hook on stdout. All fields optional;
/// fields we don't yet route still parse so older/richer scripts don't
/// fail to deserialize.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct HookOutput {
    /// `false` ⇒ stop the entire turn (universal kill).
    #[serde(rename = "continue", default = "default_true_bool")]
    pub r#continue: bool,
    /// User-visible message when `continue=false`.
    #[serde(rename = "stopReason")]
    pub stop_reason: Option<String>,
    /// Operator-visible warning shown to the user.
    #[serde(rename = "systemMessage")]
    pub system_message: Option<String>,
    /// `"block"` blocks event-appropriately. Other values ignored.
    pub decision: Option<String>,
    /// Reason text for `decision=block`.
    pub reason: Option<String>,
    /// Hide stdout from the operator transcript.
    #[serde(rename = "suppressOutput")]
    pub suppress_output: bool,
    /// Per-event richer control (PreToolUse permission decision,
    /// additionalContext, …).
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: Option<HookSpecificOutput>,
}

fn default_true_bool() -> bool {
    true
}

/// `hookSpecificOutput` block. We only deserialize the fields we route.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: Option<String>,
    /// PreToolUse: `"allow" | "deny" | "ask"`.
    #[serde(rename = "permissionDecision")]
    pub permission_decision: Option<String>,
    #[serde(rename = "permissionDecisionReason")]
    pub permission_decision_reason: Option<String>,
    /// Extra context injected next to the tool result / prompt.
    #[serde(rename = "additionalContext")]
    pub additional_context: Option<String>,
}

/// Decision returned to the caller after running a hook chain.
#[derive(Debug, Default, Clone)]
pub struct HookDecision {
    /// Some(reason) ⇒ block this event. None ⇒ allow.
    pub block: Option<String>,
    /// Some(text) ⇒ extra system reminder to splice into the model
    /// context next to this event.
    pub additional_context: Option<String>,
    /// Some(reason) ⇒ stop the whole turn (universal `continue:false`).
    pub stop: Option<String>,
}

impl HookDecision {
    pub fn allow() -> Self {
        Self::default()
    }
}

/// One compiled hook entry. Pre-compiles the regex so runtime matching
/// is cheap.
#[derive(Debug, Clone)]
pub struct CompiledHook {
    pub matcher: Regex,
    pub command: String,
    pub timeout: Duration,
    pub run_async: bool,
}

impl CompiledHook {
    fn from_config(h: &Hook) -> Result<Self> {
        let matcher = Regex::new(&h.matcher)
            .map_err(|e| anyhow::anyhow!("invalid hook regex {:?}: {e}", h.matcher))?;
        Ok(Self {
            matcher,
            command: h.command.clone(),
            timeout: Duration::from_secs_f64(h.timeout.max(0.1)),
            run_async: h.run_async,
        })
    }
}

/// Runtime-side registry of all hooks. Cheap to clone (Arc'd vectors).
#[derive(Debug, Default, Clone)]
pub struct HookEngine {
    pre: Arc<Vec<CompiledHook>>,
    post: Arc<Vec<CompiledHook>>,
    submit: Arc<Vec<CompiledHook>>,
    stop: Arc<Vec<CompiledHook>>,
}

impl HookEngine {
    pub fn from_config(cfg: &crate::config::HooksSection) -> Self {
        Self {
            pre: Arc::new(compile(&cfg.pre_tool_use, "PreToolUse")),
            post: Arc::new(compile(&cfg.post_tool_use, "PostToolUse")),
            submit: Arc::new(compile(&cfg.user_prompt_submit, "UserPromptSubmit")),
            stop: Arc::new(compile(&cfg.stop, "Stop")),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pre.is_empty()
            && self.post.is_empty()
            && self.submit.is_empty()
            && self.stop.is_empty()
    }

    pub fn merged_with(&self, other: &HookEngine) -> HookEngine {
        HookEngine {
            pre: Arc::new(self.pre.iter().chain(other.pre.iter()).cloned().collect()),
            post: Arc::new(self.post.iter().chain(other.post.iter()).cloned().collect()),
            submit: Arc::new(
                self.submit
                    .iter()
                    .chain(other.submit.iter())
                    .cloned()
                    .collect(),
            ),
            stop: Arc::new(self.stop.iter().chain(other.stop.iter()).cloned().collect()),
        }
    }

    /// Fire every PreToolUse hook whose matcher hits `tool_name`. Returns
    /// the merged decision (block / additional_context / stop).
    pub async fn pre_tool_use(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> HookDecision {
        let payload = HookInput {
            hook_event_name: HookEvent::PreToolUse.as_wire_str(),
            session_id,
            cwd: cwd_string(),
            transcript_path: None,
            tool_name: Some(tool_name),
            tool_input: Some(input),
            tool_response: None,
            prompt: None,
        };
        let mut decision = HookDecision::allow();
        for h in self.pre.iter() {
            if !h.matcher.is_match(tool_name) {
                continue;
            }
            let _scope = crate::nemo_obs::tool_scope("hook:PreToolUse");
            match invoke(h, &payload, HookEvent::PreToolUse).await {
                Ok(out) => {
                    merge_decision(&mut decision, out, HookEvent::PreToolUse);
                    if decision.block.is_some() || decision.stop.is_some() {
                        break;
                    }
                }
                Err(e) => eprintln!("[ra::hooks] PreToolUse {tool_name}: {e}"),
            }
        }
        decision
    }

    /// Fire PostToolUse hooks. Decision may carry `block` (top-level
    /// `decision:"block"`) or `additional_context` to splice in.
    pub async fn post_tool_use(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &serde_json::Value,
        output: &str,
    ) -> HookDecision {
        let payload = HookInput {
            hook_event_name: HookEvent::PostToolUse.as_wire_str(),
            session_id,
            cwd: cwd_string(),
            transcript_path: None,
            tool_name: Some(tool_name),
            tool_input: Some(input),
            tool_response: Some(output),
            prompt: None,
        };
        let mut decision = HookDecision::allow();
        for h in self.post.iter() {
            if !h.matcher.is_match(tool_name) {
                continue;
            }
            let _scope = crate::nemo_obs::tool_scope("hook:PostToolUse");
            match invoke(h, &payload, HookEvent::PostToolUse).await {
                Ok(out) => merge_decision(&mut decision, out, HookEvent::PostToolUse),
                Err(e) => eprintln!("[ra::hooks] PostToolUse {tool_name}: {e}"),
            }
        }
        decision
    }

    /// Fire UserPromptSubmit hooks. Block ⇒ refuse the prompt;
    /// additional_context ⇒ append to the user's message.
    pub async fn user_prompt_submit(
        &self,
        session_id: Option<&str>,
        user_text: &str,
    ) -> HookDecision {
        let payload = HookInput {
            hook_event_name: HookEvent::UserPromptSubmit.as_wire_str(),
            session_id,
            cwd: cwd_string(),
            transcript_path: None,
            tool_name: None,
            tool_input: None,
            tool_response: None,
            prompt: Some(user_text),
        };
        let mut decision = HookDecision::allow();
        for h in self.submit.iter() {
            if !h.matcher.is_match(user_text) {
                continue;
            }
            let _scope = crate::nemo_obs::tool_scope("hook:UserPromptSubmit");
            match invoke(h, &payload, HookEvent::UserPromptSubmit).await {
                Ok(out) => {
                    merge_decision(&mut decision, out, HookEvent::UserPromptSubmit);
                    if decision.block.is_some() || decision.stop.is_some() {
                        break;
                    }
                }
                Err(e) => eprintln!("[ra::hooks] UserPromptSubmit: {e}"),
            }
        }
        decision
    }

    /// Fire Stop hooks (legacy `agent_end`). Best-effort: errors logged.
    /// Returned decision is informational; we don't currently honour
    /// `block` on Stop because Ra's post-turn loop is single-shot.
    pub async fn stop(&self, session_id: Option<&str>) -> HookDecision {
        if self.stop.is_empty() {
            return HookDecision::allow();
        }
        let payload = HookInput {
            hook_event_name: HookEvent::Stop.as_wire_str(),
            session_id,
            cwd: cwd_string(),
            transcript_path: None,
            tool_name: None,
            tool_input: None,
            tool_response: None,
            prompt: None,
        };
        let mut decision = HookDecision::allow();
        for h in self.stop.iter() {
            let _scope = crate::nemo_obs::tool_scope("hook:Stop");
            match invoke(h, &payload, HookEvent::Stop).await {
                Ok(out) => merge_decision(&mut decision, out, HookEvent::Stop),
                Err(e) => eprintln!("[ra::hooks] Stop: {e}"),
            }
        }
        decision
    }
}

fn cwd_string() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// Merge one hook's parsed output into the running decision for an event.
fn merge_decision(acc: &mut HookDecision, out: HookOutput, ev: HookEvent) {
    if let Some(msg) = out.system_message.as_deref() {
        if !msg.is_empty() {
            eprintln!("[ra::hooks] {ev:?}: {msg}");
        }
    }
    if !out.r#continue {
        let why = out.stop_reason.unwrap_or_else(|| "stopped by hook".into());
        if acc.stop.is_none() {
            acc.stop = Some(why);
        }
        return;
    }
    // Top-level block decision (UserPromptSubmit / PostToolUse / Stop).
    if matches!(out.decision.as_deref(), Some("block")) && acc.block.is_none() {
        acc.block = Some(out.reason.unwrap_or_else(|| "blocked by hook".into()));
    }
    // PreToolUse uses hookSpecificOutput.permissionDecision.
    if let Some(hs) = out.hook_specific_output {
        if let Some(extra) = hs.additional_context {
            if !extra.is_empty() {
                let merged = match &acc.additional_context {
                    Some(prev) => format!("{prev}\n{extra}"),
                    None => extra,
                };
                acc.additional_context = Some(merged);
            }
        }
        if matches!(ev, HookEvent::PreToolUse) {
            if let Some(pd) = hs.permission_decision.as_deref() {
                if pd == "deny" && acc.block.is_none() {
                    acc.block = Some(
                        hs.permission_decision_reason
                            .unwrap_or_else(|| "denied by hook".into()),
                    );
                }
            }
        }
    }
}

fn compile(hooks: &[Hook], label: &str) -> Vec<CompiledHook> {
    hooks
        .iter()
        .filter_map(|h| match CompiledHook::from_config(h) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("[ra::hooks] skipping malformed {label} hook: {e}");
                None
            }
        })
        .collect()
}

/// Spawn one hook process, write payload to stdin, read JSON from stdout,
/// honour timeout. `run_async` fires and immediately returns Ok(default).
async fn invoke<T: Serialize>(
    hook: &CompiledHook,
    payload: &T,
    event: HookEvent,
) -> Result<HookOutput> {
    let body = serde_json::to_vec(payload)?;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(&hook.command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;

    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        stdin.write_all(&body).await.ok();
        stdin.shutdown().await.ok();
    }

    if hook.run_async {
        // Detach. We don't read stdout; just drop the child after spawn.
        return Ok(HookOutput::default());
    }

    let waited = tokio::time::timeout(hook.timeout, child.wait_with_output()).await;
    match waited {
        Ok(Ok(out)) => {
            let stderr_text = String::from_utf8_lossy(&out.stderr).into_owned();
            // Exit code 2 = block. Stderr becomes the reason.
            if let Some(2) = out.status.code() {
                let reason = stderr_text.trim().to_string();
                let reason = if reason.is_empty() {
                    "blocked by hook (exit 2)".to_string()
                } else {
                    reason
                };
                return Ok(synth_block(event, reason));
            }
            if !out.status.success() {
                eprintln!(
                    "[ra::hooks] {} exited with {}{}",
                    hook.command,
                    out.status,
                    if stderr_text.trim().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", stderr_text.trim())
                    }
                );
            }
            let stdout = out.stdout;
            if stdout.is_empty() {
                return Ok(HookOutput::default());
            }
            // Tolerate hooks that print log lines before/after the JSON
            // by trying to find the first '{' and last '}'.
            let trimmed = trim_to_json(&stdout);
            match serde_json::from_slice::<HookOutput>(trimmed) {
                Ok(parsed) => Ok(parsed),
                Err(e) => {
                    eprintln!(
                        "[ra::hooks] non-JSON or invalid hook output ({} bytes): {e}",
                        stdout.len()
                    );
                    Ok(HookOutput::default())
                }
            }
        }
        Ok(Err(e)) => Err(anyhow::anyhow!("wait_with_output: {e}")),
        Err(_elapsed) => {
            // timeout fired
            // We can't kill `child` here because wait_with_output moved it;
            // just return the warning. The OS will still reap when it exits.
            eprintln!(
                "[ra::hooks] {} timed out after {:?}",
                hook.command, hook.timeout
            );
            Ok(HookOutput::default())
        }
    }
}

/// Build a HookOutput equivalent to a top-level `decision:"block"` (or
/// PreToolUse `permissionDecision:"deny"`) from a stderr-derived reason.
fn synth_block(event: HookEvent, reason: String) -> HookOutput {
    let mut out = HookOutput {
        r#continue: true,
        ..Default::default()
    };
    match event {
        HookEvent::PreToolUse => {
            out.hook_specific_output = Some(HookSpecificOutput {
                hook_event_name: Some("PreToolUse".to_string()),
                permission_decision: Some("deny".to_string()),
                permission_decision_reason: Some(reason),
                additional_context: None,
            });
        }
        _ => {
            out.decision = Some("block".to_string());
            out.reason = Some(reason);
        }
    }
    out
}

fn trim_to_json(bytes: &[u8]) -> &[u8] {
    let Some(start) = bytes.iter().position(|&b| b == b'{') else {
        return bytes;
    };
    let Some(end) = bytes.iter().rposition(|&b| b == b'}') else {
        return bytes;
    };
    if end >= start {
        &bytes[start..=end]
    } else {
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_to_json_strips_log_lines() {
        let raw = b"audit log: ok\n{\"decision\": \"block\"}\n";
        let trimmed = trim_to_json(raw);
        let parsed: HookOutput = serde_json::from_slice(trimmed).unwrap();
        assert_eq!(parsed.decision.as_deref(), Some("block"));
    }

    #[test]
    fn trim_to_json_passthrough_no_braces() {
        let raw = b"hello world";
        assert_eq!(trim_to_json(raw), raw);
    }

    #[test]
    fn parse_pretooluse_permission_deny() {
        let raw = br#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"nope"}}"#;
        let parsed: HookOutput = serde_json::from_slice(raw).unwrap();
        let mut decision = HookDecision::allow();
        merge_decision(&mut decision, parsed, HookEvent::PreToolUse);
        assert_eq!(decision.block.as_deref(), Some("nope"));
    }

    #[test]
    fn parse_top_level_block() {
        let raw = br#"{"decision":"block","reason":"PII"}"#;
        let parsed: HookOutput = serde_json::from_slice(raw).unwrap();
        let mut decision = HookDecision::allow();
        merge_decision(&mut decision, parsed, HookEvent::UserPromptSubmit);
        assert_eq!(decision.block.as_deref(), Some("PII"));
    }

    #[test]
    fn parse_continue_false_kill_switch() {
        let raw = br#"{"continue":false,"stopReason":"emergency"}"#;
        let parsed: HookOutput = serde_json::from_slice(raw).unwrap();
        let mut decision = HookDecision::allow();
        merge_decision(&mut decision, parsed, HookEvent::PostToolUse);
        assert_eq!(decision.stop.as_deref(), Some("emergency"));
    }

    #[test]
    fn parse_additional_context_merges() {
        let raw1 = br#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"line1"}}"#;
        let raw2 = br#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"line2"}}"#;
        let p1: HookOutput = serde_json::from_slice(raw1).unwrap();
        let p2: HookOutput = serde_json::from_slice(raw2).unwrap();
        let mut decision = HookDecision::allow();
        merge_decision(&mut decision, p1, HookEvent::PostToolUse);
        merge_decision(&mut decision, p2, HookEvent::PostToolUse);
        assert_eq!(decision.additional_context.as_deref(), Some("line1\nline2"));
    }
}
