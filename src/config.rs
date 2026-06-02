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
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The whole config file. All sections are optional so a minimal
/// `version = 1` document is a valid Ra config.
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
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
    #[serde(default)]
    pub agents_md: AgentsMdSection,
    #[serde(default)]
    pub openspec: OpenSpecSection,
    #[serde(default)]
    pub graphify: GraphifySection,
    #[serde(default)]
    pub resources: ResourcesSection,
    #[serde(default)]
    pub memory: MemorySection,
    #[serde(default)]
    pub rtk: RtkSection,
    #[serde(default)]
    pub openlsp: OpenlspSection,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
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

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
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

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelSelectorSection {
    /// Name of the entry in `[[models]]` to use by default.
    #[serde(default)]
    pub default: Option<String>,
}

/// One entry in `[[models]]`. Mirrors HCP's `[model]` shape but lifted
/// into an array so Ra can advertise a catalog at session/new time.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
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

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    /// Builtin tool allow-list. Empty Vec = all enabled (default).
    /// Non-empty: only the listed names ship to the LLM.
    #[serde(default)]
    pub builtin: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillsSection {
    /// Master switch. Default true.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Auto-discover skills from Ra-native, universal/cross-agent, and
    /// Claude Code layouts: `./.ra/skills/`, `~/.ra/skills/`,
    /// `./.agents/skills/`, `~/.agents/skills/`, `./.claude/skills/`,
    /// and `~/.claude/skills/`. Default true so project and personal
    /// skills work without explicit paths.
    #[serde(default = "default_true")]
    pub discover: bool,
    /// Extra glob patterns expanded against `~` and the cwd. Each match
    /// is loaded as a SKILL.md, on top of (and de-duplicated against)
    /// whatever `discover` finds.
    #[serde(default)]
    pub paths: Vec<String>,
}

impl Default for SkillsSection {
    fn default() -> Self {
        Self {
            enabled: true,
            discover: true,
            paths: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptsSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Glob patterns. Each match becomes a slash command named after
    /// the file stem.
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct A2aSection {
    #[serde(default)]
    pub serve: A2aServeSection,
    #[serde(default)]
    pub remote_agents: Vec<A2aRemoteAgent>,
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct A2aServeSection {
    #[serde(default)]
    pub http_port: Option<u16>,
    #[serde(default)]
    pub grpc_port: Option<u16>,
    /// Optional Bearer token gate. When set, every JSON-RPC, REST, and
    /// gRPC request must carry `Authorization: Bearer <env-value>` (or
    /// the gRPC `authorization` metadata key). The well-known agent-card
    /// endpoint is exempted so unauthenticated callers can discover the
    /// auth scheme.
    #[serde(default)]
    pub auth: Option<AuthSpec>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct A2aRemoteAgent {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub auth: Option<AuthSpec>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthSpec {
    /// Env var holding a Bearer token to send as `Authorization`.
    #[serde(default)]
    pub bearer_env: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    #[serde(default)]
    pub servers: Vec<McpServer>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
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

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionSection {
    /// `default` | `plan` | `ask`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Override for the trajectory dir. Otherwise `<data_dir>/sessions`.
    #[serde(default)]
    pub trajectory_dir: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HooksSection {
    /// PreToolUse hooks (Claude Code spec name). Also accepted under
    /// the legacy snake_case alias `pre_tool_use`.
    #[serde(default, alias = "pre_tool_use", rename = "PreToolUse")]
    pub pre_tool_use: Vec<Hook>,
    #[serde(default, alias = "post_tool_use", rename = "PostToolUse")]
    pub post_tool_use: Vec<Hook>,
    #[serde(default, alias = "user_prompt_submit", rename = "UserPromptSubmit")]
    pub user_prompt_submit: Vec<Hook>,
    /// Claude Code calls this `Stop` (assistant finished). Legacy alias
    /// `agent_end` still works.
    #[serde(default, alias = "agent_end", rename = "Stop")]
    pub stop: Vec<Hook>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// Regex matched against the tool name (PreToolUse/PostToolUse) or
    /// the user message text (UserPromptSubmit). Default `.*`.
    #[serde(default = "default_match_all")]
    pub matcher: String,
    /// Shell command to invoke. Receives the event JSON on stdin.
    pub command: String,
    /// Hard timeout in seconds. Default 60. Accepted under `timeout`
    /// (Claude Code spelling) or the legacy `timeout_s`.
    #[serde(default = "default_hook_timeout", alias = "timeout_s")]
    pub timeout: f64,
    /// Fire-and-forget: don't wait for response.
    #[serde(default, alias = "async")]
    pub run_async: bool,
}

fn default_match_all() -> String {
    ".*".to_string()
}

fn default_hook_timeout() -> f64 {
    60.0
}

/// `[agents_md]` — auto-discover AGENTS.md files (https://agents.md/).
/// We walk the cwd up to the git root and concatenate every AGENTS.md
/// we find, "nearest-file-wins" by ordering them root→leaf so deeper
/// files appear later in the system prompt and override.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentsMdSection {
    /// Default true. Set false to disable AGENTS.md auto-discovery
    /// (some projects ship one but want Ra to ignore it).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for AgentsMdSection {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// `[openspec]` — native [OpenSpec](https://github.com/Fission-AI/OpenSpec)
/// discovery. When enabled (the default), Ra locates the nearest
/// `openspec/` directory (walking cwd → git root, same as AGENTS.md) and
/// folds a high-signal catalog of its capability specs and active changes
/// into the system prompt with progressive disclosure. Ra is the
/// *consumer* of the convention; it does not reimplement the `openspec`
/// CLI.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenSpecSection {
    /// Default true. Set false to disable OpenSpec auto-discovery even
    /// when an `openspec/` directory is present.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Override the directory Ra discovers. By default Ra walks cwd up
    /// to the git root looking for a folder literally named `openspec`;
    /// set this to point at a non-standard location. `~` is expanded and
    /// relative paths resolve against the cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Default true. When a project is discovered, fold a short
    /// *agent-own* spec-driven-development playbook into the system
    /// prompt: how to drive the `openspec` CLI non-interactively
    /// (`init --tools`, `new change`, the `status`/`instructions --json`
    /// state machine, `validate --strict` self-correction, `archive -y`)
    /// with no human in the loop. When no `openspec/` exists yet, a short
    /// bootstrap hint is folded in instead, so the agent can adopt the
    /// convention itself. Set false to surface only the catalog.
    #[serde(default = "default_true")]
    pub agent_own: bool,
}

impl Default for OpenSpecSection {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            agent_own: true,
        }
    }
}

/// `[graphify]` — native [Graphify](https://github.com/safishamsi/graphify)
/// support. When enabled, Ra targets a project's `graphify-out/graph.json`
/// (walking cwd → git root), treats Graphify as an agent-owned R2A graph
/// workflow, and registers ensure/impact/update plus native graph query tools.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphifySection {
    /// Default true. Set false to disable Graphify discovery and tools.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Auto-discover `graphify-out/graph.json` using cwd → git-root walking.
    /// Default true. Set false when you only want an explicit `path`.
    #[serde(default = "default_true")]
    pub discover: bool,
    /// Override the graph path or Graphify output directory. `~` is expanded,
    /// relative paths resolve against cwd, and directories imply `graph.json`.
    #[serde(default)]
    pub path: Option<String>,
}

impl Default for GraphifySection {
    fn default() -> Self {
        Self {
            enabled: true,
            discover: true,
            path: None,
        }
    }
}

/// `[resources]` — extra plain-text files concatenated into the system
/// prompt verbatim. Less structured than `[skills]`; useful for ad-hoc
/// system prompts or repo conventions kept in a non-AGENTS.md file.
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourcesSection {
    /// Optional path to a single primary system prompt file.
    #[serde(default)]
    pub system_prompt_path: Option<String>,
    /// Additional files appended after the primary one.
    #[serde(default)]
    pub append_system_prompt_paths: Vec<String>,
}

/// `[memory]` — Codex-style local generated memories. The feature is globally
/// disabled by default; enabling it lets Ra load durable memories into prompt
/// context and generate new local artifacts after eligible sessions.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemorySection {
    /// Global master switch. Default false.
    #[serde(default)]
    pub enabled: bool,
    /// Config-level default for using existing memories. Thread/session
    /// controls can further suppress use without mutating this setting.
    #[serde(default = "default_true", alias = "use")]
    pub use_memories: bool,
    /// Config-level default for contributing future memories. Thread/session
    /// controls can further suppress generation without mutating this setting.
    #[serde(default = "default_true", alias = "generate")]
    pub generate_memories: bool,
    /// Region availability gate. Exposed for parity with Codex-style policy;
    /// default true for local Ra.
    #[serde(default = "default_true")]
    pub region_available: bool,
    /// Disable memory use/generation when external context is present. Accepted
    /// aliases cover common wording in upstream and older Ra config drafts.
    #[serde(
        default,
        alias = "suppress_on_external_context",
        alias = "disable_when_external_context",
        alias = "suppress_when_external_context"
    )]
    pub disable_on_external_context: bool,
    /// Idle delay before background generation, in seconds.
    #[serde(default = "default_memory_idle_secs", alias = "min_idle_secs")]
    pub min_idle_before_generation_secs: u64,
    /// Minimum session duration before generation, in seconds.
    #[serde(default = "default_memory_session_secs", alias = "min_duration_secs")]
    pub min_session_duration_secs: u64,
    /// Skip generation when remaining context/rate-limit headroom is below
    /// this percentage.
    #[serde(default)]
    pub min_rate_limit_remaining_percent: u8,
    /// Minimum completed eligible sessions between agent-owned Dreams.
    #[serde(default = "default_memory_sessions_between_dreams")]
    pub min_sessions_between_dreams: usize,
    /// Optional root for generated memory state. Defaults to
    /// `<RA_HOME>/memories`.
    #[serde(default, alias = "path")]
    pub dir: Option<String>,
    /// Max number of memory artifacts rendered into prompt context.
    #[serde(default = "default_memory_prompt_limit")]
    pub max_prompt_memories: usize,
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            enabled: false,
            use_memories: true,
            generate_memories: true,
            region_available: true,
            disable_on_external_context: false,
            min_idle_before_generation_secs: default_memory_idle_secs(),
            min_session_duration_secs: default_memory_session_secs(),
            min_rate_limit_remaining_percent: 0,
            min_sessions_between_dreams: default_memory_sessions_between_dreams(),
            dir: None,
            max_prompt_memories: default_memory_prompt_limit(),
        }
    }
}

fn default_memory_idle_secs() -> u64 {
    10 * 60
}

fn default_memory_session_secs() -> u64 {
    60
}

fn default_memory_sessions_between_dreams() -> usize {
    10
}

fn default_memory_prompt_limit() -> usize {
    20
}

/// `[rtk]` — native [RTK (Rust Token Killer)](https://github.com/rtk-ai/rtk)
/// integration. When enabled, the BashTool runs every command through
/// `rtk rewrite` before executing it. RTK rewrites well-known dev
/// commands (git, cargo, pytest, docker, kubectl, …) into their
/// token-compressed equivalents, often shaving 60–90% off the bytes
/// fed back into the model context.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RtkSection {
    /// `auto` (default): use rtk if it's on PATH, otherwise transparent.
    /// `true` / `"on"`: require rtk; warn if missing.
    /// `false` / `"off"`: disable even if rtk is installed.
    #[serde(default = "default_rtk_mode")]
    pub mode: RtkMode,
    /// Override the rtk binary path. By default we look up `rtk` on PATH.
    #[serde(default)]
    pub binary: Option<String>,
    /// Pass `--ultra-compact` to `rtk rewrite` for even tighter output.
    #[serde(default)]
    pub ultra_compact: bool,
}

impl Default for RtkSection {
    fn default() -> Self {
        Self {
            mode: default_rtk_mode(),
            binary: None,
            ultra_compact: false,
        }
    }
}

fn default_rtk_mode() -> RtkMode {
    RtkMode::Auto
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RtkMode {
    /// Use rtk when it's on PATH; transparent fallthrough otherwise.
    Auto,
    /// Require rtk to be available; warn at startup if missing.
    On,
    /// Never invoke rtk even if it's installed.
    Off,
}

impl<'de> Deserialize<'de> for RtkMode {
    fn deserialize<D>(d: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Accept both string ("auto"/"on"/"off") and bool (true=on,
        // false=off) — TOML lets users write `mode = true`.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Bool(bool),
            Str(String),
        }
        match Repr::deserialize(d)? {
            Repr::Bool(true) => Ok(RtkMode::On),
            Repr::Bool(false) => Ok(RtkMode::Off),
            Repr::Str(s) => match s.to_ascii_lowercase().as_str() {
                "auto" => Ok(RtkMode::Auto),
                "on" | "true" => Ok(RtkMode::On),
                "off" | "false" => Ok(RtkMode::Off),
                other => Err(serde::de::Error::custom(format!(
                    "invalid rtk mode {other:?}; expected auto / on / off"
                ))),
            },
        }
    }
}

/// `[openlsp]` — native [openlsp-cli](https://github.com/trotsky1997/openlsp)
/// integration. When enabled (the default), Ra registers a built-in `lsp`
/// tool that invokes openlsp-cli as a child process with argv flags.
/// The tool is silently omitted if no openlsp-cli binary can be resolved.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenlspSection {
    /// Default true. Set false to disable the lsp built-in tool even when
    /// openlsp-cli is installed.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Override the openlsp-cli binary path. By default Ra tries `openlsp-cli`
    /// on PATH, then `openlsp` (alias), then `bunx openlsp-cli` if `bun` is
    /// available.
    #[serde(default)]
    pub binary: Option<String>,
    /// Workspace root passed to openlsp-cli via `--workspace-root`. Defaults
    /// to the session cwd when unset.
    #[serde(default)]
    pub workspace_root: Option<String>,
    /// Per-call timeout in seconds. Default 30.0.
    #[serde(default = "default_openlsp_timeout")]
    pub timeout: f64,
}

impl Default for OpenlspSection {
    fn default() -> Self {
        Self {
            enabled: true,
            binary: None,
            workspace_root: None,
            timeout: default_openlsp_timeout(),
        }
    }
}

fn default_openlsp_timeout() -> f64 {
    30.0
}

// ---------- loading -----------------------------------------------------

impl RaConfig {
    /// Resolve, read, and parse the config from the conventional locations.
    /// Returns `RaConfig::default()` if no file exists.
    pub fn load(explicit_path: Option<&str>) -> Result<Self> {
        let Some(path) = resolve_config_path(explicit_path) else {
            return Ok(Self::default());
        };
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
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

[[hooks.PreToolUse]]
matcher = "bash"
command = "./hooks/audit.sh"
timeout = 2.0
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

    #[test]
    fn memory_defaults_disabled_and_parses_aliases() {
        let cfg: RaConfig = toml::from_str("version = 1\n").unwrap();
        assert!(!cfg.memory.enabled);
        assert!(cfg.memory.use_memories);
        assert!(cfg.memory.generate_memories);
        assert_eq!(cfg.memory.min_idle_before_generation_secs, 600);
        assert_eq!(cfg.memory.min_session_duration_secs, 60);
        assert_eq!(cfg.memory.min_sessions_between_dreams, 10);

        let toml_doc = r#"
version = 1

[memory]
enabled = true
use = false
generate = true
suppress_on_external_context = true
min_idle_secs = 5
min_duration_secs = 2
min_rate_limit_remaining_percent = 25
min_sessions_between_dreams = 7
path = "./.ra/memory"
max_prompt_memories = 3
"#;
        let cfg: RaConfig = toml::from_str(toml_doc).unwrap();
        assert!(cfg.memory.enabled);
        assert!(!cfg.memory.use_memories);
        assert!(cfg.memory.generate_memories);
        assert!(cfg.memory.disable_on_external_context);
        assert_eq!(cfg.memory.min_idle_before_generation_secs, 5);
        assert_eq!(cfg.memory.min_session_duration_secs, 2);
        assert_eq!(cfg.memory.min_rate_limit_remaining_percent, 25);
        assert_eq!(cfg.memory.min_sessions_between_dreams, 7);
        assert_eq!(cfg.memory.dir.as_deref(), Some("./.ra/memory"));
        assert_eq!(cfg.memory.max_prompt_memories, 3);
    }

    #[test]
    fn parse_legacy_hook_aliases() {
        // Existing user configs use the snake_case names — they must
        // keep working after the Claude-Code rename.
        let toml_doc = r#"
version = 1

[[hooks.pre_tool_use]]
matcher = "bash"
command = "./hooks/audit.sh"
timeout_s = 1.5

[[hooks.agent_end]]
command = "./hooks/cleanup.sh"
"#;
        let cfg: RaConfig = toml::from_str(toml_doc).unwrap();
        assert_eq!(cfg.hooks.pre_tool_use.len(), 1);
        assert!((cfg.hooks.pre_tool_use[0].timeout - 1.5).abs() < f64::EPSILON);
        assert_eq!(cfg.hooks.stop.len(), 1);
    }

    #[test]
    fn openspec_defaults_on_and_parses_overrides() {
        // Omitted section => enabled by default, no path override.
        let cfg: RaConfig = toml::from_str("version = 1\n").unwrap();
        assert!(cfg.openspec.enabled);
        assert!(cfg.openspec.path.is_none());

        // Explicit section.
        let toml_doc = r#"
version = 1

[openspec]
enabled = false
path = "./docs/openspec"
"#;
        let cfg: RaConfig = toml::from_str(toml_doc).unwrap();
        assert!(!cfg.openspec.enabled);
        assert_eq!(cfg.openspec.path.as_deref(), Some("./docs/openspec"));
    }

    #[test]
    fn graphify_defaults_on_and_parses_overrides() {
        let cfg: RaConfig = toml::from_str("version = 1\n").unwrap();
        assert!(cfg.graphify.enabled);
        assert!(cfg.graphify.discover);
        assert!(cfg.graphify.path.is_none());

        let toml_doc = r#"
version = 1

[graphify]
enabled = false
discover = false
path = "./custom-graph/graph.json"
"#;
        let cfg: RaConfig = toml::from_str(toml_doc).unwrap();
        assert!(!cfg.graphify.enabled);
        assert!(!cfg.graphify.discover);
        assert_eq!(
            cfg.graphify.path.as_deref(),
            Some("./custom-graph/graph.json")
        );
    }

    #[test]
    fn openlsp_defaults_and_parses_overrides() {
        // Omitted section => enabled by default, 30s timeout.
        let cfg: RaConfig = toml::from_str("version = 1\n").unwrap();
        assert!(cfg.openlsp.enabled);
        assert!(cfg.openlsp.binary.is_none());
        assert!(cfg.openlsp.workspace_root.is_none());
        assert!((cfg.openlsp.timeout - 30.0).abs() < f64::EPSILON);

        // Explicit section with overrides.
        let toml_doc = r#"
version = 1

[openlsp]
enabled = false
binary = "/usr/local/bin/openlsp"
workspace_root = "/home/user/project"
timeout = 60.0
"#;
        let cfg: RaConfig = toml::from_str(toml_doc).unwrap();
        assert!(!cfg.openlsp.enabled);
        assert_eq!(
            cfg.openlsp.binary.as_deref(),
            Some("/usr/local/bin/openlsp")
        );
        assert_eq!(
            cfg.openlsp.workspace_root.as_deref(),
            Some("/home/user/project")
        );
        assert!((cfg.openlsp.timeout - 60.0).abs() < f64::EPSILON);
    }
}
