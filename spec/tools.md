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
CLIs. Prefer the extended `grep`, `glob`, `ls`, and `fuzzy` tools for
structured read-only search and selection. Use `bash` for project
scripts, tests, and one-off command pipelines. When a high-volume `git`
or `gh` command needs RTK output compression, run it through `bash` so
the existing RTK rewrite path can apply.

Prefer `webfetch_fetch` and `webfetch_crawl` over `bash` for
documentation retrieval. They preserve argv boundaries, return bounded
JSON, and surface missing-`npm` installation guidance in a structured
response.

Prefer `jq` over `bash` pipelines for JSON filtering. It preserves argv
boundaries, feeds input through stdin, returns bounded JSON, and surfaces
missing-`jq` installation guidance in a structured response.

Prefer `sd` over `bash` + `sed` for regex or literal find/replace across
files. Prefer `comby` over shell-composed structural rewrite commands for
template-based code checks, diffs, and in-place rewrites. Both preserve argv
boundaries, return bounded JSON, and surface missing-binary guidance in a
structured response.

Prefer `tmux_run`, `tmux_send`, `tmux_capture`, `tmux_kill`,
`tmux_listen`, and `tmux_wait` over `bash` when the task needs
persistent terminal state, interactive input, later output inspection,
or bounded waits for tmux events. They operate only on Ra-owned tmux
sessions named `ra__{session}` and surface missing-`tmux` installation
guidance in a structured response.

## Native CLI tools

These wrappers execute common developer CLIs without asking the model to
compose a shell string. In local CLI / A2A / TUI mode Ra spawns the
binary directly with `Command::args`, preserving argv boundaries. With an ACP
host attached, `git` and `gh` reuse the same permission-gated `terminal/*`
reverse-call path as `bash`, rendering the argv array as a shell-quoted command
line for the host terminal. `jq`, `mergiraf`, `sd`, `comby`, `mise`, `just`,
and `wrkflw` spawn local binaries directly so they can preserve
stdin/output-envelope behavior; ACP terminal permission prompts do not wrap
those local spawns. The central `[tools].builtin` allow-list and
PreToolUse/PostToolUse hooks still apply.

These native wrappers do not route through RTK. That tradeoff preserves
argv semantics in the local process path instead of converting the call
back into a shell command for compression. Use the `bash` tool for
verbose commands when RTK compression is more important than argv-safe
process execution.

`git` and `gh` return combined stdout+stderr. `jq`, `mergiraf`, `sd`,
`comby`, `mise`, `just`, and `wrkflw` return bounded JSON envelopes. Tool
calls emit progress and `[exit=N]` event chunks on the broadcast bus.

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

### `jq`

Run a jq filter against exactly one JSON input source. Ra reads inline
`input` or the file at `path`, passes the bytes to jq on stdin, and
spawns jq with an argv array. The tool does not expose arbitrary jq args
in v1; the supported output flags map to separate argv entries:
`raw_output -> -r`, `compact_output -> -c`, and `sort_keys -> -S`.

```json
{
  "filter": ".items[] | .name",
  "input": "{\"items\":[{\"name\":\"Ada\"}]}",
  "raw_output": true,
  "max_output_bytes": 100000
}
```

```json
{
  "filter": ".dependencies | keys[]",
  "path": "package.json",
  "cwd": ".",
  "raw_output": true
}
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| filter | string | yes | | jq filter |
| input | string | one of `input`/`path` | | Inline JSON text passed to jq stdin |
| path | string | one of `input`/`path` | | JSON file read by Ra and passed to jq stdin |
| cwd | string | no | session cwd | Resolves relative `path` and sets jq's working directory |
| raw_output | boolean | no | `false` | Maps to `-r` |
| compact_output | boolean | no | `false` | Maps to `-c` |
| sort_keys | boolean | no | `false` | Maps to `-S` |
| timeout_ms | number | no | none | Optional jq process timeout |
| max_output_bytes | number | no | `100000` | Bounds Ra's returned JSON envelope |

Returned envelope:

```json
{
  "ok": true,
  "tool": "jq",
  "filter": ".name",
  "command": { "program": "jq", "args": ["-r", ".name"] },
  "exit_code": 0,
  "stdout": "Ada\n",
  "stderr": null,
  "truncated": false
}
```

Error cases are returned as the same valid JSON envelope with
`ok:false`. Request validation failures use `error.kind:"invalid_request"`
and happen before jq is spawned. Non-zero jq exits use
`error.kind:"jq_error"` with jq stderr and exit code. Missing jq uses
`error.kind:"missing_jq"` with installation guidance. Output exceeding
`max_output_bytes` is clipped with `truncated:true`.

### `mergiraf`

Run native [`mergiraf`](https://mergiraf.org/) for syntax-aware merge
workflows. The `action` field selects one of three command shapes:

| Action | CLI mapping | Mutation behavior |
|--------|-------------|-------------------|
| `merge` | `mergiraf merge <base> <ours> <theirs> [--language language] [--compact] [--allow-parse-errors]` | Prints the merge result to stdout unless mergiraf itself decides otherwise; this wrapper does not pass `--git` or `--output`. |
| `solve` | `mergiraf solve <file>` | Lets mergiraf update the conflicted file according to its normal `solve` behavior. |
| `languages` | `mergiraf languages --gitattributes` | Read-only supported-language listing. |

```json
{
  "action": "merge",
  "base": "base.rs",
  "ours": "ours.rs",
  "theirs": "theirs.rs",
  "language": "rust",
  "compact": true
}
```

```json
{
  "action": "solve",
  "file": "src/conflicted.rs",
  "cwd": "."
}
```

```json
{ "action": "languages" }
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| action | string | yes | | `merge`, `solve`, or `languages` |
| base | string | for `merge` | | Base file path passed as one argv entry |
| ours | string | for `merge` | | Ours/current file path passed as one argv entry |
| theirs | string | for `merge` | | Theirs/other file path passed as one argv entry |
| file | string | for `solve` | | Conflict-marker file passed as one argv entry |
| language | string | no | | Maps to `--language <language>` for `merge` |
| compact | boolean | no | `false` | Maps to `--compact` for `merge` |
| allow_parse_errors | boolean | no | `false` | Maps to `--allow-parse-errors` for `merge` |
| cwd | string | no | session cwd | Working directory; relative paths resolve there |
| max_output_bytes | number | no | `100000` | Bounds Ra's returned JSON envelope; `0` means unbounded |

Returned envelope:

```json
{
  "ok": true,
  "tool": "mergiraf",
  "action": "merge",
  "command": {
    "program": "mergiraf",
    "args": ["merge", "base.rs", "ours.rs", "theirs.rs"],
    "cwd": "/repo"
  },
  "exit_code": 0,
  "stdout": "merged contents\n",
  "stderr": null,
  "truncated": false
}
```

Error cases are returned as valid JSON with `ok:false`. Missing required
action fields or an invalid `cwd` use `error.kind:"invalid_request"` before
spawning mergiraf. Non-zero exits use `error.kind:"mergiraf_error"` with
stdout, stderr, and exit code. Missing mergiraf uses
`error.kind:"missing_mergiraf"` with installation guidance including
`cargo install mergiraf`. Output exceeding `max_output_bytes` is clipped with
`truncated:true`.

### `sd`

Run native [`sd`](https://github.com/chmln/sd) for regex or literal
find/replace across explicit file paths. Ra invokes `sd` with an argv array and
never uses stdin mode, so `paths` must contain at least one entry. Because no
shell expands arguments, `paths` are passed literally; use the `glob` tool or
another file-discovery step first when you need glob expansion.

CLI mapping:

```text
sd [--fixed-strings] [extra_args...] -- <find> <replace> <paths...>
```

```json
{
  "find": "foo_(\\w+)",
  "replace": "bar_$1",
  "paths": ["src/main.rs", "src/lib.rs"],
  "timeout_ms": 30000,
  "max_output_bytes": 32768
}
```

```json
{
  "find": "a.b",
  "replace": "x",
  "paths": ["README.md"],
  "string_mode": true,
  "extra_args": ["--flags", "i"]
}
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| find | string | yes | | Regex pattern, or literal text when `string_mode:true` |
| replace | string | yes | | Replacement string; sd handles capture references such as `$1` |
| paths | array of strings | yes | | Explicit file paths; must not be empty |
| string_mode | boolean | no | `false` | Maps to `--fixed-strings` |
| extra_args | array of strings | no | `[]` | Advanced sd flags passed before find/replace |
| cwd | string | no | session cwd | Working directory; relative paths resolve there |
| timeout_ms | number | no | `30000` | Process timeout |
| max_output_bytes | number | no | `32768` | Bounds Ra's returned JSON envelope; `0` means unbounded |

Returned envelope:

```json
{
  "ok": true,
  "tool": "sd",
  "command": {
    "program": "sd",
    "args": ["--", "foo", "bar", "src/main.rs"],
    "cwd": "/repo"
  },
  "exit_code": 0,
  "stdout": "",
  "stderr": null,
  "truncated": false
}
```

Error cases are returned as valid JSON with `ok:false`. Empty `paths` or an
invalid `cwd` use `error.kind:"invalid_request"` before spawning `sd`.
Non-zero exits use `error.kind:"command_failed"` with stdout, stderr, and exit
code. Timeouts use `error.kind:"timeout"`. Missing sd uses
`error.kind:"missing_sd"` with installation guidance including `cargo install
sd`. Output exceeding `max_output_bytes` is clipped with `truncated:true`.

### `comby`

Run native [`comby`](https://comby.dev/) for structural template matching and
rewriting. The `action` field selects one of three safe command shapes:

| Action | CLI mapping |
|--------|-------------|
| `rewrite` | `comby <match_template> <rewrite_template> [extensions...] [-d directory] [-matcher matcher] [-include-files regex] [-exclude-files regex] -in-place [extra_args...]` |
| `check` | `comby <match_template> "" [extensions...] [-d directory] [-matcher matcher] [-include-files regex] [-exclude-files regex] -match-only [extra_args...]` |
| `diff` | `comby <match_template> <rewrite_template> [extensions...] [-d directory] [-matcher matcher] [-include-files regex] [-exclude-files regex] -diff [extra_args...]` |

`rewrite` mutates in place by default. `check` and `diff` do not pass
`-in-place`.

```json
{
  "action": "rewrite",
  "match_template": "foo(:[arg])",
  "rewrite_template": "bar(:[arg])",
  "extensions": [".rs"],
  "directory": "src",
  "matcher": "rust",
  "timeout_ms": 60000,
  "max_output_bytes": 65536
}
```

```json
{
  "action": "diff",
  "match_template": "old_api(:[x])",
  "rewrite_template": "new_api(:[x])",
  "extensions": [".py"],
  "matcher": "python"
}
```

```json
{
  "action": "check",
  "match_template": "deprecated(:[x])",
  "extensions": [".js"],
  "include_files": "src/.*"
}
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| action | string | yes | | `rewrite`, `check`, or `diff` |
| match_template | string | yes | | Comby template using hole syntax such as `:[arg]` |
| rewrite_template | string | for `rewrite`/`diff` | | Comby rewrite template |
| extensions | array of strings | no | `[]` | File extensions passed as positional filters |
| directory | string | no | | Passed to `-d` |
| matcher | string | no | | Passed to `-matcher`, e.g. `rust`, `python`, `generic` |
| include_files | string | no | | Regex passed to `-include-files` |
| exclude_files | string | no | | Regex passed to `-exclude-files` |
| extra_args | array of strings | no | `[]` | Advanced comby flags passed after Ra's action flag |
| cwd | string | no | session cwd | Working directory for the process |
| timeout_ms | number | no | `60000` | Process timeout |
| max_output_bytes | number | no | `65536` | Bounds Ra's returned JSON envelope; `0` means unbounded |

Returned envelope:

```json
{
  "ok": true,
  "tool": "comby",
  "action": "diff",
  "command": {
    "program": "comby",
    "args": ["foo(:[arg])", "bar(:[arg])", ".rs", "-diff"],
    "cwd": "/repo"
  },
  "exit_code": 0,
  "stdout": "--- a/src/lib.rs\n+++ b/src/lib.rs\n...",
  "stderr": null,
  "truncated": false
}
```

Error cases are returned as valid JSON with `ok:false`. Missing or empty
`rewrite_template` for `rewrite`/`diff`, empty `match_template`, or invalid
`cwd` use `error.kind:"invalid_request"` before spawning `comby`. Non-zero
exits use `error.kind:"command_failed"` with stdout, stderr, and exit code.
Timeouts use `error.kind:"timeout"`. Missing comby uses
`error.kind:"missing_comby"` with installation guidance. Output exceeding
`max_output_bytes` is clipped with `truncated:true`.

### `mise`

Run native `mise` for project task/test loops. Pass only arguments after the
binary name. Common TDD usage is `mise run test`:

```json
{
  "args": ["run", "test"],
  "cwd": ".",
  "timeout_ms": 120000,
  "max_output_bytes": 120000
}
```

### `just`

Run native `just` recipes. Common TDD usage is `just test`:

```json
{
  "args": ["test"],
  "cwd": ".",
  "timeout_ms": 120000
}
```

### `wrkflw`

Run native `wrkflw` to validate or execute GitHub Actions workflows locally
before review:

```json
{
  "args": ["validate", ".github/workflows/ci.yml"],
  "cwd": ".",
  "timeout_ms": 120000
}
```

The task/workflow tools share the same schema:

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| args | array of strings | no | `[]` | arguments only; omit the binary name |
| cwd | string | no | session cwd | Working directory; relative paths resolve against the session cwd |
| timeout_ms | number | no | none | Optional process timeout |
| max_output_bytes | number | no | `120000` | Bounds Ra's returned JSON envelope; `0` means unbounded |

Returned envelope:

```json
{
  "ok": true,
  "tool": "just",
  "command": { "program": "just", "args": ["test"], "cwd": "/repo" },
  "exit_code": 0,
  "stdout": "tests passed\n",
  "stderr": null,
  "truncated": false
}
```

Error cases are returned as valid JSON with `ok:false`. Invalid `cwd`
requests use `error.kind:"invalid_request"`. Non-zero exits use
`error.kind:"command_failed"` with stdout, stderr, and exit code.
Timeouts use `error.kind:"timeout"`. Missing binaries use
`error.kind:"missing_mise"`, `error.kind:"missing_just"`, or
`error.kind:"missing_wrkflw"` with installation guidance. Output exceeding
`max_output_bytes` is clipped with `truncated:true`. `max_output_bytes` is a
best-effort envelope budget; Ra always preserves valid JSON, so very small
budgets may still return a minimal envelope larger than the requested byte
count.

## Tmux tools

These wrappers operate on Ra-owned tmux sessions. User-facing session
names are logical names such as `dev`; Ra maps them to tmux sessions
named `ra__dev`. Session/window/pane identifiers are restricted to
simple ASCII target names so tool calls cannot accidentally address
operator-owned tmux sessions.

Every tool returns a JSON envelope:

```json
{
  "ok": true,
  "tool": "tmux_capture",
  "target": {
    "logical_session": "dev",
    "session": "ra__dev",
    "window": "main",
    "pane": null,
    "target": "ra__dev:main"
  },
  "command": { "program": "tmux", "args": [], "display": "tmux ..." },
  "exit_code": 0,
  "stdout": "...",
  "stderr": null,
  "truncated": false
}
```

If `tmux` is missing, the envelope has `ok:false` and
`error.kind:"missing_tmux"` with installation guidance.

### Shared tmux wait/listen events

`tmux_listen` and `tmux_wait` use the same event expression model:

| Event | Meaning |
|-------|---------|
| `output_update` | current pane capture differs from the initial snapshot |
| `output_match` | current pane capture matches `pattern` |
| `program_exit` | `tmux_wait.command` exits before timeout |
| `program_output` | `tmux_wait.command` produces matching output, or any output when `pattern` is omitted |
| `hook` | pane capture matches `pattern`; `hook` labels the event in the response |
| `sleep` | `tmux_wait` sleeps for `duration_ms`, bounded by `timeout_ms` |

Expression fields are shared: `pattern` is a substring by default, and
`regex:true` interprets it as a Rust regular expression. Hook events use
the same expression fields; they are tool-level labels over pane output,
not tmux-native `set-hook` integration.

### `tmux_run`

Create or reuse a named session/window and run a command. Use
`wait:false` for long-running processes; the tool returns after tmux
accepts the command. Use `wait:true` for a deterministic blocking run;
Ra respawns the target pane with a wrapper script, waits for completion,
captures the pane, and returns `command_exit_code`.

```json
{
  "session": "dev",
  "window": "tests",
  "command": "cargo watch -x test",
  "wait": false
}
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| session | string | yes | | logical name; mapped to `ra__{session}` |
| command | string | yes | | shell command executed inside tmux |
| window | string | no | `main` | target window |
| pane | string | no | | optional pane id/index |
| wait | boolean | no | `false` | wait for command exit and capture output |
| timeout_ms | number | no | `30000` | only applies when `wait:true` |
| max_output_bytes | number | no | `100000` | bounds returned captured output |

### `tmux_send`

Send literal input or tmux key names to a target pane.

```json
{ "session": "dev", "window": "tests", "keys": "q", "enter": true }
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| session | string | yes | | logical name; mapped to `ra__{session}` |
| window | string | no | `main` | target window |
| pane | string | no | | optional pane id/index |
| keys | string | yes | | literal text or whitespace-separated tmux key names |
| enter | boolean | no | `false` | send Enter after `keys` |
| literal | boolean | no | `true` | set `false` for tmux key names such as `C-c` |

### `tmux_capture`

Capture visible pane content or scrollback with optional line bounds.

```json
{ "session": "dev", "window": "tests", "start_line": -50 }
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| session | string | yes | | logical name; mapped to `ra__{session}` |
| window | string | no | `main` | target window |
| pane | string | no | | optional pane id/index |
| start_line | number | no | | passed to `tmux capture-pane -S` |
| end_line | number | no | | passed to `tmux capture-pane -E` |
| max_output_bytes | number | no | `100000` | bounds returned capture |

### `tmux_kill`

Kill a Ra-owned tmux session/window/pane, or all `ra__*` sessions.

```json
{ "session": "dev", "window": "tests" }
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| all | boolean | no | `false` | kill every `ra__*` session |
| session | string | unless `all` | | logical name; mapped to `ra__{session}` |
| window | string | no | | kill the window when set without `pane` |
| pane | string | no | | kill the target pane |

### `tmux_listen`

Poll a pane until a shared tmux event expression is observed. This is a
bounded tool call, not a background stream. Without an explicit `event`,
the tool keeps compatibility with older calls: it waits for
`output_match` when `pattern` is set and `output_update` otherwise.

```json
{
  "session": "dev",
  "window": "tests",
  "event": "output_match",
  "pattern": "Finished",
  "timeout_ms": 10000
}
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| session | string | yes | | logical name; mapped to `ra__{session}` |
| window | string | no | `main` | target window |
| pane | string | no | | optional pane id/index |
| event | string | no | inferred | `output_update`, `output_match`, or `hook` |
| pattern | string | no | | substring or regex to wait for |
| regex | boolean | no | `false` | interpret `pattern` as regex |
| hook | string | no | | label returned when `event:"hook"` |
| start_line | number | no | | passed to `tmux capture-pane -S` each poll |
| end_line | number | no | | passed to `tmux capture-pane -E` each poll |
| timeout_ms | number | no | `30000` | maximum listen duration |
| poll_ms | number | no | `500` | minimum is clamped to 10ms |
| max_output_bytes | number | no | `100000` | bounds returned capture/delta |

### `tmux_wait`

Block until a selected tmux event occurs or the required `timeout_ms`
expires. Use it as the active blocking companion to `tmux_listen`.
Program waits respawn the target pane with a wrapper script, matching
`tmux_run.wait:true` behavior, so the command exit status is captured
deterministically.

```json
{
  "session": "dev",
  "window": "tests",
  "event": "program_output",
  "command": "cargo test",
  "pattern": "test result:",
  "timeout_ms": 120000
}
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| event | string | yes | | `output_update`, `output_match`, `program_exit`, `program_output`, `hook`, or `sleep` |
| session | string | unless `sleep` | | logical name; mapped to `ra__{session}` |
| command | string | for program events | | shell command executed inside tmux |
| window | string | no | `main` | target window |
| pane | string | no | | optional pane id/index |
| pattern | string | for `output_match`/`hook` | | substring or regex expression |
| regex | boolean | no | `false` | interpret `pattern` as regex |
| hook | string | no | | label returned when `event:"hook"` |
| start_line | number | no | | passed to `tmux capture-pane -S` each poll |
| end_line | number | no | | passed to `tmux capture-pane -E` each poll |
| timeout_ms | number | yes | | maximum wait duration |
| duration_ms | number | for `sleep` | | sleep duration, bounded by `timeout_ms` |
| poll_ms | number | no | `500` | minimum is clamped to 10ms |
| max_output_bytes | number | no | `100000` | bounds returned capture/delta |

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

## Extended tools

These tools cover high-frequency workflows that benefit from typed
parameters, bounded output, and behavior that is stable across shell
environments. Read-only traversal uses gitignore-aware defaults and also
skips common build/VCS directories (`.git`, `target`, `node_modules`)
unless `no_ignore=true`.

### `grep`

Search file contents and return JSON matches.

```json
{ "pattern": "Tool", "path": "src", "glob": "*.rs" }
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| pattern | string | yes | | regex unless `fixed_string=true` |
| path | string | no | cwd | file or directory |
| glob | string | no | | `*.rs` matches nested basenames; slash patterns match relative paths |
| type | string | no | | file type alias such as `rust`, `python`, `json`, `markdown` |
| case_insensitive | boolean | no | `false` | |
| fixed_string | boolean | no | `false` | escape pattern before search |
| include_hidden | boolean | no | `false` | include hidden paths |
| no_ignore | boolean | no | `false` | disable gitignore/default skip filtering |
| max_matches | number | no | `200` | alias: `limit` |

### `glob`

Discover files or directories and return JSON paths.

```json
{ "pattern": "*.rs", "path": "src", "type": "file", "limit": 100 }
```

| Field | Type | Required | Default |
|-------|------|----------|---------|
| pattern | string | no | `**/*` |
| path | string | no | cwd |
| type | string | no | `any`; accepts `file`/`f`, `dir`/`d`, `symlink`/`l` |
| max_depth | number | no | unlimited |
| include_hidden | boolean | no | `false` |
| no_ignore | boolean | no | `false` |
| limit | number | no | `500` |

### `ls`

List directory entries as JSON.

```json
{ "path": "src", "recursive": true, "max_depth": 2 }
```

| Field | Type | Required | Default |
|-------|------|----------|---------|
| path | string | no | cwd |
| recursive | boolean | no | `false` |
| max_depth | number | no | `3` when recursive |
| include_hidden | boolean | no | `false` |
| no_ignore | boolean | no | `false` |
| limit | number | no | `500` |

### `fuzzy`

Rank or filter candidate strings without opening a TTY UI.

```json
{ "query": "main", "candidates": ["src/main.rs", "README.md"] }
```

| Field | Type | Required | Default |
|-------|------|----------|---------|
| candidates | array of strings | yes | |
| query | string | yes | |
| limit | number | no | `500` |
| no_sort | boolean | no | `false` |
| exact | boolean | no | `false` |

### `apply_patch`

Apply a unified diff through a controlled `git apply` subset. The tool
runs `git apply --check` first; `check_only=true` validates without
writing. It does not expose index-only, cached-only, reject, or
unsafe-path modes.

```json
{
  "patch": "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n"
}
```

| Field | Type | Required | Default |
|-------|------|----------|---------|
| patch | string | yes | |
| check_only | boolean | no | `false` |
| reverse | boolean | no | `false` |
| strip | number | no | git default |
| directory | string | no | | safe relative path only |
| cwd | string | no | process cwd |
| ignore_whitespace | boolean | no | `false` |
| whitespace | string | no | `nowarn`, `warn`, `fix`, `error`, or `error-all` |
| recount | boolean | no | `false` |
| unidiff_zero | boolean | no | `false` |

## Web documentation tools

These wrappers run the standalone
[`webfetch-cli`](https://github.com/trotsky1997/webfetch-cli) package via:

```text
npm exec --yes --package=github:trotsky1997/webfetch-cli -- webfetch-cli ...
```

Ra builds argv arrays directly, always asks webfetch-cli for `--json`,
and returns a bounded JSON envelope. `output=all` is still subject to
`max_output_bytes`. If `npm` is not on `PATH`, the tool returns
`ok:false` with `error.kind:"missing_npm"` and installation guidance.

Returned envelope:

```json
{
  "ok": true,
  "tool": "webfetch_fetch",
  "command": { "program": "npm", "args": [], "display": "npm exec ..." },
  "exit_code": 0,
  "stdout": "{... webfetch-cli JSON ...}",
  "stderr": null,
  "truncated": false
}
```

### `webfetch_fetch`

Fetch one page as Markdown and write a cache file under `.md/`.

```json
{
  "url": "https://example.com",
  "output": "toc-only",
  "raw_only": false,
  "max_output_bytes": 100000
}
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| url | string | yes | | HTTP(S) URL or bare host accepted by webfetch-cli |
| output | string | no | `toc-only` | `toc-only`, `path-only`, or `all` |
| timeout_ms | number | no | webfetch-cli default | Per-attempt timeout |
| cwd | string | no | session cwd | Directory where `.md/` is written |
| raw_only | boolean | no | `false` | Skip hosted Markdown services |
| max_output_bytes | number | no | `100000` | Bounds Ra's returned JSON envelope |

### `webfetch_crawl`

Crawl a bounded documentation subtree and mirror pages under `.md/`.

```json
{
  "url": "https://docs.python.org/3/library/index.html",
  "max_hops": 1,
  "max_pages": 30,
  "output": "summary"
}
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| url | string | yes | | Root URL |
| parent_domain | string | no | root hostname | Allowed hostname suffix |
| max_hops | number | no | webfetch-cli default | Link distance from root |
| max_pages | number | no | webfetch-cli default | Page cap |
| max_links | number | no | webfetch-cli default | Followed links per page |
| concurrency | number | no | webfetch-cli default | Parallel fetches |
| allow_query | boolean | no | `false` | Keep query-string URLs |
| output | string | no | `summary` | `summary`, `path-only`, or `all` |
| timeout_ms | number | no | webfetch-cli default | Per-attempt timeout |
| cwd | string | no | session cwd | Directory where `.md/` is written |
| raw_only | boolean | no | `false` | Skip hosted Markdown services |
| verbose | boolean | no | `false` | Include crawl progress from stderr |
| max_output_bytes | number | no | `200000` | Bounds Ra's returned JSON envelope |

## OpenSpec tool

`openspec` drives the agent-own [OpenSpec](https://github.com/Fission-AI/OpenSpec)
spec-driven-development loop through the upstream `openspec` CLI as a single
structured tool. It complements the system-prompt catalog/playbook (see
`src/openspec.rs`): the playbook tells the agent *how* to run the loop, this
tool gives it a narrow, argv-safe control surface to run it without composing
shell strings. Ra stays a *consumer* of the convention — the tool wraps the
upstream CLI and never reimplements OpenSpec schemas or lifecycle semantics.

Every invocation spawns `openspec` with `Command::arg` (no shell), requests
`--json` where the subcommand supports it, appends `--no-color`, and runs with
`stdin` closed so an unattended agent can never block on an interactive prompt.
The destructive `archive` action additionally requires `confirm_archive: true`
before it will run (the `-y` flag is then supplied internally). If `openspec`
is not on `PATH`, the tool returns `ok:false` with
`error.kind:"missing_openspec"` and installation guidance.

Returned envelope:

```json
{
  "ok": true,
  "tool": "openspec",
  "action": "status",
  "command": { "program": "openspec", "args": [], "display": "openspec ..." },
  "exit_code": 0,
  "stdout": "{... openspec JSON ...}",
  "stderr": null,
  "truncated": false
}
```

On a non-zero exit the envelope sets `ok:false` and adds
`error.kind:"openspec_error"`. Invalid requests (missing `change`, unconfirmed
`archive`, or a value that would be parsed as a flag — a leading-dash or
path-separator `change`/`item`/`artifact`, a leading-dash `path`/`tools`) fail
before spawning with `error.kind:"invalid_request"`. The upstream CLI is
Commander.js-based and does not honor a `--` end-of-options separator, so these
positionals are validated rather than escaped. `stdout` is bounded by
`max_output_bytes`, preserving valid JSON and setting `truncated:true` when
trimmed.

### Parameters

```json
{
  "action": "status",
  "change": "add-dark-mode"
}
```

| Field | Type | Required | Default | Notes |
|-------|------|----------|---------|-------|
| action | string | yes | | One of `status`, `list`, `show`, `instructions`, `validate`, `init`, `update`, `new_change`, `archive`, `workflow_state` |
| change | string | for `status`/`instructions`/`new_change`/`archive`/`workflow_state` | | Change name (kebab-case) |
| item | string | for `show` | | Change name or spec id; `validate` falls back to `change` |
| artifact | string | no | `apply` | `instructions` artifact: `proposal`, `design`, `specs`, `tasks`, or `apply` |
| specs | boolean | no | `false` | `list`/`validate`/`show` target specs instead of changes |
| tools | string | no | `none` | `init` tool surfaces: `all`, `none`, or a comma-separated list |
| path | string | no | working dir | `init`/`update` target directory |
| description | string | no | | `new_change` README description |
| confirm_archive | boolean | no | `false` | Required `true` for `archive` (destructive) |
| skip_specs | boolean | no | `false` | `archive` `--skip-specs` for tooling/doc-only changes |
| cwd | string | no | session cwd | Working directory for the spawned process |
| timeout_ms | number | no | none | Optional process timeout |
| max_output_bytes | number | no | `120000` | Bounds Ra's returned JSON envelope |

### Actions

| Action | Underlying command | Notes |
|--------|--------------------|-------|
| `status` | `openspec status --change <change> --json` | Apply-readiness state machine |
| `list` | `openspec list [--specs] --json` | Active changes (or specs) |
| `show` | `openspec show <item> --json --type change\|spec` | One change or spec |
| `instructions` | `openspec instructions <artifact> --change <change> --json` | Per-step template + deps |
| `validate` | `openspec validate [<item> --type …\|--changes\|--specs] --strict --json` | Strict mode forced on |
| `init` | `openspec init --tools <tools> [path]` | Non-interactive scaffold |
| `update` | `openspec update [path]` | Refresh instruction files |
| `new_change` | `openspec new change <change> [--description …]` | Create a change directory |
| `archive` | `openspec archive <change> -y [--skip-specs]` | Requires `confirm_archive:true` |
| `workflow_state` | `openspec status --change <change> --json` | Derives an apply-readiness summary |

`workflow_state` is a convenience action: it runs `status --json` and folds a
`workflow_state` object into the envelope summarizing which artifacts are
`ready`/`blocked`/`done`, whether `applyRequires` is satisfied (`applyReady`),
and a `nextActions` hint list. If the status JSON does not match the expected
shape, the summary is simply omitted rather than erroring.

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
