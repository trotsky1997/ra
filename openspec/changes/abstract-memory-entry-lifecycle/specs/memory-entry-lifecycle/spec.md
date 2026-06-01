# memoryEntry Lifecycle Delta

## ADDED Requirements

### Requirement: Generation Eligibility

Ra SHALL model whether a `memoryEntry` candidate may contribute to future memory
generation using global, regional, thread-level, and session-level policy.

#### Scenario: Globally enabled eligible session

- **GIVEN** memories are globally enabled, the region is available, the thread
  allows generation, no external-context suppression applies, the session is not
  active, and the session is long enough
- **WHEN** Ra evaluates generation after the configured idle delay and with
  sufficient rate-limit headroom
- **THEN** the candidate is allowed for background generation

#### Scenario: Global feature disabled

- **GIVEN** memories are globally disabled
- **WHEN** Ra evaluates generation
- **THEN** generation is skipped with a global-disabled reason

#### Scenario: Region unavailable

- **GIVEN** memories are unavailable in the current region
- **WHEN** Ra evaluates generation
- **THEN** generation is skipped with a region-unavailable reason

#### Scenario: Thread generation disabled

- **GIVEN** the current thread disallows future memory generation
- **WHEN** Ra evaluates generation
- **THEN** generation is skipped with a thread-generation-disabled reason

#### Scenario: External context suppression

- **GIVEN** external context is present and memory disablement on external
  context is enabled
- **WHEN** Ra evaluates generation
- **THEN** generation is skipped with an external-context reason

#### Scenario: Active or short-lived sessions skipped

- **GIVEN** the session is active or too short-lived to summarize safely
- **WHEN** Ra evaluates generation
- **THEN** generation is skipped with the corresponding session reason

### Requirement: Background Update Timing

Ra SHALL distinguish background generation that is pending idle time from
generation that is allowed or skipped.

#### Scenario: Idle delay not reached

- **GIVEN** a candidate satisfies all eligibility checks
- **WHEN** the thread has not been idle for the configured delay
- **THEN** generation remains pending rather than skipped

#### Scenario: Rate-limit threshold not met

- **GIVEN** a candidate satisfies all eligibility checks and has reached the
  idle delay
- **WHEN** the rate-limit remaining percentage is below the configured threshold
- **THEN** generation is skipped with a rate-limit reason

### Requirement: Memory Use Gating

Ra SHALL model whether an existing memory entry can be used in the current
thread separately from whether the current thread may generate future memories.

#### Scenario: Existing durable entry active for use

- **GIVEN** memories are globally enabled, the region is available, the current
  thread allows using existing memories, no external-context suppression
  applies, and the entry is durable
- **WHEN** Ra evaluates the existing memory entry
- **THEN** the entry is active for use

#### Scenario: Thread use disabled

- **GIVEN** the current thread disallows using existing memories
- **WHEN** Ra evaluates the existing memory entry
- **THEN** use is suppressed with a thread-use-disabled reason

#### Scenario: Non-durable entry cannot be used

- **GIVEN** a generated entry is not durable
- **WHEN** Ra evaluates it for use
- **THEN** use is suppressed with an entry-not-durable reason

### Requirement: Generated State and Redaction

Ra SHALL represent generated memory entries as local generated state with
redaction metadata and non-authoritative guidance.

#### Scenario: Redacted generated entry

- **GIVEN** generated memory fields required secret redaction
- **WHEN** Ra creates memory entry metadata
- **THEN** the entry records that redaction was applied

#### Scenario: Generated files are inspectable state

- **GIVEN** a generated durable memory entry exists
- **WHEN** callers ask for control guidance
- **THEN** Ra reports that memory files are generated local state, inspectable
  for troubleshooting or sharing review, and not the primary hand-edit control
  surface

#### Scenario: Team guidance remains authoritative elsewhere

- **GIVEN** project guidance is required for all agents
- **WHEN** Ra describes memory scope
- **THEN** Ra reports that required team guidance belongs in `AGENTS.md` or
  checked-in documentation, not only in memories
