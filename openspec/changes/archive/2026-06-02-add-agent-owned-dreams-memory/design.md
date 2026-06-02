## Context

Ra's memory system currently has two layers: `memory_entry.rs` is a pure policy
model for per-entry lifecycle decisions, and `memory.rs` owns local generated
memory artifacts. WS-159 concluded that Claude-style Dreams should be a
follow-on synthesis pass above that pipeline. The agent, not the platform,
decides when to dream, owns the job state, decides whether to adopt the output,
and chooses which memory store to attach to future sessions.

## Goals / Non-Goals

**Goals:**

- Add a pure local policy/state seam for agent-owned Dreams.
- Keep Dreams out of `MemoryEntryLifecycle`; that enum remains per-entry.
- Reuse `MemoryPolicy` and `decide_use` for output adoption.
- Add focused tests for scheduling decisions, input filtering, job adoption, and
  config parsing/defaults.

**Non-Goals:**

- No live Anthropic Dreams API client call in this change.
- No persistence format for remote dream job records beyond the local state type.
- No automatic scheduler that starts jobs without an agent decision.

## Decisions

1. Add Dreams to `memory_entry.rs` as pure policy/state types.

   Rationale: the existing module is already the pure lifecycle contract future
   integrations compose. A Dreams layer needs the same properties: no I/O, no
   client calls, and tests that exercise deterministic policy decisions.

   Alternative considered: a new runtime module that spawns background jobs. That
   would force API and persistence concerns into this first layer and obscure the
   agent-owned decision boundary.

2. Model scheduling as `ShouldDreamDecision` plus `DreamSkipReason`.

   Rationale: WS-160 requires explicit skip reasons for disabled memories, region
   unavailability, low rate-limit headroom, and insufficient sessions. A boolean
   API would hide actionable policy state from the agent.

3. Model dream inputs as selected `MemoryCandidate` values.

   Rationale: PR #35 already uses `MemoryCandidate` for session-level
   eligibility. Reusing it keeps active-session and minimum-duration semantics
   consistent with generation decisions while allowing Dreams to ignore idle
   delay because they consume prior sessions.

4. Gate adoption through `decide_use`.

   Rationale: dream output is generated memory state. It should not bypass the
   same global, region, thread-use, external-context, and durability checks used
   for ordinary generated memory entries.

## Risks / Trade-offs

- The first layer does not call the Claude Dreams API -> Mitigation: expose job
  state and input/output store seams explicitly so a later integration can plug
  in a client boundary without changing policy tests.
- Reusing `MemoryCandidate` means the selected inputs do not yet carry remote
  session ids -> Mitigation: this layer validates policy/filtering only; the
  runtime that owns remote session ids can retain the ids alongside candidates.

## Migration Plan

Additive only. Existing configs continue to parse because the new
`min_sessions_between_dreams` field has a default. Regenerate the config schema
and update the example config.

## Open Questions

- Which Anthropic model/client boundary should own the live `POST /v1/dreams`
  call remains for a later integration change.
