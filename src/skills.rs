//! Resource loading: skills, prompts, AGENTS.md, plain system prompts.
//!
//! Three external specs feed into one unified `ResourceBundle`:
//!
//! - **agentskills.io v1** — `SKILL.md` with YAML frontmatter
//!   (name + description required; license/compatibility/metadata/
//!   allowed-tools optional). Progressive-disclosure: only the
//!   description goes into the system prompt at startup; the LLM is
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

use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// One agent skill following the agentskills.io v1 spec.
#[derive(Debug, Clone)]
pub struct Skill {
    /// `name:` from frontmatter; lowercased dash-only identifier.
    pub name: String,
    /// `description:` from frontmatter; the LLM-visible blurb.
    pub description: String,
    /// Optional `compatibility:` field.
    pub compatibility: Option<String>,
    /// Optional `license:` field.
    pub license: Option<String>,
    /// Path to the SKILL.md file on disk.
    pub path: PathBuf,
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
        if !self.skills.is_empty() {
            buf.push_str("# Skills\n\n");
            buf.push_str(
                "The following skills are available. Each entry shows its name and a one-paragraph \
                 description. To use a skill, call the `read` tool on its path to load full \
                 instructions before acting.\n\n",
            );
            for s in &self.skills {
                buf.push_str(&format!(
                    "## {}\n- description: {}\n- path: `{}`\n",
                    s.name,
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
    pub fn prompt_map(&self) -> std::collections::HashMap<String, String> {
        self.prompts
            .iter()
            .map(|p| (p.name.clone(), p.body.clone()))
            .collect()
    }
}

// ---------- loaders -----------------------------------------------------

pub fn load_skills(patterns: &[String]) -> Vec<Skill> {
    expand_globs(patterns, "skills")
        .into_iter()
        .filter_map(|p| match parse_skill(&p) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("[ra::skills] {}: {e:#}", p.display());
                None
            }
        })
        .collect()
}

/// The default skill-discovery globs Ra scans when
/// `[skills] discover = true`. Deliberately small: just Ra's own
/// project + global folder, plus the cross-agent `./.agents/skills/`
/// + `~/.agents/skills/` layout. Anything else (per-agent
/// `.claude/skills/`, catalog-style `skills/.curated/`, …) goes
/// in `[skills] paths` explicitly so the discovery surface stays
/// predictable.
pub fn default_discover_globs() -> Vec<String> {
    vec![
        "./.ra/skills/**/SKILL.md".to_string(),
        "~/.ra/skills/**/SKILL.md".to_string(),
        "./.agents/skills/**/SKILL.md".to_string(),
        "~/.agents/skills/**/SKILL.md".to_string(),
    ]
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
    license: Option<String>,
    compatibility: Option<String>,
    #[serde(rename = "allowed-tools")]
    #[allow(dead_code)]
    allowed_tools: Option<String>,
}

fn parse_skill(p: &Path) -> Result<Skill> {
    let raw = std::fs::read_to_string(p).context("read")?;
    let (fm_raw, body) =
        split_frontmatter(&raw).with_context(|| "missing or malformed YAML frontmatter")?;
    let fm: Frontmatter = serde_yaml::from_str(fm_raw).with_context(|| "parse YAML frontmatter")?;
    let name = fm
        .name
        .ok_or_else(|| anyhow::anyhow!("frontmatter missing required `name`"))?;
    let description = fm
        .description
        .ok_or_else(|| anyhow::anyhow!("frontmatter missing required `description`"))?;
    Ok(Skill {
        name,
        description,
        compatibility: fm.compatibility,
        license: fm.license,
        path: p.to_path_buf(),
        body: body.to_string(),
    })
}

/// Split `--- ... ---` YAML frontmatter from the body. Returns `(yaml,
/// body)`. None if no leading `---` on the first non-empty line.
fn split_frontmatter(s: &str) -> Option<(&str, &str)> {
    // Skip BOM and leading whitespace.
    let s = s.strip_prefix('\u{FEFF}').unwrap_or(s);
    let s = s.trim_start_matches(|c: char| c == ' ' || c == '\t' || c == '\r');
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
        let mut all_patterns: Vec<String> = Vec::new();
        if config.skills.discover {
            all_patterns.extend(default_discover_globs());
        }
        all_patterns.extend(config.skills.paths.iter().cloned());
        if !all_patterns.is_empty() {
            bundle.skills = load_skills(&all_patterns);
            if !bundle.skills.is_empty() {
                log(&format!("[ra] loaded {} skill(s)", bundle.skills.len()));
            }
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
