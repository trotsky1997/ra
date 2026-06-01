//! Confirm that `[skills] discover = true` (the default) picks up
//! SKILL.md files from Ra, universal/cross-agent, and Claude Code
//! layouts without any explicit `paths` entry.

use ra::{
    config::RaConfig,
    skills::{
        build_resource_bundle, default_discover_globs, load_skills, ResourceBundle, SkillAgentMode,
    },
};
use std::fs;
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
context: Keep output concise.
agent: fork
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
    assert_eq!(
        skill.runtime.context.as_deref(),
        Some("Keep output concise.")
    );
    assert_eq!(skill.runtime.agent, Some(SkillAgentMode::Fork));
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
