# Ra

> 𓂀  rust-native agent · sun god, illuminating tools

A minimal agent skeleton in Rust, philosophically aligned with
[`@earendil-works/pi-coding-agent`](https://www.npmjs.com/package/@earendil-works/pi-coding-agent)
but using Rust's type system to enforce what TypeScript leaves to convention.

## Why "Ra"

Two readings:

- **Ra** — the Egyptian sun god; every `run_one_turn` re-illuminates the
  context, broadcast events radiate outward like rays.
- **Ra** — **R**ust-native **a**gent.

The eye watching from the banner is 𓂀 (U+13080).

## Mapping to pi-coding-agent

| pi-coding-agent (TS)                          | ra (Rust)                                                |
|-----------------------------------------------|----------------------------------------------------------|
| `session.subscribe(cb)`                       | `session.subscribe()` → `broadcast::Receiver<Event>`     |
| `Event` discriminated union                   | `enum Event` + exhaustive `match`                        |
| `defineTool({ parameters: Type.Object(…) })`  | `#[derive(JsonSchema, Deserialize)]`                     |
| implicit turn loop                            | explicit `loop { run_one_turn() }` until `EndTurn`       |
| `Model` provider abstraction                  | `trait Model`, `MockModel` / `PiModel` interchangeable   |
| Responses API SSE handling                    | `reqwest_eventsource` + JSON event dispatch              |
| `function_call` / `function_call_output` wire | `encode_input()` maps `Message` history to API input     |

## Layout

```
src/
├── lib.rs        re-exports
├── events.rs     Event, ToolCall, ToolResult
├── tools.rs      Tool trait + Read / Bash (schemars-derived schemas)
├── model.rs      Model trait + ToolSpec + MockModel
├── pi_model.rs   PiModel: OpenAI Responses API over SSE
├── session.rs    Session: turn loop + broadcast events
└── main.rs       print mode demo
```

## Run

```bash
# Mock model (no API key needed)
cargo run -- "bash:echo hello && uname -sr"

# Real model (Responses-API-compatible endpoint)
PI_API_KEY=sk-... \
PI_BASE_URL=https://pi-api-us.macaron.xin \
PI_MODEL=gpt-5.5 \
cargo run -- "Use bash to print the date, then summarize."
```

`PI_BASE_URL` and `PI_MODEL` default to pi's US endpoint and `gpt-5.5`.

## Status

Skeleton. Working: streaming text, parallel tool calls, multi-turn tool loop,
real Responses-API backend.

Not yet: session persistence (JSONL tree), extensions, multi-provider
(Anthropic / generic OpenAI), interactive TUI.
