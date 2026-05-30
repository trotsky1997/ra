use crate::events::Event;
use crate::tool_ctx::ToolCtx;
use anyhow::{Context, Result};
use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;

/// Unified tool interface. `dyn`-safe: no associated types, params travel as
/// `serde_json::Value` and the tool deserializes them.
///
/// In practice each tool defines its own `Params` struct with
/// `#[derive(JsonSchema, Deserialize)]`, then `schema()` returns
/// `schema_for!(Params)` so the LLM knows the call shape.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    /// JSON Schema describing the input parameters. Forwarded verbatim to the
    /// model layer for function-calling registration.
    fn schema(&self) -> serde_json::Value;

    /// Execute the tool. `ctx` carries the broadcast channel for streaming
    /// updates and an optional `ClientHandle` for ACP reverse calls.
    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String>;
}

// ---------- ReadTool ----------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// File path to read (absolute, or relative to the agent's cwd).
    pub path: String,
}

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read a file from the filesystem."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(ReadParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("read");
        let params: ReadParams =
            serde_json::from_value(input).context("invalid params for read")?;

        // Prefer host-provided fs (ACP `fs/read_text_file`); fall back to
        // local filesystem if the host does not implement it.
        if let (Some(client), Some(sid)) = (&ctx.client, &ctx.session_id) {
            match client
                .fs_read_text_file(sid, &params.path, None, None)
                .await
            {
                Ok(text) => return Ok(text),
                Err(e) => {
                    eprintln!(
                        "[ra::tools::read] reverse fs/read_text_file failed: {e:#}; \
                         falling back to local read"
                    );
                }
            }
        }

        let bytes = tokio::fs::read(&params.path)
            .await
            .with_context(|| format!("read {}", params.path))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

// ---------- BashTool ----------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BashParams {
    /// Shell command to execute via /bin/sh -c.
    pub command: String,
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Run a shell command and return its combined stdout/stderr."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(BashParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("bash");
        let params: BashParams =
            serde_json::from_value(input).context("invalid params for bash")?;

        // ACP path: ask permission, then run via host terminal. If the host
        // doesn't implement permission OR terminal, fall through to local.
        if let (Some(client), Some(sid)) = (&ctx.client, &ctx.session_id) {
            // 1. permission gate
            let perm = client
                .request_permission(
                    sid,
                    call_id,
                    &format!("Run shell: {}", truncate(&params.command, 60)),
                    "Agent wants to run a shell command via the host terminal.",
                )
                .await;

            match perm {
                Ok(crate::tool_ctx::PermissionOutcome::Denied) => {
                    return Err(anyhow::anyhow!("user denied bash invocation"));
                }
                Ok(crate::tool_ctx::PermissionOutcome::Cancelled) => {
                    return Err(anyhow::anyhow!("permission request cancelled"));
                }
                Ok(crate::tool_ctx::PermissionOutcome::Allowed) => {
                    // 2. host terminal
                    match client.run_terminal(sid, &params.command).await {
                        Ok(res) => {
                            let _ = ctx.events.send(Event::ToolCallUpdate {
                                id: call_id.to_string(),
                                chunk: format!("[exit={}]", res.exit_code.unwrap_or(-1)),
                            });
                            return Ok(res.output);
                        }
                        Err(e) => {
                            eprintln!(
                                "[ra::tools::bash] reverse terminal/* failed: {e:#}; \
                                 falling back to local /bin/sh"
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[ra::tools::bash] permission request failed: {e:#}; \
                         falling back to local /bin/sh"
                    );
                }
            }
        }

        // Local fallback path.
        let output = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&params.command)
            .output()
            .await
            .with_context(|| format!("spawn `{}`", params.command))?;

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
