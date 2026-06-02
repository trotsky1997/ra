//! Resource loading: skills, prompts, AGENTS.md, plain system prompts.
//!
//! Three external specs feed into one unified `ResourceBundle`:
//!
//! - **Claude Code / agentskills.io skills** — `SKILL.md` with YAML
//!   frontmatter. `name` and `description` are optional in Claude Code:
//!   the command name comes from the skill directory, display name falls
//!   back to that command name, and description falls back to the first
//!   Markdown paragraph. Progressive-disclosure: only model-invocable
//!   skill descriptions go into the system prompt at startup; the LLM is
//!   instructed to call the `read` tool on the SKILL.md path when it
//!   wants the full body.
//! - **agents.md** — free-form `AGENTS.md` files autodiscovered
//!   walking up from cwd to the git root. "Nearest file wins":
//!   files are ordered root→leaf and concatenated, so a deeper
//!   AGENTS.md appears later in the prompt.
//! - **HCP `[resources]`** — plain markdown system prompts and
//!   appendices, configured by path.
//!
//! Plus prompt templates from `[prompts]` for slash commands.

use crate::config::HooksSection;
use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const BUILTIN_REVIEW_SKILL: &str = r#"---
name: review
description: Review local code changes for correctness, regressions, and missing tests.
---

# Review Skill

Use this skill when the user asks for a code review or asks you to inspect a change before merging.

## Instructions

- Focus on concrete bugs, behavioral regressions, security issues, and missing tests.
- Ground findings in file and line references where possible.
- Put findings before summaries.
- If no issues are found, say that directly and mention any residual test risk.
"#;

/// Provenance of a skill after discovery. Lower-priority sources can be
/// shadowed by higher-priority sources with the same command name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    Explicit,
    Project,
    Global,
    Registry,
    Builtin,
}

impl SkillSource {
    fn precedence(self) -> u8 {
        match self {
            SkillSource::Explicit => 0,
            SkillSource::Project => 1,
            SkillSource::Global => 2,
            SkillSource::Registry => 3,
            SkillSource::Builtin => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SkillSource::Explicit => "explicit",
            SkillSource::Project => "project",
            SkillSource::Global => "global",
            SkillSource::Registry => "registry",
            SkillSource::Builtin => "builtin",
        }
    }
}

/// One agent skill following Claude Code / agentskills.io conventions.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Slash command name, derived from the containing directory.
    pub command_name: String,
    /// Display name from frontmatter `name`, falling back to
    /// `command_name`.
    pub name: String,
    /// Model-facing blurb from `description` + `when_to_use`, or the
    /// first Markdown paragraph when omitted.
    pub description: String,
    /// Optional `when_to_use:` field appended to the model-facing blurb.
    pub when_to_use: Option<String>,
    /// Named positional arguments from frontmatter `arguments`.
    pub arguments: Vec<String>,
    /// Optional `compatibility:` field.
    pub compatibility: Option<String>,
    /// Optional `license:` field.
    pub license: Option<String>,
    /// Runtime-only Claude Code fields that apply to direct skill invocation.
    pub runtime: SkillRuntimeOptions,
    /// If true, omit this skill from the model-facing catalog.
    pub disable_model_invocation: bool,
    /// If false, do not expose this skill as a direct slash command.
    pub user_invocable: bool,
    /// Path to the SKILL.md file on disk.
    pub path: PathBuf,
    /// Where this skill came from before registry resolution.
    pub source: SkillSource,
    /// Optional version provenance for git-backed registry skills.
    pub source_revision: Option<String>,
    /// Internal skills are hidden from builtin loading unless explicitly
    /// requested. External skills still load normally; this flag mainly
    /// preserves `npx skills` metadata for Ra-shipped entries.
    pub internal: bool,
    /// The Markdown body **after** the frontmatter. Not loaded into the
    /// system prompt at startup (progressive disclosure); the LLM reads
    /// it on demand.
    pub body: String,
}

/// One slash-command prompt template (a plain `.md` file).
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    pub name: String,
    pub path: PathBuf,
    pub body: String,
}

/// Runtime controls carried by Claude Code skill frontmatter and applied
/// only when the skill is directly invoked as `/skill-name`.
#[derive(Debug, Default, Clone)]
pub struct SkillRuntimeOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub context: Option<String>,
    pub agent: Option<String>,
    pub shell: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub hooks: HooksSection,
}

impl SkillRuntimeOptions {
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.effort.is_none()
            && self.context.is_none()
            && self.agent.is_none()
            && self.shell.is_none()
            && self.allowed_tools.is_empty()
            && self.disallowed_tools.is_empty()
            && self.hooks.pre_tool_use.is_empty()
            && self.hooks.post_tool_use.is_empty()
            && self.hooks.user_prompt_submit.is_empty()
            && self.hooks.stop.is_empty()
    }

    pub fn is_fork(&self) -> bool {
        self.context
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("fork"))
            .unwrap_or(false)
            || self
                .agent
                .as_deref()
                .map(|s| {
                    let s = s.trim();
                    s.eq_ignore_ascii_case("fork") || s.eq_ignore_ascii_case("subagent")
                })
                .unwrap_or(false)
    }

    pub fn prompt_context(&self) -> Option<&str> {
        self.context
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("fork"))
    }
}

/// Slash-command template handed to [`crate::session_runner::SessionRunner`].
#[derive(Debug, Clone)]
pub struct SlashTemplate {
    pub body: String,
    pub arguments: Vec<String>,
    pub append_arguments_fallback: bool,
    pub runtime: Option<SkillRuntimeOptions>,
}

impl SlashTemplate {
    pub fn prompt(body: String) -> Self {
        Self {
            body,
            arguments: Vec::new(),
            append_arguments_fallback: false,
            runtime: None,
        }
    }

    pub fn skill(body: String, arguments: Vec<String>, runtime: SkillRuntimeOptions) -> Self {
        Self {
            body,
            arguments,
            append_arguments_fallback: true,
            runtime: Some(runtime),
        }
    }
}

/// One AGENTS.md found while walking up the project tree.
#[derive(Debug, Clone)]
pub struct AgentsMd {
    pub path: PathBuf,
    pub body: String,
    /// Distance from cwd: 0 = same dir, 1 = parent, …
    pub depth: usize,
}

/// A free-form Markdown file pulled in via `[resources]`.
#[derive(Debug, Clone)]
pub struct PlainResource {
    pub path: PathBuf,
    pub body: String,
}

/// One bundle of every resource Ra can compose into the LLM context
/// or expose as a slash command. Built once at startup.
#[derive(Debug, Default, Clone)]
pub struct ResourceBundle {
    pub skills: Vec<Skill>,
    pub prompts: Vec<PromptTemplate>,
    pub agents_md: Vec<AgentsMd>,
    pub plain: Vec<PlainResource>,
    /// Discovered OpenSpec project (`openspec/` dir), if any.
    pub openspec: Option<crate::openspec::OpenSpecProject>,
    /// When true and no `openspec/` project was discovered, fold a short
    /// *bootstrap* hint into the prompt so the agent knows it can adopt
    /// OpenSpec itself (`openspec init --tools …`). Set by `main` from
    /// `[openspec] enabled && agent_own` when discovery comes up empty;
    /// ignored when `openspec` is `Some`.
    pub openspec_bootstrap: bool,
    /// Agent-owned Graphify R2A graph workflow, if enabled.
    pub graphify: Option<crate::graphify::GraphifyWorkflow>,
}

impl ResourceBundle {
    /// Compose every loaded resource into a single system prompt
    /// suitable for `Session::set_system_prompt`. Returns `None` if
    /// the bundle is empty.
    ///
    /// Composition order:
    /// 1. AGENTS.md walked from project root → cwd (so deepest wins)
    /// 2. Plain `[resources]` files in the order they were configured
    /// 3. OpenSpec catalog (capability specs + active changes), if any
    /// 4. Graphify R2A graph workflow and native tool hints, if any
    /// 5. Skill descriptions only (progressive disclosure), each with
    ///    a path hint so the LLM can `read` the SKILL.md when needed
    pub fn build_system_prompt(&self) -> Option<String> {
        let mut buf = String::new();

        // 1. AGENTS.md (root first; closest last so it overrides)
        let mut sorted_agents = self.agents_md.clone();
        sorted_agents.sort_by_key(|a| std::cmp::Reverse(a.depth));
        for a in &sorted_agents {
            buf.push_str(&format!("# AGENTS.md ({})\n\n", a.path.display()));
            buf.push_str(&a.body);
            ensure_trailing_blank(&mut buf);
        }

        // 2. plain resources
        for p in &self.plain {
            buf.push_str(&format!("# {}\n\n", p.path.display()));
            buf.push_str(&p.body);
            ensure_trailing_blank(&mut buf);
        }

        // 3. OpenSpec catalog (progressive disclosure, like skills).
        if let Some(section) = self
            .openspec
            .as_ref()
            .and_then(|p| p.build_system_prompt_section())
        {
            buf.push_str(&section);
            ensure_trailing_blank(&mut buf);
        } else if self.openspec_bootstrap {
            // No `openspec/` discovered, but agent-own SDD is enabled:
            // tell the agent it may adopt the convention itself. Without
            // this, the catalog (which carries the `init` instructions)
            // never renders on a greenfield repo — a chicken-and-egg gap.
            buf.push_str(&crate::openspec::bootstrap_prompt_section());
            ensure_trailing_blank(&mut buf);
        }

        // 4. Graphify catalog (progressive disclosure, query-first).
        if let Some(section) = self
            .graphify
            .as_ref()
            .and_then(|p| p.build_system_prompt_section())
        {
            buf.push_str(&section);
            ensure_trailing_blank(&mut buf);
        }

        // 5. skills as a structured catalog (description only).
        let model_visible_skills: Vec<&Skill> = self
            .skills
            .iter()
            .filter(|s| !s.disable_model_invocation)
            .collect();
        if !model_visible_skills.is_empty() {
            buf.push_str("# Skills\n\n");
            buf.push_str(
                "The following skills are available. Each entry shows its name and a one-paragraph \
                 description. To use a skill, call the `read` tool on its path to load full \
                 instructions before acting.\n\n",
            );
            for s in model_visible_skills {
                buf.push_str(&format!(
                    "## {}\n- command: /{}\n- description: {}\n- path: `{}`\n",
                    s.name,
                    s.command_name,
                    s.description.trim(),
                    s.path.display()
                ));
                if let Some(compat) = &s.compatibility {
                    buf.push_str(&format!("- compatibility: {compat}\n"));
                }
                buf.push('\n');
            }
        }

        if buf.trim().is_empty() {
            None
        } else {
            Some(buf)
        }
    }

    /// Slash-command name → template body, ready to hand to SessionRunner.
    pub fn prompt_map(&self) -> HashMap<String, SlashTemplate> {
        let mut map: HashMap<String, SlashTemplate> = self
            .skills
            .iter()
            .filter(|s| s.user_invocable)
            .map(|s| {
                (
                    s.command_name.clone(),
                    SlashTemplate::skill(s.body.clone(), s.arguments.clone(), s.runtime.clone()),
                )
            })
            .collect();
        for p in &self.prompts {
            map.insert(p.name.clone(), SlashTemplate::prompt(p.body.clone()));
        }
        map
    }
}

// ---------- loaders -----------------------------------------------------

pub fn load_skills(patterns: &[String]) -> Vec<Skill> {
    load_skills_with_source(patterns, SkillSource::Global, None)
}

fn load_skills_with_source(
    patterns: &[String],
    source: SkillSource,
    source_revision: Option<String>,
) -> Vec<Skill> {
    expand_globs(patterns, "skills")
        .into_iter()
        .filter_map(
            |p| match parse_skill_with_source(&p, source, source_revision.clone()) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("[ra::skills] {}: {e:#}", p.display());
                    None
                }
            },
        )
        .collect()
}

/// The default skill-discovery globs Ra scans when `[skills] discover = true`.
/// Project-relative entries are anchored at cwd; `build_resource_bundle` adds
/// cwd → git-root project skill directories on top.
pub fn default_discover_globs() -> Vec<String> {
    vec![
        "./.ra/skills/**/SKILL.md".to_string(),
        "~/.ra/skills/**/SKILL.md".to_string(),
        "./.agents/skills/**/SKILL.md".to_string(),
        "~/.agents/skills/**/SKILL.md".to_string(),
        "~/.codex/skills/**/SKILL.md".to_string(),
        "./.claude/skills/**/SKILL.md".to_string(),
        "~/.claude/skills/**/SKILL.md".to_string(),
    ]
}

fn project_skill_globs(cwd: &Path) -> Vec<String> {
    discover_project_skill_globs(cwd)
}

fn global_skill_globs() -> Vec<String> {
    vec![
        "~/.ra/skills/**/SKILL.md".to_string(),
        "~/.agents/skills/**/SKILL.md".to_string(),
        "~/.codex/skills/**/SKILL.md".to_string(),
        "~/.claude/skills/**/SKILL.md".to_string(),
    ]
}

fn builtin_skill_docs() -> Vec<(&'static str, &'static str)> {
    vec![("review", BUILTIN_REVIEW_SKILL)]
}

fn load_builtin_skills(config: &crate::config::BuiltinSkillsSection) -> Vec<Skill> {
    if !config.enabled {
        return Vec::new();
    }
    let include = normalized_name_set(&config.include);
    let exclude = normalized_name_set(&config.exclude);
    builtin_skill_docs()
        .into_iter()
        .filter_map(|(name, raw)| {
            let command = normalize_name(name);
            if !include.is_empty() && !include.contains(&command) {
                return None;
            }
            if exclude.contains(&command) {
                return None;
            }
            let path = materialize_builtin_skill(name, raw)
                .unwrap_or_else(|| PathBuf::from(format!("<builtin>/skills/{name}/SKILL.md")));
            match parse_skill_document(raw, &path, SkillSource::Builtin, None) {
                Ok(skill) if skill.internal && !config.include_internal => None,
                Ok(skill) => Some(skill),
                Err(e) => {
                    eprintln!("[ra::skills] builtin {name}: {e:#}");
                    None
                }
            }
        })
        .collect()
}

fn materialize_builtin_skill(name: &str, raw: &str) -> Option<PathBuf> {
    let cache_root = dirs::cache_dir()?
        .join("ra")
        .join("builtin-skills")
        .join(env!("CARGO_PKG_VERSION"))
        .join(name);
    if let Err(e) = std::fs::create_dir_all(&cache_root) {
        eprintln!(
            "[ra::skills] failed to create builtin skill cache {}: {e}",
            cache_root.display()
        );
        return None;
    }
    let path = cache_root.join("SKILL.md");
    if let Err(e) = std::fs::write(&path, raw) {
        eprintln!(
            "[ra::skills] failed to write builtin skill cache {}: {e}",
            path.display()
        );
        return None;
    }
    Some(path)
}

fn load_registry_skills(config: &crate::config::SkillRegistrySection) -> Vec<Skill> {
    if !config.enabled {
        return Vec::new();
    }
    let Some(root) = registry_root(config) else {
        return Vec::new();
    };
    if !root.exists() {
        return Vec::new();
    }
    let Some(revision) = git_head_revision(&root) else {
        eprintln!(
            "[ra::skills] registry {} is not a git checkout; skipping",
            root.display()
        );
        return Vec::new();
    };
    let include = normalized_name_set(&config.include);
    let exclude = normalized_name_set(&config.exclude);
    let patterns = vec![
        root.join("skills/**/SKILL.md")
            .to_string_lossy()
            .into_owned(),
        root.join("skills/.curated/**/SKILL.md")
            .to_string_lossy()
            .into_owned(),
        root.join("skills/.experimental/**/SKILL.md")
            .to_string_lossy()
            .into_owned(),
        root.join("skills/.system/**/SKILL.md")
            .to_string_lossy()
            .into_owned(),
        root.join(".agents/skills/**/SKILL.md")
            .to_string_lossy()
            .into_owned(),
        root.join(".claude/skills/**/SKILL.md")
            .to_string_lossy()
            .into_owned(),
    ];
    load_skills_with_source(&patterns, SkillSource::Registry, Some(revision))
        .into_iter()
        .filter(|skill| {
            let name = normalize_name(&skill.command_name);
            (include.is_empty() || include.contains(&name)) && !exclude.contains(&name)
        })
        .collect()
}

fn registry_root(config: &crate::config::SkillRegistrySection) -> Option<PathBuf> {
    match config
        .path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(path) => Some(PathBuf::from(shellexpand::tilde(path).to_string())),
        None => dirs::home_dir().map(|home| home.join(".ra").join("skill-registry")),
    }
}

fn git_head_revision(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!revision.is_empty()).then_some(revision)
}

fn resolve_skills(mut skills: Vec<Skill>) -> Vec<Skill> {
    skills.sort_by(|a, b| {
        a.source
            .precedence()
            .cmp(&b.source.precedence())
            .then_with(|| a.command_name.cmp(&b.command_name))
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut seen = std::collections::HashSet::new();
    let mut resolved = Vec::new();
    for skill in skills {
        let key = normalize_name(&skill.command_name);
        if seen.insert(key) {
            resolved.push(skill);
        }
    }
    resolved.sort_by(|a, b| a.command_name.cmp(&b.command_name));
    resolved
}

fn normalized_name_set(values: &[String]) -> std::collections::HashSet<String> {
    values.iter().map(|s| normalize_name(s)).collect()
}

fn normalize_name(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub fn discover_project_skill_globs(cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for dir in project_walk_dirs(cwd) {
        out.push(
            dir.join(".ra/skills/**/SKILL.md")
                .to_string_lossy()
                .into_owned(),
        );
        out.push(
            dir.join(".agents/skills/**/SKILL.md")
                .to_string_lossy()
                .into_owned(),
        );
        out.push(
            dir.join(".claude/skills/**/SKILL.md")
                .to_string_lossy()
                .into_owned(),
        );
    }
    out
}

pub fn load_prompts(patterns: &[String]) -> Vec<PromptTemplate> {
    expand_globs(patterns, "prompts")
        .into_iter()
        .filter_map(|p| {
            let body = std::fs::read_to_string(&p).ok()?;
            let name = p.file_stem()?.to_str()?.to_string();
            Some(PromptTemplate {
                name,
                path: p,
                body,
            })
        })
        .collect()
}

/// Walk from `cwd` up to (and including) the git root, collecting every
/// `AGENTS.md` we find. Returns root-first ordering by `depth`.
pub fn discover_agents_md(cwd: &Path) -> Vec<AgentsMd> {
    let mut out = Vec::new();
    let mut current: PathBuf = cwd.to_path_buf();
    let mut depth = 0usize;
    let mut hit_git_root = false;
    loop {
        let candidate = current.join("AGENTS.md");
        if let Ok(body) = std::fs::read_to_string(&candidate) {
            out.push(AgentsMd {
                path: candidate,
                body,
                depth,
            });
        }
        if current.join(".git").exists() {
            hit_git_root = true;
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
        depth += 1;
        if depth > 16 {
            // Safety: don't walk forever on weird filesystems.
            break;
        }
    }
    let _ = hit_git_root;
    out
}

pub fn load_plain_resources(paths: &[String]) -> Vec<PlainResource> {
    let mut out = Vec::new();
    for raw in paths {
        let expanded = shellexpand::tilde(raw).to_string();
        let path = PathBuf::from(expanded);
        match std::fs::read_to_string(&path) {
            Ok(body) => out.push(PlainResource { path, body }),
            Err(e) => eprintln!("[ra::resources] {}: {e}", path.display()),
        }
    }
    out
}

// ---------- frontmatter -------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    when_to_use: Option<String>,
    license: Option<String>,
    compatibility: Option<String>,
    #[serde(rename = "argument-hint")]
    #[allow(dead_code)]
    argument_hint: Option<String>,
    arguments: Option<serde_yaml::Value>,
    #[serde(rename = "disable-model-invocation")]
    disable_model_invocation: Option<bool>,
    #[serde(rename = "user-invocable")]
    user_invocable: Option<bool>,
    #[serde(rename = "allowed-tools")]
    allowed_tools: Option<serde_yaml::Value>,
    #[serde(rename = "disallowed-tools")]
    disallowed_tools: Option<serde_yaml::Value>,
    model: Option<String>,
    effort: Option<String>,
    context: Option<String>,
    agent: Option<String>,
    hooks: Option<HooksSection>,
    #[allow(dead_code)]
    paths: Option<serde_yaml::Value>,
    shell: Option<String>,
    metadata: Option<SkillMetadata>,
}

#[derive(Debug, Default, Deserialize)]
struct SkillMetadata {
    internal: Option<bool>,
}

fn parse_skill(p: &Path) -> Result<Skill> {
    let raw = std::fs::read_to_string(p).context("read")?;
    parse_skill_document(&raw, p, SkillSource::Global, None)
}

fn parse_skill_with_source(
    p: &Path,
    source: SkillSource,
    source_revision: Option<String>,
) -> Result<Skill> {
    let raw = std::fs::read_to_string(p).context("read")?;
    parse_skill_document(&raw, p, source, source_revision)
}

fn parse_skill_document(
    raw: &str,
    p: &Path,
    source: SkillSource,
    source_revision: Option<String>,
) -> Result<Skill> {
    let (fm_raw, body) =
        split_frontmatter(&raw).with_context(|| "missing or malformed YAML frontmatter")?;
    let fm: Frontmatter = serde_yaml::from_str(fm_raw).with_context(|| "parse YAML frontmatter")?;
    let command_name = skill_command_name(p)?;
    let name = fm.name.clone().unwrap_or_else(|| command_name.clone());
    let description = skill_description(&fm, body);
    let arguments = parse_arguments(fm.arguments.as_ref());
    let runtime = SkillRuntimeOptions {
        model: normalize_opt_string(fm.model),
        effort: normalize_opt_string(fm.effort),
        context: normalize_opt_string(fm.context),
        agent: normalize_opt_string(fm.agent),
        shell: normalize_opt_string(fm.shell),
        allowed_tools: parse_tool_list(fm.allowed_tools.as_ref()),
        disallowed_tools: parse_tool_list(fm.disallowed_tools.as_ref()),
        hooks: fm.hooks.unwrap_or_default(),
    };
    Ok(Skill {
        command_name,
        name,
        description,
        when_to_use: fm.when_to_use,
        arguments,
        compatibility: fm.compatibility,
        license: fm.license,
        runtime,
        disable_model_invocation: fm.disable_model_invocation.unwrap_or(false),
        user_invocable: fm.user_invocable.unwrap_or(true),
        path: p.to_path_buf(),
        source,
        source_revision,
        internal: fm
            .metadata
            .as_ref()
            .and_then(|m| m.internal)
            .unwrap_or(false),
        body: body.to_string(),
    })
}

fn normalize_opt_string(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn parse_tool_list(raw: Option<&serde_yaml::Value>) -> Vec<String> {
    match raw {
        Some(serde_yaml::Value::String(s)) => split_tool_list(s),
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .flat_map(split_tool_list)
            .collect(),
        _ => Vec::new(),
    }
}

fn split_tool_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|tool| !tool.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_arguments(raw: Option<&serde_yaml::Value>) -> Vec<String> {
    match raw {
        Some(serde_yaml::Value::String(s)) => s
            .split_whitespace()
            .filter(|arg| !arg.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::trim)
            .filter(|arg| !arg.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn skill_command_name(p: &Path) -> Result<String> {
    p.parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("cannot derive skill command name from path"))
}

fn skill_description(fm: &Frontmatter, body: &str) -> String {
    let mut description = fm
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| first_markdown_paragraph(body).unwrap_or_default());
    if let Some(when) = fm
        .when_to_use
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !description.is_empty() {
            description.push(' ');
        }
        description.push_str(when);
    }
    description
}

fn first_markdown_paragraph(body: &str) -> Option<String> {
    let mut paragraph: Vec<String> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if trimmed.starts_with('#') && paragraph.is_empty() {
            continue;
        }
        paragraph.push(trimmed.to_string());
    }
    if paragraph.is_empty() {
        None
    } else {
        Some(paragraph.join(" "))
    }
}

/// Split `--- ... ---` YAML frontmatter from the body. Returns `(yaml,
/// body)`. None if no leading `---` on the first non-empty line.
fn split_frontmatter(s: &str) -> Option<(&str, &str)> {
    // Skip BOM and leading whitespace.
    let s = s.strip_prefix('\u{FEFF}').unwrap_or(s);
    let s = s.trim_start_matches([' ', '\t', '\r']);
    let after_first = s
        .strip_prefix("---\n")
        .or_else(|| s.strip_prefix("---\r\n"))?;
    // Find the closing `---` on its own line.
    for (i, _) in after_first.match_indices("\n---") {
        // Ensure it's `\n---` followed by newline or EOF.
        let close_end = i + 4;
        let rest = &after_first[close_end..];
        if rest.is_empty() || rest.starts_with('\n') || rest.starts_with("\r\n") {
            let yaml = &after_first[..i];
            let body_start = if rest.starts_with("\r\n") {
                close_end + 2
            } else if rest.starts_with('\n') {
                close_end + 1
            } else {
                close_end
            };
            return Some((yaml, &after_first[body_start..]));
        }
    }
    None
}

// ---------- glob expansion ----------------------------------------------

fn expand_globs(patterns: &[String], category: &str) -> Vec<PathBuf> {
    if patterns.is_empty() {
        return Vec::new();
    }
    let mut builder = GlobSetBuilder::new();
    let mut search_roots: Vec<PathBuf> = Vec::new();
    for p in patterns {
        let expanded = shellexpand::tilde(p).to_string();
        let root = glob_root(&expanded);
        search_roots.push(root);
        match Glob::new(&expanded) {
            Ok(g) => {
                builder.add(g);
            }
            Err(e) => {
                eprintln!("[ra::{category}] bad glob '{p}': {e}");
            }
        }
    }
    let set = match builder.build() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[ra::{category}] globset build failed: {e}");
            return Vec::new();
        }
    };
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in search_roots {
        for entry in walk_files(&root) {
            if !seen.insert(entry.clone()) {
                continue;
            }
            if set.is_match(&entry) {
                out.push(entry);
            }
        }
    }
    out.sort();
    out
}

fn project_walk_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut current: PathBuf = cwd.to_path_buf();
    let mut depth = 0usize;
    loop {
        out.push(current.clone());
        if current.join(".git").exists() {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
        depth += 1;
        if depth > 16 {
            break;
        }
    }
    out
}

fn glob_root(pat: &str) -> PathBuf {
    let mut root = PathBuf::new();
    for component in Path::new(pat).components() {
        let s = component.as_os_str().to_string_lossy();
        if s.contains('*') || s.contains('?') || s.contains('[') {
            break;
        }
        root.push(component);
    }
    if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else if root.is_file() {
        root.parent()
            .map(PathBuf::from)
            .unwrap_or(PathBuf::from("."))
    } else {
        root
    }
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                out.push(p);
            }
        }
    }
    out
}

fn ensure_trailing_blank(buf: &mut String) {
    if !buf.ends_with("\n\n") {
        if !buf.ends_with('\n') {
            buf.push('\n');
        }
        buf.push('\n');
    }
}

/// Build the full [`ResourceBundle`] from config: skills, prompts,
/// AGENTS.md, OpenSpec, and plain `[resources]`. Shared by every entry
/// point (`run` / `acp` / `serve` via `main`, and `tui`) so they all
/// surface the same context — previously the TUI hand-rolled a subset
/// and silently dropped OpenSpec.
///
/// `emit_logs` controls the `[ra] …` stderr breadcrumbs (on for the CLI
/// paths, off for the TUI where stderr is the alternate screen).
pub fn build_resource_bundle(config: &crate::config::RaConfig, emit_logs: bool) -> ResourceBundle {
    let log = |msg: &str| {
        if emit_logs {
            eprintln!("{msg}");
        }
    };
    let mut bundle = ResourceBundle::default();

    if config.skills.enabled {
        let mut skills = Vec::new();
        if config.skills.discover {
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            skills.extend(load_skills_with_source(
                &project_skill_globs(&cwd),
                SkillSource::Project,
                None,
            ));
            skills.extend(load_skills_with_source(
                &global_skill_globs(),
                SkillSource::Global,
                None,
            ));
        }
        skills.extend(load_skills_with_source(
            &config.skills.paths,
            SkillSource::Explicit,
            None,
        ));
        skills.extend(load_registry_skills(&config.skills.registry));
        skills.extend(load_builtin_skills(&config.skills.builtin));
        bundle.skills = resolve_skills(skills);
        if !bundle.skills.is_empty() {
            log(&format!("[ra] loaded {} skill(s)", bundle.skills.len()));
        }
    }
    if config.prompts.enabled {
        bundle.prompts = load_prompts(&config.prompts.paths);
        if !bundle.prompts.is_empty() {
            log(&format!(
                "[ra] loaded {} prompt template(s)",
                bundle.prompts.len()
            ));
        }
    }
    if config.agents_md.enabled {
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        bundle.agents_md = discover_agents_md(&cwd);
        if !bundle.agents_md.is_empty() {
            log(&format!(
                "[ra] discovered {} AGENTS.md file(s)",
                bundle.agents_md.len()
            ));
        }
    }
    if config.openspec.enabled {
        load_openspec_into(&mut bundle, config, &log);
    }

    let mut plain_paths = Vec::new();
    if let Some(p) = &config.resources.system_prompt_path {
        plain_paths.push(p.clone());
    }
    plain_paths.extend(config.resources.append_system_prompt_paths.clone());
    if !plain_paths.is_empty() {
        bundle.plain = load_plain_resources(&plain_paths);
        if !bundle.plain.is_empty() {
            log(&format!(
                "[ra] loaded {} plain resource(s)",
                bundle.plain.len()
            ));
        }
    }

    bundle
}

/// OpenSpec discovery + agent-own / bootstrap wiring. A discovered
/// `openspec/` directory is always treated as an *initialized* project —
/// even one with empty `specs/`/`changes/` (exactly what `openspec init`
/// produces) — so the agent-own playbook renders and the agent can create
/// its first change. The bootstrap hint fires only when no directory
/// exists at all.
fn load_openspec_into(
    bundle: &mut ResourceBundle,
    config: &crate::config::RaConfig,
    log: &dyn Fn(&str),
) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let project = match &config.openspec.path {
        // Explicit override: load exactly that dir if it exists.
        Some(p) => {
            let dir = crate::config::RaConfig::expand_path(p, &cwd);
            if dir.is_dir() {
                Some(crate::openspec::load(&dir))
            } else {
                log(&format!(
                    "[ra] openspec.path {} is not a directory; skipping",
                    dir.display()
                ));
                None
            }
        }
        // Default: walk cwd → git root for an `openspec/` dir.
        None => crate::openspec::discover(&cwd),
    };
    match project {
        // A directory was found → initialized project (even if empty).
        Some(mut p) => {
            p.agent_own = config.openspec.agent_own;
            log(&format!(
                "[ra] discovered OpenSpec project at {} ({} spec(s), {} active change(s))",
                p.root.display(),
                p.specs.len(),
                p.changes.len()
            ));
            bundle.openspec = Some(p);
        }
        // No directory at all → offer the bootstrap hint when agent-own.
        None => {
            if config.openspec.agent_own {
                bundle.openspec_bootstrap = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_basic() {
        let s = "---\nname: pdf-processing\ndescription: Process PDFs.\n---\nbody here\n";
        let (fm, body) = split_frontmatter(s).unwrap();
        assert!(fm.contains("pdf-processing"));
        assert_eq!(body, "body here\n");
    }

    #[test]
    fn parse_frontmatter_crlf() {
        let s = "---\r\nname: x\r\ndescription: y\r\n---\r\nbody\r\n";
        let (fm, body) = split_frontmatter(s).unwrap();
        assert!(fm.contains("name: x"));
        assert_eq!(body, "body\r\n");
    }

    #[test]
    fn parse_skill_full() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("SKILL.md");
        std::fs::write(
            &p,
            "---\nname: code-review\ndescription: Review pull requests carefully.\nlicense: Apache-2.0\n---\n# steps\n1. read diff\n",
        )
        .unwrap();
        let skill = parse_skill(&p).unwrap();
        assert_eq!(skill.name, "code-review");
        assert_eq!(skill.description, "Review pull requests carefully.");
        assert_eq!(skill.license.as_deref(), Some("Apache-2.0"));
        assert!(skill.body.contains("read diff"));
    }

    #[test]
    fn glob_root_extracts_fixed_prefix() {
        assert_eq!(glob_root("/tmp/a/*.md"), PathBuf::from("/tmp/a"));
        assert_eq!(glob_root("./skills/**/SKILL.md"), PathBuf::from("./skills"));
        assert_eq!(glob_root("*.md"), PathBuf::from("."));
    }
}
