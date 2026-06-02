## MODIFIED Requirements

### Requirement: Native Task Workflow Tool Catalog

Ra SHALL include `mise`, `just`, `wrkflw`, and `mergiraf` in the default built-in catalog when `[tools].builtin` is empty.

#### Scenario: Empty allow-list exposes task workflow tools

- **WHEN** Ra builds the default built-in tool catalog with an empty `[tools].builtin` allow-list
- **THEN** the catalog includes `mise`, `just`, `wrkflw`, and `mergiraf`

#### Scenario: Non-empty allow-list remains exact

- **WHEN** Ra builds the default built-in tool catalog with `[tools].builtin` containing only `mise`
- **THEN** the catalog contains `mise` and omits unspecified tools
