//! Native [OpenSpec](https://github.com/Fission-AI/OpenSpec) discovery.
//!
//! OpenSpec is a lightweight spec-driven-development convention: a
//! project keeps an `openspec/` directory that captures the *current*
//! behaviour of the system as specs and the *proposed* behaviour as
//! changes. The layout (mirroring the upstream tool):
//!
//! ```text
//! openspec/
//! ├── project.md                     project context (free-form)
//! ├── specs/
//! │   └── <capability>/spec.md        current behaviour
//! │       ## Purpose
//! │       ## Requirements
//! │       ### Requirement: <name>     (SHALL / MUST statement)
//! │       #### Scenario: <name>       (WHEN / THEN bullets)
//! └── changes/
//!     ├── <change-id>/
//!     │   ├── proposal.md             ## Why / ## What Changes / ## Impact
//!     │   ├── tasks.md                markdown checkbox list
//!     │   └── specs/<capability>/spec.md   delta:
//!     │       ## ADDED Requirements
//!     │       ## MODIFIED Requirements
//!     │       ## REMOVED Requirements
//!     └── archive/                    landed changes (skipped by default)
//! ```
//!
//! Ra is the **consumer** of this convention, not a reimplementation
//! of the `openspec` CLI — same stance the project takes toward
//! skills.sh. We discover the nearest `openspec/` directory (walking
//! cwd → git root, like AGENTS.md), parse just enough to build a
//! high-signal catalog, and fold it into the system prompt with
//! progressive disclosure: the LLM sees capability names, requirement
//! counts, and the list of active changes with task progress up front,
//! and is told to `read` the underlying `spec.md` / `proposal.md` /
//! `tasks.md` when it needs the full text.

use std::path::{Path, PathBuf};

/// One capability spec under `openspec/specs/<id>/spec.md`.
#[derive(Debug, Clone)]
pub struct CapabilitySpec {
    /// Directory name, e.g. `user-auth`.
    pub id: String,
    /// Path to the `spec.md` file on disk.
    pub path: PathBuf,
    /// First paragraph under `## Purpose`, if present.
    pub purpose: Option<String>,
    /// Number of `### Requirement:` headings.
    pub requirement_count: usize,
    /// Number of `#### Scenario:` headings.
    pub scenario_count: usize,
}

/// One in-flight change under `openspec/changes/<id>/`.
#[derive(Debug, Clone)]
pub struct Change {
    /// Directory name, e.g. `add-dark-mode`.
    pub id: String,
    /// Directory holding `proposal.md`, `tasks.md`, `specs/…`.
    pub dir: PathBuf,
    /// `proposal.md` path, if it exists.
    pub proposal_path: Option<PathBuf>,
    /// First paragraph under `## Why`, summarising the proposal.
    pub why: Option<String>,
    /// `tasks.md` path, if it exists.
    pub tasks_path: Option<PathBuf>,
    /// Completed `- [x]` checkboxes in `tasks.md`.
    pub tasks_done: usize,
    /// Total checkboxes (`- [ ]` + `- [x]`) in `tasks.md`.
    pub tasks_total: usize,
    /// Capability ids this change carries a delta spec for, derived
    /// from `specs/<capability>/spec.md` subdirectories.
    pub delta_capabilities: Vec<String>,
}

impl Change {
    /// `true` when the change has tasks and every one is checked.
    pub fn is_complete(&self) -> bool {
        self.tasks_total > 0 && self.tasks_done == self.tasks_total
    }
}

/// The whole discovered `openspec/` project.
#[derive(Debug, Clone)]
pub struct OpenSpecProject {
    /// Path to the `openspec/` directory itself.
    pub root: PathBuf,
    /// `openspec/project.md`, if present and non-empty.
    pub project_md: Option<PlainDoc>,
    /// Capability specs, sorted by id.
    pub specs: Vec<CapabilitySpec>,
    /// Active (non-archived) changes, sorted by id.
    pub changes: Vec<Change>,
    /// When true, the prompt section appends the *agent-own* spec-driven
    /// playbook (non-interactive `openspec` CLI loop) after the catalog.
    /// Defaults to true from [`load`]; `main` overrides it from
    /// `[openspec] agent_own`.
    pub agent_own: bool,
}

/// A free-form markdown doc we point the LLM at (project.md).
#[derive(Debug, Clone)]
pub struct PlainDoc {
    pub path: PathBuf,
    pub body: String,
}

impl OpenSpecProject {
    /// `true` when nothing worth surfacing was found.
    pub fn is_empty(&self) -> bool {
        self.project_md.is_none() && self.specs.is_empty() && self.changes.is_empty()
    }

    /// Render the progressive-disclosure system-prompt section. Returns
    /// `None` when the project carries no signal. Mirrors the structure
    /// `ResourceBundle::build_system_prompt` uses for skills: a short
    /// catalog with `read`-the-file hints, never the full spec bodies.
    pub fn build_system_prompt_section(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut buf = String::new();
        buf.push_str("# OpenSpec\n\n");
        buf.push_str(&format!(
            "This project follows the OpenSpec spec-driven-development convention. \
             Its `openspec/` directory ({}) is the source of truth for *current* \
             behaviour (`specs/`) and *proposed* behaviour (`changes/`). Before \
             implementing a change, consult the relevant spec; when proposing one, \
             follow the existing format. Use the `read` tool on the paths below to \
             load full requirements, proposals, or task lists before acting.\n\n",
            self.root.display()
        ));

        if let Some(doc) = &self.project_md {
            buf.push_str(&format!(
                "## Project context\n- path: `{}`\n\n",
                doc.path.display()
            ));
        }

        if !self.specs.is_empty() {
            buf.push_str("## Capability specs (current behaviour)\n\n");
            for s in &self.specs {
                buf.push_str(&format!(
                    "- **{}** — {} requirement(s), {} scenario(s); path: `{}`\n",
                    s.id,
                    s.requirement_count,
                    s.scenario_count,
                    s.path.display()
                ));
                if let Some(p) = &s.purpose {
                    buf.push_str(&format!("  - purpose: {}\n", one_line(p)));
                }
            }
            buf.push('\n');
        }

        if !self.changes.is_empty() {
            buf.push_str("## Active changes (proposed behaviour)\n\n");
            for c in &self.changes {
                let progress = if c.tasks_total > 0 {
                    if c.is_complete() {
                        " — tasks: ✓ complete".to_string()
                    } else {
                        format!(" — tasks: {}/{}", c.tasks_done, c.tasks_total)
                    }
                } else {
                    String::new()
                };
                buf.push_str(&format!(
                    "- **{}**{progress}; dir: `{}`\n",
                    c.id,
                    c.dir.display()
                ));
                if let Some(why) = &c.why {
                    buf.push_str(&format!("  - why: {}\n", one_line(why)));
                }
                if !c.delta_capabilities.is_empty() {
                    buf.push_str(&format!(
                        "  - touches: {}\n",
                        c.delta_capabilities.join(", ")
                    ));
                }
            }
            buf.push('\n');
        }

        if self.agent_own {
            buf.push_str(&agent_own_playbook());
        }

        Some(buf)
    }
}

/// The *agent-own* spec-driven-development playbook: a compact, verified
/// recipe for driving the `openspec` CLI **non-interactively, with no
/// human in the loop**. Folded into the prompt after the catalog when
/// `[openspec] agent_own` is on (the default).
///
/// Ra stays a *consumer* of the convention — this is guidance pointing at
/// the upstream `openspec` CLI, not a reimplementation of it. The key
/// facts encoded here (each confirmed against `openspec` 1.3.x):
/// `init` is interactive unless `--tools` is passed; `status --json` is a
/// dependency state machine; `instructions --json` carries the per-step
/// template; `validate --strict` emits machine-actionable errors; and
/// `archive -y` promotes delta specs into `specs/` non-interactively.
fn agent_own_playbook() -> String {
    // Kept deliberately terse — progressive disclosure still applies; the
    // model `read`s the actual artifacts. This is the *control flow*, plus
    // the substitutions an unattended agent must make for the upstream
    // templates' "ask the user" stops.
    "## Driving OpenSpec autonomously (agent-own SDD)\n\n\
     You can run the whole spec-driven loop yourself via the `openspec` CLI \
     (through `bash`), with no slash commands and no interactive prompts. \
     The CLI is `--json`-first: treat it as a state machine.\n\n\
     - **State**: `openspec status --change <name> --json` → each artifact's \
     `status` (`ready`/`blocked`/`done`), its `missingDeps`, and \
     `applyRequires` (what must be `done` before implementing).\n\
     - **Per-step instructions**: `openspec instructions <artifact|apply> \
     --change <name> --json` → what to read (`dependencies`/`contextFiles`), \
     what to write (`template`, `instruction`), and where \
     (`resolvedOutputPath`). `context`/`rules` are constraints for *you* — \
     never copy them into the artifact file.\n\
     - **Self-correction**: `openspec validate <name> --strict` emits precise, \
     machine-actionable errors (e.g. a requirement missing a `#### Scenario:`); \
     fix the artifact and re-run instead of stopping.\n\n\
     **Loop** (all non-interactive):\n\
     1. Bootstrap once per project: `openspec init --tools <agent>` — bare \
     `openspec init` *prompts* for tools and will hang; always pass `--tools`. \
     (Existing project: `openspec update`, not re-init.)\n\
     2. `openspec new change <kebab-name>` (derive the name from the task).\n\
     3. While any artifact is `ready`: pull its `instructions --json`, read its \
     `dependencies`, write `resolvedOutputPath` from `template`; re-check \
     `status` until every `applyRequires` artifact is `done`.\n\
     4. Implement against `instructions apply --json` `contextFiles`; flip \
     `- [ ]` → `- [x]` in `tasks.md` as each task lands.\n\
     5. `openspec archive <name> -y` — promotes delta specs into `specs/` and \
     moves the change under `changes/archive/`. `-y` skips confirmation.\n\n\
     **No human in the loop**: the upstream command templates pause to ask the \
     user (requirement clarification, change selection). Replace those — derive \
     intent from the task/issue, auto-select when only one change is active, and \
     for non-critical ambiguity make a reasonable default and keep momentum \
     (record it in the proposal/design). Stop and escalate only on a genuine \
     blocker (design conflict, missing critical input, hard error) — never guess.\n\n"
        .to_string()
}

/// Bootstrap hint for a project that has **no** `openspec/` directory yet,
/// emitted when discovery is enabled and `agent_own` is on. Closes the
/// chicken-and-egg gap: the full playbook only renders once a project is
/// discovered, so on a greenfield repo the agent would otherwise never be
/// told it can adopt OpenSpec. This is a short, *opt-in* nudge — it does
/// not push the agent to init unprompted, only tells it how when a task
/// actually calls for spec-driven development.
pub fn bootstrap_prompt_section() -> String {
    "# OpenSpec (not yet initialized)\n\n\
     This project has no `openspec/` directory. If a task calls for \
     spec-driven development — durable, reviewable specs that outlive a \
     single session — you can adopt the \
     [OpenSpec](https://github.com/Fission-AI/OpenSpec) convention yourself, \
     non-interactively, via the `openspec` CLI (through `bash`):\n\n\
     - `openspec init --tools <agent>` — bootstrap once. Bare `openspec init` \
     *prompts* for tools and will hang; always pass `--tools` (e.g. `claude`, \
     or `all`). Creates `openspec/specs/` + `openspec/changes/`.\n\
     - Then drive it autonomously: `openspec new change <kebab-name>` → write \
     artifacts off `openspec status`/`instructions --change <name> --json` \
     until `applyRequires` is `done` → implement, flipping `- [ ]`→`- [x]` in \
     `tasks.md` → `openspec validate <name> --strict` → `openspec archive \
     <name> -y` (promotes deltas into `specs/`).\n\n\
     Adopt it only when the work warrants it — don't initialize OpenSpec for a \
     trivial one-off change. If the `openspec` CLI is not installed, treat this \
     as unavailable rather than a blocker.\n\n"
        .to_string()
}

/// Locate the nearest `openspec/` directory walking from `cwd` up to
/// (and including) the git root, then parse it. Returns `None` when no
/// such directory exists. "Nearest wins": the first `openspec/` found
/// walking upward is the one we load.
pub fn discover(cwd: &Path) -> Option<OpenSpecProject> {
    let root = find_openspec_dir(cwd)?;
    Some(load(&root))
}

/// Walk cwd → git root looking for a directory literally named
/// `openspec`. Stops at the git root (inclusive) or the filesystem
/// root, whichever comes first, with the same 16-level safety cap as
/// AGENTS.md discovery.
fn find_openspec_dir(cwd: &Path) -> Option<PathBuf> {
    let mut current = cwd.to_path_buf();
    let mut depth = 0usize;
    loop {
        let candidate = current.join("openspec");
        if candidate.is_dir() {
            return Some(candidate);
        }
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
    None
}

/// Parse an `openspec/` directory into a project. Tolerant of missing
/// pieces: a directory with only `specs/` (and no changes) is valid, as
/// is one with only `changes/`.
pub fn load(root: &Path) -> OpenSpecProject {
    let project_md = load_project_md(&root.join("project.md"));
    let specs = load_specs(&root.join("specs"));
    let changes = load_changes(&root.join("changes"));
    OpenSpecProject {
        root: root.to_path_buf(),
        project_md,
        specs,
        changes,
        agent_own: true,
    }
}

fn load_project_md(path: &Path) -> Option<PlainDoc> {
    let body = std::fs::read_to_string(path).ok()?;
    if body.trim().is_empty() {
        return None;
    }
    Some(PlainDoc {
        path: path.to_path_buf(),
        body,
    })
}

/// Load every `openspec/specs/<id>/spec.md`. Each immediate subdirectory
/// of `specs/` is a capability whose `spec.md` is parsed for counts.
fn load_specs(specs_dir: &Path) -> Vec<CapabilitySpec> {
    let mut out = Vec::new();
    for id in child_dir_names(specs_dir) {
        let path = specs_dir.join(&id).join("spec.md");
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (requirement_count, scenario_count) = count_requirements(&body);
        out.push(CapabilitySpec {
            id,
            purpose: section_first_paragraph(&body, "Purpose"),
            requirement_count,
            scenario_count,
            path,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Load every active change under `openspec/changes/<id>/`, skipping the
/// `archive/` directory (landed changes).
fn load_changes(changes_dir: &Path) -> Vec<Change> {
    let mut out = Vec::new();
    for id in child_dir_names(changes_dir) {
        if id == "archive" {
            continue;
        }
        let dir = changes_dir.join(&id);

        let proposal_path = existing_file(&dir.join("proposal.md"));
        let why = proposal_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|body| section_first_paragraph(&body, "Why"));

        let tasks_path = existing_file(&dir.join("tasks.md"));
        let (tasks_done, tasks_total) = tasks_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|body| count_checkboxes(&body))
            .unwrap_or((0, 0));

        let delta_capabilities = child_dir_names(&dir.join("specs"));

        out.push(Change {
            id,
            dir,
            proposal_path,
            why,
            tasks_path,
            tasks_done,
            tasks_total,
            delta_capabilities,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

// ---------- markdown parsing -------------------------------------------

/// Count `### Requirement:` and `#### Scenario:` headings. Matching is
/// tolerant of leading whitespace and a flexible run of `#`, but anchors
/// on the `Requirement:` / `Scenario:` label so prose mentioning the
/// words doesn't inflate the count.
fn count_requirements(body: &str) -> (usize, usize) {
    let mut reqs = 0;
    let mut scenarios = 0;
    for line in body.lines() {
        let t = line.trim_start();
        let Some(after_hashes) = strip_atx_hashes(t) else {
            continue;
        };
        let after_hashes = after_hashes.trim_start();
        if after_hashes.starts_with("Requirement:") {
            reqs += 1;
        } else if after_hashes.starts_with("Scenario:") {
            scenarios += 1;
        }
    }
    (reqs, scenarios)
}

/// Count markdown task checkboxes. Returns `(done, total)` where `done`
/// counts `- [x]` / `- [X]` and total includes `- [ ]` as well. Tolerant
/// of leading indentation (nested task lists) and `*`/`+` bullet markers.
fn count_checkboxes(body: &str) -> (usize, usize) {
    let mut done = 0;
    let mut total = 0;
    for line in body.lines() {
        let t = line.trim_start();
        let rest = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "));
        let Some(rest) = rest else { continue };
        if let Some(mark) = checkbox_mark(rest) {
            total += 1;
            if mark == 'x' || mark == 'X' {
                done += 1;
            }
        }
    }
    (done, total)
}

/// If `rest` begins with a `[ ]` / `[x]` / `[X]` checkbox, return the
/// character inside the brackets.
fn checkbox_mark(rest: &str) -> Option<char> {
    let bytes = rest.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'[' && bytes[2] == b']' {
        Some(bytes[1] as char)
    } else {
        None
    }
}

/// Strip a leading ATX heading marker (`#`..`######`) followed by a
/// space. Returns the text after the marker, or `None` if the line
/// isn't an ATX heading.
fn strip_atx_hashes(line: &str) -> Option<&str> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    rest.strip_prefix(' ')
}

/// Find a top-level `## <name>` section and return the first non-empty
/// paragraph beneath it (up to the next heading). Used for `## Purpose`
/// and `## Why` summaries. Matching on the section title is
/// case-insensitive and ignores trailing punctuation.
fn section_first_paragraph(body: &str, name: &str) -> Option<String> {
    let mut lines = body.lines();
    // Locate the heading line.
    let mut in_section = false;
    let mut para: Vec<&str> = Vec::new();
    for line in &mut lines {
        let t = line.trim_start();
        if let Some(after) = strip_atx_hashes(t) {
            let title = after.trim().trim_end_matches(':');
            if in_section {
                // Next heading ends the section.
                break;
            }
            if title.eq_ignore_ascii_case(name) {
                in_section = true;
            }
            continue;
        }
        if in_section {
            if line.trim().is_empty() {
                if !para.is_empty() {
                    break;
                }
            } else {
                para.push(line.trim());
            }
        }
    }
    if para.is_empty() {
        None
    } else {
        Some(para.join(" "))
    }
}

// ---------- fs helpers --------------------------------------------------

/// Immediate subdirectory names of `dir`, sorted, hidden dirs skipped.
fn child_dir_names(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with('.') {
                continue;
            }
            out.push(name.to_string());
        }
    }
    out.sort();
    out
}

fn existing_file(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

/// Collapse internal whitespace and clamp to a single readable line so
/// one stray long paragraph can't blow up the system prompt.
fn one_line(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 200;
    if collapsed.chars().count() > MAX {
        let truncated: String = collapsed.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn counts_requirements_and_scenarios() {
        let spec = "\
# List Command Specification

## Purpose
The command SHALL provide an overview.

## Requirements
### Requirement: Command Execution
The command SHALL scan changes.

#### Scenario: Scanning for changes
- **WHEN** run without flags
- **THEN** scan the directory

#### Scenario: Scanning for specs
- **WHEN** run with --specs
- **THEN** scan specs

### Requirement: Output Format
The command SHALL display a table.

#### Scenario: Displaying list
- **WHEN** displaying
- **THEN** show a table
";
        let (reqs, scenarios) = count_requirements(spec);
        assert_eq!(reqs, 2);
        assert_eq!(scenarios, 3);
    }

    #[test]
    fn counts_checkboxes_with_indentation_and_markers() {
        let tasks = "\
## 1. Section
- [x] 1.1 done
- [ ] 1.2 todo
  - [x] 1.2.1 nested done
* [ ] star bullet
+ [X] plus bullet capital
- not a checkbox
- [ ]malformed-no-space-still-counts
";
        let (done, total) = count_checkboxes(tasks);
        assert_eq!(total, 6, "six checkbox lines (incl. the no-space variant)");
        assert_eq!(done, 3, "1.1, nested, plus-capital");
    }

    #[test]
    fn section_paragraph_is_case_insensitive_and_stops_at_next_heading() {
        let proposal = "\
## Why

Parallel changes touch the same capabilities.
Second line of the same paragraph.

More detail after a blank line.

## What Changes
- something
";
        let why = section_first_paragraph(proposal, "Why").unwrap();
        assert_eq!(
            why,
            "Parallel changes touch the same capabilities. Second line of the same paragraph."
        );
        // case-insensitive title match
        assert!(section_first_paragraph(proposal, "why").is_some());
        // missing section
        assert!(section_first_paragraph(proposal, "Nope").is_none());
    }

    #[test]
    fn strip_atx_requires_space_after_hashes() {
        assert_eq!(strip_atx_hashes("## Title"), Some("Title"));
        assert_eq!(strip_atx_hashes("###### Deep"), Some("Deep"));
        assert_eq!(strip_atx_hashes("####### TooMany"), None);
        assert_eq!(strip_atx_hashes("##NoSpace"), None);
        assert_eq!(strip_atx_hashes("plain"), None);
    }

    #[test]
    fn discover_walks_up_to_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // git root marker + openspec at the top, nested cwd below.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(
            &root.join("openspec/specs/user-auth/spec.md"),
            "## Purpose\nAuth.\n\n### Requirement: Login\nThe system SHALL log in.\n\n#### Scenario: ok\n- **WHEN** valid\n- **THEN** allow\n",
        );
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        let project = discover(&nested).expect("should find openspec walking up");
        assert_eq!(project.specs.len(), 1);
        assert_eq!(project.specs[0].id, "user-auth");
        assert_eq!(project.specs[0].requirement_count, 1);
        assert_eq!(project.specs[0].scenario_count, 1);
        assert_eq!(project.specs[0].purpose.as_deref(), Some("Auth."));
    }

    #[test]
    fn load_parses_changes_and_skips_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let os = tmp.path().join("openspec");
        write(&os.join("project.md"), "# My Project\nContext here.\n");
        write(
            &os.join("changes/add-dark-mode/proposal.md"),
            "## Why\nUsers want dark mode.\n\n## What Changes\n- add toggle\n",
        );
        write(
            &os.join("changes/add-dark-mode/tasks.md"),
            "## 1. UI\n- [x] 1.1 toggle\n- [ ] 1.2 persist\n",
        );
        write(
            &os.join("changes/add-dark-mode/specs/theming/spec.md"),
            "## ADDED Requirements\n### Requirement: Theme\nThe app SHALL theme.\n",
        );
        // Archived change must be ignored.
        write(
            &os.join("changes/archive/2025-01-01-old/proposal.md"),
            "## Why\nold.\n",
        );

        let project = load(&os);
        assert!(project.project_md.is_some());
        assert_eq!(project.changes.len(), 1, "archive/ excluded");
        let c = &project.changes[0];
        assert_eq!(c.id, "add-dark-mode");
        assert_eq!((c.tasks_done, c.tasks_total), (1, 2));
        assert!(!c.is_complete());
        assert_eq!(c.why.as_deref(), Some("Users want dark mode."));
        assert_eq!(c.delta_capabilities, vec!["theming".to_string()]);
        assert!(c.proposal_path.is_some());
        assert!(c.tasks_path.is_some());
    }

    #[test]
    fn empty_project_yields_no_section() {
        let tmp = tempfile::tempdir().unwrap();
        let os = tmp.path().join("openspec");
        std::fs::create_dir_all(&os).unwrap();
        let project = load(&os);
        assert!(project.is_empty());
        assert!(project.build_system_prompt_section().is_none());
    }

    #[test]
    fn system_prompt_section_lists_specs_and_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let os = tmp.path().join("openspec");
        write(
            &os.join("specs/user-auth/spec.md"),
            "## Purpose\nHandle auth.\n\n### Requirement: Login\nSHALL.\n\n#### Scenario: ok\n- **WHEN** x\n- **THEN** y\n",
        );
        write(
            &os.join("changes/add-dark-mode/tasks.md"),
            "- [x] done\n- [ ] todo\n",
        );
        let project = load(&os);
        let section = project.build_system_prompt_section().unwrap();
        assert!(section.contains("# OpenSpec"));
        assert!(section.contains("user-auth"));
        assert!(section.contains("1 requirement(s), 1 scenario(s)"));
        assert!(section.contains("add-dark-mode"));
        assert!(section.contains("tasks: 1/2"));
        // progressive disclosure: tells the LLM to read the files
        assert!(section.contains("read"));
    }

    #[test]
    fn agent_own_playbook_present_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let os = tmp.path().join("openspec");
        write(
            &os.join("specs/user-auth/spec.md"),
            "## Purpose\nAuth.\n\n### Requirement: Login\nSHALL.\n\n#### Scenario: ok\n- **WHEN** x\n- **THEN** y\n",
        );
        let project = load(&os);
        assert!(project.agent_own, "load() defaults agent_own to true");
        let section = project.build_system_prompt_section().unwrap();
        // The non-interactive control-flow playbook is folded in.
        assert!(section.contains("agent-own SDD"));
        assert!(section.contains("--tools"));
        assert!(section.contains("status --change"));
        assert!(section.contains("archive <name> -y"));
        assert!(section.contains("No human in the loop"));
    }

    #[test]
    fn agent_own_playbook_suppressed_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let os = tmp.path().join("openspec");
        write(
            &os.join("specs/user-auth/spec.md"),
            "## Purpose\nAuth.\n\n### Requirement: Login\nSHALL.\n\n#### Scenario: ok\n- **WHEN** x\n- **THEN** y\n",
        );
        let mut project = load(&os);
        project.agent_own = false;
        let section = project.build_system_prompt_section().unwrap();
        // Catalog still rendered…
        assert!(section.contains("# OpenSpec"));
        assert!(section.contains("user-auth"));
        // …but no autonomous-loop playbook.
        assert!(!section.contains("agent-own SDD"));
        assert!(!section.contains("No human in the loop"));
    }

    #[test]
    fn discover_returns_none_without_openspec_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        assert!(discover(tmp.path()).is_none());
    }
}
