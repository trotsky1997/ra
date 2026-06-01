# Tasks

## 1. Product and Spec Artifacts

- [x] 1.1 Add a PRD for the `memoryEntry` lifecycle abstraction.
- [x] 1.2 Create/update the GitHub issue record for the work.
- [x] 1.3 Add OpenSpec proposal, design, and delta spec artifacts.

## 2. Lifecycle Implementation

- [x] 2.1 Add a pure Rust module for memory policy, candidates, entries,
  lifecycle states, and decision reasons.
- [x] 2.2 Implement generation eligibility, pending-idle, and rate-limit
  decisions.
- [x] 2.3 Implement existing-memory use gating separately from generation.
- [x] 2.4 Represent secret-redaction metadata and generated-state/team-guidance
  guidance.

## 3. Tests and Validation

- [x] 3.1 Add focused Rust tests for allowed, pending, skipped, suppressed, and
  redacted lifecycle behavior.
- [x] 3.2 Run focused lifecycle tests.
- [x] 3.3 Run OpenSpec strict validation for the change.
- [x] 3.4 Run full Rust tests.
