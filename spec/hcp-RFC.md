# RFC-0002: Harness Configuration Protocol (HCP)

```
Status:        Draft
Author:        Di Zhang (and contributors)
Drafted:       2026-05-20
Last updated:  2026-05-20
Discussion:    (TBD on PR review)
Reference impl: pi-coding-agent-python-sdk v0.4.0 @ commit 6d9608ab
Supersedes:    (none)
Superseded by: (none)
```

## Abstract

This RFC specifies the **Harness Configuration Protocol (HCP)**: a TOML-based
configuration contract for recreating a Pi coding-agent runtime. HCP describes
runtime selection, bridge settings, environment passing, model registration,
provider options, tool policy, local resources, MCP servers, hooks, session
storage, session snapshots, embedded resource files, and optional workspace
manifest declarations.

HCP is a constrained configuration protocol. A valid file is more than a bag of
fields: it has path-resolution rules, secret-handling rules, mutually exclusive
tool modes, resource materialization rules, optional workspace staging rules,
and deterministic load order. The reference Python implementation is
`pi_coding_agent.HarnessConfig` in `pi-coding-agent-python-sdk` v0.4.0.

## Motivation

Benchmarks and harnesses need to hand a coding-agent run from one process to
another without relying on local CLI flags, hidden environment setup, or
untracked resource files. A Python caller, a Harbor adapter, a CI worker, and a
future non-Python implementation should agree on:

1. which model is selected and how credentials are referenced;
2. which filesystem paths are part of the run;
3. which tools and hooks are enabled;
4. where session state comes from;
5. which files must be embedded for a portable handoff;
6. which workspace inputs and outputs are part of a sandboxed run when the
   producer has enough information to describe them.

HCP defines that contract. The TOML file is the interchange artifact; the SDK
APIs are one implementation of it.

## Terminology

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHALL NOT**,
**SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**, and **OPTIONAL** in this
document are to be interpreted as described in
[RFC 2119](https://www.rfc-editor.org/rfc/rfc2119).

- **Harness config**: a TOML document implementing this protocol.
- **Consumer**: code that loads a harness config and creates a runtime.
- **Producer**: code that writes or rewrites a harness config.
- **Runtime**: the created Pi session machinery, including backend, model,
  resources, MCP, hooks, and session manager.
- **Bridge**: the Node or Bun worker that runs the TypeScript Pi runtime.
- **CWD**: the resolved working directory for the runtime.
- **Portable config**: a harness config that can be moved to another checkout or
  worker and still recreate its referenced resources.
- **Embedded resource**: a file stored inside `[resources].embedded` so a
  consumer can materialize it before runtime creation.
- **Workspace manifest**: optional provider-neutral declarations under
  `[workspace]` that describe run inputs, sandbox placement, visibility,
  snapshot policy, and outputs. A manifest is a contract; a staging-capable
  harness or backend realizes it.
- **Workspace entry**: one declared input or staged resource in the workspace
  manifest.
- **Workspace output**: one declared output path or artifact collection rule in
  the workspace manifest.
- **Staging backend**: code outside the core Pi runtime that can realize
  workspace entries by copying, uploading, extracting, downloading, mounting, or
  otherwise preparing files for a run.

## File Format

An HCP document **MUST** be UTF-8 TOML. The top-level `version` field **MUST** be
an integer. This RFC defines `version = 1`.

```toml
version = 1

[run]
cwd = "."
backend = "node"
offline = true

[model]
provider = "novita"
id = "zai-org/glm-5.1"
api = "openai-completions"
base_url_env = "NOVITA_BASE_URL"
api_key_env = "NOVITA_API_KEY"
context_window = 128000
max_tokens = 32000

[tools]
allow = ["read", "write", "edit", "bash", "grep", "find", "ls"]

[session]
mode = "in_memory"
```

Consumers **MUST** reject malformed TOML. Consumers **SHOULD** reject unsupported
future major versions. Producers that round-trip a config **SHOULD** preserve
unknown top-level sections and unknown keys unless they are intentionally
normalizing a file and document that loss.

Field names in this RFC use snake_case. Consumers **MAY** accept camelCase
aliases where listed. Producers **SHOULD** emit snake_case.

## Load Order

A consumer creating a runtime from HCP **MUST** use this order:

1. Parse TOML and set `version = 1` when a dict input omits it.
2. Resolve the runtime CWD.
3. Install configured skill sources that need local materialization.
4. Materialize `[resources].embedded` under the embedded resource root.
5. Rewrite explicit resource references to the materialized files when needed.
6. Validate `[workspace]` declarations when present. Consumers without a
   staging backend MUST fail if required workspace entries need realization.
7. Realize required workspace entries before resources, hooks, MCP commands, or
   prompts rely on them.
8. Resolve environment variables.
9. Apply direct model API keys to runtime-only environment state.
10. Build auth storage and model registry files.
11. Build settings, resources, MCP, hooks, backend, tools, and session manager.
12. Create the session runtime.

This order matters. Embedded resources must exist before resources, hooks, MCP,
and session snapshots are resolved. Environment variables must be resolved
before model registry files and bridge startup.

## Path Resolution

If a config is loaded from a TOML file, `run.cwd` is resolved relative to that
file's parent directory. If the caller passes an explicit `cwd` override, the
override wins. If neither is present, the consumer's process CWD is used.

After CWD is resolved, relative paths in these sections **MUST** be resolved
relative to CWD:

- `run.agent_dir`
- `auth.path`
- `env.files`
- local paths under `extensions`, `skills`, `prompts` / `prompt_templates`,
  `themes`, and `resources`
- `mcp.config_path` and string-valued `mcp`
- hook `script_path`, `path`, and detectable local script paths in commands
- `session.path`, `session.session_path`, and `session.snapshot_path`
- `resources.embedded_dir`
- local source paths and relative targets in `workspace.entries` and
  `workspace.outputs`, subject to the workspace rules below

Embedded resource entry paths are different. Their `path` values are always
relative paths inside the embedded resource root. They **MUST NOT** be absolute
and **MUST NOT** contain `..`.

## Top-Level Sections

The following top-level sections are defined:

| Section | Purpose |
|---|---|
| `run` | CWD, backend, offline mode, startup verbosity, agent data directory |
| `js` | Node/Bun bridge runtime and request timeouts |
| `env` | Environment files, required names, optional names, direct set values |
| `auth` | Auth storage path |
| `model` | Selected model and provider registration |
| `provider_options` | Provider request options passed to the runtime |
| `tools` | Tool allowlist and disabling policy |
| `extensions` | TypeScript extension paths and package sources |
| `skills` | Skill paths and install/package sources |
| `prompts` / `prompt_templates` | Prompt template paths |
| `themes` | Theme paths |
| `resources` | AGENTS/context files, system prompts, embedded resources |
| `workspace` | Optional provider-neutral workspace inputs, outputs, visibility, and snapshot policy |
| `mcp` | MCP servers, direct tools, settings, adapter source |
| `hooks` | Pi hook configuration |
| `session` | Session mode, path, directory, and snapshots |

Consumers **MAY** accept top-level `cwd`, `backend`, `offline`,
`thinking_level`, `models`, `tools`, `no_tools`, `tool_choice`, and
`parallel_tool_calls` aliases for compatibility. Producers **SHOULD** place
these values in their named sections.

## Runtime Section

`[run]` controls where and how the runtime starts.

```toml
[run]
cwd = "."
agent_dir = ".pi/agent"
backend = "node"
offline = true
verbose = false
```

`backend` accepts:

- `node`: use the bridge worker.
- `in_process`: use deterministic Python-only behavior for tests and offline
  examples.

Other backend values are implementation-specific. Consumers **MUST NOT** treat
`in_process` as provider-backed parity with the bridge runtime.

When `offline = true`, consumers **MUST** set runtime state equivalent to
`PI_OFFLINE=1` and `PI_SKIP_VERSION_CHECK=1`. When `verbose = false`, consumers
**SHOULD** request quiet startup from the bridge.

`agent_dir` is where generated auth, model, and session-adjacent files may be
placed. Benchmark harnesses **SHOULD** point it at an isolated run directory
instead of a shared checkout path.

## Bridge Section

`[js]` configures the bridge worker.

```toml
[js]
runtime = "node"
runtime_path = "/usr/bin/node"
request_timeout_sec = 30
tool_timeout_sec = 120
compact_timeout_sec = 300
```

`runtime` accepts `node` or `bun`. `runtime_path` pins the executable path.
Timeouts are seconds.

`tool_timeout_sec` **SHOULD** be greater than or equal to the longest tool
callback timeout expected by the harness. A sandboxed `bash` timeout of 60
seconds paired with a 30 second bridge timeout is invalid for reliable runs.

## Environment Section

`[env]` declares which environment variables are visible to the runtime.

```toml
[env]
files = [".env"]
override = false
required = ["NOVITA_API_KEY"]
optional = ["HTTP_PROXY", "HTTPS_PROXY"]
passthrough = ["CI"]
set = { MODEL_TEMPERATURE = "0" }
```

Consumers **MUST** read `files` before checking `required`. Environment files
use simple `KEY=value` lines; blank lines and lines beginning with `#` are
ignored. Single or double quotes around a whole value are stripped.

Merge rules:

- With `override = false`, process environment values win over file values.
- With `override = true`, file values win over process environment values.
- `set` values always win and are always passed to the runtime.
- `required` and `passthrough` names must exist after merging.
- `optional` names are passed only when present.
- `model.id_env`, `model.model_env`, `model.base_url_env`, and
  `model.api_key_env` are treated as required names.

Missing required names **MUST** fail before the bridge or model provider is
called.

Shareable HCP files **MUST NOT** contain secret values. They **MUST** use
environment-variable references such as `api_key_env` and `bearerTokenEnv`.
Local ephemeral configs **MAY** use `model.api_key`, but producers **MUST NOT**
write that field into portable TOML artifacts unless the caller explicitly asks
for a secret-bearing file.

## Model Section

`[model]` selects the runtime model and, for custom providers, supplies enough
metadata to generate a bridge model registry file.

```toml
[model]
provider = "novita"
id = "zai-org/glm-5.1"
api = "openai-completions"
base_url_env = "NOVITA_BASE_URL"
api_key_env = "NOVITA_API_KEY"
context_window = 128000
max_tokens = 32000
reasoning = true
input = ["text"]
models = ["novita/*"]
thinking_level = "off"

[model.compat]
supportsDeveloperRole = false
supportsReasoningEffort = false
thinkingFormat = "zai"
```

`provider` and `id` identify the selected model. `model` is accepted as an
alias for `id`. `id_env`, `model_env`, and `modelEnv` are accepted as aliases
for obtaining the selected ID from the environment.

Custom OpenAI-compatible providers **SHOULD** declare:

- `api`, defaulting to `openai-completions` when omitted by the reference
  implementation;
- `base_url` or `base_url_env`;
- `api_key_env`;
- `context_window`;
- `max_tokens`;
- provider compatibility flags under `[model.compat]`.

`api_type`, `apiType`, `llm_api`, and `llmApi` are accepted aliases for `api`.
Known upstream API values include `openai-completions`, `openai-responses`,
`openai-codex-responses`, `azure-openai-responses`, `anthropic-messages`,
`google-generative-ai`, `mistral-conversations`, `bedrock-converse-stream`,
and `google-vertex`.

`thinking_level` accepts `off`, `minimal`, `low`, `medium`, `high`, and
`xhigh`. The top-level aliases `thinking_level` and `thinkingLevel` are
accepted for compatibility. `models` maps to scoped model selection and may
contain provider prefixes, globs, and `:<thinking>` suffixes.

Generated model registry files **MUST** store API key variable names, not API
key values.

## Provider Options

`[provider_options]` is passed through to the provider runtime after removing
first-class session controls.

```toml
[provider_options]
temperature = 0
tool_choice = "auto"
parallel_tool_calls = true
```

`tool_choice` / `toolChoice` and `parallel_tool_calls` / `parallelToolCalls`
are first-class runtime options. Consumers **MUST** pass the remaining keys as
provider options without interpreting provider-specific meaning.

## Tool Policy

Tools can be configured as a list, comma-separated string, or table.

```toml
tools = ["read", "bash"]
```

```toml
[tools]
allow = ["read", "write", "edit", "bash", "grep", "find", "ls"]
```

`allow`, `enabled`, and `names` are accepted list keys. A non-empty allowlist
means only those named tools are enabled.

`process` is a valid EFP capability name, but HCP producers **SHOULD NOT** put
it in the default agent-facing allowlist. Consumers that need `process` for
sandbox MCP or service hosting should invoke it as an internal bridge/harness
capability. Exposing `process` to the model should require explicit opt-in.

Tool disabling is mutually exclusive with a non-empty allowlist. Producers
**MUST NOT** emit both in one config. Consumers **SHOULD** treat a non-empty
allowlist as higher priority for compatibility with the reference
implementation.

To disable all tools:

```toml
[tools]
enabled = false
```

Equivalent disabling forms are:

- top-level `tools = []`
- top-level `no_tools = "all"`
- `[tools].mode = "none"`
- `[tools].no_tools = "all"`

To disable only Pi built-in tools while leaving extension and custom tools
available:

```toml
[tools]
builtin = false
```

Accepted builtin-disabling aliases include `builtins`, `builtin_tools`, and
`builtinTools`.

## Resource Sections

HCP covers explicit resources and Pi-native discovery.

```toml
[extensions]
paths = ["./extensions/todo.ts"]
sources = ["git:github.com/example/pi-extension", "npm:lsp-pi"]
enabled = true

[skills]
paths = ["./skills/reviewer"]
sources = [
  { source = "vercel-labs/agent-skills", skills = ["frontend-design"] },
]
package_sources = [
  { source = "example/pi-skills", skills = ["repo-map"] },
]
install_dir = ".pi/harness/skills-sources"

[prompt_templates]
paths = ["./prompts/deploy.md"]

[themes]
paths = ["./themes/plain.json"]

[resources]
agents_files = ["./AGENTS.md"]
context_files = false
system_prompt = "You are a focused coding agent."
append_system_prompt_paths = ["./extra-system.md"]
```

Mappings:

- `extensions.paths` maps to extension paths.
- `extensions.enabled = false` disables extension discovery.
- `extensions.sources` and `extensions.package_sources` are package sources.
- `skills.paths` maps to skill paths.
- `skills.enabled = false` disables skill discovery.
- `skills.sources` may install skills into `skills.install_dir`.
- `skills.package_sources` registers package-loaded skills.
- `prompts.paths` and `prompt_templates.paths` are aliases.
- `themes.paths` maps to theme paths.
- `resources.agents_files` declares explicit AGENTS/CLAUDE context files.
- `resources.context_files = false` disables context-file discovery.
- `resources.system_prompt`, `system_prompt_path`,
  `append_system_prompts`, and `append_system_prompt_paths` configure system
  prompts.

Remote package sources are executable supply-chain inputs. Producers **SHOULD**
pin versions or commits when a benchmark run needs reproducibility.

## Embedded Resources

Portable configs store files under `[resources].embedded`.

```toml
[[resources.embedded]]
kind = "skill"
path = ".pi/skills/reviewer/SKILL.md"
original_path = "skills/reviewer/SKILL.md"
encoding = "utf-8"
content = "...\n"
sha256 = "..."
auto_discover = true
```

Each embedded entry **MUST** include:

- `kind`: resource class;
- `path`: relative destination path under the embedded resource root;
- `encoding`: `utf-8` or `base64`;
- `content`: string content;
- `sha256`: lowercase hex SHA-256 of the decoded bytes.

Consumers **MUST** reject unsupported encodings, absolute paths, `..` path
segments, and checksum mismatches. Producers **SHOULD** include
`original_path` for auditability.

Defined `kind` values:

| Kind | Destination |
|---|---|
| `agents` | `AGENTS.md`, `CLAUDE.md`, or `.pi/agents/<n>-<filename>` |
| `skill` | `.pi/skills/<rel>` or `.agents/skills/<rel>` |
| `prompt` | `.pi/prompts/<rel>` |
| `extension` | `.pi/extensions/<rel>` |
| `theme` | `.pi/themes/<rel>` |
| `mcp` | `.pi/mcp/<rel>` |
| `mcp_adapter` | `.pi/mcp/<rel>` |
| `hook` | `.pi/hooks/<event>/<rel>` |
| `system_prompt` | `.pi/SYSTEM.md` |
| `append_system_prompt` | `.pi/APPEND_SYSTEM.md` |
| `settings` | `.pi/settings.json` |

`resources.embedded_dir` overrides the materialization root. It defaults to the
runtime CWD.

When `auto_discover = true`, consumers **MUST NOT** also rewrite explicit
`skills.paths`, `prompts.paths`, or `resources.agents_files` to the
materialized path. Pi-native discovery is expected to find the file. Consumers
**SHOULD** dedupe discovered resources by resolved file path.

The reference implementation captures two classes of files:

1. explicit local references from config sections;
2. Pi-native discovery paths under CWD:
   `.pi/skills/`, `.pi/prompts/`, `.pi/extensions/`, `.pi/themes/`,
   `.pi/SYSTEM.md`, `.pi/APPEND_SYSTEM.md`, `.pi/settings.json`,
   `.agents/skills/`, and ancestor `.agents/skills/` up to the git root.

Native sweeps skip `node_modules/`, `.git/`, `__pycache__/`, `.venv/`,
`venv/`, and hidden files inside swept subtrees. Producers **SHOULD** apply
file-count and byte-size limits before writing portable TOML.

## Workspace Manifest Section

`[workspace]` is an optional HCP extension for describing the filesystem shape
of a sandboxed run. It declares what inputs should be staged, where they should
appear, which entries are visible to the tested agent, which entries are
verifier-only or runtime-only, what should be snapshotted, and which outputs
should be collected.

`[workspace]` is a contract, not a transport. HCP consumers that implement only
the Pi runtime MAY reject configs with required workspace entries. A
staging-capable harness, such as a Harbor adapter using EFP, is responsible for
realizing workspace entries through provider-specific upload, download, copy,
extract, mount, or object-storage operations.

`resources.embedded` remains the protocol mechanism for portable Pi/Harness
resources. A workspace manifest may reference those embedded resources as one
source type, but it MUST NOT treat `[resources].embedded` as a generic dataset
or artifact container.

Example:

```toml
[workspace]
root = "/workspace"

[[workspace.entries]]
name = "harness_resources"
source = "hcp.resources.embedded"
target = "."
mode = "materialize"
snapshot = true
visibility = "agent"

[[workspace.entries]]
name = "sample_metadata"
source = "local:tests/sample_metadata.json"
target = "benchmark/sample_metadata.json"
mode = "copy"
required = true
visibility = "agent"

[[workspace.entries]]
name = "livingbench_assets"
source = "local:tests/assets/livingbench-current.tar.gz"
target = "benchmark/livingbench"
mode = "extract"
required = true
visibility = "runtime"

[[workspace.outputs]]
name = "verifier_summary"
path = "/logs/verifier/native_summary.json"
required = true
backend = "harness-artifact"

[[workspace.outputs]]
name = "agent_artifacts"
path = "/logs/artifacts"
required = false
backend = "harness-artifact"
```

### Workspace Root

`workspace.root` is the preferred sandbox working directory for the run. It MAY
be absolute inside the sandbox, such as `/workspace`, or relative to `run.cwd`
when the consumer uses a local process sandbox. If omitted, consumers MAY use
`run.cwd` or a benchmark-defined workspace root.

`workspace.root` is not a host path. Producers MUST NOT write host-specific
absolute paths into portable configs unless the entry is marked host-only and
excluded from portable snapshots.

### Workspace Entries

Each `[[workspace.entries]]` item declares one staged input or resource.

Required fields:

- `name`: stable logical name unique within the manifest;
- `source`: source descriptor;
- `target`: sandbox target path;
- `mode`: realization mode.

Optional fields:

- `required`: boolean, defaulting to `true`;
- `visibility`: `agent`, `runtime`, or `verifier`, defaulting to `agent`;
- `snapshot`: boolean, defaulting to `false`;
- `max_files`, `max_file_bytes`, and `max_total_bytes`: per-entry limits;
- `sha256`: expected source digest when the source is a single file or archive;
- `description`: human-readable purpose.

Defined `source` forms:

| Source form | Meaning |
|---|---|
| `hcp.resources.embedded` | The current HCP `[resources].embedded` entries |
| `local:<path>` | A local file or directory path resolved relative to CWD |
| `inline:<name>` | Data generated by the producer or harness by logical name |
| `artifact:<name>` | A harness-managed artifact reference |
| `s3:<uri>` / `gs:<uri>` / `az:<uri>` / `r2:<uri>` | Optional object-storage references |

Object-storage source forms require a consumer-provided backend binding. S3 is
supported by the unified harness when an S3-compatible resolver is configured.
GCS, Azure Blob, and R2 remain reserved source forms until matching resolvers
exist. Portable configs MUST store backend references and environment-variable
names, not secret values.

Defined `mode` values:

| Mode | Meaning |
|---|---|
| `copy` | Copy a file or directory to `target` |
| `extract` | Extract an archive into `target` |
| `mount` | Expose the source at `target` through a backend mount when available |
| `materialize` | Let the consumer materialize an HCP-native source such as embedded resources |

Consumers MAY implement `mount` as copy-on-start for backends without native
mount support, but they MUST report the actual realization mode in provenance.
When native mount support is available, the harness MUST pass only descriptors
or credential references to the sandbox provider. Portable manifests MUST NOT
inline object-storage secrets.

Entry `target` paths are sandbox paths. Relative targets are resolved under
`workspace.root`. Unless a consumer explicitly supports absolute sandbox
targets, targets MUST stay under `workspace.root`. Consumers MUST reject parent
directory escapes in relative targets.

### Visibility

`visibility` describes who may observe the staged entry:

| Visibility | Meaning |
|---|---|
| `agent` | The tested agent may read or modify the entry through normal tools |
| `runtime` | Runtime services may use the entry, but it is not intentionally exposed to the tested agent |
| `verifier` | Verifier-only input; it MUST NOT be placed in an agent-visible path |

Visibility is an HCP policy label. It does not guarantee isolation by itself.
The staging backend MUST realize isolation using sandbox paths, mounts,
permissions, separate verifier containers, or equivalent harness mechanisms.

### Workspace Outputs

Each `[[workspace.outputs]]` item declares one output path or artifact expected
after the run or verifier completes.

Required fields:

- `name`: stable logical output name unique within the manifest;
- `path`: sandbox or harness path to collect.

Optional fields:

- `required`: boolean, defaulting to `false`;
- `backend`: destination backend, such as `harness-artifact`, `local`,
  `s3`, `gs`, `az`, or `r2`;
- `destination`: backend destination descriptor, required by object-storage
  output backends such as `s3`;
- `visibility`: `runtime`, `verifier`, or `public`, defaulting to `runtime`;
- `max_files`, `max_file_bytes`, and `max_total_bytes`: collection limits;
- `description`: human-readable purpose.

Workspace outputs are artifacts. They MUST NOT be merged into
`resources.embedded` unless an output is explicitly also a Pi/Harness resource
snapshot.

### Snapshot Policy

For entries with `snapshot = true`, a staging-capable consumer SHOULD collect
the current run-end state under the entry's target, subject to configured
limits. When the source is `hcp.resources.embedded`, snapshots SHOULD merge back
into `[resources].embedded` using the embedded resource rules in this RFC.

For other source types, snapshot output SHOULD be written as artifact
provenance or a future manifest diff. Producers SHOULD NOT embed arbitrary
benchmark workspaces, datasets, logs, or verifier outputs into
`resources.embedded`.

### Workspace Provenance

Consumers that realize a workspace manifest SHOULD emit provenance in session
metadata, verifier summaries, ATIF exports, or an equivalent run artifact.
Provenance SHOULD include:

- manifest version and normalized entries;
- source descriptors after redaction;
- target paths;
- realized mode;
- hashes when available;
- missing optional entries;
- failed required entries;
- collected outputs;
- snapshot summaries.

Provenance MUST redact secret-shaped values and MUST NOT expose verifier-only
content in public artifacts.

### Workspace Validation

Consumers that support `[workspace]` MUST fail before provider calls when:

- an entry lacks `name`, `source`, `target`, or `mode`;
- duplicate entry or output names appear;
- a required source cannot be found or resolved;
- a target path escapes the allowed workspace root;
- a verifier-only entry would be staged into an agent-visible path;
- the selected realization mode is unsupported and no compatible fallback
  exists;
- an expected `sha256` does not match.

Consumers SHOULD warn or fail when:

- two entries write to the same target path;
- a portable config contains host-only absolute local paths;
- an object-storage source lacks a configured backend;
- staging or output collection exceeds configured file or byte limits.

## MCP Section

MCP can be inline or path-based.

```toml
[mcp.servers.time]
command = "uvx"
args = ["mcp-server-time"]
placement = "bridge"

[mcp.servers.playwright]
command = "npx"
args = ["@playwright/mcp@latest"]
env = { PLAYWRIGHT_MCP_ENV = "enabled" }
auth = "bearer"
bearerTokenEnv = "MCP_TEST_BEARER_TOKEN"
```

`mcp = "./mcp.json"` is accepted. `servers` is a TOML-friendly alias for
`mcpServers`. `mcp.config_path` / `configPath` points to an MCP JSON file.
`mcp.adapter_source` / `adapterSource` points to an adapter extension file.

`placement` controls where a server command runs:

- `bridge` means the command starts beside the Pi bridge. This is the default.
- `sandbox` means the command starts inside the EFP Environment.

For `placement = "sandbox"`, `transport = "stdio"` requires a process-capable
EFP implementation. The consumer **MUST** start the MCP command through EFP
`process`, forward MCP stdio bytes between the bridge and the sandbox process,
and stop the process when the session ends. If EFP `process` is unavailable,
the consumer **MUST** fail fast. It **MUST NOT** fall back to running the MCP
server on the bridge host.

For `placement = "sandbox"` with `transport = "http"` or `transport = "sse"`,
the consumer **MUST** provide a sandbox URL or tunnel configuration and **MUST**
fail fast if the sandbox endpoint cannot be reached.
If the consumer starts that server from a sandbox command, it **MUST** own the
process lifecycle and stop the server when the session ends.

MCP bearer tokens and other secrets **MUST** be referenced by environment
variable names. Inline token values are not valid for portable HCP files.

## Hooks

Hooks use Pi's settings shape. HCP also defines a compact TOML shape:

```toml
[hooks.PreToolUse.block_rm]
matcher = "bash"
command = "python ./hooks/block_rm.py"
timeout = 5
condition = "bash(*)"
run_async = false
```

Consumers **MUST** normalize compact hook tables into Pi hook settings before
session creation. Hook commands are executable code. Producers **SHOULD** embed
local hook scripts when writing portable configs and **SHOULD NOT** rely on a
bare relative command path being present on the destination machine.

When an embedded hook records `command_path_index`, consumers **SHOULD** rewrite
that command argument to the materialized path.

## Session Section

`[session]` controls session persistence and restore.

```toml
[session]
mode = "create"
session_dir = ".pi/sessions"
```

Modes:

- `create`: create a new persistent session.
- `continue_recent`: open the newest session in the session directory, or
  create one.
- `open`: open `session.path`.
- `in_memory`: keep session state in memory only.

`open` mode **MUST** include `path`, `session_path`, or `sessionPath`.

Snapshots can be external:

```toml
[session]
mode = "in_memory"
snapshot_path = "session-snapshot.json"
```

or embedded:

```toml
[session]
mode = "in_memory"

[session.snapshot]
format = "pi-session"
encoding = "zlib+base64+json"
content = "eJyrVspMUbIy1FEqT8pJBYq..."
```

Embedded snapshots **MUST** use `encoding = "zlib+base64+json"` for the
compressed JSON payload defined by the reference implementation. Snapshot
restore **MUST** create an in-memory session manager and **MUST NOT** continue
writing to the original session file named inside the snapshot.

Consumers **MAY** accept native SDK session JSON, Pi JSONL sessions, Harbor ATIF
trajectory JSON, and AgentForge Qwen35 training records as external snapshot
payloads.

## Canonical Producer Behavior

An HCP producer that emits portable TOML **SHOULD**:

- emit `version = 1`;
- use snake_case field names;
- keep secrets out of TOML;
- write selected model credentials as environment-variable names;
- embed local resources needed for a destination run;
- include SHA-256 for every embedded resource;
- set `session.mode = "in_memory"` when embedding a session snapshot;
- write files atomically when updating an on-disk config;
- preserve unknown sections and keys when round-tripping an existing config.

The reference writer serializes unsupported Python `None` values as empty
strings. Protocol producers **SHOULD NOT** rely on `None` as a meaningful TOML
value.

## Consumer Validation

Consumers **MUST** fail before provider calls when:

- TOML parsing fails;
- a required environment variable is missing;
- `session.mode = "open"` lacks a session path;
- an embedded resource path escapes its root;
- an embedded resource checksum does not match;
- an embedded resource encoding is unsupported;
- the selected backend or session mode is unsupported.

Consumers **SHOULD** warn or fail when:

- a portable config contains inline secret-shaped values;
- disabling tool modes are combined with a non-empty allowlist;
- bridge tool timeout is lower than known harness tool timeouts;
- remote package sources are unpinned;
- embedded resources exceed local size policy.

## Security Considerations

HCP can start processes, load code, call model providers, read local files, and
restore session history. Treat incoming HCP files as executable configuration.

- Secrets belong in environment variables, not TOML.
- Producers must redact token-shaped values in exported provenance.
- Consumers must validate embedded resource paths before writing files.
- Hook commands, extension paths, skill sources, and MCP server commands should
  be reviewed before execution.
- Environment files should be local to the run directory and should not be
  embedded in shareable configs by default.
- Session snapshots can contain user prompts, tool outputs, and model text.
  Exporters should apply the same redaction policy used for trajectories.

## Test-by-Contract Recommendations

HCP implementations **SHOULD** have tests for:

- TOML load -> dump -> load round trips for all defined sections;
- path resolution relative to config file and CWD;
- missing environment variable failure before bridge creation;
- custom provider model registry generation without secret values;
- tool allowlist and no-tools precedence;
- resource embedding, path rejection, base64 decode, and checksum mismatch;
- MCP `servers` alias conversion;
- MCP `placement = "sandbox"` with `transport = "stdio"` failing fast when EFP
  `process` is unavailable;
- MCP `placement = "sandbox"` with `transport = "sse"` failing fast when no
  reachable sandbox URL can be resolved;
- hook compact-shape normalization;
- session snapshot encode/decode and in-memory restore;
- unknown section preservation.
- optional workspace manifest validation for safe targets, required sources,
  duplicate names, visibility conflicts, unsupported modes, and output
  declarations;
- workspace staging provenance redaction when a staging backend is used.

At least one live test **SHOULD** create a provider-backed bridge session from a
TOML config and verify that selected model, provider options, tools, and
resource overrides reach the bridge.

## Relationship to EFP

RFC-0001 defines how model-callable tools are forwarded to an execution
environment. HCP defines how the agent runtime is configured before those tools
exist. The protocols meet at resource lifecycle boundaries: HCP can embed and
materialize files; an EFP implementation can restore those files into a sandbox
and snapshot edits back into a current harness config.

HCP does not change the seven default agent-facing EFP tool names or the
optional `process` capability. It can, however, require a process-capable EFP
implementation when MCP servers or other runtime services are placed inside a
sandbox.

The optional `[workspace]` manifest is also an HCP contract that may be realized
through EFP or through a harness-specific sandbox backend. HCP defines the
logical entries, paths, visibility labels, and output declarations. EFP or the
harness backend performs concrete file operations such as upload, download,
archive extraction, process startup, and artifact collection. Workspace staging
operations are lifecycle actions and MUST NOT be exposed as default
model-callable tools.

## Reference Implementation

Python: `pi-coding-agent-python-sdk` v0.4.0:

- `src/pi_coding_agent/harness_config.py`
- `src/pi_coding_agent/harness_schema.py`
- `src/pi_coding_agent/schemas/harness-config.schema.json`

Primary public entry points:

- `HarnessConfig.from_dict`
- `HarnessConfig.from_toml`
- `HarnessConfig.load_toml`
- `HarnessConfig.to_toml`
- `HarnessConfig.embed_resources`
- `HarnessConfig.with_session_snapshot`
- `HarnessConfig.dump_with_session_snapshot`
- `create_agent_session_from_config`
- `create_agent_session_runtime_from_config`

Reference tests:

- `tests/test_harness_config.py`
- `tests/test_harness_schema.py`
- `tests/test_sdk_example_parity.py`
- `tests/test_llm_api_wire.py`

## Changelog

- 2026-05-20: Draft.
- 2026-05-20: Add optional `[workspace]` manifest extension for sandbox
  inputs, outputs, visibility, snapshot policy, and staging provenance.
