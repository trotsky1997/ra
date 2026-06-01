# Ra

> 𓂀  rust-native agent · sun god, illuminating tools

A Rust-native coding agent. Implements the **Agent Client Protocol (ACP)**
end-to-end, speaks the **Agent2Agent (A2A)** protocol bidirectionally,
loads **Model Context Protocol (MCP)** servers as tools, persists
trajectories in **ATIF** (Harbor RFC-0001), and emits observability
events in **ATOF** (NeMo Relay).

The name has two readings:

- **Ra** — the Egyptian sun god; every turn re-illuminates the context,
  broadcast events radiate outward like rays.
- **Ra** — **R**ust-native **a**gent.

The eye watching from the banner is 𓂀 (U+13080).

## What you get

- **One binary, four personalities.** `ra run` for one-shot prompts,
  `ra acp` to serve as an ACP agent over stdio for editors (Zed,
  Neovim ACP, …), `ra serve` to expose itself as an A2A agent over
  HTTP/JSON-RPC + REST + gRPC, and **`ra tui`** for an interactive
  terminal chat (requires `--features tui`, which pulls in
  [opentui_rust](https://github.com/Dicklesworthstone/opentui_rust)
  and needs nightly Rust).
- **Built-in tool set.** `read`, `write`, `edit`, and `bash`. Use
  `bash` for shell-native commands such as `grep`, `find`, and `ls`
  instead of separate function-call wrappers. ACP hosts also get
  `fs/read_text_file`, `fs/write_text_file`, and `terminal/*`
  reverse-call routing automatically.
- **Multi-backend LLM layer.** Anthropic, OpenAI, Google, DeepSeek,
  Ollama, Groq, xAI — and any OpenAI-compatible Responses-API endpoint
  — via the [graniet/llm](https://github.com/graniet/llm) crate.
- **Skills + prompts + AGENTS.md.** [agentskills.io v1](https://agentskills.io/)
  SKILL.md files (YAML frontmatter, progressive disclosure),
  user-defined slash-command prompt templates, and
  [agents.md](https://agents.md/) auto-discovery, all unified through a
  single resource bundle that becomes the agent's system prompt. Ships
  native [skills.sh](https://github.com/vercel-labs/skills) support:
  `npx skills add <repo> -a universal` lands SKILL.md files in
  `./.agents/skills/` and Ra picks them up automatically — no config
  needed.
- **Native OpenSpec.** Auto-discovers a project's
  [OpenSpec](https://github.com/Fission-AI/OpenSpec) `openspec/`
  directory (walking cwd → git root, like AGENTS.md) and folds a catalog
  of its capability specs and active changes — with requirement counts
  and task progress — into the system prompt via progressive disclosure.
  By default it also folds in an **agent-own spec-driven playbook**: how
  to drive the `openspec` CLI non-interactively (`init --tools`,
  `new change`, the `status`/`instructions --json` state machine,
  `validate --strict`, `archive -y`) with no human in the loop — and on a
  repo with no `openspec/` yet, a bootstrap hint so the agent can adopt
  the convention itself (`[openspec] agent_own = false` for catalog only).
  Ra consumes the convention; it doesn't reimplement the `openspec` CLI.
- **Agent-owned Graphify support.** Auto-discovers or targets
  [Graphify](https://github.com/safishamsi/graphify)
  `graphify-out/graph.json`, folds an R2A graph workflow into the
  system prompt, and exposes `graphify_ensure`, `graphify_impact`, and
  `graphify_update` alongside native query/path/explain tools. Ra can
  detect missing or stale graphs, guide or run a low-cost AST refresh,
  and use the graph for requirement intake, planning, verification, and
  artifact traceability without requiring the user to run Graphify first.
- **HCP-flavored TOML config.** Single `ra.toml` (or `~/.ra.toml`)
  configures models, tools, skills, prompts, hooks, MCP servers, A2A
  serve/auth, remote A2A agents, observability backend, …
- **Lifecycle hooks.** Wire-compatible with the
  [Claude Code hooks spec](https://code.claude.com/docs/en/hooks.md):
  `PreToolUse` / `PostToolUse` / `UserPromptSubmit` / `Stop` events
  with the standard `decision: "block"` / `permissionDecision: "deny"`
  / `additionalContext` / `continue: false` decision schema, plus
  exit-code-2 handling.
- **MCP client.** [rmcp 1.7](https://github.com/modelcontextprotocol/rust-sdk)
  with stdio + Streamable HTTP transports; remote tools surface as
  Ra tools the LLM can call.
- **Native RTK ([Rust Token Killer](https://github.com/rtk-ai/rtk)) support.**
  When `rtk` is on PATH, `bash` commands are routed through
  `rtk rewrite` before execution, swapping verbose `git status` /
  `cargo test` / `kubectl` output for RTK's token-compressed
  equivalents (60–90% savings).
  Configurable via `[rtk] mode = "auto" | "on" | "off"`.
- **A2A everywhere.** Serve as an A2A agent (with optional Bearer auth
  on JSON-RPC/REST/gRPC) and consume remote A2A agents as local tools.
- **Trajectories on disk.** Every session is persisted as ATIF v1.7
  JSONL trees under `RA_HOME/sessions/<cwd-hash>/`.
- **Observability.** Wired through NeMo Relay so every prompt, tool
  call, and turn produces ATOF events; pipe them to stderr / a file /
  an OTLP endpoint.

## Quick start

```bash
# Create a project-local config and skills directory
cargo run -- init

# Mock model (no API key — uses the bash:/read:/write:/edit: prefixes)
cargo run -- "bash:echo hello && uname -sr"

# Real model — Anthropic
ANTHROPIC_API_KEY=sk-ant-... cargo run -- "Use bash to grep for every TODO."

# Real model — OpenAI
OPENAI_API_KEY=sk-... cargo run -- "Read Cargo.toml and explain it."

# Real model — pi (Responses API endpoint)
PI_API_KEY=sk-... \
PI_BASE_URL=https://pi-api-us.macaron.xin/v1/ \
PI_MODEL=gpt-5.5 \
cargo run -- "Use bash to print the date, then summarize."
```

`ra init` writes a minimal `ra.toml` and creates `.ra/skills/`.
Existing files are left untouched; pass `--force` to replace generated
files, or `--example-skill` to add `.ra/skills/example/SKILL.md`.

To serve as an editor agent over ACP:

```bash
cargo run -- acp
# (your editor connects to Ra over stdio JSON-RPC 2.0)
```

To serve as a remote A2A agent on the network:

```bash
cargo run -- serve --http-port 3000 --grpc-port 50051
# agent card:    http://localhost:3000/.well-known/agent-card.json
# JSON-RPC:      http://localhost:3000/jsonrpc
# REST:          http://localhost:3000/rest
# gRPC:          localhost:50051
```

To pick up a saved trajectory and continue the conversation:

```bash
ra sessions                     # list saved sessions in this cwd's bucket
ra resume <id> "follow-up prompt"
```

To launch the interactive terminal UI (requires nightly Rust + the
`tui` feature, since opentui_rust uses edition 2024):

```bash
rustup toolchain install nightly
cargo +nightly run --features tui --bin ra -- tui
# Type a prompt, Enter submits. Ctrl-C cancels a running prompt;
# press Ctrl-D or Ctrl-C twice to quit. Trajectory is saved on exit
# under the same bucket `ra resume` reads from.
```

A2A clients reconnecting with a known `task_id` are auto-resumed —
the server hydrates the message log from disk before processing the
new turn, so editors / agent-team teammates can pick up exactly where
they left off across `ra serve` restarts.

## Configuration

A minimal `ra.toml`:

```toml
version = 1

[model]
default = "anthropic"

[[models]]
name = "anthropic"
backend = "anthropic"
model_id = "claude-opus-4-5"
api_key_env = "ANTHROPIC_API_KEY"
```

For the full annotated example covering every section
(`[run]`, `[obs]`, `[tools]`, `[skills]`, `[prompts]`, `[agents_md]`,
`[openspec]`, `[graphify]`, `[resources]`, `[a2a.serve]` + auth,
`[[a2a.remote_agents]]`, `[[mcp.servers]]`,
`[[hooks.PreToolUse]]`, …) see
[`spec/ra.toml.example`](spec/ra.toml.example). The schema is in
[`spec/ra-config.schema.json`](spec/ra-config.schema.json).

Resolution order: `--config <path>` → `$RA_CONFIG` → `./ra.toml` →
`~/.ra.toml`. Env vars still take precedence over file values.

## Built-in tools

| Tool   | Notes |
|--------|-------|
| `read`  | Reads a file; routes through ACP `fs/read_text_file` when an editor is connected, else local fs. |
| `write` | Writes a file in full; ACP `fs/write_text_file` when available. Auto-creates parent dirs locally. |
| `edit`  | Claude-Code-shaped: `{path, old_string, new_string, replace_all}`. Refuses ambiguous matches by default. |
| `bash`  | Runs a shell command; ACP `terminal/*` (with permission gating) when available, else `/bin/sh -c`. |
| `git`   | Runs native `git` with argv-safe arguments; ACP hosts use the terminal reverse-call with shell quoting. This path does not use RTK. |
| `gh`    | Runs native GitHub CLI (`gh`) with argv-safe arguments; ACP hosts use the terminal reverse-call with shell quoting. This path does not use RTK. |
| `graphify_ensure` / `graphify_impact` / `graphify_update` / `graphify_query` / `graphify_path` / `graphify_explain` | Added when `[graphify]` is enabled; maintains and uses Graphify as Ra's R2A project graph. |

Toggle the catalog via `[tools] builtin = […]`; an empty allow-list
ships every built-in tool. Prefer `git` / `gh` for those CLIs; `grep`,
`find`, `ls`, and similar shell commands intentionally go through
`bash` rather than separate built-in tool definitions.

Native `git` / `gh` prioritize argv safety over RTK rewriting. If a
high-volume `git` or `gh` command needs RTK output compression, run it
through `bash` instead so the existing RTK rewrite path can apply.

## Protocols & specs

Authoritative schemas live in [`spec/`](spec/) — see
[`spec/README.md`](spec/README.md) for the index.

| Surface | Spec | Status |
|---------|------|--------|
| ACP | `spec/acp-v1.json` (+ `unstable`) | full v1 implementation |
| A2A | `spec/a2a.proto`, `spec/a2a-v1.json` | client + server (HTTP/REST/gRPC) |
| ATIF | `spec/atif-v1.7.json` | written on every session |
| ATOF | `spec/atof-v0.1.json` | emitted via NeMo Relay |
| HCP-flavored config | `spec/ra-config.schema.json` | live JSON Schema |
| Skills | [agentskills.io v1](https://agentskills.io/specification.md) | YAML frontmatter, progressive disclosure |
| AGENTS.md | [agents.md](https://agents.md/) | nearest-file-wins discovery |
| OpenSpec | [Fission-AI/OpenSpec](https://github.com/Fission-AI/OpenSpec) | `openspec/` discovery, progressive disclosure |
| MCP | rmcp 1.7 (stdio + streamable HTTP) | client only |
| Hooks | [Claude Code hooks](https://code.claude.com/docs/en/hooks.md) | wire-compatible subset |

## Architecture

```
src/
├── lib.rs           re-exports
├── main.rs          CLI: run / acp / serve
├── config.rs        ra.toml loader (HCP-flavored)
├── model.rs         Model trait + MockModel
├── llm_model.rs     graniet/llm bridge → multi-backend
├── session.rs       turn loop + tool execution + hook gates
├── session_runner.rs streaming runner over Session
├── events.rs        Event / ToolCall / ToolResult
├── tools/           built-in tool catalog
│   ├── core.rs      Tool trait, Read, Bash
│   └── fs.rs        Write, Edit
├── tool_ctx.rs      ToolCtx + ClientHandle (ACP reverse calls)
├── acp_server.rs    serve as ACP agent over stdio
├── a2a_server.rs    serve as A2A agent (HTTP + gRPC, optional Bearer)
├── a2a_tool.rs      consume remote A2A agents as local tools
├── mcp.rs           rmcp client → Ra tools
├── skills.rs        Skill / Prompt / AGENTS.md / ResourceBundle
├── openspec.rs      OpenSpec project discovery → system-prompt catalog
├── hooks.rs         Claude-Code-spec hook engine
├── store.rs         ATIF trajectory persistence
├── atif_codec.rs    in-memory ↔ ATIF JSON
└── nemo_obs.rs      NeMo Relay observability scopes
```

## Project status

What's done, what's next, and the anti-roadmap (deliberate non-goals)
all live in [ROADMAP.md](ROADMAP.md). Contributors and AI coding
agents working on this repo should also read [CLAUDE.md](CLAUDE.md)
for build commands, project conventions, and the load-bearing
invariants of the hot files.
