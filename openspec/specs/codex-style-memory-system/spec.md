# codex-style-memory-system Specification

## Purpose
TBD - created by archiving change implement-codex-style-memory-system. Update Purpose after archive.
## Requirements
### Requirement: Memory Configuration

Ra SHALL expose a disabled-by-default memory configuration that can enable local
memory use and generation without requiring code changes.

#### Scenario: Memories disabled by default

- **WHEN** Ra loads a minimal config
- **THEN** the memory policy disables generated memories globally

#### Scenario: Config enables memories and thresholds

- **WHEN** Ra loads `[memory]` settings for use, generation, idle delay,
  minimum session duration, rate-limit threshold, storage path, and external
  context suppression
- **THEN** Ra maps those settings into the runtime memory policy

#### Scenario: External context suppression alias

- **WHEN** Ra loads the alias for disabling memory when external context is
  present
- **THEN** Ra treats it the same as the canonical external-context suppression
  setting

### Requirement: Local Memory Storage

Ra SHALL persist generated durable memories as inspectable generated JSON state
under the Ra home/state directory or a configured memory directory.

#### Scenario: Default storage path

- **WHEN** memories are stored for a working directory and no explicit memory
  directory is configured
- **THEN** Ra writes them under `<RA_HOME>/memories/<cwd-hash>/`

#### Scenario: Persist and load durable memories

- **WHEN** Ra persists a generated durable memory artifact
- **THEN** a later runtime can load the same artifact and preserve its metadata,
  redaction flag, and content fields

#### Scenario: Generated state guidance

- **WHEN** Ra exposes or renders memory guidance
- **THEN** it states that memory files are generated local state and that
  required team guidance belongs in `AGENTS.md` or checked-in documentation

### Requirement: Thread Memory Controls

Ra SHALL allow each session/thread to control whether it can use existing
memories and whether it can contribute future memories without changing global
settings.

#### Scenario: Thread disables memory use

- **WHEN** global memories are enabled and a session disables memory use
- **THEN** Ra suppresses prompt memory context for that session

#### Scenario: Thread disables memory generation

- **WHEN** global memories are enabled and a session disables memory generation
- **THEN** Ra skips future memory generation for that session but can still use
  existing memories if use is enabled

### Requirement: Prompt Memory Context

Ra SHALL load eligible durable memories into prompt context when memory use is
enabled and suppression settings allow it.

#### Scenario: Durable memories render into system prompt

- **WHEN** memory use is globally enabled, thread use is enabled, and durable
  memories exist for the current working directory
- **THEN** Ra appends a bounded local memory context section to the system prompt

#### Scenario: External context suppresses prompt memories

- **WHEN** external context is present and external-context suppression is
  enabled
- **THEN** Ra omits memory context from the system prompt

### Requirement: Memory Generation Pipeline

Ra SHALL evaluate completed prior sessions for generated memories using the
existing memoryEntry lifecycle policy before writing artifacts.

#### Scenario: Eligible completed session generates memory

- **WHEN** a completed session is long enough, idle long enough, inactive, above
  the rate-limit threshold, and allowed by global and thread policy
- **THEN** Ra writes a generated durable memory artifact

#### Scenario: Active, short-lived, idle-pending, and rate-limited sessions skip

- **WHEN** a session is active, too short-lived, still within the idle delay, or
  below the configured rate-limit percentage
- **THEN** Ra does not write a memory artifact and reports the lifecycle
  decision reason

#### Scenario: Secrets are redacted before storage

- **WHEN** generated memory fields contain likely secrets
- **THEN** Ra redacts those fields before writing and records that redaction was
  applied

### Requirement: Dream Scheduling Configuration

Ra SHALL expose a memory configuration threshold for the minimum number of
eligible sessions between agent-owned Dreams.

#### Scenario: Default dream scheduling threshold

- **WHEN** Ra loads a minimal config
- **THEN** the memory policy uses a conservative default of 10 sessions between
  Dreams

#### Scenario: Configured dream scheduling threshold

- **WHEN** Ra loads `[memory] min_sessions_between_dreams`
- **THEN** Ra maps that value into the runtime memory policy

