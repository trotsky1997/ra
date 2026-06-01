# PRD: Claude Code Skills Alignment

## Overview / Problem Statement

Ra advertises skills support, but its current definition is still closer to an older Agent Skills shape than current Claude Code behavior. Users bringing existing Claude Code skills into Ra can hit avoidable incompatibilities: `.claude/skills/` is not auto-discovered, frontmatter fields that Claude treats as optional are required by Ra, model-only and user-only invocation controls are ignored, and direct `/skill-name` invocation is not available.

## Goals & Success Metrics

- Existing Claude Code project and personal skills load in Ra without additional configuration.
- Skills without `name` or `description` frontmatter remain usable using Claude Code fallbacks.
- Skills marked `disable-model-invocation: true` do not appear in the model-facing skills catalog but can still be directly invoked.
- Skills marked `user-invocable: false` remain model-visible when otherwise eligible but are not slash-invocable.
- Focused tests cover discovery, parsing, catalog visibility, and slash expansion.

## User Personas & Stories

- As a Claude Code user, I want Ra to discover my committed `.claude/skills/` so that project workflows work without duplicate config.
- As a skill author, I want directory-name command semantics so that `/deploy` matches `.claude/skills/deploy/SKILL.md`.
- As an agent/runtime integrator, I want model-facing and user-facing visibility to be distinct so that workflow-only skills do not pollute automatic invocation.

## Functional Requirements

| Priority | Requirement |
| --- | --- |
| Must | Auto-discover project and personal `.claude/skills/**/SKILL.md` paths in addition to Ra and universal skills paths. |
| Must | Derive the slash command name from the skill directory name. |
| Must | Treat `name` as an optional display name, falling back to the command name. |
| Must | Treat `description` as optional, falling back to the first markdown paragraph. |
| Must | Hide `disable-model-invocation: true` skills from the system prompt catalog. |
| Must | Hide `user-invocable: false` skills from slash-command templates. |
| Should | Include `when_to_use` in model-facing descriptions. |
| Could | Preserve parsed Claude Code frontmatter fields for future runtime enforcement. |
| Won't | Implement subagent execution, model/effort overrides, scoped tool permissions, or dynamic shell context injection in this change. |

## Non-Functional Requirements

- Preserve existing Ra `.ra/skills/` and `.agents/skills/` behavior.
- Avoid loading skill bodies into the startup system prompt.
- Keep behavior deterministic and testable with local filesystem fixtures.
- Keep the change backward-compatible for current config files.

## Design Considerations

The user-facing behavior should mirror Claude Code where Ra already has matching surfaces: discovery, model catalog, and slash command expansion. Unsupported advanced fields should be parsed or tolerated without implying enforcement.

## Technical Considerations

The implementation will live primarily in `src/skills.rs` and reuse the existing prompt-template slash dispatcher. `ResourceBundle::prompt_map` can expose eligible skill bodies alongside legacy prompt templates, so ACP/A2A/TUI paths all inherit the same behavior.

## Timeline & Milestones

| Milestone | Owner | Target |
| --- | --- | --- |
| Spec and artifact draft | Agent | Before implementation |
| Code and tests | Agent | Same change |
| PR review | Maintainer | After PR opens |

## Open Questions & Risks

- Claude Code now includes many advanced fields. This change deliberately tolerates and records some of them but does not enforce all runtime semantics.
- Dynamic context injection can run shell commands before model invocation; implementing it requires a separate permissions and safety design.
- Claude Code skill discovery includes nested on-demand behavior tied to file work. This change starts with startup discovery paths already available to Ra.

## Appendix

Reference: current Claude Code skills documentation at `https://code.claude.com/docs/en/skills.md` and `https://code.claude.com/docs/zh-CN/skills.md`, checked during this change.
