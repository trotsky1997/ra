use crate::events::{Event, ToolResult};
use crate::model::{Message, Model, ModelChunk, StopReason, ToolSpec};
use crate::tool_ctx::{ClientHandle, FileChangeApprover, ToolCtx};
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::PathBuf;
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
    /// Session working directory used by local tools for relative paths.
    cwd: PathBuf,
    /// Current operating mode (e.g. "default", "plan", "ask"). Surfaced via
    /// `session/set_mode` and `SessionUpdate::CurrentModeUpdate`.
    mode: RwLock<String>,
    /// Per-session config values keyed by ACP `SessionConfigId`. Set by
    /// `session/set_config_option`; surfaced via `ConfigOptionUpdate`. The
    /// value is stored as a JSON Value to round-trip both string-id and
    /// boolean payloads.
    config: RwLock<HashMap<String, serde_json::Value>>,
    /// Optional system-style preamble prepended to every turn's history.
    /// Populated from skill bodies; not part of the persisted message log.
    system_prompt: RwLock<Option<String>>,
    /// Optional hook engine that fires PreToolUse / PostToolUse around
    /// every tool execution. None = no hooks configured.
    hooks: Option<Arc<crate::hooks::HookEngine>>,
    /// Optional interactive file-change approver. TUI mode uses this to show
    /// write/edit diffs before disk mutation; non-interactive sessions keep it
    /// unset and preserve existing behavior.
    file_approver: Option<Arc<dyn FileChangeApprover>>,
    /// Optional RTK rewriter handed to every ToolCtx so shell commands
    /// can pre-route through `rtk rewrite`. Default
    /// (`RtkRewriter::default()`) is a no-op pass-through.
    rtk: crate::tools::RtkRewriter,
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
            cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
            mode: RwLock::new("default".into()),
            config: RwLock::new(HashMap::new()),
            system_prompt: RwLock::new(None),
            hooks: None,
            file_approver: None,
            rtk: crate::tools::RtkRewriter::default(),
        }
    }

    /// Builder-style: attach a hook engine that will fire around every
    /// tool execution and at prompt boundaries.
    #[must_use]
    pub fn with_hooks(mut self, hooks: Arc<crate::hooks::HookEngine>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Builder-style: attach an [`RtkRewriter`](crate::tools::RtkRewriter)
    /// so shell commands can route through RTK for token-compressed output.
    #[must_use]
    pub fn with_rtk(mut self, rtk: crate::tools::RtkRewriter) -> Self {
        self.rtk = rtk;
        self
    }

    /// Builder-style: set the session cwd used by local tools to resolve
    /// relative paths. ACP/A2A pass their per-session cwd here; CLI/TUI
    /// sessions default to the process cwd.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    /// Builder-style: attach an interactive file-change approver. File
    /// mutation tools call this before writing when present.
    #[must_use]
    pub fn with_file_approver(mut self, approver: Arc<dyn FileChangeApprover>) -> Self {
        self.file_approver = Some(approver);
        self
    }

    pub fn hooks(&self) -> Option<&Arc<crate::hooks::HookEngine>> {
        self.hooks.as_ref()
    }

    /// Set or replace the system-style preamble injected at the head of
    /// every turn's history. Pass an empty string to clear.
    pub async fn set_system_prompt(&self, sp: impl Into<String>) {
        let s = sp.into();
        *self.system_prompt.write().await = if s.is_empty() { None } else { Some(s) };
    }

    pub async fn system_prompt(&self) -> Option<String> {
        self.system_prompt.read().await.clone()
    }

    /// Hot-swap the active model. Used by `session/set_model` (ACP unstable).
    pub async fn set_model(&self, model: Arc<dyn Model>) {
        *self.model.write().await = model;
    }

    /// Set the active mode. The new value flows back to ACP clients via
    /// `SessionUpdate::CurrentModeUpdate`, which the caller emits.
    pub async fn set_mode(&self, mode: impl Into<String>) {
        *self.mode.write().await = mode.into();
    }

    pub async fn mode(&self) -> String {
        self.mode.read().await.clone()
    }

    /// Store / overwrite a config option value.
    pub async fn set_config(&self, id: impl Into<String>, value: serde_json::Value) {
        self.config.write().await.insert(id.into(), value);
    }

    /// Snapshot the current config map. Caller can iterate / serialize.
    pub async fn config_snapshot(&self) -> HashMap<String, serde_json::Value> {
        self.config.read().await.clone()
    }

    /// Manually emit an `AgentEnd` event into the broadcast channel.
    /// Used by the ACP server to terminate the event-forwarder when a
    /// prompt is handled entirely server-side (slash command) and never
    /// goes through the turn loop.
    pub fn signal_agent_end(&self) -> impl std::future::Future<Output = ()> + '_ {
        let tx = self.tx.clone();
        async move {
            let _ = tx.send(Event::AgentEnd);
        }
    }

    /// Append a synthetic AgentMessageChunk-style event without going through
    /// the model, useful for slash commands that produce text directly.
    pub fn emit_text(&self, s: impl Into<String>) {
        let _ = self.tx.send(Event::TextDelta(s.into()));
    }

    /// Best-effort token estimate of the current conversation, used as the
    /// `used` field on `SessionUpdate::UsageUpdate` when the model layer
    /// hasn't surfaced exact usage. Uses tiktoken's o200k_base singleton —
    /// approximate for non-OpenAI models, but always nonzero and stable.
    pub async fn estimate_used_tokens(&self) -> u64 {
        let messages = self.messages.lock().await.clone();
        let bpe = tiktoken_rs::o200k_base_singleton();
        let mut total = 0u64;
        for m in &messages {
            // 4 tokens of overhead per message is the conventional rough
            // estimate for chat-completions framing.
            total += 4;
            match m {
                Message::User { content } => {
                    total += bpe.encode_with_special_tokens(content).len() as u64;
                }
                Message::Assistant {
                    content,
                    tool_calls,
                } => {
                    total += bpe.encode_with_special_tokens(content).len() as u64;
                    for c in tool_calls {
                        total += bpe.encode_with_special_tokens(&c.name).len() as u64;
                        total += bpe.encode_with_special_tokens(&c.input.to_string()).len() as u64;
                    }
                }
                Message::ToolResult(r) => {
                    total += bpe.encode_with_special_tokens(&r.content).len() as u64;
                }
            }
        }
        total
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
    pub fn with_client(
        mut self,
        client: Arc<dyn ClientHandle>,
        session_id: impl Into<String>,
    ) -> Self {
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

        self.messages.lock().await.push(Message::User {
            content: user_text.into(),
        });
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
        // Prepend the system-style preamble (if any) as a synthetic User
        // message tagged with [SYSTEM]. graniet/llm's ChatRole only has
        // User/Assistant, so we route system content through user with a
        // marker; most providers cooperate.
        let history = if let Some(sp) = self.system_prompt.read().await.clone() {
            let mut h = Vec::with_capacity(history.len() + 1);
            h.push(Message::User {
                content: format!("[SYSTEM]\n{sp}"),
            });
            h.extend(history);
            h
        } else {
            history
        };
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
        // ATOF: scope the whole streamed response from the model layer.
        // Drops at the end of the stream-consumption loop, before tool
        // execution starts, so each tool gets its own sibling Tool scope
        // rather than nesting under the LLM scope.
        let llm_scope = crate::nemo_obs::llm_scope("model.stream");
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
        drop(llm_scope);

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

            // PreToolUse hook: any deny short-circuits the tool entirely.
            let pre_decision = if let Some(h) = &self.hooks {
                h.pre_tool_use(self.session_id.as_deref(), &call.name, &call.input)
                    .await
            } else {
                crate::hooks::HookDecision::allow()
            };
            if let Some(reason) = pre_decision.block.clone() {
                let result = ToolResult {
                    call_id: call.id.clone(),
                    is_error: true,
                    content: format!("denied by hook: {reason}"),
                };
                let _ = self.tx.send(Event::ToolCallEnd(result.clone()));
                self.messages.lock().await.push(Message::ToolResult(result));
                continue;
            }

            let ctx = ToolCtx {
                events: self.tx.clone(),
                client: self.client.clone(),
                session_id: self.session_id.clone(),
                cwd: self.cwd.clone(),
                file_approver: self.file_approver.clone(),
                rtk: self.rtk.clone(),
            };
            let exec = tool.execute(&call.id, call.input.clone(), &ctx).await;
            let mut result = match exec {
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

            // Splice PreToolUse `additionalContext` next to the result so
            // the model sees it on the next turn.
            if let Some(extra) = pre_decision.additional_context {
                result.content = format!("{}\n\n[hook context]\n{extra}", result.content);
            }

            // PostToolUse hook: may block, kill, or append context.
            if let Some(h) = &self.hooks {
                let post = h
                    .post_tool_use(
                        self.session_id.as_deref(),
                        &call.name,
                        &call.input,
                        &result.content,
                    )
                    .await;
                if let Some(reason) = post.block {
                    result.is_error = true;
                    result.content = format!(
                        "{}\n\n[blocked by PostToolUse hook] {reason}",
                        result.content
                    );
                }
                if let Some(extra) = post.additional_context {
                    result.content = format!("{}\n\n[hook context]\n{extra}", result.content);
                }
            }

            let _ = self.tx.send(Event::ToolCallEnd(result.clone()));
            self.messages.lock().await.push(Message::ToolResult(result));
        }

        let _ = self.tx.send(Event::TurnEnd);
        Ok(stop)
    }
}
