use crate::events::ToolCall;
use crate::model::{Message, Model, ModelChunk, StopReason, ToolSpec};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use reqwest_eventsource::{Event as SseEvent, EventSource};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// 对接 OpenAI Responses API 风格的 endpoint（pi / codex / openai 都走这个 wire）。
///
/// `base_url` 不带末尾斜杠，例如 "https://pi-api-us.macaron.xin"。
/// 实际请求路径是 `{base_url}/v1/responses`。
pub struct PiModel {
    base_url: String,
    api_key: String,
    model_id: String,
    client: reqwest::Client,
}

impl PiModel {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model_id: model_id.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Model for PiModel {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, ModelChunk>> {
        let body = json!({
            "model": self.model_id,
            "input": encode_input(messages),
            "stream": true,
            "tools": encode_tools(tools),
        });

        let url = format!("{}/v1/responses", self.base_url);
        let req = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&body);

        let mut es = EventSource::new(req).context("create EventSource")?;
        let (tx, rx) = mpsc::unbounded_channel::<ModelChunk>();

        // 解析 SSE 在后台 task 里跑，前台返回一个 stream。
        tokio::spawn(async move {
            // function_call item_id → 累积的参数 JSON 字符串
            let mut pending_calls: HashMap<String, PendingCall> = HashMap::new();
            let mut stop = StopReason::EndTurn;

            while let Some(event) = es.next().await {
                match event {
                    Ok(SseEvent::Open) => {}
                    Ok(SseEvent::Message(msg)) => {
                        let Ok(payload): Result<Value, _> = serde_json::from_str(&msg.data) else {
                            continue;
                        };
                        let kind = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

                        match kind {
                            "response.output_text.delta" => {
                                if let Some(d) = payload.get("delta").and_then(|v| v.as_str()) {
                                    let _ = tx.send(ModelChunk::TextDelta(d.to_string()));
                                }
                            }
                            "response.output_item.added" => {
                                if let Some(item) = payload.get("item") {
                                    if item.get("type").and_then(|v| v.as_str())
                                        == Some("function_call")
                                    {
                                        let item_id = item
                                            .get("id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let call_id = item
                                            .get("call_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let name = item
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        pending_calls.insert(
                                            item_id,
                                            PendingCall { call_id, name, args: String::new() },
                                        );
                                    }
                                }
                            }
                            "response.function_call_arguments.delta" => {
                                let item_id = payload
                                    .get("item_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let delta = payload
                                    .get("delta")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if let Some(p) = pending_calls.get_mut(item_id) {
                                    p.args.push_str(delta);
                                }
                            }
                            "response.output_item.done" => {
                                let item = payload.get("item").cloned().unwrap_or(Value::Null);
                                if item.get("type").and_then(|v| v.as_str())
                                    == Some("function_call")
                                {
                                    let item_id = item
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    if let Some(p) = pending_calls.remove(&item_id) {
                                        // 用 done 事件里的 arguments 覆盖（更可靠）
                                        let args_str = item
                                            .get("arguments")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                            .unwrap_or(p.args);
                                        let input: Value = serde_json::from_str(&args_str)
                                            .unwrap_or(Value::Object(Default::default()));
                                        let _ = tx.send(ModelChunk::ToolCall(ToolCall {
                                            id: p.call_id,
                                            name: p.name,
                                            input,
                                        }));
                                        stop = StopReason::ToolUse;
                                    }
                                }
                            }
                            "response.completed" => {
                                let _ = tx.send(ModelChunk::End { stop_reason: stop });
                                es.close();
                                break;
                            }
                            "response.failed" | "error" => {
                                let _ = tx.send(ModelChunk::End { stop_reason: stop });
                                es.close();
                                break;
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        // EventSource 在正常关闭时会发 StreamEnded；跳过它，其他报错就吐 End 收尾。
                        if matches!(e, reqwest_eventsource::Error::StreamEnded) {
                            break;
                        }
                        eprintln!("[ra::pi_model] sse error: {e}");
                        let _ = tx.send(ModelChunk::End { stop_reason: stop });
                        break;
                    }
                }
            }
        });

        Ok(UnboundedReceiverStream::new(rx).boxed())
    }
}

struct PendingCall {
    call_id: String,
    name: String,
    args: String,
}

/// 把内部 Message[] 转成 Responses API 的 input 数组。
fn encode_input(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        match m {
            Message::User { content } => {
                out.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": content}],
                }));
            }
            Message::Assistant { content, tool_calls } => {
                if !content.is_empty() {
                    out.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": content}],
                    }));
                }
                for c in tool_calls {
                    out.push(json!({
                        "type": "function_call",
                        "call_id": c.id,
                        "name": c.name,
                        "arguments": serde_json::to_string(&c.input).unwrap_or_else(|_| "{}".into()),
                    }));
                }
            }
            Message::ToolResult(r) => {
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": r.call_id,
                    "output": r.content,
                }));
            }
        }
    }
    out
}

/// 把 ToolSpec 转成 Responses API 期望的 tools 数组。
///
/// schemars 派生出来的 schema 带 $schema/title 字段，Responses API 不接受额外属性，
/// 这里清洗一下。
fn encode_tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let params = clean_schema(&t.parameters);
            json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": params,
            })
        })
        .collect()
}

fn clean_schema(schema: &Value) -> Value {
    let mut v = schema.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
        obj.remove("definitions");
        // Responses API 在 strict 模式下要求 additionalProperties=false
        obj.entry("additionalProperties").or_insert(json!(false));
    }
    v
}

/// 让外层能用 anyhow 做 ? 但保留具体上下文
#[allow(dead_code)]
fn convert_err<E: std::fmt::Display>(e: E) -> anyhow::Error {
    anyhow!("{e}")
}
