# Align Skills Definition And Behavior With Claude Code

## Problem

Ra's skills support does not yet match current Claude Code skills behavior. Existing Claude Code users expect project skills in `.claude/skills/`, optional frontmatter, direct `/skill-name` invocation, and separate controls for model invocation vs user invocation.

## Proposed Scope

- Discover `.claude/skills/**/SKILL.md` from project and home locations by default.
- Parse Claude Code frontmatter fields without requiring `name` or `description`.
- Derive command names from skill directories and use frontmatter `name` only as display metadata.
- Fall back to the first markdown paragraph when `description` is omitted.
- Respect `disable-model-invocation: true` for the model-facing skills catalog.
- Respect `user-invocable: false` for slash-command availability.
- Treat directly invoked skills as prompt templates using the existing slash-command path.
- Update docs, schema descriptions, and tests.

## Out Of Scope

- Dynamic context injection with `` !`command` `` or ` ```! ` blocks.
- Subagent/fork execution.
- Skill-scoped model, effort, hook, allowed-tools, or disallowed-tools enforcement.
- Live filesystem watching.

## Acceptance Criteria

- A skill at `.claude/skills/deploy/SKILL.md` is discovered with no config changes.
- A skill with no `name` and no `description` still loads using command-name and first-paragraph fallbacks.
- A skill with `disable-model-invocation: true` is omitted from the generated system prompt.
- A skill with `user-invocable: false` is not available as `/skill-name`.
- `/skill-name args` expands to the skill body plus arguments through `SessionRunner`.
- Focused Rust tests pass.
