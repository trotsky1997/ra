# PRD: Advanced Claude Code Skills Runtime Alignment

## Overview / Problem Statement

Ra now supports the basic Claude Code skill shape, but advanced Claude Code skills still lose important runtime semantics. Users with existing skills expect dynamic shell context injection, isolated subagent-style execution, and skill-scoped model, tool, and hook constraints to affect the actual invocation rather than being silently ignored.

## Goals & Success Metrics

- Direct `/skill-name` invocation renders dynamic shell context from the original skill template before arguments are inserted.
- Skills that request forked execution run against an isolated copy of the conversation and do not mutate the parent session transcript with internal subagent turns.
- Skills that declare `model`, `allowed-tools`, `disallowed-tools`, or `hooks` apply those constraints for that skill invocation only.
- Existing prompt-template behavior and previously aligned skill discovery/frontmatter behavior remain backward-compatible.
- Focused Rust tests cover dynamic shell rendering, scoped tool filtering, scoped hooks, scoped model selection, and fork transcript isolation.

## User Personas & Stories

- As a Claude Code skill author, I want `` !`command` `` and fenced ` ```! ` blocks to inject command output so that skills can include live project state.
- As an agent operator, I want high-risk skills to restrict tools and hooks at invocation time so that local policy travels with the skill.
- As a runtime integrator, I want forked skill execution so that exploratory skill work can produce a result without polluting the parent session history.

## Functional Requirements

| Priority | Requirement |
| --- | --- |
| Must | Parse and persist skill frontmatter for `model`, `allowed-tools`, `disallowed-tools`, `hooks`, `context`, `agent`, and `shell`. |
| Must | Replace inline `` !`command` `` expressions with captured stdout before the skill prompt is submitted, using one pass over the original skill template. |
| Must | Replace fenced ` ```! ` command blocks with captured stdout before the skill prompt is submitted, using one pass over the original skill template. |
| Must | Run shell context commands from the current session cwd, using `/bin/sh -c` by default and `bash -lc` when `shell: bash` is declared. |
| Must | Insert a readable error marker when a shell context command exits unsuccessfully instead of aborting the entire skill invocation. |
| Must | Support `context: fork` as an isolated skill execution mode that runs on a child transcript snapshot and appends only a new final assistant result to the parent transcript. |
| Must | Apply `allowed-tools` as an invocation-scoped allow list for tool specs and execution. |
| Must | Apply `disallowed-tools` as an invocation-scoped deny list on top of the allow list. |
| Must | Apply skill-scoped `hooks` in addition to session hooks for that invocation. |
| Must | Apply `model` as an invocation-scoped model override when the host can build the named model. |
| Should | Treat unsupported shell names as the default shell and include the declaration in tests/docs as best-effort compatibility. |
| Could | Parse `effort` for future model parameters without changing the model trait in this change. |
| Won't | Implement live filesystem watching or automatic nested skill discovery during an already-running session. |

## Non-Functional Requirements

- Scope changes to skill rendering and the shared session runner/session path.
- Preserve deterministic behavior in tests without network calls.
- Avoid weakening existing session-level hooks and tool filters.
- Do not silently broaden constrained tool declarations such as `bash(...)` to the whole tool.
- Keep failed shell context commands visible to the model for debugging.

## Design Considerations

The user-facing behavior should be compatible where Ra has the necessary runtime surfaces today. Skill-scoped behavior should be temporary and should restore the parent session model, hooks, tool visibility, and transcript after the invocation completes.

## Technical Considerations

The implementation will extend `Skill`/`SlashTemplate`, `SessionRunner`, and `Session`. `RunnerHost` will expose model construction so the runner can honor `model` overrides without making protocol-specific code leak into skill rendering. Session-level scoped runtime state will filter advertised and executable tools and combine hooks for a single invocation.

## Timeline & Milestones

| Milestone | Owner | Target |
| --- | --- | --- |
| Updated PRD, issue draft, and OpenSpec change | Agent | Before implementation |
| Runtime implementation and focused tests | Agent | Same PR |
| New GitHub issue and PR | Agent | After validation |

## Open Questions & Risks

- Claude Code's exact internal fork/subagent transcript behavior is not public API. Ra will implement a practical equivalent: cloned parent context for the skill and parent transcript isolation except for the final skill result.
- Skill-scoped model overrides depend on configured model IDs. Unknown model IDs should fail visibly rather than silently using the wrong model.
- Shell context injection executes local commands and therefore inherits Ra's existing local execution risk profile.

## Appendix

Reference: current Claude Code skills documentation at `https://code.claude.com/docs/en/skills.md` and `https://code.claude.com/docs/zh-CN/skills.md`, checked during this change.
