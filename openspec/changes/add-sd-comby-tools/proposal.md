# Add Native sd and comby Tools

## Why

Ra agents need safer first-class options for bulk text rewriting and structural
code rewriting. Today those workflows fall back to shell-assembled `sed` or
`comby` commands, which hides argv shape from the tool catalog, makes quoting
fragile, and provides no consistent result envelope or missing-binary guidance.

## What Changes

- Add a native built-in `sd` tool for regex or literal find/replace across
  explicit file path arguments.
- Add a native built-in `comby` tool for structural code check, diff, and
  in-place rewrite actions.
- Execute both tools through argv-safe process spawning with no shell
  interpolation and no stdin mode.
- Return bounded JSON envelopes with command metadata, exit status, stdout,
  stderr, truncation state, validation errors, timeout errors, and structured
  missing-binary guidance.
- Register both tools in the default built-in catalog and preserve exact
  `[tools].builtin` allow-list behavior using the tool names `sd` and `comby`.
- Add fake-binary integration tests and document both tools in `spec/tools.md`.

## Capabilities

### New Capabilities

### Modified Capabilities

- `tools`: add native `sd` and `comby` built-in tool contracts to the existing
  tool catalog requirements.

## Impact

- Affected code: `src/tools/`, tool registration in `src/tools/mod.rs`, and
  focused integration tests under `tests/`.
- Affected docs: `spec/tools.md` and any tool catalog comments that enumerate
  built-ins.
- Runtime dependencies: `sd` and `comby` must be discoverable on `PATH`; Ra
  does not install, vendor, or version-probe either binary.
- No breaking changes. Existing `edit`, `apply_patch`, and `bash` semantics are
  unchanged.
