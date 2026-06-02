## Why

Ra now has a Codex-style local memory pipeline, but it lacks a policy surface for
an agent to explicitly decide when to consolidate many sessions into a cleaner
memory store. Claude-style Dreams should sit above the per-entry lifecycle as an
agent-owned background synthesis job, not as another `MemoryEntryLifecycle`
state.

## What Changes

- Add a pure Dreams policy/state layer for agent-owned scheduling decisions.
- Represent dream jobs with explicit pending, running, completed, failed, and
  canceled states, including input and optional output memory store ids.
- Add input selection that filters active and too-short sessions and caps inputs
  at the Claude Dreams API limit of 100 sessions.
- Gate adoption of a completed dream output through the existing memory use
  policy instead of bypassing `decide_use`.
- Add memory config support for `min_sessions_between_dreams`, defaulting to 10.
- Document that dream output is generated memory state while source sessions and
  the input store remain source evidence.

## Capabilities

### New Capabilities

- `agent-owned-dreams-memory`: Agent-owned Dreams scheduling, job state, input
  selection, and output adoption policy for Ra memory synthesis.

### Modified Capabilities

- `codex-style-memory-system`: Add the memory configuration threshold that
  controls the minimum number of sessions between Dreams.

## Impact

- Affected code: `src/memory_entry.rs`, `src/memory.rs`, `src/config.rs`,
  `src/lib.rs`, config schema/example, and memory tests.
- No live Anthropic Dreams API integration is included in this layer; the new
  API is the local policy/state seam that a later client integration can call.
- No new external dependencies.
