//! Native OpenSpec discovery: confirm Ra picks up a project's
//! `openspec/` directory, walking cwd → git root like AGENTS.md, and
//! folds a progressive-disclosure catalog into the system prompt.

use std::fs;

use ra::openspec;
use ra::skills::ResourceBundle;
use tempfile::TempDir;

fn write(root: &std::path::Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// Build a representative OpenSpec layout under `root`.
fn scaffold(root: &std::path::Path) {
    // git-root marker so discovery has a stop boundary.
    fs::create_dir_all(root.join(".git")).unwrap();

    write(
        root,
        "openspec/project.md",
        "# Demo Project\n\nTech stack: Rust.\n",
    );

    write(
        root,
        "openspec/specs/user-auth/spec.md",
        "# User Auth Specification\n\n\
         ## Purpose\nHandle authentication and sessions.\n\n\
         ## Requirements\n\
         ### Requirement: Login\nThe system SHALL authenticate users.\n\n\
         #### Scenario: Valid credentials\n- **WHEN** valid\n- **THEN** allow\n\n\
         #### Scenario: Invalid credentials\n- **WHEN** invalid\n- **THEN** deny\n\n\
         ### Requirement: Session Expiry\nThe system SHALL expire idle sessions.\n\n\
         #### Scenario: Idle timeout\n- **WHEN** idle 30m\n- **THEN** expire\n",
    );

    write(
        root,
        "openspec/changes/add-dark-mode/proposal.md",
        "## Why\nUsers asked for a dark theme to reduce eye strain.\n\n\
         ## What Changes\n- add a theme toggle\n",
    );
    write(
        root,
        "openspec/changes/add-dark-mode/tasks.md",
        "## 1. UI\n- [x] 1.1 add toggle\n- [ ] 1.2 persist preference\n- [ ] 1.3 docs\n",
    );
    write(
        root,
        "openspec/changes/add-dark-mode/specs/theming/spec.md",
        "## ADDED Requirements\n### Requirement: Theme Toggle\nThe app SHALL support a dark theme.\n",
    );

    // An archived change that must be ignored.
    write(
        root,
        "openspec/changes/archive/2025-01-01-bootstrap/proposal.md",
        "## Why\nInitial scaffold.\n",
    );
}

#[test]
fn discovery_walks_up_and_parses_specs_and_changes() {
    let tmp = TempDir::new().unwrap();
    scaffold(tmp.path());

    // Discover from a nested working directory, not the project root.
    let nested = tmp.path().join("crates").join("app").join("src");
    fs::create_dir_all(&nested).unwrap();

    let project = openspec::discover(&nested).expect("should discover openspec/ walking up");

    assert!(project.project_md.is_some(), "project.md picked up");

    assert_eq!(project.specs.len(), 1);
    let spec = &project.specs[0];
    assert_eq!(spec.id, "user-auth");
    assert_eq!(spec.requirement_count, 2);
    assert_eq!(spec.scenario_count, 3);
    assert_eq!(
        spec.purpose.as_deref(),
        Some("Handle authentication and sessions.")
    );

    assert_eq!(project.changes.len(), 1, "archive/ must be excluded");
    let change = &project.changes[0];
    assert_eq!(change.id, "add-dark-mode");
    assert_eq!((change.tasks_done, change.tasks_total), (1, 3));
    assert!(!change.is_complete());
    assert_eq!(
        change.why.as_deref(),
        Some("Users asked for a dark theme to reduce eye strain.")
    );
    assert_eq!(change.delta_capabilities, vec!["theming".to_string()]);
}

#[test]
fn openspec_folds_into_resource_bundle_system_prompt() {
    let tmp = TempDir::new().unwrap();
    scaffold(tmp.path());

    let project = openspec::discover(tmp.path()).unwrap();
    let bundle = ResourceBundle {
        openspec: Some(project),
        ..Default::default()
    };

    let prompt = bundle
        .build_system_prompt()
        .expect("openspec alone should produce a system prompt");

    // Heading + the high-signal catalog entries.
    assert!(prompt.contains("# OpenSpec"));
    assert!(prompt.contains("user-auth"));
    assert!(prompt.contains("2 requirement(s), 3 scenario(s)"));
    assert!(prompt.contains("add-dark-mode"));
    assert!(prompt.contains("tasks: 1/3"));
    assert!(prompt.contains("touches: theming"));
    // Progressive disclosure: instruct the model to read the files.
    assert!(prompt.contains("read"));
    assert!(prompt.contains("spec.md"));
    // agent_own defaults on through discover() → the autonomous playbook
    // is folded in after the catalog.
    assert!(prompt.contains("agent-own SDD"));
    assert!(prompt.contains("openspec init --tools"));
    assert!(prompt.contains("archive <name> -y"));
}

#[test]
fn agent_own_disabled_keeps_catalog_drops_playbook() {
    let tmp = TempDir::new().unwrap();
    scaffold(tmp.path());

    let mut project = openspec::discover(tmp.path()).unwrap();
    project.agent_own = false;
    let bundle = ResourceBundle {
        openspec: Some(project),
        ..Default::default()
    };

    let prompt = bundle.build_system_prompt().unwrap();
    // Catalog still present…
    assert!(prompt.contains("# OpenSpec"));
    assert!(prompt.contains("user-auth"));
    // …playbook gone.
    assert!(!prompt.contains("agent-own SDD"));
    assert!(!prompt.contains("No human in the loop"));
}

#[test]
fn no_openspec_directory_means_no_section() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();

    assert!(openspec::discover(tmp.path()).is_none());

    // An empty bundle still yields no prompt.
    let bundle = ResourceBundle::default();
    assert!(bundle.build_system_prompt().is_none());
}

#[test]
fn bootstrap_hint_when_enabled_and_no_project() {
    // Greenfield repo: no openspec/, but agent-own SDD is on. The bundle
    // should fold in the bootstrap nudge so the agent knows it can init.
    let bundle = ResourceBundle {
        openspec_bootstrap: true,
        ..Default::default()
    };
    let prompt = bundle
        .build_system_prompt()
        .expect("bootstrap hint alone should produce a prompt");
    assert!(prompt.contains("OpenSpec (not yet initialized)"));
    assert!(prompt.contains("openspec init --tools"));
    // It must NOT pretend a project exists.
    assert!(!prompt.contains("Capability specs"));
}

#[test]
fn bootstrap_hint_suppressed_when_project_present() {
    // When a real project is discovered, the catalog/playbook render and
    // the bootstrap nudge must not (even if the flag is set).
    let tmp = TempDir::new().unwrap();
    scaffold(tmp.path());
    let project = openspec::discover(tmp.path()).unwrap();
    let bundle = ResourceBundle {
        openspec: Some(project),
        openspec_bootstrap: true, // ignored because openspec is Some
        ..Default::default()
    };
    let prompt = bundle.build_system_prompt().unwrap();
    assert!(prompt.contains("# OpenSpec\n"));
    assert!(!prompt.contains("not yet initialized"));
}
