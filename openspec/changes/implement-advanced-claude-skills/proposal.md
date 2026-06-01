## Why

The merged Claude Code skills alignment made existing skills discoverable and directly invocable, but it still ignores advanced runtime semantics that users now require. Advanced Claude Code skills often depend on live shell context, isolated/forked execution, and per-skill model/tool/hook policy; treating those fields as inert metadata causes behavior drift and can bypass author intent.

## What Changes

- Parse and retain advanced Claude Code skill frontmatter fields used at runtime.
- Render inline and fenced dynamic shell context blocks before a skill prompt reaches the model.
- Add invocation-scoped runtime options for direct skill slash commands.
- Support `agent: fork` by running a skill against an isolated child transcript and returning only the final result to the parent session.
- Enforce skill-scoped model overrides, tool allow/deny lists, and hooks for the duration of a skill invocation.
- Add PRD, GitHub issue draft, OpenSpec requirements, focused tests, and documentation comments for the new runtime behavior.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- `claude-code-skills`: Extend previously aligned skill discovery/invocation behavior with advanced runtime semantics for shell context, forked execution, and scoped model/tool/hook policy.

## Impact

- Affects `src/skills.rs`, `src/session_runner.rs`, `src/session.rs`, `src/hooks.rs`, protocol runner host implementations, docs, and tests.
- Adds no new external runtime dependency.
- Keeps existing prompt-template and basic skill invocation behavior backward-compatible.
