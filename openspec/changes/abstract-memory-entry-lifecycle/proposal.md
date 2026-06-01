# Abstract memoryEntry Lifecycle

## Why

Codex Memories behavior has enough policy that Ra should not scatter it across
future session, storage, and UI work. A small `memoryEntry` lifecycle contract
lets future memory features reason consistently about when a memory candidate is
eligible, generated, skipped, durable, or usable.

## What Changes

- Add a PRD and GitHub issue record for the `memoryEntry` lifecycle abstraction.
- Introduce an OpenSpec capability named `memory-entry-lifecycle`.
- Add a pure Rust lifecycle module that evaluates generation/use decisions and
  exposes explicit states and suppression reasons.
- Add focused tests for global, regional, thread-level, external-context,
  active/short-lived-session, idle-delay, rate-limit, redaction, and generated
  local-state behavior.

## Capabilities

### New Capabilities

- `memory-entry-lifecycle`: lifecycle states and policy decisions for
  Codex-style memory entries.

### Modified Capabilities

- None.

## Impact

- Affected code: new lifecycle module under `src/`, library exports, and focused
  tests under `tests/`.
- Affected docs: PRD and GitHub issue record under `docs/`.
- Affected specs: new OpenSpec delta under
  `openspec/changes/abstract-memory-entry-lifecycle/specs/`.
- No runtime behavior changes yet; this change is a testable abstraction for
  future memory storage/extraction work.
