# Design

## Context

The issue source material describes Codex Memories as generated local recall:
globally off by default, unavailable in some regions at launch, controllable per
thread, disabled around external context when configured, generated only after a
thread has been idle long enough, skipped for active or short-lived sessions,
rate-limit aware, and secret-redacted. Required team guidance still belongs in
`AGENTS.md` or checked-in docs.

Ra does not have memory storage or extraction code today. That makes the safest
first step a pure lifecycle module that models decisions and states without
introducing background workers, model calls, or file IO.

## Goals / Non-Goals

**Goals:**

- Define the `memoryEntry` lifecycle as a small, typed state machine.
- Keep use decisions separate from generation decisions.
- Preserve explicit reasons for skipped/suppressed decisions.
- Represent generated-state guidance and secret-redaction state.
- Cover the lifecycle with deterministic tests.

**Non-Goals:**

- Do not implement model extraction or consolidation.
- Do not add persistent memory file formats.
- Do not add session integration or `/memories` UI controls.
- Do not encode a permanent region blocklist in Ra; accept availability as
  input because launch availability can change.

## Decisions

### Use a pure policy module

The new module returns lifecycle decisions from caller-provided configuration
and observations. It does not spawn background tasks or read/write memory files.
This keeps the change reviewable and lets future integrations reuse one policy
surface.

### Separate generation from use

Codex thread controls can independently govern whether existing memories are
used and whether a thread contributes future memories. The API therefore has a
generation decision and a use decision instead of one shared allow/deny boolean.

### Represent pending background generation explicitly

A thread that is otherwise eligible but not idle long enough is not a hard skip.
It is pending background generation. This matters because Codex does not update
memories immediately when a thread ends; it waits until the thread has been idle
long enough to avoid summarizing ongoing work.

### Keep region availability injected

The source material names the European Economic Area, United Kingdom, and
Switzerland as unavailable at launch. Because availability is product policy
that can change, Ra should receive `region_available` as input rather than
shipping a static geography table.

### Track redaction as metadata on generated entries

Secret redaction is a lifecycle attribute of generated memory output. The module
does not need to implement secret detection; it records whether redaction was
applied so callers can surface or test that behavior.

## API Shape

- `MemoryPolicy`: global and thread-level controls plus thresholds.
- `MemoryCandidate`: observed session/thread properties.
- `MemoryEntry`: generated durable entry metadata.
- `GenerationDecision`: allowed, pending, or skipped with reason.
- `UseDecision`: active or suppressed with reason.
- `MemoryEntryLifecycle`: high-level state enum for diagnostics/tests.

## Risks / Trade-offs

- A pure module does not prove end-to-end memory behavior. That is intentional;
  it establishes the contract first and leaves extraction/storage integration to
  later PRs.
- The enum surface may need extension when storage work begins. Keeping variants
  typed and reasoned should make additions backward-compatible for callers.
- Rate-limit data is provider-specific. The module only compares an optional
  remaining percentage to the configured threshold.
