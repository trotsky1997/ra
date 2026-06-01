# Tools Delta

## ADDED Requirements

### Requirement: Native Webfetch Tool Catalog

Ra SHALL include `webfetch_fetch` and `webfetch_crawl` in the default built-in
catalog when `[tools].builtin` is empty.

#### Scenario: Empty allow-list exposes webfetch tools

- **GIVEN** `[tools].builtin` is empty
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog includes `webfetch_fetch` and `webfetch_crawl`

#### Scenario: Non-empty allow-list remains exact

- **GIVEN** `[tools].builtin` contains only `webfetch_fetch`
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog contains `webfetch_fetch` and omits `webfetch_crawl`

### Requirement: Webfetch Fetch Wrapper

Ra SHALL provide a `webfetch_fetch` tool that invokes `webfetch-cli fetch`
through `npm exec` with argv-safe arguments and bounded output.

#### Scenario: Fetch maps parameters to webfetch-cli argv

- **GIVEN** a caller provides a URL, output mode, timeout, cwd, and raw-only flag
- **WHEN** `webfetch_fetch` builds its invocation
- **THEN** it invokes `npm exec --yes --package=github:trotsky1997/webfetch-cli -- webfetch-cli fetch <url>` with the matching flags

#### Scenario: Fetch all output is bounded

- **GIVEN** `webfetch_fetch` is called with `output: "all"`
- **WHEN** the upstream output exceeds `max_output_bytes`
- **THEN** Ra returns valid JSON with `truncated: true`

### Requirement: Webfetch Crawl Wrapper

Ra SHALL provide a `webfetch_crawl` tool that invokes `webfetch-cli crawl`
through `npm exec` with argv-safe arguments and bounded output.

#### Scenario: Crawl maps bounds to webfetch-cli argv

- **GIVEN** a caller provides crawl limits and query behavior
- **WHEN** `webfetch_crawl` builds its invocation
- **THEN** it invokes `webfetch-cli crawl` with the matching
  `--parent-domain`, `--max-hops`, `--max-pages`, `--max-links`,
  `--concurrency`, and `--allow-query` flags

#### Scenario: Crawl verbose progress is observable

- **GIVEN** `webfetch_crawl` is called with `verbose: true`
- **WHEN** webfetch-cli emits progress on stderr
- **THEN** Ra includes bounded stderr in the returned JSON envelope

### Requirement: Missing Npm Guidance

Ra SHALL return a structured, actionable response when `npm` is not available
instead of surfacing an opaque spawn failure.

#### Scenario: npm is missing

- **GIVEN** `npm` is not found on `PATH`
- **WHEN** `webfetch_fetch` or `webfetch_crawl` executes
- **THEN** the tool returns JSON with `ok: false`, `error.kind:
  "missing_npm"`, and installation guidance
