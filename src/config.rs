//! Ra runtime configuration loaded from TOML.
//!
//! Spirit borrowed from Harbor's HCP (RFC-0002), but the surface is
//! tailored to Ra's actual capabilities — see spec/hcp-RFC.md for the
//! original; the deltas live in spec/README.md.
//!
//! Resolution order (first hit wins):
//!   1. CLI `--config <path>`
//!   2. `$RA_CONFIG`
//!   3. `./ra.toml` in cwd
//!   4. `~/.ra.toml`
//!
//! Env vars still take precedence over config-file values for any field
//! that has both. The config file just makes env-only setups easier to
//! reproduce; existing env-only deployments are unchanged.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The whole config file. All sections are optional so a minimal
/// `version = 1` document is a valid Ra config.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RaConfig {
    /// Schema version. Currently 1; future breaks bump it.
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub run: RunSection,
    #[serde(default)]
    pub obs: ObsSection,
    #[serde(default)]
    pub model: ModelSelectorSection,
    /// Array-of-tables: `[[models]]` entries.
    #[serde(default)]
    pub models: Vec<ModelEntrySection>,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub skills: SkillsSection,
    #[serde(default)]
    pub prompts: PromptsSection,
    #[serde(default)]
    pub a2a: A2aSection,
    #[serde(default)]
    pub mcp: McpSection,
    #[serde(default)]
    pub session: SessionSection,
    #[serde(default)]
    pub hooks: HooksSection,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSection {
    /// Print the Ra banner on stderr at startup. Default true.
    #[serde(default = "default_true")]
    pub banner: bool,
    /// Root for ATIF trajectories, ATOF logs, etc. Defaults to
    /// `dirs::data_local_dir() / "ra"`.
    #[serde(default)]
    pub data_dir: Option<String>,
    /// Default working directory used for session bucketing when an ACP
    /// client doesn't pin one. Defaults to `std::env::current_dir()`.
    #[serde(default)]
    pub default_cwd: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObsSection {
    /// `stderr` | `file` | `otel` | `none`. Maps to `RA_OBS_BACKEND`.
    /// Empty string treated as unset.
    #[serde(default)]
    pub backend: Option<String>,
    /// OTLP endpoint when backend=otel. Equivalent of
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`.
    #[serde(default)]
    pub otel_endpoint: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelectorSection {
    /// Name of the entry in `[[models]]` to use by default.
    #[serde(default)]
    pub default: Option<String>,
}

/// One entry in `[[models]]`. Mirrors HCP's `[model]` shape but lifted
/// into an array so Ra can advertise a catalog at session/new time.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntrySection {
    pub name: String,
    /// `openai` | `anthropic` | `google` | `deepseek` | `ollama` |
    /// `groq` | `xai`. Mapped to the graniet/llm `LLMBackend` enum.
    pub backend: String,
    pub model_id: String,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Env var holding the API key. Required unless `api_key` is given.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// API key inline. Strongly discouraged; prefer `api_key_env`.
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    /// Builtin tool allow-list. Empty Vec = all enabled (default).
    /// Non-empty: only the listed names ship to the LLM.
    #[serde(default)]
    pub builtin: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Glob patterns; expanded against `~` and the current working
    /// directory. Each match is loaded as a SKILL.md.
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptsSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Glob patterns. Each match becomes a slash command named after
    /// the file stem.
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct A2aSection {
    #[serde(default)]
    pub serve: A2aServeSection,
    #[serde(default)]
    pub remote_agents: Vec<A2aRemoteAgent>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct A2aServeSection {
    #[serde(default)]
    pub http_port: Option<u16>,
    #[serde(default)]
    pub grpc_port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct A2aRemoteAgent {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub auth: Option<AuthSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSpec {
    /// Env var holding a Bearer token to send as `Authorization`.
    #[serde(default)]
    pub bearer_env: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    #[serde(default)]
    pub servers: Vec<McpServer>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub name: String,
    /// `stdio` | `http`. Default `stdio`.
    #[serde(default = "default_stdio")]
    pub transport: String,
    // stdio-only:
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    // http-only:
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub auth: Option<AuthSpec>,
}

fn default_stdio() -> String {
    "stdio".to_string()
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSection {
    /// `default` | `plan` | `ask`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Override for the trajectory dir. Otherwise `<data_dir>/sessions`.
    #[serde(default)]
    pub trajectory_dir: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksSection {
    #[serde(default)]
    pub pre_tool_use: Vec<Hook>,
    #[serde(default)]
    pub post_tool_use: Vec<Hook>,
    #[serde(default)]
    pub user_prompt_submit: Vec<Hook>,
    #[serde(default)]
    pub agent_end: Vec<Hook>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// Regex matched against the tool name (PreToolUse/PostToolUse) or
    /// the user message text (UserPromptSubmit). Default `.*`.
    #[serde(default = "default_match_all")]
    pub matcher: String,
    /// Shell command to invoke. Receives the event JSON on stdin.
    pub command: String,
    /// Hard timeout in seconds. Default 5.
    #[serde(default = "default_hook_timeout")]
    pub timeout_s: f64,
    /// Fire-and-forget: don't wait for response.
    #[serde(default)]
    pub run_async: bool,
}

fn default_match_all() -> String {
    ".*".to_string()
}

fn default_hook_timeout() -> f64 {
    5.0
}

// ---------- loading -----------------------------------------------------

impl RaConfig {
    /// Resolve, read, and parse the config from the conventional locations.
    /// Returns `RaConfig::default()` if no file exists.
    pub fn load(explicit_path: Option<&str>) -> Result<Self> {
        let Some(path) = resolve_config_path(explicit_path) else {
            return Ok(Self::default());
        };
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("parse {}", path.display()))?;
        if cfg.version != 1 {
            anyhow::bail!(
                "{}: unsupported config version {} (expected 1)",
                path.display(),
                cfg.version
            );
        }
        Ok(cfg)
    }

    /// Expand `~` and resolve relative paths against `base`. Returns
    /// the absolute form, never panics.
    pub fn expand_path(s: &str, base: &Path) -> PathBuf {
        let expanded = shellexpand::tilde(s).to_string();
        let p = PathBuf::from(expanded);
        if p.is_absolute() {
            p
        } else {
            base.join(p)
        }
    }
}

/// Conventional lookup: --config flag → $RA_CONFIG → ./ra.toml → ~/.ra.toml.
fn resolve_config_path(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        let pb = PathBuf::from(p);
        return if pb.exists() { Some(pb) } else { None };
    }
    if let Ok(p) = std::env::var("RA_CONFIG") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let cwd_toml = std::env::current_dir()
        .ok()
        .map(|d| d.join("ra.toml"))
        .filter(|p| p.exists());
    if let Some(p) = cwd_toml {
        return Some(p);
    }
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".ra.toml");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

// ---------- helpers -----------------------------------------------------

impl ModelEntrySection {
    /// Resolve the API key for this entry: prefer `api_key_env`, then
    /// inline `api_key`. Returns `None` if neither is set or the env var
    /// is missing.
    pub fn resolve_api_key(&self) -> Option<String> {
        if let Some(name) = &self.api_key_env {
            if let Ok(v) = std::env::var(name) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        self.api_key.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let toml_doc = r#"
version = 1
[run]
banner = false
"#;
        let cfg: RaConfig = toml::from_str(toml_doc).unwrap();
        assert_eq!(cfg.version, 1);
        assert!(!cfg.run.banner);
    }

    #[test]
    fn parse_full_shape() {
        let toml_doc = r#"
version = 1
[run]
data_dir = "~/.local/share/ra"

[obs]
backend = "stderr"

[model]
default = "pi"

[[models]]
name = "pi"
backend = "openai"
model_id = "gpt-5.5"
base_url = "https://pi-api-us.macaron.xin/v1/"
api_key_env = "PI_API_KEY"

[tools]
builtin = ["read", "bash"]

[skills]
paths = ["./skills/**/SKILL.md"]

[prompts]
paths = ["./prompts/*.md"]

[a2a.serve]
http_port = 3000
grpc_port = 50051

[[a2a.remote_agents]]
name = "helper"
url = "http://localhost:3001"

[[mcp.servers]]
name = "fs"
transport = "stdio"
command = "npx"
args = ["@modelcontextprotocol/server-filesystem", "/tmp"]

[session]
mode = "default"

[[hooks.pre_tool_use]]
matcher = "bash"
command = "./hooks/audit.sh"
timeout_s = 2.0
"#;
        let cfg: RaConfig = toml::from_str(toml_doc).unwrap();
        assert_eq!(cfg.models.len(), 1);
        assert_eq!(cfg.models[0].name, "pi");
        assert_eq!(cfg.tools.builtin, vec!["read", "bash"]);
        assert_eq!(cfg.a2a.serve.http_port, Some(3000));
        assert_eq!(cfg.a2a.remote_agents.len(), 1);
        assert_eq!(cfg.mcp.servers.len(), 1);
        assert_eq!(cfg.hooks.pre_tool_use.len(), 1);
        assert_eq!(cfg.hooks.pre_tool_use[0].matcher, "bash");
    }
}
