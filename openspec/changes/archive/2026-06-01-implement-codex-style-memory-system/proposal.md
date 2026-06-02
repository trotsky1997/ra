## Why

Ra already models Codex-style memoryEntry lifecycle decisions, but it does not
yet persist memories, load them into prompts, or run the session-level gates
that make the policy useful. This change turns the existing abstraction into a
small runtime memory system while keeping generated memories local and
non-authoritative.

## What Changes

- Add config wiring for a globally disabled-by-default `[memory]` section and
  per-thread/session use and generation controls.
- Persist generated memory artifacts under the Ra home/state directory and load
  durable memories into the system prompt when eligible.
- Add a generation pipeline that evaluates idle, active-session, short-lived
  session, rate-limit, external-context, and redaction gates through the
  existing `memory_entry` lifecycle abstraction.
- Keep model extraction quality behind a narrow interface with a deterministic
  local extractor suitable for tests and future model-backed extraction.
- Document that required team guidance belongs in `AGENTS.md` or checked-in
  docs, not only generated memory files.

## Capabilities

### New Capabilities

- `codex-style-memory-system`: runtime storage, loading, thread controls, and
  generation behavior for Codex-style local memories.

### Modified Capabilities

- None.

## Impact

- Affected code: config parsing/schema, a new memory runtime module, system
  prompt composition, session runner end-of-turn integration, CLI/TUI/ACP/A2A
  session construction, and tests.
- Affected state: generated memory JSON files under the Ra home/state root.
- Affected docs: PRD/issue notes and generated-state guidance.
- No new external dependency is expected.
