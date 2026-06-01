# Add Webfetch Built-In Tools

## Why

Ra agents currently fetch web documentation through `bash` or external skills.
That works, but it asks the model to compose shell strings, hides the command
shape from the tool catalog, and leaves output-size control to ad hoc prompting.

## What Changes

- Add `webfetch_fetch` for single-page Markdown fetches backed by
  `webfetch-cli fetch`.
- Add `webfetch_crawl` for bounded documentation crawls backed by
  `webfetch-cli crawl`.
- Invoke the upstream package through
  `npm exec --yes --package=github:trotsky1997/webfetch-cli -- webfetch-cli`
  using argv-safe process spawning.
- Return a stable JSON envelope with command metadata, exit status, stdout,
  stderr, truncation state, and structured install guidance when `npm` is
  missing.
- Bound all returned output through `max_output_bytes`, including
  `output=all`.

## Non-Goals

- Do not add a `[webfetch]` config section yet.
- Do not vendor or reimplement webfetch-cli's fetching and crawling logic.
- Do not remove the standalone `webfetch-cli` skill; it remains useful outside
  Ra-native tool environments.
