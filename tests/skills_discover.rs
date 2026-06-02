//! Confirm that `[skills] discover = true` (the default) picks up
//! SKILL.md files from Ra, universal/cross-agent, and Claude Code
//! layouts without any explicit `paths` entry.

use ra::{
    config::RaConfig,
    skills::{build_resource_bundle, default_discover_globs, load_skills, ResourceBundle},
};
use std::fs;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use tempfile::TempDir;

fn cwd_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn write_skill(root: &std::path::Path, slug: &str, body: &str) {
    let dir = root.join(slug);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {slug}\ndescription: test skill {slug}\n---\n\n{body}\n"),
    )
    .unwrap();
}

fn git_commit_all(dir: &std::path::Path) -> String {
    init_git_repo(dir);
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-qm", "add skills"])
        .current_dir(dir)
        .status()
        .unwrap();
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn init_git_repo(dir: &std::path::Path) {
    Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "ra@example.invalid"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Ra Test"])
        .current_dir(dir)
        .status()
        .unwrap();
}

#[test]
fn default_discovery_finds_ra_and_agents_layouts() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    // Ra-native, universal/cross-agent, and Claude Code project layouts.
    write_skill(&cwd.join(".ra/skills"), "alpha", "Ra-native");
    write_skill(&cwd.join(".agents/skills"), "beta", "Cross-agent");
    write_skill(&cwd.join(".claude/skills"), "gamma", "Claude Code");

    std::env::set_current_dir(cwd).unwrap();

    let globs = default_discover_globs();
    assert!(
        globs.iter().any(|glob| glob.contains("~/.codex/skills")),
        "global Codex skills installed by npx skills should be discoverable"
    );
    let skills = load_skills(&globs);
    let names: Vec<&str> = skills.iter().map(|s| s.command_name.as_str()).collect();

    for expected in ["alpha", "beta", "gamma"] {
        assert!(
            names.contains(&expected),
            "expected discovery to find skill {expected:?}, got {:?}",
            names
        );
    }
}

#[test]
fn build_resource_bundle_loads_builtin_skills_by_default() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();

    let bundle = build_resource_bundle(&RaConfig::default(), false);
    let review = bundle
        .skills
        .iter()
        .find(|skill| skill.command_name == "review")
        .expect("builtin review skill should load by default");

    assert_eq!(review.source.as_str(), "builtin");
    assert!(bundle.prompt_map().contains_key("review"));
    assert!(bundle
        .build_system_prompt()
        .expect("skills prompt")
        .contains("Review local code changes"));
}

#[test]
fn project_skill_shadows_builtin_skill() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    write_skill(
        &cwd.join(".agents/skills"),
        "review",
        "Project review body.",
    );
    std::env::set_current_dir(cwd).unwrap();

    let bundle = build_resource_bundle(&RaConfig::default(), false);
    let review = bundle
        .skills
        .iter()
        .find(|skill| skill.command_name == "review")
        .expect("review skill should resolve");

    assert_eq!(review.source.as_str(), "project");
    assert_eq!(review.body.trim(), "Project review body.");
    assert_eq!(
        bundle
            .skills
            .iter()
            .filter(|skill| skill.command_name == "review")
            .count(),
        1,
        "only the active winner should be exposed"
    );
}

#[test]
fn builtin_skill_exclude_suppresses_builtin() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();

    let mut config = RaConfig::default();
    config.skills.builtin.exclude = vec!["review".to_string()];
    let bundle = build_resource_bundle(&config, false);

    assert!(
        !bundle
            .skills
            .iter()
            .any(|skill| skill.command_name == "review"),
        "excluded builtin skill should not load"
    );
}

#[test]
fn git_backed_registry_loads_skills_with_revision() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    let registry = tmp.path().join("registry");
    write_skill(
        &registry.join("skills"),
        "plan-review",
        "Registry skill body.",
    );
    let revision = git_commit_all(&registry);
    std::env::set_current_dir(&cwd).unwrap();

    let mut config = RaConfig::default();
    config.skills.builtin.enabled = false;
    config.skills.registry.path = Some(registry.to_string_lossy().into_owned());
    let bundle = build_resource_bundle(&config, false);
    let skill = bundle
        .skills
        .iter()
        .find(|skill| skill.command_name == "plan-review")
        .expect("registry skill should load");

    assert_eq!(skill.source.as_str(), "registry");
    assert_eq!(skill.source_revision.as_deref(), Some(revision.as_str()));
}

#[test]
fn registry_path_must_be_git_worktree_root() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let parent = tmp.path().join("parent");
    let registry = parent.join("registry");
    write_skill(&registry.join("skills"), "nested-registry", "Nested body.");
    git_commit_all(&parent);

    let cwd = tmp.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    std::env::set_current_dir(&cwd).unwrap();

    let mut config = RaConfig::default();
    config.skills.builtin.enabled = false;
    config.skills.registry.path = Some(registry.to_string_lossy().into_owned());
    let bundle = build_resource_bundle(&config, false);

    assert!(
        !bundle
            .skills
            .iter()
            .any(|skill| skill.command_name == "nested-registry"),
        "a registry subdirectory inside a parent repo must not be treated as its own versioned registry"
    );
}

#[test]
fn registry_skips_dirty_or_untracked_content() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let registry = tmp.path().join("registry");
    fs::create_dir_all(&registry).unwrap();
    init_git_repo(&registry);
    write_skill(
        &registry.join("skills"),
        "uncommitted-registry",
        "Uncommitted body.",
    );

    let cwd = tmp.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    std::env::set_current_dir(&cwd).unwrap();

    let mut config = RaConfig::default();
    config.skills.builtin.enabled = false;
    config.skills.registry.path = Some(registry.to_string_lossy().into_owned());
    let bundle = build_resource_bundle(&config, false);

    assert!(
        !bundle
            .skills
            .iter()
            .any(|skill| skill.command_name == "uncommitted-registry"),
        "registry skills must come from committed git content so HEAD provenance is meaningful"
    );
}

#[test]
fn claude_skill_optional_frontmatter_uses_fallbacks() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let skill_dir = cwd.join(".claude/skills/summarize-changes");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nwhen_to_use: Use when asked about current changes.\nallowed-tools: Bash(git *)\n---\n\n# Summary\n\nSummarize uncommitted changes.\n\nMore details stay in the body.\n",
    )
    .unwrap();

    std::env::set_current_dir(cwd).unwrap();

    let skills = load_skills(&default_discover_globs());
    let skill = skills
        .iter()
        .find(|s| s.command_name == "summarize-changes")
        .expect("expected Claude Code skill to load");

    assert_eq!(skill.name, "summarize-changes");
    assert!(
        skill
            .description
            .contains("Summarize uncommitted changes. Use when asked about current changes."),
        "description should fall back to the first paragraph and append when_to_use, got {:?}",
        skill.description
    );
}

#[test]
fn model_and_user_invocation_visibility_are_separate() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    write_skill(
        &cwd.join(".claude/skills"),
        "visible",
        "Visible body that should be listed and invocable.",
    );
    fs::write(
        cwd.join(".claude/skills/visible/SKILL.md"),
        "---\ndescription: Visible skill\n---\n\nVisible body.\n",
    )
    .unwrap();

    let hidden_model = cwd.join(".claude/skills/manual-only");
    fs::create_dir_all(&hidden_model).unwrap();
    fs::write(
        hidden_model.join("SKILL.md"),
        "---\ndescription: Manual only\ndisable-model-invocation: true\n---\n\nManual body.\n",
    )
    .unwrap();

    let hidden_user = cwd.join(".claude/skills/background");
    fs::create_dir_all(&hidden_user).unwrap();
    fs::write(
        hidden_user.join("SKILL.md"),
        "---\ndescription: Background knowledge\nuser-invocable: false\n---\n\nBackground body.\n",
    )
    .unwrap();

    std::env::set_current_dir(cwd).unwrap();

    let bundle = ResourceBundle {
        skills: load_skills(&default_discover_globs()),
        ..ResourceBundle::default()
    };
    let prompt = bundle.build_system_prompt().expect("skills prompt");
    assert!(prompt.contains("Visible skill"));
    assert!(prompt.contains("Background knowledge"));
    assert!(
        !prompt.contains("Manual only"),
        "disable-model-invocation must hide from model-facing catalog"
    );

    let slash = bundle.prompt_map();
    assert!(slash.contains_key("visible"));
    assert!(
        slash.contains_key("manual-only"),
        "model-disabled skills remain user-invocable by default"
    );
    assert!(
        !slash.contains_key("background"),
        "user-invocable=false must hide from slash map"
    );
}

#[test]
fn build_resource_bundle_discovers_project_skills_from_cwd_to_repo_root() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join(".git")).unwrap();
    let nested = root.join("packages/frontend/src");
    fs::create_dir_all(&nested).unwrap();
    write_skill(
        &root.join(".claude/skills"),
        "repo-root",
        "Repo root project skill.",
    );
    write_skill(
        &root.join("packages/frontend/.claude/skills"),
        "package",
        "Package project skill.",
    );

    std::env::set_current_dir(&nested).unwrap();

    let config = RaConfig::default();
    let bundle = build_resource_bundle(&config, false);
    let names: Vec<&str> = bundle
        .skills
        .iter()
        .map(|s| s.command_name.as_str())
        .collect();

    for expected in ["repo-root", "package"] {
        assert!(
            names.contains(&expected),
            "expected cwd→repo-root discovery to find {expected:?}, got {:?}",
            names
        );
    }
}

#[test]
fn prompt_map_preserves_skill_arguments_metadata() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let skill_dir = cwd.join(".claude/skills/migrate");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Migrate a component\narguments: [component, source, target]\n---\n\nMigrate $component from $source to $target.\n",
    )
    .unwrap();

    std::env::set_current_dir(cwd).unwrap();

    let bundle = ResourceBundle {
        skills: load_skills(&default_discover_globs()),
        ..ResourceBundle::default()
    };
    let slash = bundle.prompt_map();
    let template = slash.get("migrate").expect("migrate slash template");

    assert_eq!(
        template.arguments,
        vec![
            "component".to_string(),
            "source".to_string(),
            "target".to_string()
        ]
    );
    assert!(template.append_arguments_fallback);
}

#[test]
fn skill_parser_preserves_advanced_runtime_frontmatter() {
    let _guard = cwd_lock();
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let skill_dir = cwd.join(".claude/skills/advanced");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
description: Advanced skill
model: review-model
effort: high
context: fork
agent: Explore
shell: bash
allowed-tools: [read, "Bash(git status:*)"]
disallowed-tools: "write, edit"
hooks:
  PreToolUse:
    - matcher: bash
      command: "echo hook"
      timeout: 1
---

Advanced body.
"#,
    )
    .unwrap();

    std::env::set_current_dir(cwd).unwrap();

    let skills = load_skills(&default_discover_globs());
    let skill = skills
        .iter()
        .find(|s| s.command_name == "advanced")
        .expect("advanced skill should load");

    assert_eq!(skill.runtime.model.as_deref(), Some("review-model"));
    assert_eq!(skill.runtime.effort.as_deref(), Some("high"));
    assert_eq!(skill.runtime.context.as_deref(), Some("fork"));
    assert_eq!(skill.runtime.agent.as_deref(), Some("Explore"));
    assert!(skill.runtime.is_fork());
    assert_eq!(skill.runtime.shell.as_deref(), Some("bash"));
    assert_eq!(
        skill.runtime.allowed_tools,
        vec!["read".to_string(), "Bash(git status:*)".to_string()]
    );
    assert_eq!(
        skill.runtime.disallowed_tools,
        vec!["write".to_string(), "edit".to_string()]
    );
    assert_eq!(skill.runtime.hooks.pre_tool_use.len(), 1);

    let bundle = ResourceBundle {
        skills,
        ..ResourceBundle::default()
    };
    let slash = bundle.prompt_map();
    let template = slash.get("advanced").expect("advanced slash template");
    assert!(
        template.runtime.is_some(),
        "advanced runtime metadata must flow into slash templates"
    );
}
