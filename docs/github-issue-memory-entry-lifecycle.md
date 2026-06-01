# Abstract memoryEntry lifecycle

## Problem

Ra needs a small, explicit abstraction for the Codex Memories `memoryEntry`
lifecycle so future memory-related work has a testable contract instead of
scattered policy prose.

Source material comes from Codex Memories behavior: memories are globally
feature-gated, can be disabled per thread, may be unavailable by region, skip
active or short-lived sessions, redact secrets, update in the background only
after enough idle time, and may skip generation when rate-limit headroom is
below a configured threshold. Memory files are generated local state under the
Codex home directory and should not replace checked-in team guidance.

## Proposed Scope

- Add a PRD documenting the lifecycle abstraction and acceptance criteria.
- Add an OpenSpec change for the `memory-entry-lifecycle` capability.
- Add a pure Rust `memory_entry` lifecycle module with focused tests.
- Keep this as policy/state modeling only; do not add model extraction, storage,
  or UI integration in this change.

## Acceptance Criteria

- The lifecycle distinguishes eligibility, generation gating, background update
  timing, activation/use, durability, and suppression.
- Region, global feature flags, thread-level controls, external context
  disablement, short/active sessions, rate-limit threshold, and secret redaction
  are represented in the API.
- Tests cover happy path, suppression reasons, pending idle background
  generation, low-rate-limit skip behavior, use gating, and generated-state
  guidance.
- OpenSpec validation and focused Rust tests pass.

## Non-Goals

- No memory extraction prompt/model integration.
- No persistent memory file format.
- No `/memories` UI or TUI command work.
- No replacement for `AGENTS.md` or checked-in project documentation.
