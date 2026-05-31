//! Reverse-direction A2A: wrap a remote A2A agent as a Ra `Tool`.
//!
//! Discovery: at startup the agent registry parses
//! `RA_A2A_AGENTS=name1=url1,name2=url2`, fetches each
//! `<url>/.well-known/agent-card.json`, and builds an `A2aTool` per
//! entry. Each tool is then injected into the SharedState alongside
//! `ReadTool` and `BashTool`, so the LLM sees them in its function-
//! calling schema and may decide to delegate work.
//!
//! Wire: we prefer JSON-RPC over HTTP for simplicity, but the
//! `A2AClientFactory` will pick whichever binding from the agent
//! card we have a transport factory registered for.

use crate::tool_ctx::ToolCtx;
use crate::tools::Tool;
use a2a::{
    AgentCard, Message as A2aMessage, Part, PartContent, Role, SendMessageRequest,
    SendMessageResponse, TRANSPORT_PROTOCOL_HTTP_JSON, TRANSPORT_PROTOCOL_JSONRPC,
};
use a2a_client::agent_card::AgentCardResolver;
use a2a_client::jsonrpc::JsonRpcTransportFactory;
use a2a_client::rest::RestTransportFactory;
use a2a_client::{A2AClient, A2AClientFactory, Transport};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;
use ulid::Ulid;

/// One configured remote A2A agent.
pub struct A2aTool {
    name: String,
    description: String,
    /// Cached A2A client. Built once at registration time so we don't
    /// re-resolve the agent card on every tool call.
    client: Arc<A2AClient<Box<dyn Transport>>>,
}

#[async_trait]
impl Tool for A2aTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> serde_json::Value {
        // Single 'prompt' string parameter — the LLM sends free-form
        // text, the remote agent figures out the rest.
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Free-form prompt to send to the remote A2A agent."
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        })
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope(&self.name);
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .context("missing 'prompt' in A2aTool input")?
            .to_string();

        let req = SendMessageRequest {
            message: A2aMessage::new(Role::User, vec![Part::text(prompt)]),
            configuration: None,
            metadata: None,
            tenant: None,
        };
        let resp = self
            .client
            .send_message(&req)
            .await
            .with_context(|| format!("a2a send_message → {}", self.name))?;

        Ok(extract_text(&resp))
    }
}

/// Pull all text parts out of a SendMessageResponse, joining with newlines.
fn extract_text(resp: &SendMessageResponse) -> String {
    let parts = match resp {
        SendMessageResponse::Task(t) => t.status.message.as_ref().map(|m| &m.parts[..]),
        SendMessageResponse::Message(m) => Some(&m.parts[..]),
    };
    let Some(parts) = parts else { return String::new() };
    let mut out = String::new();
    for p in parts {
        if let PartContent::Text(t) = &p.content {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(t);
        }
    }
    out
}

/// Discover the agents named in `RA_A2A_AGENTS=name=url,…` and return one
/// `A2aTool` per reachable entry. Failures are logged and skipped — a
/// dead remote agent should not prevent Ra from booting.
pub async fn load_remote_tools_from_env() -> Vec<Arc<dyn Tool>> {
    let raw = match std::env::var("RA_A2A_AGENTS") {
        Ok(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };
    load_remote_tools_from_env_string(&raw).await
}

/// Same as `load_remote_tools_from_env` but takes the agent list inline,
/// for callers (config loader) that have already parsed it elsewhere.
/// Format: `name1=url1,name2=url2`.
pub async fn load_remote_tools_from_env_string(raw: &str) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for entry in raw.split(',') {
        if entry.trim().is_empty() {
            continue;
        }
        let Some((name, url)) = entry.split_once('=') else {
            eprintln!("[ra::a2a-tool] skipping malformed entry: {entry}");
            continue;
        };
        match build_tool(name.trim(), url.trim()).await {
            Ok(t) => {
                eprintln!(
                    "[ra::a2a-tool] registered remote agent '{}' → {}",
                    name.trim(),
                    url.trim()
                );
                tools.push(Arc::new(t));
            }
            Err(e) => {
                eprintln!(
                    "[ra::a2a-tool] skipping '{}' ({}) — {e:#}",
                    name.trim(),
                    url.trim()
                );
            }
        }
    }
    tools
}

async fn build_tool(name: &str, url: &str) -> Result<A2aTool> {
    // Sanitize tool name for LLM function-calling: must match
    // [a-zA-Z0-9_-]+ on most providers. Slugify naively.
    let tool_name = format!(
        "a2a_{}",
        name.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            })
            .collect::<String>()
    );

    let resolver = AgentCardResolver::new(None);
    let card: AgentCard = resolver
        .resolve(url)
        .await
        .with_context(|| format!("resolve agent card at {url}"))?;
    let description = build_description(&card);

    let factory = A2AClientFactory::builder()
        .register(Arc::new(JsonRpcTransportFactory::new(None)))
        .register(Arc::new(RestTransportFactory::new(None)))
        .preferred_bindings(vec![
            TRANSPORT_PROTOCOL_JSONRPC.to_string(),
            TRANSPORT_PROTOCOL_HTTP_JSON.to_string(),
        ])
        .build();

    let client = factory
        .create_from_card(&card)
        .await
        .with_context(|| format!("build a2a client from card for {name}"))?;

    Ok(A2aTool {
        name: tool_name,
        description,
        client: Arc::new(client),
    })
}

fn build_description(card: &AgentCard) -> String {
    let mut buf = format!(
        "Remote A2A agent '{}' (v{}). {}",
        card.name, card.version, card.description
    );
    if !card.skills.is_empty() {
        buf.push_str(" Skills: ");
        let mut first = true;
        for s in &card.skills {
            if !first {
                buf.push_str(", ");
            }
            first = false;
            buf.push_str(&s.name);
        }
        buf.push('.');
    }
    // Keep the description compact for the LLM; some providers cap at
    // ~1024 chars.
    if buf.len() > 800 {
        let _ = Ulid::new(); // silence unused on the slim path
        buf.truncate(796);
        buf.push_str(" …");
    }
    buf
}
