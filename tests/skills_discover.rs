//! Confirm that `[skills] discover = true` (the default) picks up
//! SKILL.md files from the standard skills.sh / cross-agent layouts
//! without any explicit `paths` entry.

use std::fs;
use ra::skills::{default_discover_globs, load_skills};
use tempfile::TempDir;

fn write_skill(root: &std::path::Path, slug: &str, body: &str) {
    let dir = root.join(slug);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: {slug}\ndescription: test skill {slug}\n---\n\n{body}\n"
        ),
    )
    .unwrap();
}

#[test]
fn default_discovery_finds_ra_and_agents_layouts() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    // Ra-native and the cross-agent `./.agents/` layout — those are
    // the only two project-relative roots we discover by default.
    write_skill(&cwd.join(".ra/skills"), "alpha", "Ra-native");
    write_skill(&cwd.join(".agents/skills"), "beta", "Cross-agent");
    // A per-agent folder we deliberately do NOT auto-discover; the
    // user has to add it via `[skills] paths` if they want it.
    write_skill(&cwd.join(".claude/skills"), "ignored", "Should not appear");

    std::env::set_current_dir(cwd).unwrap();

    let globs = default_discover_globs();
    let skills = load_skills(&globs);
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();

    for expected in ["alpha", "beta"] {
        assert!(
            names.contains(&expected),
            "expected discovery to find skill {expected:?}, got {:?}",
            names
        );
    }
    assert!(
        !names.contains(&"ignored"),
        ".claude/skills/ should NOT be in the default discovery set; \
         got {:?}",
        names
    );
}
