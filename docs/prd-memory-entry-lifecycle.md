# PRD: memoryEntry Lifecycle Abstraction

## Overview / Problem Statement

Ra needs an explicit `memoryEntry` lifecycle contract before it can model
Codex-style memory behavior safely. The current source material is policy-heavy:
memories are globally gated, region-limited, thread-controllable, generated in
the background, secret-redacted, and stored as generated local state. Without a
small abstraction, future memory work risks spreading those rules across
session code, storage code, and docs.

## Goals & Success Metrics

- Provide a documented lifecycle model for a `memoryEntry`, from candidate
  intake through durable generated state and future use.
- Represent all known gating rules from the Codex Memories behavior: global
  feature flags, region availability, thread-level controls, external-context
  suppression, active/short-lived sessions, idle delay, rate-limit threshold,
  and secret redaction.
- Keep required team guidance explicitly out of memory semantics; memories are
  helpful recall, not authoritative project policy.
- Add deterministic Rust tests for the lifecycle decisions before integrating
  any model extraction or persistence backend.
- Keep the first implementation local and pure so later storage, UI, and model
  extraction work can consume it without changing policy behavior.

## User Personas & Stories

- As a Ra maintainer, I want memory eligibility and suppression reasons in one
  place so that future memory features are easy to review.
- As an agent operator, I want memory use to honor both global and thread-level
  controls so that local recall never surprises me.
- As a privacy-conscious user, I want secret redaction and generated-state
  guidance to be part of the lifecycle contract rather than a best-effort
  implementation detail.

## Functional Requirements

| Priority | Requirement |
| --- | --- |
| Must | Model a `memoryEntry` candidate as eligible, pending background generation, skipped, generated, durable, active for use, or suppressed. |
| Must | Block generation when memories are globally disabled or unavailable in the current region. |
| Must | Block generation when the current thread opts out of future memory generation. |
| Must | Block use when the current thread opts out of existing memory use. |
| Must | Optionally block use and generation when external context is present and the corresponding config flag is enabled. |
| Must | Skip background generation for active or short-lived sessions. |
| Must | Keep generation pending until the thread has been idle long enough. |
| Must | Skip a generation pass when rate-limit remaining percentage is below the configured threshold. |
| Must | Mark generated entries that required secret redaction. |
| Must | Mark memory files as generated local state that can be inspected but should not be hand-edited as the primary control surface. |
| Should | Preserve separate suppression reasons for diagnostics and future UI copy. |
| Could | Add persistence or extraction adapters in later changes using this lifecycle module. |
| Won't | Implement model-based memory extraction, consolidation, file storage, or `/memories` UI controls in this change. |
| Won't | Treat memories as a replacement for `AGENTS.md` or checked-in team documentation. |

## Non-Functional Requirements

- Keep the lifecycle logic deterministic and testable without network calls,
  model calls, background tasks, or filesystem writes.
- Avoid adding external dependencies.
- Use clear enum variants and decision structs instead of stringly typed state.
- Make privacy and policy failures explicit rather than silently falling back.
- Keep the API small enough to be reused by future storage/session work.

## Design Considerations

The abstraction should read like a state machine but stay lightweight: callers
provide configuration and observations about a candidate thread/session, and the
module returns a decision with an explicit state and reason. The first version
should not own background scheduling or file IO. It should instead define when a
background pass is allowed, when it is pending, and how a generated entry becomes
durable and usable later.

## Technical Considerations

Implementation is expected to add a small module under `src/`, export it from
`src/lib.rs`, and cover it with focused tests. The module can later be consumed
by session or store code, but this change should avoid changing existing
runtime behavior. OpenSpec should introduce a new `memory-entry-lifecycle`
capability so future implementation work has a stable requirement source.

## Timeline & Milestones

| Milestone | Owner | Target |
| --- | --- | --- |
| PRD and GitHub issue record | Agent | Before implementation |
| OpenSpec proposal/design/spec/tasks | Agent | Before code changes |
| Lifecycle module and focused tests | Agent | Implementation phase |
| Validation, PR, and review request | Agent | Before handoff |

## Open Questions & Risks

- The exact Codex production scheduler and extraction prompts are outside this
  repository. This PR models observable lifecycle policy, not proprietary
  implementation details.
- Region availability may change over time; the API should accept availability
  as input rather than baking a permanent list into code.
- Rate-limit percentage semantics are intentionally coarse in this abstraction;
  future integration can decide how to read provider-specific quota data.

## Appendix

Source behavior from the Multica issue: Codex Memories are off by default, not
available in the European Economic Area, United Kingdom, or Switzerland at
launch; settings include generation/use flags, external-context disablement,
minimum rate-limit remaining percentage, and extraction/consolidation model
choices; memories are generated local state under the Codex home directory and
are not the primary home for required team guidance.
