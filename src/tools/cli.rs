//! Native CLI wrappers for common developer commands.
//!
//! These tools keep `git` and GitHub CLI calls out of the generic shell tool
//! when Ra can execute them locally. The local path uses `Command::arg` for an
//! argv-safe process spawn. ACP hosts currently expose only a terminal-string
//! reverse call, so the host path renders a shell-quoted command line and then
//! reuses the same permission/terminal flow as `bash`.

use crate::events::Event;
use crate::tool_ctx::ToolCtx;
use crate::tools::core::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use tokio::process::Command;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NativeCliParams {
    /// Arguments passed to the command, excluding the binary name.
    ///
    /// Example: `{ "args": ["status", "--short"] }` runs
    /// `git status --short`.
    #[serde(default)]
    pub args: Vec<String>,
}

pub struct GitTool;

pub struct GhTool;

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Run the native git CLI with an argv array. Prefer this over `bash` \
         for git subcommands; pass only arguments, not the `git` binary."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(NativeCliParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        execute_native_cli("git", call_id, input, ctx).await
    }
}

#[async_trait]
impl Tool for GhTool {
    fn name(&self) -> &str {
        "gh"
    }

    fn description(&self) -> &str {
        "Run the native GitHub CLI (`gh`) with an argv array. Prefer this \
         over `bash` for GitHub issue, PR, repo, and auth commands; pass \
         only arguments, not the `gh` binary."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(NativeCliParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        execute_native_cli("gh", call_id, input, ctx).await
    }
}

async fn execute_native_cli(
    binary: &'static str,
    call_id: &str,
    input: serde_json::Value,
    ctx: &ToolCtx,
) -> Result<String> {
    let _scope = crate::nemo_obs::tool_scope(binary);
    let params: NativeCliParams =
        serde_json::from_value(input).with_context(|| format!("invalid params for {binary}"))?;
    let command_line = render_command_line(binary, &params.args);

    if let (Some(client), Some(sid)) = (&ctx.client, &ctx.session_id) {
        let perm = client
            .request_permission(
                sid,
                call_id,
                &format!("Run {binary}: {}", truncate(&command_line, 60)),
                &format!("Agent wants to run `{binary}` via the host terminal."),
            )
            .await;

        match perm {
            Ok(crate::tool_ctx::PermissionOutcome::Denied) => {
                return Err(anyhow::anyhow!("user denied {binary} invocation"));
            }
            Ok(crate::tool_ctx::PermissionOutcome::Cancelled) => {
                return Err(anyhow::anyhow!("permission request cancelled"));
            }
            Ok(crate::tool_ctx::PermissionOutcome::Allowed) => {
                match client.run_terminal(sid, &command_line).await {
                    Ok(res) => {
                        let _ = ctx.events.send(Event::ToolCallUpdate {
                            id: call_id.to_string(),
                            chunk: format!("[exit={}]", res.exit_code.unwrap_or(-1)),
                        });
                        return Ok(res.output);
                    }
                    Err(e) => {
                        eprintln!(
                            "[ra::tools::{binary}] reverse terminal/* failed: {e:#}; \
                             falling back to local process"
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "[ra::tools::{binary}] permission request failed: {e:#}; \
                     falling back to local process"
                );
            }
        }
    }

    let output = Command::new(binary)
        .args(&params.args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .with_context(|| format!("spawn `{command_line}`"))?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[exit={}]", output.status.code().unwrap_or(-1)),
    });

    Ok(combined)
}

fn render_command_line(binary: &str, args: &[String]) -> String {
    std::iter::once(shell_quote(binary))
        .chain(args.iter().map(|arg| shell_quote(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes().all(|b| {
        matches!(
            b,
            b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'_'
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b'='
                | b'+'
                | b','
        )
    }) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_ctx::{ClientHandle, PermissionOutcome, TerminalRunResult};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    struct CapturingClient {
        command: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl ClientHandle for CapturingClient {
        async fn fs_read_text_file(
            &self,
            _session_id: &str,
            _path: &str,
            _line: Option<u32>,
            _limit: Option<u32>,
        ) -> Result<String> {
            anyhow::bail!("not implemented")
        }

        async fn fs_write_text_file(
            &self,
            _session_id: &str,
            _path: &str,
            _content: &str,
        ) -> Result<()> {
            anyhow::bail!("not implemented")
        }

        async fn run_terminal(
            &self,
            _session_id: &str,
            command: &str,
        ) -> Result<TerminalRunResult> {
            *self.command.lock().unwrap() = Some(command.to_string());
            Ok(TerminalRunResult {
                exit_code: Some(0),
                output: "ok\n".to_string(),
            })
        }

        async fn request_permission(
            &self,
            _session_id: &str,
            _tool_call_id: &str,
            _title: &str,
            _description: &str,
        ) -> Result<PermissionOutcome> {
            Ok(PermissionOutcome::Allowed)
        }
    }

    #[test]
    fn command_line_quotes_shell_metacharacters_for_host_terminal() {
        let args = vec![
            "status".to_string(),
            "--short".to_string(),
            "weird 'path'.txt".to_string(),
            "semi;colon".to_string(),
        ];

        assert_eq!(
            render_command_line("git", &args),
            "git status --short 'weird '\\''path'\\''.txt' 'semi;colon'"
        );
    }

    #[tokio::test]
    async fn git_tool_uses_host_terminal_when_client_is_available() {
        let (events, _) = tokio::sync::broadcast::channel(16);
        let command = Arc::new(Mutex::new(None));
        let mut ctx = ToolCtx::local(events);
        ctx.client = Some(Arc::new(CapturingClient {
            command: command.clone(),
        }));
        ctx.session_id = Some("session-1".to_string());

        let out = GitTool
            .execute(
                "call-git",
                serde_json::json!({
                    "args": ["status", "--short", "weird 'path'.txt"]
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out, "ok\n");
        assert_eq!(
            command.lock().unwrap().as_deref(),
            Some("git status --short 'weird '\\''path'\\''.txt'")
        );
    }

    #[tokio::test]
    async fn git_tool_runs_local_process_with_argv() {
        let (events, _) = tokio::sync::broadcast::channel(16);
        let ctx = ToolCtx::local(events);

        let out = GitTool
            .execute(
                "call-git",
                serde_json::json!({ "args": ["--version"] }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(out.starts_with("git version"), "out={out:?}");
    }
}
