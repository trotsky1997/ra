# Ra built-in tools

Authoritative reference for the tools shipped in `src/tools/`. Every
JSON schema below is also embedded in the agent's tool catalog at
runtime — the LLM sees identical wire shapes.

The set is filtered by the `[tools] builtin = [...]` allow-list in
`ra.toml`. An empty allow-list ships every tool whose external
dependencies resolve at startup. Missing external binaries
(`rg`, `fd`/`fdfind`, `eza`/`exa`) cause the corresponding tool to be
silently dropped from the catalog with a single `[ra::tools] skipping
'X': … not on PATH` log line; the LLM never sees a tool it can't run.

## File-system tools

These prefer the host editor's filesystem view via ACP reverse-calls
(`fs/read_text_file`, `fs/write_text_file`) when a `ClientHandle` is
attached. Without an editor (CLI / A2A serve mode) they fall back to
direct `tokio::fs`.

### `read`

Read a text file.

```json
{ "path": "src/main.rs" }
```

| Field | Type   | Required | Notes |
|-------|--------|----------|-------|
| path  | string | yes      | absolute or relative to the agent's cwd |

### `write`

Overwrite a text file. Parent directories are created if missing
(local fallback only — ACP hosts decide).

```json
{ "path": "out/notes.md", "content": "# title\n…" }
```

| Field   | Type   | Required |
|---------|--------|----------|
| path    | string | yes      |
| content | string | yes      |

### `edit`

Replace a literal substring inside a text file. Shape mirrors Claude
Code's edit tool. By default the substring must be **unique** in the
file; the call errors with a clear message otherwise (asking the model
to add context or set `replace_all=true`).

```json
{
  "path": "src/lib.rs",
  "old_string": "pub fn foo()",
  "new_string": "pub fn foo() // renamed",
  "replace_all": false
}
```

| Field        | Type    | Required | Default |
|--------------|---------|----------|---------|
| path         | string  | yes      |         |
| old_string   | string  | yes      |         |
| new_string   | string  | yes      |         |
| replace_all  | boolean | no       | `false` |

Errors:
- `old_string and new_string are identical; nothing to do`
- `old_string must not be empty`
- `old_string not found in <path>`
- `old_string matches N places in <path>; add more context to make it unique or set replace_all=true`

## Shell tool

### `bash`

Run a shell command. With an ACP host attached, this routes through
`session/request_permission` + `terminal/create` + `terminal/output` +
`terminal/release`; without one, it falls back to `/bin/sh -c`.

```json
{ "command": "uname -sr" }
```

| Field   | Type   | Required |
|---------|--------|----------|
| command | string | yes      |

Output is the command's combined stdout+stderr; the local-fallback
path also emits a `[exit=N]` event chunk on the broadcast bus.

## Search tools

These wrap external binaries chosen for being the de-facto fast,
sane, ignore-aware replacements for `grep` / `find` / `ls`. Each
tool's `detect()` runs at startup; absence drops the tool from the
catalog. All search-tool output is capped at 64 KiB per call (a stray
`find /` won't blow up the model context).

### `grep` — wraps ripgrep (`rg`)

Recursive content search, respects `.gitignore` by default.

```json
{
  "pattern": "TODO\\(\\w+\\)",
  "path": "src",
  "case_insensitive": false,
  "fixed_string": false,
  "glob": "*.rs",
  "type": "rust",
  "max_count": 5,
  "context": 2,
  "files_with_matches": false
}
```

| Field                | Type    | Required | rg flag |
|----------------------|---------|----------|---------|
| pattern              | string  | yes      | (positional) |
| path                 | string  | no       | (positional) |
| case_insensitive     | boolean | no       | `-i` |
| fixed_string         | boolean | no       | `-F` |
| glob                 | string  | no       | `--glob` |
| type                 | string  | no       | `--type` |
| max_count            | u32     | no       | `--max-count` |
| context              | u32     | no       | `--context` |
| files_with_matches   | boolean | no       | `-l` |

`--color=never` and `--line-number` are always set.

### `find` — wraps fd (`fd` / `fdfind` on Debian)

File discovery by name. Honours `.gitignore` by default.

```json
{
  "pattern": "test_.*\\.rs",
  "path": "src",
  "glob": false,
  "type": "f",
  "extension": "rs",
  "hidden": false,
  "no_ignore": false,
  "max_results": 100
}
```

| Field        | Type    | Required | fd flag |
|--------------|---------|----------|---------|
| pattern      | string  | no       | (positional; empty = list all) |
| path         | string  | no       | (positional) |
| glob         | boolean | no       | `--glob` |
| type         | string  | no       | `--type` (`f`/`d`/`l`/`x`) |
| extension    | string  | no       | `--extension` |
| hidden       | boolean | no       | `--hidden` |
| no_ignore    | boolean | no       | `--no-ignore` |
| max_results  | u32     | no       | `--max-results` |

### `ls` — wraps eza (`eza` / `exa`)

Directory listing. Single-level by default; `tree=true` plus an
optional `level` recurses.

```json
{
  "path": "src",
  "all": false,
  "long": true,
  "tree": false,
  "level": null,
  "sort_modified": false
}
```

| Field          | Type    | Required | eza flag |
|----------------|---------|----------|----------|
| path           | string  | no       | (positional) |
| all            | boolean | no       | `-a` |
| long           | boolean | no       | `-l` |
| tree           | boolean | no       | `--tree` |
| level          | u32     | no       | `--level` |
| sort_modified  | boolean | no       | `--sort=modified` |

## Adding new tools

Implement the `Tool` trait (`src/tools/core.rs`). The minimal shape:

```rust
pub struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "What it does, in one paragraph." }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(MyParams)).unwrap()
    }
    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let params: MyParams = serde_json::from_value(input)?;
        let _scope = crate::nemo_obs::tool_scope("my_tool");
        // …
    }
}
```

Then register it from `tools::default_builtins` (or pass it in via
the `extra_tools` slot at the call site).

## Future work

The three external-binary tools (`grep` / `find` / `ls`) are stand-ins
until we wire their underlying Rust crates directly: ripgrep ships as
`grep` + `ignore` on crates.io, fd's walking is the same `ignore`
crate, and eza's listing is straightforward `tokio::fs` + tabular
formatting. Going internal removes the runtime PATH probe and means
`ra` works in scratch containers with nothing but the binary.
