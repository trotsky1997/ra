# Implement Advanced Claude Code Skills Runtime Semantics

## Problem

The first Claude Code skills alignment PR covered discovery, frontmatter tolerance, model/user visibility, direct slash invocation, argument substitution, and cwd-to-root project discovery. The remaining advanced semantics are now required: dynamic shell context injection, forked subagent-style execution, and skill-scoped model/tool/hook enforcement.

## Proposed Scope

- Parse and retain advanced skill frontmatter fields:
  - `model`
  - `effort`
  - `allowed-tools`
  - `disallowed-tools`
  - `hooks`
  - `context`
  - `agent`
  - `shell`
- Render dynamic shell context before direct skill invocation reaches the model:
  - inline `` !`command` ``
  - fenced ` ```! ` command blocks
- Preserve one-pass semantics: dynamic shell context is rendered from the original skill template before argument substitution, and inserted arguments are not rescanned.
- Run shell context commands from the session cwd using a deterministic shell choice.
- Support `context: fork` by running the skill in an isolated child transcript and appending only a new final assistant result to the parent transcript.
- Enforce `allowed-tools` and `disallowed-tools` for the invoked skill only.
- Run skill-scoped hooks for the invoked skill only, in addition to normal session hooks.
- Override the model for the invoked skill only when the named model is available from the host model factory.
- Add focused tests for all required runtime behaviors.

## Out Of Scope

- Filesystem watching or live rediscovery after startup.
- Adding new model-effort provider APIs.
- Expanding the global config format beyond what is needed to run skill-scoped frontmatter.

## Acceptance Criteria

- A directly invoked skill containing `` !`printf context` `` sends `context` to the model in place of the inline expression.
- A directly invoked skill containing a fenced ` ```! ` block sends the command output to the model in place of the command block.
- Shell context command failures are visible in the rendered prompt as an error marker.
- `context: fork` leaves the parent transcript free of the skill's internal user prompt while preserving only a new final assistant result.
- `allowed-tools` limits the tool specs advertised to the model and blocks out-of-scope tool execution.
- `disallowed-tools` removes denied tools even when the allow list would include them.
- Skill-scoped hooks can block a tool call during that skill invocation and do not remain active afterward.
- `model` selects the requested model for that skill invocation and restores the previous model afterward.
- `cargo fmt --check`, focused tests, full `cargo test`, and `openspec validate implement-advanced-claude-skills --strict` pass.
