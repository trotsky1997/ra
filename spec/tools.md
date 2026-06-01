# Ra built-in tools

Authoritative reference for the tools shipped in `src/tools/`. Every
JSON schema below is also embedded in the agent's tool catalog at
runtime — the LLM sees identical wire shapes.

The set is filtered by the `[tools] builtin = [...]` allow-list in
`ra.toml`. An empty allow-list ships every built-in tool.

## RTK (Rust Token Killer) integration

`bash` consults [RTK](https://github.com/rtk-ai/rtk) before executing.
With `[rtk] mode = "auto"` (default) and `rtk` on PATH, Ra passes the
command to `rtk rewrite`, and if RTK has a recipe, executes the
rewritten command via `/bin/sh -c` instead. The model sees RTK's
compressed output, often 60–90% smaller than the original. The
transformation is logged on the broadcast bus as a `ToolCallUpdate`
chunk so it's visible to the operator.

`mode = "off"` disables the integration even when rtk is installed;
`mode = "on"` requires rtk and warns at startup if it's missing.

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

Shell-native commands such as `grep`, `find`, and `ls` intentionally
go through `bash`; Ra does not expose separate built-in wrappers for
them.

## Structural search tools

### `ast_grep`

Search code with [ast-grep](https://ast-grep.github.io/) and return
JSON. This tool shells out directly to `ast-grep run --json=stream`
(or `sg` when `ast-grep` is not on PATH), parses the JSON stream, and
returns a stable object. Exit status `1` means “no matches” and is
returned as an empty result rather than an error.

```json
{
  "pattern": "if ($COND) { $BODY }",
  "lang": "rust",
  "paths": ["src"],
  "globs": ["src/**/*.rs", "!target/**"]
}
```

| Field            | Type     | Required | Default | Notes |
|------------------|----------|----------|---------|-------|
| pattern          | string   | yes      |         | AST pattern to match |
| paths            | string[] | no       | `["."]` | Files or directories to search |
| lang             | string   | no       | inferred | `rust`, `python`, `typescript`, `tsx`, … |
| selector         | string   | no       |         | AST kind inside `pattern` used as matcher |
| strictness       | string   | no       | ast-grep default | `cst`, `smart`, `ast`, `relaxed`, `signature`, `template` |
| globs            | string[] | no       | `[]`    | Include/exclude globs; prefix with `!` to exclude |
| follow           | boolean  | no       | `false` | Follow symlinks during traversal |
| context          | integer  | no       |         | Context lines around matches; conflicts with `before`/`after` |
| before           | integer  | no       |         | Lines before matches |
| after            | integer  | no       |         | Lines after matches |
| max_matches      | integer  | no       | `50`    | Set `0` for no match-count cap |
| max_output_bytes | integer  | no       | `100000` | Drops trailing matches to preserve valid JSON |

Returned shape:

```json
{
  "matches": [],
  "total_matches": 0,
  "truncated": false,
  "exit_code": 1,
  "stderr": null
}
```

Errors:
- `ast_grep requires a non-empty pattern`
- `context conflicts with before/after`
- `ast-grep not found on PATH; install ast-grep or sg to use ast_grep`
- `ast-grep exited with status N: <output>`

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
