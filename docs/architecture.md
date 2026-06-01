# Ra architecture

How a prompt becomes events, tool calls, and a saved trajectory. The
`spec/` collection is normative for the *wire* formats; this document
covers the *internal* flow that sits behind them.

> Companion reading: [`../README.md`](../README.md) for the feature
> surface, [`../ROADMAP.md`](../ROADMAP.md) for what's done / next,
> [`../CLAUDE.md`](../CLAUDE.md) for build commands and hot-file
> invariants, and [`../spec/README.md`](../spec/README.md) for the
> protocol schemas.

## The one-paragraph version

Every entry point (CLI `run`, ACP `session/prompt`, A2A `message/send`,
TUI submit) ends up calling `Session::prompt`. `Session` owns the
message history, the model pointer, the tool catalogue, and a
`tokio::sync::broadcast` bus. One prompt drives a **turn loop**: each
turn streams one model response, emits text / thinking / tool-call
deltas on the bus, executes any requested tools (with hook gates and
optional ACP reverse-calls), appends the results to history, and loops
until the model stops with `EndTurn`. Consumers of the bus (the print
dispatcher, the ATOF observability bridge, the TUI, the ACP/A2A
adapters) turn those `Event`s into their respective output. On exit the
message log is encoded to ATIF and written to disk so `ra resume` can
hydrate it later.

## The request path

```
                          ┌──────────────────────────────────────────┐
  CLI `ra run` ───────────┤                                          │
  ACP session/prompt ─────┤  SessionRunner.run_input (optional)      │
  A2A message/send ───────┤    └─ expands /slash prompt templates    │
  TUI submit ─────────────┤                                          │
                          └───────────────────┬──────────────────────┘
                                              │
                                              ▼
                                   Session::prompt(user_text)
                                              │   push User msg, emit AgentStart
                                              │   select! { cancel.token  vs  run_loop }
                                              ▼
                                   run_loop:  loop { run_one_turn } until EndTurn
                                              │
                                              ▼
                            ┌─────────────  run_one_turn  ─────────────┐
                            │ emit TurnStart                          │
                            │ history (+ [SYSTEM] preamble) + ToolSpecs│
                            │ model.stream(&history, &specs)          │
                            │   ModelChunk::TextDelta   → Event::TextDelta
                            │   ModelChunk::ThinkingDelta→ Event::ThinkingDelta
                            │   ModelChunk::ToolCall     → Event::ToolCallStart
                            │   ModelChunk::End{stop}    → break       │
                            │ push Assistant msg                      │
                            │ for each pending tool call:             │
                            │   PreToolUse hook gate (deny→short-circuit)
                            │   tool.execute(id, input, &ToolCtx)     │
                            │   PostToolUse hook                      │
                            │   emit Event::ToolCallEnd               │
                            │   push ToolResult msg                   │
                            │ emit TurnEnd, return StopReason         │
                            └──────────────────────────────────────────┘
                                              │
                                              ▼
                                   emit AgentEnd → encode ATIF → store
```

Source map: `src/session_runner.rs` (`run_input`), `src/session.rs`
(`prompt` → `run_loop` → `run_one_turn`), `src/model.rs` /
`src/llm_model.rs` (`Model::stream`), `src/tools/` (`Tool::execute`),
`src/tool_ctx.rs` (`ToolCtx` + `ClientHandle` reverse-calls),
`src/store.rs` + `src/atif_codec.rs` (persistence).

## The core types

### `Session` (`src/session.rs`) — the engine

Holds everything one conversation needs and is shared as
`Arc<Session>` across the protocol adapters:

- **`model: RwLock<Arc<dyn Model>>`** — hot-swappable. ACP
  `session/set_model` replaces the pointer mid-conversation without
  invalidating outstanding `Arc<Session>`s; the next turn reads the new
  pointer.
- **`messages: Arc<Mutex<Vec<Message>>>`** — the persisted log. Only
  `User` / `Assistant` / `ToolResult` variants; the system preamble is
  *not* stored here (see below).
- **`tx: broadcast::Sender<Event>`** — the bus. Every observer
  (`subscribe()`) sees the same ordered event stream.
- **`cancel: Mutex<CancellationToken>`** — re-armed on every `prompt`
  so a cancelled session stays reusable.
- **`client` / `session_id`** — `Some` only under ACP, enabling
  reverse-calls. `None` for CLI / A2A / TUI, where tools run locally.
- **`hooks` / `rtk` / `system_prompt` / `mode` / `config`** — optional
  cross-cutting state threaded into each turn.

`prompt` is the single public driver: re-arm cancel token, push the
user message, emit `AgentStart`, then `tokio::select!` the cancel token
against `run_loop`. `run_loop` calls `run_one_turn` repeatedly until
the model returns `StopReason::EndTurn`; a `ToolUse` stop reason means
"I called tools, feed me the results" and the loop continues.

> **Invariant (load-bearing):** the event emission order in
> `run_one_turn` — `TurnStart` → deltas → `ToolCallStart` →
> `ToolCallEnd` → `TurnEnd`, bracketed by `AgentStart` / `AgentEnd` — is
> relied on by ACP, A2A, the TUI, and `tests/multi_turn.rs`
> simultaneously. Reordering it silently breaks all four. See the doc
> comment at the top of `src/session.rs`.

### The system preamble is synthetic

graniet/llm's `ChatRole` is only `User` / `Assistant`, so the system
prompt (assembled from skills + AGENTS.md + prompt templates by
`src/skills.rs`) is **not** a stored message. `run_one_turn` prepends it
each turn as a `User` message tagged `[SYSTEM]`. That is why
`snapshot_messages` / the ATIF log never contain it — resume rebuilds it
from the resource bundle, not from disk.

### `Model` (`src/model.rs`, `src/llm_model.rs`) — the LLM boundary

`Model::stream(&history, &specs) -> Stream<ModelChunk>` is the only
contract the turn loop depends on. Two implementors:

- **`MockModel`** — drives the `bash:` / `read:` / `grep:` … prefixes
  for the no-API-key quick start and the `ScriptedModel` test pattern.
- **`LlmModel`** (`src/llm_model.rs`) — bridges to graniet/llm, which
  fans out to Anthropic / OpenAI / Google / DeepSeek / Ollama / Groq /
  xAI and any OpenAI-compatible Responses endpoint. Backend selection
  is config- or env-driven (`src/main.rs::EnvModelFactory`).

### `Tool` + `ToolCtx` (`src/tools/`, `src/tool_ctx.rs`)

Each tool implements `Tool` (`name` / `description` / `schema` /
`execute`). `run_one_turn` builds a `ToolSpec` per tool and hands them
to the model; when a tool is called it constructs a `ToolCtx` carrying
the event sender, the optional ACP `ClientHandle`, the session id, and
the RTK rewriter, then awaits `tool.execute`. The `ClientHandle` is the
seam that lets the same `read`/`write`/`bash` tool either hit local fs
(`ToolCtx::local`) or route through ACP `fs/*` and `terminal/*`
reverse-calls when an editor host is connected — the tool code doesn't
branch, the context does.

## The protocol adapters

All four sit *on top of* `Session` and translate its `Event` bus to/from
their wire format:

- **`src/main.rs` (print mode)** — `spawn_event_printer` exhaustively
  matches `Event` and writes to stdout. The simplest consumer; a good
  place to see the full event vocabulary.
- **`src/acp_server.rs`** — serves ACP v1 over stdio JSON-RPC. Maps the
  bus to `session/update` notifications and implements the reverse-call
  side (`fs/read_text_file`, `fs/write_text_file`, `terminal/*`,
  `session/request_permission`) via `ClientHandle`.
- **`src/a2a_server.rs`** — serves A2A over HTTP-JSON-RPC + REST + gRPC
  (optional Bearer auth). Reconnecting `task_id`s are auto-resumed by
  hydrating the message log from disk before the new turn. Note the
  deliberate `reqwest13` alias here (see CLAUDE.md hot-zones).
- **`src/tui.rs`** — `--features tui`, nightly-only. Owns a `!Send`
  renderer, so render + input + bus-consumption all stay on the main
  thread. Bridges the bus to ATOF so TUI sessions still appear in
  traces.

Plus two that consume rather than serve: **`src/a2a_tool.rs`** wraps a
remote A2A agent as a local `Tool`, and **`src/mcp.rs`** turns rmcp MCP
server tools into Ra tools. Both surface to the LLM identically to a
built-in tool.

## Cross-cutting layers

- **Hooks (`src/hooks.rs`)** — `PreToolUse` gates each call (a deny
  short-circuits to an error `ToolResult` without running the tool);
  `PostToolUse` fires after. Wire format follows the Claude Code hooks
  spec verbatim — see the hot-zone note in CLAUDE.md.
- **Observability (`src/nemo_obs.rs`)** — ATOF scopes/marks via NeMo
  Relay. `run_one_turn` opens an `llm_scope` around the model stream and
  drops it *before* tool execution so each tool gets a sibling scope,
  not a nested one.
- **RTK (`src/tools/rtk.rs`)** — shell-flavoured tools consult
  `rtk rewrite` first for token-compressed output. Trust signal is
  "stdout non-empty", not exit status (RTK exits 3 on a hit).
- **Persistence (`src/store.rs`, `src/atif_codec.rs`, `src/atif.rs`)** —
  the message log encodes to ATIF v1.7 JSONL under
  `RA_HOME/sessions/<cwd-hash>/`. `atif_codec` must round-trip;
  `tests/resume.rs` is the canary. `ra sessions` lists, `ra resume <id>`
  hydrates via `Session::restore_messages`.

## Where to start reading

1. `tests/multi_turn.rs` — a `ScriptedModel` driving the full loop with
   assertions on event ordering. The clearest executable spec of the
   contract above.
2. `src/session.rs` — `prompt` / `run_loop` / `run_one_turn`.
3. The adapter you care about (`acp_server.rs` / `a2a_server.rs` /
   `tui.rs`) to see how the bus maps to a wire format.
