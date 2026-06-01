## ADDED Requirements

### Requirement: Agent can invoke LSP operations via built-in lsp tool
The system SHALL provide a built-in tool named `lsp` that dispatches openlsp command envelopes and returns structured JSON results.

#### Scenario: Diagnostics on a file
- **WHEN** the agent calls `lsp` with `{"operation": "lsp", "sub_command": "diagnostics", "file": "src/main.rs"}`
- **THEN** the tool returns a JSON array of diagnostic objects from openlsp

#### Scenario: Go-to-definition
- **WHEN** the agent calls `lsp` with `{"operation": "lsp", "sub_command": "goToDefinition", "file": "src/main.rs", "line": 10, "character": 5}`
- **THEN** the tool returns location information for the symbol at that position

#### Scenario: Capabilities query
- **WHEN** the agent calls `lsp` with `{"operation": "capabilities"}`
- **THEN** the tool returns the list of operations supported by the openlsp instance

### Requirement: lsp tool is omitted when openlsp binary is not available
The system SHALL silently omit the `lsp` tool from the built-in catalog when no openlsp binary can be resolved, without failing agent startup.

#### Scenario: openlsp not on PATH
- **WHEN** `openlsp` is not on PATH and no `[openlsp] binary` override is configured
- **THEN** the agent starts normally and the `lsp` tool is absent from the tool list

#### Scenario: bunx fallback
- **WHEN** `openlsp` is not on PATH but `bun` is available
- **THEN** the tool resolves to `bunx openlsp` and is included in the catalog

### Requirement: openlsp binary path and timeout are configurable
The system SHALL allow users to configure the openlsp binary path, workspace root, and per-call timeout via the `[openlsp]` section in `ra.toml`.

#### Scenario: Custom binary path
- **WHEN** `[openlsp] binary = "/usr/local/bin/openlsp"` is set in `ra.toml`
- **THEN** the lsp tool uses that binary instead of PATH resolution

#### Scenario: Custom timeout
- **WHEN** `[openlsp] timeout = 60.0` is set in `ra.toml`
- **THEN** the lsp tool waits up to 60 seconds for openlsp to respond before returning an error

### Requirement: lsp tool respects the builtin allow-list
The system SHALL exclude the `lsp` tool when the `[tools] builtin` allow-list is non-empty and does not include `"lsp"`.

#### Scenario: Allow-list excludes lsp
- **WHEN** `[tools] builtin = ["read", "bash"]` is configured
- **THEN** the `lsp` tool is not included in the agent's tool catalog

#### Scenario: Allow-list includes lsp
- **WHEN** `[tools] builtin = ["read", "bash", "lsp"]` is configured and openlsp is available
- **THEN** the `lsp` tool is included in the agent's tool catalog
