## ADDED Requirements

### Requirement: Claude Code Skill Discovery
Ra SHALL discover Claude Code skill entrypoints from project and personal `.claude/skills/**/SKILL.md` directories when default skill discovery is enabled.

#### Scenario: Project Claude skill is discovered
- **WHEN** `[skills].discover` is enabled and `.claude/skills/deploy/SKILL.md` exists below the current project
- **THEN** Ra loads the `deploy` skill without an explicit `[skills].paths` entry

#### Scenario: Existing layouts remain discovered
- **WHEN** `[skills].discover` is enabled and skills exist in `.ra/skills/` or `.agents/skills/`
- **THEN** Ra continues to load those skills

#### Scenario: Project skills are discovered from cwd to repository root
- **WHEN** Ra starts in a nested project directory below a repository root that contains `.claude/skills/repo-skill/SKILL.md`
- **THEN** Ra loads `repo-skill` without requiring the user to start at the repository root

### Requirement: Claude Code Frontmatter Compatibility
Ra SHALL parse Claude Code skills with optional `name` and `description` frontmatter fields.

#### Scenario: Missing name falls back to directory command
- **WHEN** a skill at `.claude/skills/release/SKILL.md` omits frontmatter `name`
- **THEN** Ra uses `release` as the skill command name and display fallback

#### Scenario: Missing description falls back to markdown body
- **WHEN** a skill omits frontmatter `description`
- **THEN** Ra uses the first non-empty markdown paragraph as the model-facing description

#### Scenario: Additional Claude Code fields are tolerated
- **WHEN** a skill contains Claude Code frontmatter fields such as `when_to_use`, `allowed-tools`, `context`, or `agent`
- **THEN** Ra loads the skill instead of rejecting the file

### Requirement: Model Invocation Visibility
Ra SHALL exclude skills marked `disable-model-invocation: true` from the model-facing system prompt catalog.

#### Scenario: Model-disabled skill is hidden from catalog
- **WHEN** a loaded skill has `disable-model-invocation: true`
- **THEN** `ResourceBundle::build_system_prompt` omits that skill from the skills listing

#### Scenario: User-only skill remains directly invocable
- **WHEN** a loaded skill has `disable-model-invocation: true` and does not set `user-invocable: false`
- **THEN** the skill remains available for direct slash invocation

### Requirement: Direct Skill Invocation
Ra SHALL expose user-invocable skills as slash commands that expand to the skill body through the existing prompt-template dispatch path.

#### Scenario: Skill slash command expands to body
- **WHEN** the user enters `/deploy staging`
- **THEN** Ra sends the `deploy` skill body to the model and includes `ARGUMENTS: staging` when the skill body has no argument placeholders

#### Scenario: Non-user-invocable skill is hidden from slash map
- **WHEN** a skill has `user-invocable: false`
- **THEN** Ra does not expose it as a slash-command template

#### Scenario: Skill arguments replace placeholders
- **WHEN** the user enters `/migrate SearchBar React Vue` and the skill body contains `$ARGUMENTS`, `$ARGUMENTS[1]`, `$0`, or named argument placeholders such as `$component`
- **THEN** Ra replaces those placeholders before sending the rendered prompt to the model
