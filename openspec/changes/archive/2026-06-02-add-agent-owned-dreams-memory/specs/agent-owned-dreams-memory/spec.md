## ADDED Requirements

### Requirement: Agent-Owned Dream Scheduling

Ra SHALL expose a pure agent-side Dreams scheduling decision that returns an
explicit decision object instead of silently skipping work.

#### Scenario: Dream allowed

- **WHEN** memories are enabled, the region is available, rate-limit headroom is
  not below policy, and enough sessions have occurred since the last dream
- **THEN** the scheduling decision allows the agent to start a dream

#### Scenario: Dream skipped with reason

- **WHEN** memories are disabled, the region is unavailable, rate-limit headroom
  is too low, or too few sessions have occurred since the last dream
- **THEN** the scheduling decision skips the dream and reports the matching
  skip reason

### Requirement: Dream Input Selection

Ra SHALL allow the agent to select dream input sessions from session candidates
while excluding ineligible sessions and enforcing the Claude Dreams API input
limit.

#### Scenario: Eligible dream inputs

- **WHEN** candidate sessions are inactive and meet the configured minimum
  session duration
- **THEN** Ra selects them as dream inputs

#### Scenario: Ineligible and excess dream inputs

- **WHEN** candidate sessions are active, shorter than the configured minimum
  session duration, or exceed the 100-session input cap
- **THEN** Ra excludes active and too-short sessions and returns at most 100
  inputs

### Requirement: Dream Job State

Ra SHALL represent agent-owned dream jobs as state that tracks pending, running,
completed, failed, and canceled statuses, the input memory store id, and an
optional output memory store id.

#### Scenario: Dream job tracks output store

- **WHEN** a dream job is completed with an output memory store id
- **THEN** Ra preserves both the original input store id and the completed output
  store id in the job state

### Requirement: Dream Output Adoption Gate

Ra SHALL require completed dream outputs to pass through the existing memory use
policy before they can become active for a future session.

#### Scenario: Completed output is considered for use

- **WHEN** a dream job is completed and has an output memory store id
- **THEN** Ra evaluates adoption with the same memory use gate used for generated
  durable memory entries

#### Scenario: Non-completed output is suppressed

- **WHEN** a dream job is pending, running, failed, or canceled
- **THEN** Ra suppresses adoption instead of treating the output as active

### Requirement: Dream Evidence Boundary

Ra SHALL document that dream output is generated memory state while original
sessions and the input memory store remain source evidence.

#### Scenario: Developer guidance distinguishes generated state and evidence

- **WHEN** developers read Ra memory documentation for Dreams
- **THEN** the guidance identifies dream output as generated state and identifies
  original sessions plus the input memory store as source evidence
