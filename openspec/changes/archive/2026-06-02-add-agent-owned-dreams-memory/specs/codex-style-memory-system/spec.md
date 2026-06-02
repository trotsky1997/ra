## ADDED Requirements

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
