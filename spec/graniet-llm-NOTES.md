# graniet/llm — not a protocol, a client SDK

This file documents what we use from `llm` (the crate at https://github.com/graniet/llm),
not a wire spec. The crate is a multi-backend LLM client; each backend
ultimately speaks its own provider's HTTP API. There is no single
"graniet/llm spec" because the project is purely a Rust SDK over OpenAI,
Anthropic, Google, Ollama, DeepSeek, xAI, Cohere, Mistral, HuggingFace,
ElevenLabs, Phind, and Groq.

## What Ra uses from it

We integrate the `llm` crate via `src/llm_model.rs`. Effective wire format
depends on which backend is selected at construction time:

| `LLMBackend`   | Wire format                                      |
|----------------|--------------------------------------------------|
| `OpenAI`       | OpenAI **Responses API** (`POST /v1/responses`,  |
|                | SSE stream). Used for both `api.openai.com` and  |
|                | OpenAI-compatible endpoints like pi-api-us.      |
| `Anthropic`    | Anthropic **Messages API** v2026-...             |
| `Google`       | Gemini `generateContent` / `streamGenerateContent` |
| `Ollama`       | Ollama native `/api/chat`                        |
| `DeepSeek`     | OpenAI-compat                                    |
| `Groq`         | OpenAI-compat                                    |
| `xAI`          | OpenAI-compat                                    |

## Authoritative references for each backend's wire spec

- **OpenAI Responses API** — the schema we hit in production:
  https://platform.openai.com/docs/api-reference/responses
- **Anthropic Messages API**:
  https://docs.anthropic.com/en/api/messages
- **Gemini generateContent**:
  https://ai.google.dev/api/generate-content
- **Ollama**:
  https://github.com/ollama/ollama/blob/main/docs/api.md

## What ATIF/ACP/A2A look like at the LLM seam

graniet/llm internal types we cross every prompt:

- `ChatMessage { role: ChatRole, message_type: MessageType, content: String }`
- `ChatRole = User | Assistant`
- `MessageType = Text | Image(...) | Pdf(...) | Audio(...) | ImageURL(...) | ToolUse(Vec<ToolCall>) | ToolResult(Vec<ToolCall>)`
- `Tool { tool_type: "function", function: FunctionTool { name, description, parameters: serde_json::Value } }`
- `StreamChunk = Text(String) | ToolUseStart {...} | ToolUseInputDelta {...} | ToolUseComplete { tool_call } | Done { stop_reason }`

These are the only contract Ra makes against graniet/llm; everything below
(HTTP, SSE, request bodies) is the backend provider's spec, not graniet's.
