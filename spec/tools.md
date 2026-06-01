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

Prefer the native `git` and `gh` tools for argv-safe calls to those
CLIs. Shell-native commands such as `grep`, `find`, and `ls`
intentionally go through `bash`; Ra does not expose separate built-in
wrappers for them. When a high-volume `git` or `gh` command needs RTK
output compression, run it through `bash` so the existing RTK rewrite
path can apply.

## Native CLI tools

These wrappers execute common developer CLIs without asking the model to
compose a shell string. In local CLI / A2A / TUI mode Ra spawns the
binary directly with `Command::args`, preserving argv boundaries. With
an ACP host attached, Ra reuses the same permission-gated
`terminal/*` reverse-call path as `bash`, rendering the argv array as a
shell-quoted command line for the host terminal.

These native wrappers do not route through RTK. That tradeoff preserves
argv semantics in the local process path instead of converting the call
back into a shell command for compression. Use the `bash` tool for
verbose `git` / `gh` commands when RTK compression is more important
than argv-safe process execution.

Output is the command's combined stdout+stderr; both local and ACP
paths emit a `[exit=N]` event chunk on the broadcast bus.

### `git`

Run native `git`.

```json
{ "args": ["status", "--short"] }
```

| Field | Type             | Required | Default | Notes |
|-------|------------------|----------|---------|-------|
| args  | array of strings | no       | `[]`    | arguments only; omit the `git` binary |

### `gh`

Run native GitHub CLI (`gh`).

```json
{ "args": ["pr", "view", "--json", "title,url"] }
```

| Field | Type             | Required | Default | Notes |
|-------|------------------|----------|---------|-------|
| args  | array of strings | no       | `[]`    | arguments only; omit the `gh` binary |

## Structural search tools

### `ast_grep`

Search code with [ast-grep](https://ast-grep.github.io/) and return
JSON. This tool shells out directly to `ast-grep run --json=stream`
(or an `sg` alias whose `--version` output identifies it as ast-grep),
parses the JSON stream, and returns a stable object. Relative paths are
resolved against the session cwd. Exit status `1` with empty stderr
means “no matches” and is returned as an empty result rather than an
error; status `1` with stderr is treated as an ast-grep error.

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
| max_matches      | integer  | no       | `50`    | Set `0` for no match-count cap; search stops after the budget is reached |
| max_output_bytes | integer  | no       | `100000` | Search stops before adding a match that would exceed the JSON budget |

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
- `ast-grep not found on PATH; install ast-grep or an ast-grep sg alias to use ast_grep`
- `ast-grep exited with status N: <output>`

## Graphify tools

When `[graphify]` is enabled, Ra registers an agent-owned R2A graph
workflow even if `graphify-out/graph.json` is missing. The graph is used
as shared project memory across requirement intake, SWE planning,
implementation guidance, verification, artifact traceability, and
multi-agent continuity.

Ra reads Graphify's NetworkX node-link JSON output directly for
query/path/explain. Build/refresh is explicit through
`graphify_update`: the default path runs `graphify update <root>
--no-cluster` for a local AST-only graph; semantic extraction must be
requested explicitly because it may send project material to an LLM
backend. Graphify's optional MCP server is not required.

### `graphify_ensure`

Check graph location, freshness, CLI availability, and the executable
build/refresh path. With `refresh: true`, it can trigger the default
update path when the graph is missing, stale, or invalid.

```json
{ "phase": "intake", "requirement": "add SSO login", "refresh": false }
```

| Field       | Type    | Required | Default |
|-------------|---------|----------|---------|
| phase       | string  | no       | `intake` |
| requirement | string  | no       |         |
| refresh     | boolean | no       | `false` |
| semantic    | boolean | no       | `false` |
| backend     | string  | no       |         |
| timeout_sec | integer | no       | `600`   |

### `graphify_impact`

Map a requirement and/or touched files into the project graph. The result
summarizes seed nodes, planning-focus files, relationship edges,
verification candidates, docs/artifacts, and report traceability.

```json
{
  "phase": "verify",
  "requirement": "add SSO login",
  "changed_files": ["src/auth.rs"],
  "depth": 2,
  "max_nodes": 32
}
```

| Field         | Type          | Required | Default |
|---------------|---------------|----------|---------|
| phase         | string        | no       | `intake` |
| requirement   | string        | no       |         |
| changed_files | array<string> | no       | `[]`    |
| depth         | integer       | no       | `2`     |
| max_nodes     | integer       | no       | `32`    |

### `graphify_update`

Build or refresh the graph through the installed `graphify` CLI without
using a shell. If the CLI is missing, the tool returns install guidance
instead of failing opaquely.

```json
{ "mode": "update", "no_cluster": true, "timeout_sec": 600 }
```

| Field       | Type    | Required | Default |
|-------------|---------|----------|---------|
| mode        | string  | no       | `auto`  |
| backend     | string  | no       |         |
| model       | string  | no       |         |
| force       | boolean | no       | `false` |
| no_cluster  | boolean | no       | `true` for `update` |
| no_viz      | boolean | no       | `true` for `cluster-only` |
| timeout_sec | integer | no       | `600`   |

### `graphify_query`

Search matching nodes and return a focused neighborhood subgraph.

```json
{ "question": "what connects auth to the database?", "depth": 2, "max_nodes": 24 }
```

| Field     | Type    | Required | Default |
|-----------|---------|----------|---------|
| question  | string  | yes      |         |
| depth     | integer | no       | `2`     |
| max_nodes | integer | no       | `24`    |

### `graphify_path`

Find the shortest relationship path between two matching nodes.

```json
{ "source": "AuthService", "target": "DatabasePool" }
```

| Field  | Type   | Required |
|--------|--------|----------|
| source | string | yes      |
| target | string | yes      |

### `graphify_explain`

Explain one matching node and list its neighboring relationships.

```json
{ "node": "AuthService", "max_neighbors": 20 }
```

| Field         | Type    | Required | Default |
|---------------|---------|----------|---------|
| node          | string  | yes      |         |
| max_neighbors | integer | no       | `20`    |

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
