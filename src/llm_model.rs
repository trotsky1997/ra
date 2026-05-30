//! Model adapter that delegates to graniet/llm's `LLMProvider` (the unified
//! multi-backend trait).
//!
//! Replaces the hand-rolled `PiModel` (`reqwest` + SSE parser). The OpenAI
//! backend in graniet/llm targets the `/v1/responses` endpoint by default,
//! which is exactly what pi-api-us uses, so the wire format stays identical.
//! Other backends (Anthropic, Google, Ollama, ...) come "for free" by
//! changing the LLMBackend in build_provider().

use std::pin::Pin;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use llm::{
    builder::{LLMBackend, LLMBuilder},
    chat::{ChatMessage, MessageType, StreamChunk, Tool as LlmTool, FunctionTool},
    LLMProvider, ToolCall as LlmToolCall, FunctionCall as LlmFunctionCall,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::events::ToolCall as RaToolCall;
use crate::model::{Message, Model, ModelChunk, StopReason, ToolSpec};

/// Configuration knobs for `LlmModel::build`. Anything beyond what `LLMBuilder`
/// consumes belongs in higher layers; we just pass through.
pub struct LlmModelConfig {
    pub backend: LLMBackend,
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
}

pub struct LlmModel {
    provider: Arc<dyn LLMProvider>,
}

impl LlmModel {
    pub fn build(cfg: LlmModelConfig) -> Result<Self> {
        let mut builder = LLMBuilder::new()
            .backend(cfg.backend)
            .api_key(cfg.api_key)
            .model(cfg.model)
            // Stream tool calls in fragments rather than waiting for `done`,
            // so our session events feel responsive.
            .normalize_response(false);

        if let Some(url) = cfg.base_url {
            builder = builder.base_url(url);
        }

        let provider = builder.build().context("LLMBuilder::build")?;
        Ok(Self { provider: Arc::from(provider) })
    }
}

#[async_trait]
impl Model for LlmModel {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, ModelChunk>> {
        let chat_msgs = encode_messages(messages);
        let llm_tools = encode_tools(tools);
        let provider = self.provider.clone();

        // Drive the upstream Stream from a spawned task and forward our
        // internal ModelChunk via an mpsc. This keeps Send bounds simple
        // (`StreamChunk` is Send, `ModelChunk` is Send) and lets the caller
        // treat the result as a regular `BoxStream`.
        let (tx, rx) = mpsc::unbounded_channel::<ModelChunk>();

        tokio::spawn(async move {
            let stream_res: Result<
                Pin<Box<dyn futures::Stream<Item = Result<StreamChunk, llm::error::LLMError>> + Send>>,
                _,
            > = provider
                .chat_stream_with_tools(
                    &chat_msgs,
                    if llm_tools.is_empty() { None } else { Some(&llm_tools) },
                )
                .await;

            let mut s = match stream_res {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[ra::llm_model] chat_stream_with_tools: {e}");
                    let _ = tx.send(ModelChunk::End { stop_reason: StopReason::EndTurn });
                    return;
                }
            };

            let mut stop = StopReason::EndTurn;

            while let Some(item) = s.next().await {
                match item {
                    Ok(StreamChunk::Text(t)) => {
                        let _ = tx.send(ModelChunk::TextDelta(t));
                    }
                    Ok(StreamChunk::ToolUseStart { .. })
                    | Ok(StreamChunk::ToolUseInputDelta { .. }) => {
                        // We deliver one ToolCall per ToolUseComplete; the
                        // intermediate fragments are not surfaced (yet).
                    }
                    Ok(StreamChunk::ToolUseComplete { tool_call, .. }) => {
                        let input: serde_json::Value =
                            serde_json::from_str(&tool_call.function.arguments)
                                .unwrap_or(serde_json::Value::Object(Default::default()));
                        let _ = tx.send(ModelChunk::ToolCall(RaToolCall {
                            id: tool_call.id,
                            name: tool_call.function.name,
                            input,
                        }));
                        stop = StopReason::ToolUse;
                    }
                    Ok(StreamChunk::Done { .. }) => break,
                    Err(e) => {
                        eprintln!("[ra::llm_model] stream error: {e}");
                        break;
                    }
                }
            }

            let _ = tx.send(ModelChunk::End { stop_reason: stop });
        });

        Ok(UnboundedReceiverStream::new(rx).boxed())
    }
}

// --- Encoders: Ra <-> graniet/llm types --------------------------------------

fn encode_messages(msgs: &[Message]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
        match m {
            Message::User { content } => {
                out.push(ChatMessage::user().content(content.clone()).build());
            }
            Message::Assistant { content, tool_calls } => {
                if !content.is_empty() {
                    out.push(ChatMessage::assistant().content(content.clone()).build());
                }
                if !tool_calls.is_empty() {
                    let calls: Vec<LlmToolCall> = tool_calls
                        .iter()
                        .map(|c| LlmToolCall {
                            id: c.id.clone(),
                            call_type: "function".into(),
                            function: LlmFunctionCall {
                                name: c.name.clone(),
                                arguments: c.input.to_string(),
                            },
                        })
                        .collect();
                    out.push(
                        ChatMessage::assistant()
                            .tool_use(calls)
                            .build(),
                    );
                }
            }
            Message::ToolResult(r) => {
                // graniet/llm models tool results as a User-side message
                // bearing MessageType::ToolResult. The content carries the
                // textual payload, the ToolCall echoes id/name so the
                // provider can correlate.
                let echo = LlmToolCall {
                    id: r.call_id.clone(),
                    call_type: "function".into(),
                    function: LlmFunctionCall {
                        name: String::new(),
                        arguments: r.content.clone(),
                    },
                };
                out.push(
                    ChatMessage::user()
                        .content(r.content.clone())
                        .tool_result(vec![echo])
                        .build(),
                );
            }
        }
    }
    out
}

fn encode_tools(tools: &[ToolSpec]) -> Vec<LlmTool> {
    tools
        .iter()
        .map(|t| LlmTool {
            tool_type: "function".into(),
            function: FunctionTool {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: clean_schema(&t.parameters),
            },
            cache_control: None,
        })
        .collect()
}

/// schemars 派生出来的 schema 带 $schema/title/definitions 等字段；Responses API
/// 在 strict 模式下要求 additionalProperties=false，并不喜欢顶层多余 key。
fn clean_schema(schema: &serde_json::Value) -> serde_json::Value {
    let mut v = schema.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
        obj.remove("definitions");
        obj.entry("additionalProperties").or_insert(serde_json::json!(false));
    }
    v
}

#[allow(dead_code)]
fn _unused_anyhow_marker() -> anyhow::Error {
    anyhow!("ra::llm_model placeholder")
}

// silence the MessageType import lint; we reference it implicitly via builder
const _: Option<MessageType> = None;
