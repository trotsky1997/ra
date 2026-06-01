//! Built-in `lsp` tool — wraps the openlsp-cli for language-aware code
//! intelligence (diagnostics, go-to-definition, hover, references, format,
//! analyze) without requiring the user to configure an MCP server.
//!
//! openlsp-cli is argv-based: each invocation is a subcommand with flags.
//! Example: `openlsp-cli lsp --operation diagnostics --file-path src/main.rs --json`
//!
//! Binary resolution order:
//!   1. `[openlsp] binary` config override
//!   2. `openlsp-cli` on PATH (canonical published name)
//!   3. `openlsp` on PATH (alias some installs use)
//!   4. `bunx openlsp-cli` if `bun` is on PATH

use crate::config::OpenlspSection;
use crate::tool_ctx::ToolCtx;
use crate::tools::core::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

/// Resolved openlsp-cli invocation: binary + any prefix args (e.g. `["bun",
/// "x", "openlsp-cli"]`).
#[derive(Debug, Clone)]
pub struct ResolvedBinary {
    pub program: String,
    pub prefix_args: Vec<String>,
}

/// Resolve the openlsp-cli binary according to the config and PATH.
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

    // 2. `openlsp-cli` on PATH (canonical published package name).
    if which::which("openlsp-cli").is_ok() {
        return Some(ResolvedBinary {
            program: "openlsp-cli".to_string(),
            prefix_args: vec![],
        });
    }

    // 3. `openlsp` on PATH (alias used by some installs).
    if which::which("openlsp").is_ok() {
        return Some(ResolvedBinary {
            program: "openlsp".to_string(),
            prefix_args: vec![],
        });
    }

    // 4. `bunx openlsp-cli` if bun is available.
    if which::which("bun").is_ok() {
        return Some(ResolvedBinary {
            program: "bun".to_string(),
            prefix_args: vec!["x".to_string(), "openlsp-cli".to_string()],
        });
    }

    None
}

/// Input schema for the `lsp` tool.
///
/// Maps directly to openlsp-cli argv flags. The `command` selects the
/// subcommand (`lsp`, `format`, `analyze`, `capabilities`, `config`,
/// `session-close`). For `lsp`, `operation` selects the LSP method.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LspParams {
    /// The openlsp-cli subcommand. Examples: `"lsp"`, `"format"`,
    /// `"analyze"`, `"capabilities"`, `"config"`, `"session-close"`.
    pub command: String,
    /// LSP operation for the `lsp` subcommand (e.g. `"diagnostics"`,
    /// `"goToDefinition"`, `"hover"`, `"findReferences"`, `"rename"`).
    #[serde(default)]
    pub operation: Option<String>,
    /// File path for file-scoped operations (`--file-path`).
    #[serde(default)]
    pub file_path: Option<String>,
    /// Line number (1-based) for position-scoped operations (`--line`).
    #[serde(default)]
    pub line: Option<u32>,
    /// Character offset (1-based) for position-scoped operations (`--character`).
    #[serde(default)]
    pub character: Option<u32>,
    /// Workspace root override (`--workspace-root`). When unset, the
    /// session cwd is used.
    #[serde(default)]
    pub workspace_root: Option<String>,
    /// Session id for `session-close` (`--session`).
    #[serde(default)]
    pub session_id: Option<String>,
    /// Timeout in milliseconds passed to openlsp-cli (`--timeout`).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

pub struct LspTool {
    pub binary: ResolvedBinary,
    /// Config-level workspace root override. When `None`, the session cwd
    /// from `ToolCtx` is used as the default.
    pub workspace_root: Option<String>,
    pub timeout_secs: f64,
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        "Run an openlsp-cli operation and return structured JSON. \
         Set `command` to the subcommand (`lsp`, `format`, `analyze`, \
         `capabilities`, `config`, `session-close`). For `lsp`, also set \
         `operation` (e.g. `diagnostics`, `goToDefinition`, `hover`, \
         `findReferences`) and `file_path`. Requires openlsp-cli on PATH \
         or configured via [openlsp] binary in ra.toml."
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
        let params: LspParams = serde_json::from_value(input).context("invalid params for lsp")?;

        let mut cmd = Command::new(&self.binary.program);
        for arg in &self.binary.prefix_args {
            cmd.arg(arg);
        }

        // Subcommand.
        cmd.arg(&params.command);

        // LSP operation.
        if let Some(ref op) = params.operation {
            cmd.arg("--operation").arg(op);
        }

        // File path.
        if let Some(ref fp) = params.file_path {
            cmd.arg("--file-path").arg(fp);
        }

        // Position.
        if let Some(l) = params.line {
            cmd.arg("--line").arg(l.to_string());
        }
        if let Some(c) = params.character {
            cmd.arg("--character").arg(c.to_string());
        }

        // Session id.
        if let Some(ref sid) = params.session_id {
            cmd.arg("--session").arg(sid);
        }

        // Timeout.
        if let Some(ms) = params.timeout_ms {
            cmd.arg("--timeout").arg(ms.to_string());
        }

        // Workspace root: param override > config override > session cwd.
        let workspace_root = params
            .workspace_root
            .as_deref()
            .or(self.workspace_root.as_deref())
            .map(|s| s.to_string())
            .unwrap_or_else(|| ctx.cwd.to_string_lossy().into_owned());
        cmd.arg("--workspace-root").arg(&workspace_root);

        // Always request JSON output.
        cmd.arg("--json");

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let deadline = Duration::from_secs_f64(self.timeout_secs);
        let output = timeout(deadline, async {
            let child = cmd.spawn().context("spawn openlsp-cli")?;
            child.wait_with_output().await.context("wait openlsp-cli")
        })
        .await
        .map_err(|_| anyhow::anyhow!("openlsp-cli timed out after {:.1}s", self.timeout_secs))??;

        let _ = ctx.events.send(crate::events::Event::ToolCallUpdate {
            id: call_id.to_string(),
            chunk: format!("[exit={}]", output.status.code().unwrap_or(-1)),
        });

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        // Non-zero exit always means an error.
        if !output.status.success() {
            let msg = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!(
                    "openlsp-cli exited with status {}",
                    output.status.code().unwrap_or(-1)
                )
            };
            anyhow::bail!("openlsp-cli error: {}", msg.trim());
        }

        // Return stdout; include stderr as a trailing note if non-empty.
        let mut out = stdout;
        if !stderr.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&stderr);
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
            binary: Some("true".to_string()),
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
    fn resolve_falls_back_without_panic() {
        let cfg = OpenlspSection {
            enabled: true,
            binary: None,
            workspace_root: None,
            timeout: 30.0,
        };
        // Just assert it doesn't panic regardless of what's on PATH.
        let _ = resolve_openlsp_binary(&cfg);
    }

    /// Contract test: verify the argv shape Ra passes to openlsp-cli.
    ///
    /// Uses a fake binary (`sh -c 'echo "$@"'`) that echoes its arguments
    /// so we can assert the exact flags without a real openlsp-cli install.
    #[tokio::test]
    async fn argv_shape_lsp_diagnostics() {
        let (events, _) = tokio::sync::broadcast::channel(16);
        let ctx = ToolCtx::local(events);

        // Use `sh` as the fake binary; prefix_args echo the argv.
        let tool = LspTool {
            binary: ResolvedBinary {
                program: "sh".to_string(),
                prefix_args: vec![
                    "-c".to_string(),
                    r#"echo "$@""#.to_string(),
                    "--".to_string(),
                ],
            },
            workspace_root: None,
            timeout_secs: 5.0,
        };

        let input = serde_json::json!({
            "command": "lsp",
            "operation": "diagnostics",
            "file_path": "src/main.rs"
        });

        // sh exits 0, so we get the echoed args back.
        let out = tool.execute("call-1", input, &ctx).await.unwrap();
        // The output should contain the key argv flags.
        assert!(out.contains("lsp"), "missing subcommand: {out}");
        assert!(out.contains("--operation"), "missing --operation: {out}");
        assert!(
            out.contains("diagnostics"),
            "missing operation value: {out}"
        );
        assert!(out.contains("--file-path"), "missing --file-path: {out}");
        assert!(out.contains("src/main.rs"), "missing file path: {out}");
        assert!(
            out.contains("--workspace-root"),
            "missing --workspace-root: {out}"
        );
        assert!(out.contains("--json"), "missing --json: {out}");
    }

    /// Contract test: non-zero exit is always an error.
    #[tokio::test]
    async fn nonzero_exit_is_error() {
        let (events, _) = tokio::sync::broadcast::channel(16);
        let ctx = ToolCtx::local(events);

        let tool = LspTool {
            binary: ResolvedBinary {
                program: "sh".to_string(),
                prefix_args: vec![
                    "-c".to_string(),
                    "echo 'usage error' >&2; exit 1".to_string(),
                ],
            },
            workspace_root: None,
            timeout_secs: 5.0,
        };

        let input = serde_json::json!({"command": "lsp"});
        let err = tool.execute("call-2", input, &ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("openlsp-cli error"),
            "expected error: {err}"
        );
    }

    /// Contract test: workspace_root falls back to session cwd when unset.
    #[tokio::test]
    async fn workspace_root_defaults_to_session_cwd() {
        let (events, _) = tokio::sync::broadcast::channel(16);
        let mut ctx = ToolCtx::local(events);
        ctx.cwd = std::path::PathBuf::from("/my/project");

        let tool = LspTool {
            binary: ResolvedBinary {
                program: "sh".to_string(),
                prefix_args: vec![
                    "-c".to_string(),
                    r#"echo "$@""#.to_string(),
                    "--".to_string(),
                ],
            },
            workspace_root: None,
            timeout_secs: 5.0,
        };

        let input = serde_json::json!({"command": "capabilities"});
        let out = tool.execute("call-3", input, &ctx).await.unwrap();
        assert!(
            out.contains("/my/project"),
            "expected session cwd as workspace-root: {out}"
        );
    }
}
