# Design

## Tool Shape

Two new built-ins live in `src/tools/webfetch.rs` and implement the existing
`Tool` trait:

- `webfetch_fetch` wraps `webfetch-cli fetch <url>`.
- `webfetch_crawl` wraps `webfetch-cli crawl <url>`.

Both tools resolve `npm` from `PATH`, then spawn:

```text
npm exec --yes --package=github:trotsky1997/webfetch-cli -- webfetch-cli ...
```

Arguments are built with `tokio::process::Command::args`; no shell string is
constructed for execution.

## Parameters

`webfetch_fetch` accepts URL, output mode, timeout, cwd, raw-only mode, and a
`max_output_bytes` budget. Fetch output modes are `toc-only`, `path-only`, and
`all`; the default is `toc-only`.

`webfetch_crawl` accepts URL, parent domain, hop/page/link/concurrency limits,
query allowance, output mode, timeout, cwd, raw-only mode, verbose progress,
and a `max_output_bytes` budget. Crawl output modes are `summary`, `path-only`,
and `all`; the default is `summary`.

`output=all` is still bounded by `max_output_bytes`. The upstream CLI receives
`--json` so Ra can preserve structured details whenever they fit in the budget.

## Missing npm

If `npm` is not found, the tool returns a successful structured JSON response
with:

- `ok: false`
- `error.kind: "missing_npm"`
- install guidance for Node.js/npm
- the command that would be run after installation

This avoids an opaque spawn error and gives the model actionable recovery
instructions.

## Output Budgeting

The returned JSON envelope is truncated by character boundary before returning
to the model. When truncation is needed, `stdout` is clipped first, then
`stderr`, while preserving valid JSON and setting `truncated: true`.

## Registration

The tools are part of the default built-in catalog when `[tools].builtin` is
empty. A non-empty allow-list remains exact.
