//! MCP (Model Context Protocol) client integration.
//!
//! Reads `[[mcp.servers]]` from the config, connects to each server
//! (stdio child-process or streamable-HTTP), enumerates its tools, and
//! wraps each one as a Ra `Tool`. The wrapped tools join the agent's
//! tool list alongside the built-in `read` / `bash` / `a2a_*`.
//!
//! Failures are isolated: an unreachable MCP server logs a warning and
//! is skipped; the rest of the agent boots normally.

use crate::tool_ctx::ToolCtx;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, RawContent};
use rmcp::service::RunningService;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use std::sync::Arc;
use tokio::process::Command;

/// One mounted remote MCP tool. Holds an `Arc` to the shared
/// `RunningService` so a single MCP session can back many tool
/// instances (one per advertised tool).
struct McpTool {
    /// Slugged name visible to the LLM (e.g. `mcp__fs__read_file`).
    name: String,
    description: String,
    /// JSON Schema for the tool's params (forwarded verbatim from the
    /// server's `inputSchema`).
    schema: serde_json::Value,
    /// Original tool name as declared by the MCP server. We send this
    /// (not the slugged Ra-side name) when calling the tool.
    remote_name: String,
    /// Shared MCP session. Cheap to clone (Arc).
    service: Arc<RunningService<RoleClient, ()>>,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope(&self.name);
        let arguments = match input {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => {
                anyhow::bail!("MCP tool '{}' expects object args, got {other}", self.name)
            }
        };
        let mut req = CallToolRequestParams::default();
        req.name = self.remote_name.clone().into();
        req.arguments = arguments;
        let result = self
            .service
            .peer()
            .call_tool(req)
            .await
            .with_context(|| format!("MCP call_tool {}", self.remote_name))?;

        // Concatenate every text part into one output string.
        let mut out = String::new();
        for part in &result.content {
            if let RawContent::Text(t) = &part.raw {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&t.text);
            }
        }
        if let Some(structured) = &result.structured_content {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&structured.to_string());
        }
        if result.is_error.unwrap_or(false) {
            anyhow::bail!("MCP tool '{}' returned error: {out}", self.remote_name);
        }
        Ok(out)
    }
}

/// Connect to every server in the config, query its tools, and return
/// one `Arc<dyn Tool>` per advertised tool. Idempotent: failures
/// produce a stderr warning and the entry is dropped.
pub async fn load_mcp_tools(servers: &[crate::config::McpServer]) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for s in servers {
        match connect_and_list(s).await {
            Ok(loaded) => {
                eprintln!(
                    "[ra::mcp] {} (transport={}): {} tool(s) registered",
                    s.name,
                    s.transport,
                    loaded.len()
                );
                tools.extend(loaded);
            }
            Err(e) => {
                eprintln!("[ra::mcp] skipping server '{}': {e:#}", s.name);
            }
        }
    }
    tools
}

async fn connect_and_list(s: &crate::config::McpServer) -> Result<Vec<Arc<dyn Tool>>> {
    let service = match s.transport.as_str() {
        "stdio" => connect_stdio(s).await?,
        "http" | "streamable-http" => connect_http(s).await?,
        other => anyhow::bail!("unsupported MCP transport '{other}'"),
    };
    let service = Arc::new(service);
    let tools = service
        .peer()
        .list_all_tools()
        .await
        .context("MCP list_tools")?;
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    for t in tools {
        out.push(Arc::new(McpTool {
            name: format!("mcp__{}__{}", slug(&s.name), slug(&t.name)),
            description: t
                .description
                .as_ref()
                .map(|c| c.as_ref().to_string())
                .unwrap_or_default(),
            schema: serde_json::to_value(&*t.input_schema).unwrap_or(serde_json::json!({})),
            remote_name: t.name.to_string(),
            service: service.clone(),
        }));
    }
    Ok(out)
}

async fn connect_stdio(s: &crate::config::McpServer) -> Result<RunningService<RoleClient, ()>> {
    let cmd_str = s
        .command
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("stdio transport requires `command`"))?;
    let mut cmd = Command::new(cmd_str);
    cmd.args(&s.args);
    if let Some(cwd) = &s.cwd {
        cmd.current_dir(shellexpand::tilde(cwd).as_ref());
    }
    for (k, v) in &s.env {
        cmd.env(k, v);
    }
    let transport = TokioChildProcess::new(cmd).context("spawn MCP child process")?;
    let service = ().serve(transport).await.context("MCP stdio handshake")?;
    Ok(service)
}

async fn connect_http(s: &crate::config::McpServer) -> Result<RunningService<RoleClient, ()>> {
    let url = s
        .url
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("http transport requires `url`"))?;
    let transport = StreamableHttpClientTransport::from_uri(url.as_str());
    let service = ().serve(transport).await.context("MCP http handshake")?;
    Ok(service)
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
