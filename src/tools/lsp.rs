//! Built-in `lsp` tool — wraps the openlsp CLI for language-aware code
//! intelligence (diagnostics, go-to-definition, hover, references, format,
//! analyze) without requiring the user to configure an MCP server.
//!
//! openlsp accepts a JSON command envelope on stdin and writes structured
//! JSON to stdout. Ra spawns it as a one-shot child process per call,
//! matching the pattern used by `git` and `gh`.
//!
//! Binary resolution order:
//!   1. `[openlsp] binary` config override
//!   2. `openlsp` on PATH (via `which`)
//!   3. `bunx openlsp` if `bun` is on PATH

use crate::config::OpenlspSection;
use crate::tool_ctx::ToolCtx;
use crate::tools::core::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

/// Resolved openlsp invocation: binary + any prefix args (e.g. `["bunx",
/// "openlsp"]`).
#[derive(Debug, Clone)]
pub struct ResolvedBinary {
    pub program: String,
    pub prefix_args: Vec<String>,
}

/// Resolve the openlsp binary according to the config and PATH.
/// Returns `None` if no binary can be found.
pub fn resolve_openlsp_binary(cfg: &OpenlspSection) -> Option<ResolvedBinary> {
    if !cfg.enabled {
        return None;
    }

    // 1. Config override.
    if let Some(ref bin) = cfg.binary {
        if which::which(bin).is_ok() || std::path::Path::new(bin).exists() {
            return Some(ResolvedBinary {
                program: bin.clone(),
                prefix_args: vec![],
            });
        }
    }

    // 2. `openlsp` on PATH.
    if which::which("openlsp").is_ok() {
        return Some(ResolvedBinary {
            program: "openlsp".to_string(),
            prefix_args: vec![],
        });
    }

    // 3. `bunx openlsp` if bun is available.
    if which::which("bun").is_ok() {
        return Some(ResolvedBinary {
            program: "bun".to_string(),
            prefix_args: vec!["x".to_string(), "openlsp".to_string()],
        });
    }

    None
}

/// Input schema for the `lsp` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LspParams {
    /// The openlsp operation to run. Examples: `"lsp"`, `"format"`,
    /// `"analyze"`, `"capabilities"`, `"config"`, `"session-close"`.
    pub operation: String,
    /// Sub-command for `lsp` operations (e.g. `"diagnostics"`,
    /// `"goToDefinition"`, `"hover"`, `"references"`).
    #[serde(default)]
    pub sub_command: Option<String>,
    /// File path for file-scoped operations.
    #[serde(default)]
    pub file: Option<String>,
    /// Line number (1-based) for position-scoped operations.
    #[serde(default)]
    pub line: Option<u32>,
    /// Character offset (1-based) for position-scoped operations.
    #[serde(default)]
    pub character: Option<u32>,
    /// Extra fields forwarded verbatim into the openlsp JSON envelope.
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

pub struct LspTool {
    pub binary: ResolvedBinary,
    pub workspace_root: Option<String>,
    pub timeout_secs: f64,
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        "Run an openlsp operation (diagnostics, goToDefinition, hover, \
         references, format, analyze, capabilities) and return structured \
         JSON. Pass `operation` (the openlsp command type) and any \
         additional fields such as `sub_command`, `file`, `line`, \
         `character`. Requires openlsp on PATH or configured via \
         [openlsp] binary in ra.toml."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(LspParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("lsp");
        let params: LspParams =
            serde_json::from_value(input).context("invalid params for lsp")?;

        // Build the openlsp JSON envelope.
        let mut envelope = serde_json::Map::new();
        envelope.insert("command".to_string(), params.operation.clone().into());
        if let Some(sc) = &params.sub_command {
            envelope.insert("subCommand".to_string(), sc.clone().into());
        }
        if let Some(f) = &params.file {
            envelope.insert("file".to_string(), f.clone().into());
        }
        if let Some(l) = params.line {
            envelope.insert("line".to_string(), l.into());
        }
        if let Some(c) = params.character {
            envelope.insert("character".to_string(), c.into());
        }
        if let Some(serde_json::Value::Object(extra)) = params.extra {
            for (k, v) in extra {
                envelope.entry(k).or_insert(v);
            }
        }
        let envelope_json = serde_json::to_string(&serde_json::Value::Object(envelope))
            .context("serialize openlsp envelope")?;

        let mut cmd = Command::new(&self.binary.program);
        for arg in &self.binary.prefix_args {
            cmd.arg(arg);
        }
        cmd.arg("--json");
        if let Some(ref root) = self.workspace_root {
            cmd.arg("--workspace-root").arg(root);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let deadline = Duration::from_secs_f64(self.timeout_secs);
        let result = timeout(deadline, async {
            let mut child = cmd.spawn().context("spawn openlsp")?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(envelope_json.as_bytes())
                    .await
                    .context("write openlsp stdin")?;
            }
            child.wait_with_output().await.context("wait openlsp")
        })
        .await
        .map_err(|_| anyhow::anyhow!("openlsp timed out after {:.1}s", self.timeout_secs))??;

        let _ = ctx.events.send(crate::events::Event::ToolCallUpdate {
            id: call_id.to_string(),
            chunk: format!("[exit={}]", result.status.code().unwrap_or(-1)),
        });

        let mut out = String::from_utf8_lossy(&result.stdout).into_owned();
        if !result.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&stderr);
        }

        if !result.status.success() && out.trim().is_empty() {
            anyhow::bail!(
                "openlsp exited with status {}",
                result.status.code().unwrap_or(-1)
            );
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OpenlspSection;

    #[test]
    fn resolve_skips_when_disabled() {
        let cfg = OpenlspSection {
            enabled: false,
            binary: Some("true".to_string()), // `true` is always on PATH
            workspace_root: None,
            timeout: 30.0,
        };
        assert!(resolve_openlsp_binary(&cfg).is_none());
    }

    #[test]
    fn resolve_uses_config_override_when_binary_exists() {
        // `true` is a POSIX utility guaranteed to be on PATH.
        let cfg = OpenlspSection {
            enabled: true,
            binary: Some("true".to_string()),
            workspace_root: None,
            timeout: 30.0,
        };
        let resolved = resolve_openlsp_binary(&cfg).expect("should resolve");
        assert_eq!(resolved.program, "true");
        assert!(resolved.prefix_args.is_empty());
    }

    #[test]
    fn resolve_falls_back_to_bunx_when_openlsp_absent() {
        // Only meaningful when openlsp is NOT on PATH but bun IS.
        // We can't guarantee the test environment, so just verify the
        // function returns Some or None without panicking.
        let cfg = OpenlspSection {
            enabled: true,
            binary: None,
            workspace_root: None,
            timeout: 30.0,
        };
        // Just assert it doesn't panic.
        let _ = resolve_openlsp_binary(&cfg);
    }
}
