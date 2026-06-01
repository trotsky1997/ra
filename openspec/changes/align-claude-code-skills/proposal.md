## Why

Ra advertises skills support, but current behavior does not match the Claude Code skills users are likely to bring into a project. Aligning the common skill definition and invocation semantics reduces migration friction and makes the existing feature claim accurate.

## What Changes

- Discover Claude Code project and personal skill directories by default.
- Parse Claude Code skill frontmatter with optional `name` and `description` fields.
- Derive the slash command name from the skill location, not from display metadata.
- Fall back to the first markdown paragraph when a skill omits `description`.
- Expose user-invocable skills as slash-command prompt templates.
- Hide skills from the model catalog when `disable-model-invocation: true`.
- Hide skills from direct slash invocation when `user-invocable: false`.
- Update README, config comments, schema descriptions, and tests.

## Capabilities

### New Capabilities

- `claude-code-skills`: Claude Code-compatible skill discovery, parsing, catalog visibility, and direct invocation behavior.

### Modified Capabilities

- None.

## Impact

- Affects `src/skills.rs`, config/schema docs, init templates, README, tests, and generated schema.
- Keeps existing `.ra/skills/` and `.agents/skills/` behavior.
- Does not implement advanced Claude Code runtime features such as subagents, dynamic shell context injection, or skill-scoped model/tool overrides.
