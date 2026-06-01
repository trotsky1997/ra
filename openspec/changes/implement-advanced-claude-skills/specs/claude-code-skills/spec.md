## MODIFIED Requirements

### Requirement: Claude Code Frontmatter Compatibility
Ra SHALL parse Claude Code skills with optional `name` and `description` frontmatter fields and retain advanced runtime frontmatter fields for direct invocation.

#### Scenario: Missing name falls back to directory command
- **WHEN** a skill at `.claude/skills/release/SKILL.md` omits frontmatter `name`
- **THEN** Ra uses `release` as the skill command name and display fallback

#### Scenario: Missing description falls back to markdown body
- **WHEN** a skill omits frontmatter `description`
- **THEN** Ra uses the first non-empty markdown paragraph as the model-facing description

#### Scenario: Additional Claude Code fields are retained
- **WHEN** a skill contains Claude Code frontmatter fields such as `model`, `allowed-tools`, `disallowed-tools`, `hooks`, `context`, `agent`, or `shell`
- **THEN** Ra loads the skill and carries those fields into the slash invocation runtime template

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

#### Scenario: Inline dynamic shell context is rendered
- **WHEN** a directly invoked skill body contains `` !`printf context` ``
- **THEN** Ra replaces the expression with the captured command output before sending the prompt to the model

#### Scenario: Fenced dynamic shell context is rendered
- **WHEN** a directly invoked skill body contains a fenced ` ```! ` command block
- **THEN** Ra replaces the block with the captured command output before sending the prompt to the model

#### Scenario: Failed dynamic shell command remains visible
- **WHEN** a dynamic shell context command exits unsuccessfully
- **THEN** Ra includes a readable command failure marker in the rendered prompt

### Requirement: Skill Scoped Runtime Enforcement
Ra SHALL enforce supported skill-scoped runtime fields for a direct skill invocation only.

#### Scenario: Skill scoped allowed tools limit model tool specs
- **WHEN** a directly invoked skill declares `allowed-tools: [read]`
- **THEN** the model receives only the `read` tool spec for that invocation

#### Scenario: Skill scoped disallowed tools deny execution
- **WHEN** a directly invoked skill declares `disallowed-tools: [bash]`
- **THEN** a model-requested `bash` tool call is returned as an error instead of executing

#### Scenario: Skill scoped hooks are temporary
- **WHEN** a directly invoked skill declares a PreToolUse hook that denies `bash`
- **THEN** the `bash` call is denied during that skill invocation and the hook does not affect later non-skill prompts

#### Scenario: Skill scoped model override is temporary
- **WHEN** a directly invoked skill declares `model: review-model` and the host can build that model
- **THEN** Ra uses `review-model` for the skill invocation and restores the prior model after it completes

### Requirement: Forked Skill Invocation
Ra SHALL support direct skill invocation with isolated fork/subagent-style transcript behavior.

#### Scenario: Forked skill isolates internal transcript
- **WHEN** a directly invoked skill declares `agent: fork`
- **THEN** Ra runs the skill against a child transcript initialized from the parent snapshot and restores the parent transcript after the child run

#### Scenario: Forked skill returns final result to parent
- **WHEN** a forked skill completes with assistant text
- **THEN** Ra appends the final assistant result to the parent transcript without appending the skill's internal user prompt
