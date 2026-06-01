# Tools Delta

## ADDED Requirements

### Requirement: Extended Tool Catalog

Ra SHALL include `grep`, `glob`, `ls`, `fuzzy`, and `apply_patch` in the
default built-in catalog when `[tools].builtin` is empty.

#### Scenario: Empty allow-list exposes extended tools

- **GIVEN** `[tools].builtin` is empty
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog includes `grep`, `glob`, `ls`, `fuzzy`, and
  `apply_patch`

#### Scenario: Non-empty allow-list remains exact

- **GIVEN** `[tools].builtin` contains only `grep`
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog contains `grep` and omits unspecified tools

### Requirement: Gitignore-Aware Read-Only Tools

Ra SHALL provide structured read-only tools for text search, file discovery,
and directory listing that skip hidden and gitignored paths by default and do
not fail the whole call because one child entry is unreadable.

#### Scenario: `grep` respects path filters

- **GIVEN** files exist in nested directories
- **WHEN** `grep` is called with `glob: "*.rs"`
- **THEN** Rust files below the search root can match while non-Rust files do
  not match

#### Scenario: read-only tools return relative result paths

- **GIVEN** `grep`, `glob`, or `ls` searches below a requested root
- **WHEN** the tool returns matching entries
- **THEN** result paths are relative to the requested root when possible

#### Scenario: `grep` reports truncation for single-file searches

- **GIVEN** a single file contains more matches than the requested limit
- **WHEN** `grep` searches that file
- **THEN** the response sets `truncated` to true

#### Scenario: `glob` rejects unknown kind filters

- **GIVEN** a caller provides an unsupported `type` filter
- **WHEN** `glob` validates the request
- **THEN** the call fails instead of treating the filter as `any`

#### Scenario: traversal skips ignored directories

- **GIVEN** a directory tree contains `.git/`, `target/`, or `node_modules/`
- **WHEN** `glob` or `ls` traverses recursively with defaults
- **THEN** those directories are skipped unless hidden or ignore behavior is
  explicitly disabled

### Requirement: Non-Interactive Fuzzy Filtering

Ra SHALL provide a non-interactive `fuzzy` tool that ranks or filters candidate
strings without opening a terminal UI.

#### Scenario: fuzzy returns ranked matches

- **GIVEN** candidates `src/main.rs` and `README.md`
- **WHEN** `fuzzy` is called with query `main`
- **THEN** `src/main.rs` is returned ahead of unrelated candidates

### Requirement: Controlled Patch Application

Ra SHALL provide an `apply_patch` tool backed by `git apply` that validates a
patch before applying it and supports `check_only`.

#### Scenario: check-only does not modify files

- **GIVEN** a patch that would modify a file
- **WHEN** `apply_patch` is called with `check_only: true`
- **THEN** the call succeeds if the patch applies cleanly and the file remains
  unchanged

#### Scenario: failed check does not modify files

- **GIVEN** a patch that does not apply cleanly
- **WHEN** `apply_patch` is called without `check_only`
- **THEN** the call fails before applying and the file remains unchanged

#### Scenario: invalid cwd is rejected before applying

- **GIVEN** `apply_patch` receives a `cwd` that is missing or not a directory
- **WHEN** the tool validates the request
- **THEN** the call fails before invoking `git apply`
