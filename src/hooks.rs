//! Lifecycle hooks: spawn external processes at key turn-loop boundaries.
//!
//! Wire format (per Claude Code's de-facto convention; HCP RFC-0002 doesn't
//! pin a protocol):
//!
//!   stdin  ← JSON: { "event": "<kind>", "session_id": "...",
//!                    "tool_name": "...", "input": ..., ... }
//!   stdout → JSON: { "deny": bool, "reason": "...", "transform": "..." }
//!
//! Anything other than a clean `{"deny": false}` (or no JSON at all) is
//! treated as 'allow'. This biases toward availability over strictness:
//! a misconfigured hook should not break the agent.
//!
//! Each hook is matched by regex against:
//!   - Pre/PostToolUse: the tool name
//!   - UserPromptSubmit: the user message text
//!   - AgentEnd: always matches
//!
//! Hooks run synchronously by default with a hard timeout; setting
//! `run_async = true` fires-and-forgets (the response is ignored).

use crate::config::Hook;
use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// One of the four hook event kinds. `&'static str` keys keep the wire
/// stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    AgentEnd,
}

impl HookEvent {
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "pre_tool_use",
            HookEvent::PostToolUse => "post_tool_use",
            HookEvent::UserPromptSubmit => "user_prompt_submit",
            HookEvent::AgentEnd => "agent_end",
        }
    }
}

/// JSON payload sent to the hook's stdin.
#[derive(Debug, Serialize)]
pub struct HookInput<'a> {
    pub event: &'static str,
    pub session_id: Option<&'a str>,
    /// PreToolUse / PostToolUse: the tool that's about to run / just ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<&'a str>,
    /// PreToolUse: tool input. PostToolUse: same input as posted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<&'a serde_json::Value>,
    /// PostToolUse: tool output (text).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<&'a str>,
    /// UserPromptSubmit: the raw user text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_text: Option<&'a str>,
}

/// JSON payload returned by the hook on stdout. All fields optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct HookOutput {
    pub deny: bool,
    pub reason: Option<String>,
    /// PostToolUse only: replace the tool output verbatim with this text.
    pub transform: Option<String>,
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
            timeout: Duration::from_secs_f64(h.timeout_s.max(0.1)),
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
    end: Arc<Vec<CompiledHook>>,
}

impl HookEngine {
    pub fn from_config(cfg: &crate::config::HooksSection) -> Self {
        Self {
            pre: Arc::new(compile(&cfg.pre_tool_use, "pre_tool_use")),
            post: Arc::new(compile(&cfg.post_tool_use, "post_tool_use")),
            submit: Arc::new(compile(&cfg.user_prompt_submit, "user_prompt_submit")),
            end: Arc::new(compile(&cfg.agent_end, "agent_end")),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pre.is_empty() && self.post.is_empty() && self.submit.is_empty() && self.end.is_empty()
    }

    /// Fire every PreToolUse hook whose matcher hits `tool_name`. Returns
    /// `Some(reason)` if any hook denies; otherwise `None` to allow.
    pub async fn pre_tool_use(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Option<String> {
        let payload = HookInput {
            event: HookEvent::PreToolUse.as_wire_str(),
            session_id,
            tool_name: Some(tool_name),
            input: Some(input),
            output: None,
            user_text: None,
        };
        for h in self.pre.iter() {
            if !h.matcher.is_match(tool_name) {
                continue;
            }
            let _scope = crate::nemo_obs::tool_scope("hook:pre_tool_use");
            match invoke(h, &payload).await {
                Ok(out) if out.deny => {
                    return Some(out.reason.unwrap_or_else(|| "denied by hook".into()));
                }
                Ok(_) => {}
                Err(e) => eprintln!("[ra::hooks] pre_tool_use {tool_name}: {e}"),
            }
        }
        None
    }

    /// Fire PostToolUse hooks. Returns the (possibly transformed)
    /// output text. The first hook that returns a `transform` wins
    /// (later hooks see the original output, on purpose: simpler
    /// to reason about).
    pub async fn post_tool_use(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &serde_json::Value,
        output: &str,
    ) -> String {
        let payload = HookInput {
            event: HookEvent::PostToolUse.as_wire_str(),
            session_id,
            tool_name: Some(tool_name),
            input: Some(input),
            output: Some(output),
            user_text: None,
        };
        let mut transformed: Option<String> = None;
        for h in self.post.iter() {
            if !h.matcher.is_match(tool_name) {
                continue;
            }
            let _scope = crate::nemo_obs::tool_scope("hook:post_tool_use");
            match invoke(h, &payload).await {
                Ok(out) => {
                    if transformed.is_none() {
                        if let Some(t) = out.transform {
                            transformed = Some(t);
                        }
                    }
                }
                Err(e) => eprintln!("[ra::hooks] post_tool_use {tool_name}: {e}"),
            }
        }
        transformed.unwrap_or_else(|| output.to_string())
    }

    /// Fire UserPromptSubmit hooks. Returns `Some(reason)` if any hook
    /// denies the prompt.
    pub async fn user_prompt_submit(
        &self,
        session_id: Option<&str>,
        user_text: &str,
    ) -> Option<String> {
        let payload = HookInput {
            event: HookEvent::UserPromptSubmit.as_wire_str(),
            session_id,
            tool_name: None,
            input: None,
            output: None,
            user_text: Some(user_text),
        };
        for h in self.submit.iter() {
            if !h.matcher.is_match(user_text) {
                continue;
            }
            let _scope = crate::nemo_obs::tool_scope("hook:user_prompt_submit");
            match invoke(h, &payload).await {
                Ok(out) if out.deny => {
                    return Some(out.reason.unwrap_or_else(|| "denied by hook".into()));
                }
                Ok(_) => {}
                Err(e) => eprintln!("[ra::hooks] user_prompt_submit: {e}"),
            }
        }
        None
    }

    /// Fire AgentEnd hooks. Best-effort: errors are logged and swallowed.
    pub async fn agent_end(&self, session_id: Option<&str>) {
        if self.end.is_empty() {
            return;
        }
        let payload = HookInput {
            event: HookEvent::AgentEnd.as_wire_str(),
            session_id,
            tool_name: None,
            input: None,
            output: None,
            user_text: None,
        };
        for h in self.end.iter() {
            let _scope = crate::nemo_obs::tool_scope("hook:agent_end");
            if let Err(e) = invoke(h, &payload).await {
                eprintln!("[ra::hooks] agent_end: {e}");
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
async fn invoke<T: Serialize>(hook: &CompiledHook, payload: &T) -> Result<HookOutput> {
    let body = serde_json::to_vec(payload)?;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(&hook.command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn()?;

    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        stdin.write_all(&body).await.ok();
        stdin.shutdown().await.ok();
    }

    if hook.run_async {
        // Detach. We don't read stdout; just drop the child after spawn.
        // The OS will reap it when it exits.
        return Ok(HookOutput::default());
    }

    let waited = tokio::time::timeout(hook.timeout, child.wait_with_output()).await;
    match waited {
        Ok(Ok(out)) => {
            if !out.status.success() {
                eprintln!("[ra::hooks] {} exited with {}", hook.command, out.status);
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
            eprintln!("[ra::hooks] {} timed out after {:?}", hook.command, hook.timeout);
            Ok(HookOutput::default())
        }
    }
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
        let raw = b"audit log: ok\n{\"deny\": false}\n";
        let trimmed = trim_to_json(raw);
        let parsed: HookOutput = serde_json::from_slice(trimmed).unwrap();
        assert!(!parsed.deny);
    }

    #[test]
    fn trim_to_json_passthrough_no_braces() {
        let raw = b"hello world";
        assert_eq!(trim_to_json(raw), raw);
    }
}
