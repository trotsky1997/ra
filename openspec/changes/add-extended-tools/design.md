# Design

## Tool Shape

The extended tools live beside the existing built-ins under `src/tools/`.
`grep`, `glob`, `ls`, and `fuzzy` return deterministic text suitable for model
consumption, with bounded output. `apply_patch` is a mutation tool and uses the
existing `FileChangeApprover` flow before changing disk state.

## Traversal

Search and listing traversal uses the `ignore` crate so default behavior
matches developer expectations from tools such as `rg` and `fd`: hidden files
and gitignored paths are skipped unless the caller opts in. Traversal errors on
individual entries are skipped so one unreadable path does not fail the whole
operation.

## Patch Application

`apply_patch` accepts patch text and a small set of safe options. It runs
`git apply --check` first unless the caller explicitly requests `check_only`.
Application uses stdin rather than shell interpolation. The tool does not expose
index-only, partial-reject, or unsafe-path modes.

## Compatibility

The existing allow-list semantics stay intact. Empty `[tools].builtin` exposes
all built-ins. A non-empty allow-list exposes only explicitly listed tools.
