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

- **One binary, three personalities.** `ra run` for one-shot prompts,
  `ra acp` to serve as an ACP agent over stdio for editors (Zed,
  Neovim ACP, …), `ra serve` to expose itself as an A2A agent over
  HTTP/JSON-RPC + REST + gRPC.
- **Built-in tool set.** `read`, `write`, `edit`, `bash`, plus
  `grep` (ripgrep), `find` (fd), `ls` (eza/exa). External-binary tools
  detect themselves at startup; missing dependencies are logged once
  and the tool drops out of the registry. ACP hosts also get
  `fs/read_text_file`, `fs/write_text_file`, and `terminal/*`
  reverse-call routing automatically.
- **Multi-backend LLM layer.** Anthropic, OpenAI, Google, DeepSeek,
  Ollama, Groq, xAI — and any OpenAI-compatible Responses-API endpoint
  — via the [graniet/llm](https://github.com/graniet/llm) crate.
- **Skills + prompts + AGENTS.md.** [agentskills.io v1](https://agentskills.io/)
  SKILL.md files (YAML frontmatter, progressive disclosure),
  user-defined slash-command prompt templates, and
  [agents.md](https://agents.md/) auto-discovery, all unified through a
  single resource bundle that becomes the agent's system prompt.
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
- **A2A everywhere.** Serve as an A2A agent (with optional Bearer auth
  on JSON-RPC/REST/gRPC) and consume remote A2A agents as local tools.
- **Trajectories on disk.** Every session is persisted as ATIF v1.7
  JSONL trees under `RA_HOME/sessions/<cwd-hash>/`.
- **Observability.** Wired through NeMo Relay so every prompt, tool
  call, and turn produces ATOF events; pipe them to stderr / a file /
  an OTLP endpoint.

## Quick start

```bash
# Mock model (no API key — uses the bash:/read:/grep:/find:/edit:… prefixes)
cargo run -- "bash:echo hello && uname -sr"

# Real model — Anthropic
ANTHROPIC_API_KEY=sk-ant-... cargo run -- "Use grep to find every TODO."

# Real model — OpenAI
OPENAI_API_KEY=sk-... cargo run -- "Read Cargo.toml and explain it."

# Real model — pi (Responses API endpoint)
PI_API_KEY=sk-... \
PI_BASE_URL=https://pi-api-us.macaron.xin/v1/ \
PI_MODEL=gpt-5.5 \
cargo run -- "Use bash to print the date, then summarize."
```

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
`[resources]`, `[a2a.serve]` + auth, `[[a2a.remote_agents]]`,
`[[mcp.servers]]`, `[[hooks.PreToolUse]]`, …) see
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
| `grep`  | Wraps `rg` (ripgrep). pattern + path + glob/type/case/context filters. Skipped if `rg` is missing. |
| `find`  | Wraps `fd` (or `fdfind`). pattern + path + type/extension/hidden filters. Skipped if absent. |
| `ls`    | Wraps `eza` (or `exa`). path + all/long/tree/level. Skipped if absent. |

Output from external-binary tools is capped at 64 KiB per call.
Toggle the catalog via `[tools] builtin = […]`; an empty allow-list
ships every tool whose dependencies resolve.

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
│   ├── fs.rs        Write, Edit
│   └── search.rs    Grep (rg), Find (fd), Ls (eza)
├── tool_ctx.rs      ToolCtx + ClientHandle (ACP reverse calls)
├── acp_server.rs    serve as ACP agent over stdio
├── a2a_server.rs    serve as A2A agent (HTTP + gRPC, optional Bearer)
├── a2a_tool.rs      consume remote A2A agents as local tools
├── mcp.rs           rmcp client → Ra tools
├── skills.rs        Skill / Prompt / AGENTS.md / ResourceBundle
├── hooks.rs         Claude-Code-spec hook engine
├── store.rs         ATIF trajectory persistence
├── atif_codec.rs    in-memory ↔ ATIF JSON
└── nemo_obs.rs      NeMo Relay observability scopes
```

## Project status

Working: streaming text + parallel tool calls + multi-turn tool loop,
real LLM backends (Anthropic / OpenAI / Google / pi / …), ACP v1
end-to-end, A2A bidirectional, MCP stdio + HTTP, ATIF/ATOF on disk,
**session resumption from a saved trajectory** (`ra resume <id>` /
`ra sessions`, plus auto-resume on the A2A path), HCP TOML config,
skills/prompts/AGENTS.md system-prompt unification, Claude-Code-shaped
hooks, optional Bearer auth on A2A serve.

Not yet: interactive TUI, internal-Rust replacements for ripgrep / fd
/ eza (planned).
