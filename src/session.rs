use crate::events::{Event, ToolResult};
use crate::model::{Message, Model, ModelChunk, StopReason, ToolSpec};
use crate::tool_ctx::{ClientHandle, ToolCtx};
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

/// 等价于 TS SDK 的 AgentSession。
///
/// Cancellation: each session owns one `CancellationToken`. `cancel()` triggers
/// it (idempotent); `prompt()` races the token against the turn loop via
/// `tokio::select!` and returns `PromptOutcome::Cancelled` when the token wins.
/// We then re-arm the token by `child_token`-ing — so a fresh prompt after a
/// cancel still works without recreating the session.
pub struct Session {
    /// Hot-swappable model. `set_model` replaces this without invalidating
    /// outstanding `Arc<Session>`s; the next turn picks up the new pointer.
    model: RwLock<Arc<dyn Model>>,
    tools: HashMap<String, Arc<dyn Tool>>,
    messages: Arc<Mutex<Vec<Message>>>,
    tx: broadcast::Sender<Event>,
    cancel: Mutex<CancellationToken>,
    /// Optional ACP client handle. When `Some`, tools may issue reverse calls
    /// (`fs/read_text_file`, `terminal/*`, `session/request_permission`).
    client: Option<Arc<dyn ClientHandle>>,
    /// ACP session id, required by every reverse call. Set whenever `client` is.
    session_id: Option<String>,
}

/// Result of one `prompt()` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOutcome {
    /// Turn loop ran to natural completion.
    Completed,
    /// `cancel()` was called before the turn loop finished.
    Cancelled,
}

impl Session {
    pub fn new(model: Arc<dyn Model>, tools: Vec<Arc<dyn Tool>>) -> Self {
        let (tx, _rx) = broadcast::channel(256);
        let mut map = HashMap::new();
        for t in tools {
            map.insert(t.name().to_string(), t);
        }
        Self {
            model: RwLock::new(model),
            tools: map,
            messages: Arc::new(Mutex::new(Vec::new())),
            tx,
            cancel: Mutex::new(CancellationToken::new()),
            client: None,
            session_id: None,
        }
    }

    /// Hot-swap the active model. Used by `session/set_model` (ACP unstable).
    pub async fn set_model(&self, model: Arc<dyn Model>) {
        *self.model.write().await = model;
    }

    /// Snapshot the current message log. Useful for `session/fork`.
    pub async fn snapshot_messages(&self) -> Vec<Message> {
        self.messages.lock().await.clone()
    }

    /// Replace the message log wholesale (used when forking from a snapshot).
    pub async fn restore_messages(&self, msgs: Vec<Message>) {
        *self.messages.lock().await = msgs;
    }

    /// Builder-style injector: when called the session will route tool
    /// execution through the host editor instead of the local filesystem
    /// and terminal.
    #[must_use]
    pub fn with_client(mut self, client: Arc<dyn ClientHandle>, session_id: impl Into<String>) -> Self {
        self.client = Some(client);
        self.session_id = Some(session_id.into());
        self
    }

    /// Subscribe to the event broadcast (drop the receiver to unsubscribe).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub async fn messages(&self) -> Vec<Message> {
        self.messages.lock().await.clone()
    }

    /// Signal the active prompt to abort. Idempotent; safe to call from any task.
    pub async fn cancel(&self) {
        self.cancel.lock().await.cancel();
    }

    /// Send a user message and run the turn loop to completion or cancellation.
    pub async fn prompt(&self, user_text: impl Into<String>) -> Result<PromptOutcome> {
        // Re-arm the cancel token for this prompt invocation.
        let token = {
            let mut guard = self.cancel.lock().await;
            *guard = CancellationToken::new();
            guard.clone()
        };

        self.messages
            .lock()
            .await
            .push(Message::User { content: user_text.into() });
        let _ = self.tx.send(Event::AgentStart);

        let outcome = tokio::select! {
            biased;
            _ = token.cancelled() => PromptOutcome::Cancelled,
            res = self.run_loop() => {
                res?;
                PromptOutcome::Completed
            }
        };

        let _ = self.tx.send(Event::AgentEnd);
        Ok(outcome)
    }

    async fn run_loop(&self) -> Result<()> {
        loop {
            let stop = self.run_one_turn().await?;
            if stop == StopReason::EndTurn {
                break;
            }
        }
        Ok(())
    }

    /// One turn = one streamed model response, plus execution of any tool
    /// calls it requested.
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
        let model = self.model.read().await.clone();
        let mut stream = model.stream(&history, &specs).await?;

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

        self.messages.lock().await.push(Message::Assistant {
            content: text_acc,
            tool_calls: pending_calls.clone(),
        });

        for call in pending_calls {
            let tool = self
                .tools
                .get(&call.name)
                .ok_or_else(|| anyhow!("unknown tool: {}", call.name))?
                .clone();

            let ctx = ToolCtx {
                events: self.tx.clone(),
                client: self.client.clone(),
                session_id: self.session_id.clone(),
            };
            let exec = tool.execute(&call.id, call.input.clone(), &ctx).await;
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
