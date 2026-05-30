use crate::events::{Event, ToolResult};
use crate::model::{Message, Model, ModelChunk, StopReason, ToolSpec};
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio_stream::StreamExt;

/// 等价于 TS SDK 的 AgentSession。
///
/// 设计要点：
/// - 内部状态（messages）用 Mutex 保护，方便后续多消费者读取。
/// - 事件用 tokio broadcast，订阅者多对一，落后太多会丢消息（语义上对应 TS 里
///   subscribe(cb) 不会反压模型）。
pub struct Session {
    model: Arc<dyn Model>,
    tools: HashMap<String, Arc<dyn Tool>>,
    messages: Arc<Mutex<Vec<Message>>>,
    tx: broadcast::Sender<Event>,
}

impl Session {
    pub fn new(model: Arc<dyn Model>, tools: Vec<Arc<dyn Tool>>) -> Self {
        let (tx, _rx) = broadcast::channel(256);
        let mut map = HashMap::new();
        for t in tools {
            map.insert(t.name().to_string(), t);
        }
        Self {
            model,
            tools: map,
            messages: Arc::new(Mutex::new(Vec::new())),
            tx,
        }
    }

    /// 像 TS 的 session.subscribe(cb)，返回 receiver；丢弃即取消订阅。
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub async fn messages(&self) -> Vec<Message> {
        self.messages.lock().await.clone()
    }

    /// 主入口：发送一条用户消息，跑完整个 turn loop 才返回。
    pub async fn prompt(&self, user_text: impl Into<String>) -> Result<()> {
        self.messages
            .lock()
            .await
            .push(Message::User { content: user_text.into() });
        let _ = self.tx.send(Event::AgentStart);

        loop {
            let stop = self.run_one_turn().await?;
            if stop == StopReason::EndTurn {
                break;
            }
            // ToolUse: tool 结果已经写进 messages，下一轮让模型继续。
        }

        let _ = self.tx.send(Event::AgentEnd);
        Ok(())
    }

    /// 一个 turn = 模型流式响应一次 + 执行它发起的所有工具。
    async fn run_one_turn(&self) -> Result<StopReason> {
        let _ = self.tx.send(Event::TurnStart);

        let history = self.messages.lock().await.clone();
        let specs: Vec<ToolSpec> = self
            .tools
            .values()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.schema(),
            })
            .collect();
        let mut stream = self.model.stream(&history, &specs).await?;

        let mut text_acc = String::new();
        let mut pending_calls = Vec::new();
        let mut stop = StopReason::EndTurn;

        while let Some(chunk) = stream.next().await {
            match chunk {
                ModelChunk::TextDelta(s) => {
                    text_acc.push_str(&s);
                    let _ = self.tx.send(Event::TextDelta(s));
                }
                ModelChunk::ThinkingDelta(s) => {
                    let _ = self.tx.send(Event::ThinkingDelta(s));
                }
                ModelChunk::ToolCall(call) => {
                    let _ = self.tx.send(Event::ToolCallStart(call.clone()));
                    pending_calls.push(call);
                }
                ModelChunk::End { stop_reason } => {
                    stop = stop_reason;
                    break;
                }
            }
        }

        // 把 assistant 这一回合的输出固化到历史里
        self.messages.lock().await.push(Message::Assistant {
            content: text_acc,
            tool_calls: pending_calls.clone(),
        });

        // 执行所有 tool_call，把每个结果作为 ToolResult 写回历史
        for call in pending_calls {
            let tool = self
                .tools
                .get(&call.name)
                .ok_or_else(|| anyhow!("unknown tool: {}", call.name))?
                .clone();

            let exec = tool.execute(&call.id, call.input.clone(), &self.tx).await;
            let result = match exec {
                Ok(text) => ToolResult {
                    call_id: call.id.clone(),
                    is_error: false,
                    content: text,
                },
                Err(e) => ToolResult {
                    call_id: call.id.clone(),
                    is_error: true,
                    content: format!("{e:#}"),
                },
            };

            let _ = self.tx.send(Event::ToolCallEnd(result.clone()));
            self.messages
                .lock()
                .await
                .push(Message::ToolResult(result));
        }

        let _ = self.tx.send(Event::TurnEnd);
        Ok(stop)
    }
}
