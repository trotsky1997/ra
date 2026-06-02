use crate::events::{Event, ToolResult};
use crate::model::{Message, Model, ModelChunk, StopReason, ToolSpec};
use crate::tool_ctx::{ClientHandle, FileChangeApprover, ToolCtx};
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    /// Optional generated local memory context. Rendered per turn so
    /// thread-level controls and external-context suppression can apply.
    memory_prompt: RwLock<Option<crate::memory::MemoryPrompt>>,
    /// Per-session memory controls. ACP config options can change these
    /// without mutating global config.
    memory_controls: RwLock<crate::memory::MemoryThreadControls>,
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
    /// Invocation-scoped runtime constraints for direct skill execution.
    runtime_scope: Arc<Mutex<Option<SessionRuntimeScope>>>,
    /// Serializes prompt invocations so scoped overrides cannot overlap.
    prompt_lock: Arc<Mutex<()>>,
    /// Monotonic timing used by memory generation gates.
    created_at: Instant,
    last_activity_at: Mutex<Instant>,
    active: Mutex<bool>,
}

/// Temporary runtime controls used for a single `prompt()` invocation.
#[derive(Clone)]
pub struct SessionRuntimeScope {
    pub model: Option<Arc<dyn Model>>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub hooks: Option<Arc<crate::hooks::HookEngine>>,
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryTiming {
    pub session_duration: Duration,
    pub idle_for: Duration,
    pub is_active: bool,
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
            memory_prompt: RwLock::new(None),
            memory_controls: RwLock::new(crate::memory::MemoryThreadControls::default()),
            hooks: None,
            file_approver: None,
            rtk: crate::tools::RtkRewriter::default(),
            runtime_scope: Arc::new(Mutex::new(None)),
            prompt_lock: Arc::new(Mutex::new(())),
            created_at: Instant::now(),
            last_activity_at: Mutex::new(Instant::now()),
            active: Mutex::new(false),
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

    pub fn effective_hooks_for_scope(
        &self,
        scope: Option<&SessionRuntimeScope>,
    ) -> Option<Arc<crate::hooks::HookEngine>> {
        effective_hooks(self.hooks.as_ref(), scope)
    }

    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
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

    pub async fn set_memory_prompt(&self, prompt: Option<crate::memory::MemoryPrompt>) {
        *self.memory_prompt.write().await = prompt;
    }

    pub async fn set_memory_controls(&self, controls: crate::memory::MemoryThreadControls) {
        *self.memory_controls.write().await = controls;
    }

    pub async fn memory_controls(&self) -> crate::memory::MemoryThreadControls {
        self.memory_controls.read().await.clone()
    }

    pub async fn set_memory_use_enabled(&self, enabled: bool) {
        self.memory_controls.write().await.use_memories = enabled;
    }

    pub async fn set_memory_generation_enabled(&self, enabled: bool) {
        self.memory_controls.write().await.generate_memories = enabled;
    }

    pub async fn set_memory_external_context(&self, has_external_context: bool) {
        self.memory_controls.write().await.has_external_context = has_external_context;
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

    pub async fn memory_timing(&self) -> MemoryTiming {
        let now = Instant::now();
        let last_activity_at = *self.last_activity_at.lock().await;
        let is_active = *self.active.lock().await;
        MemoryTiming {
            session_duration: now.saturating_duration_since(self.created_at),
            idle_for: now.saturating_duration_since(last_activity_at),
            is_active,
        }
    }

    /// Signal the active prompt to abort. Idempotent; safe to call from any task.
    pub async fn cancel(&self) {
        self.cancel.lock().await.cancel();
    }

    /// Send a user message and run the turn loop to completion or cancellation.
    pub async fn prompt(&self, user_text: impl Into<String>) -> Result<PromptOutcome> {
        let _prompt_guard = self.prompt_lock.clone().lock_owned().await;
        self.prompt_unlocked(user_text.into()).await
    }

    /// Send a user message with temporary runtime controls. The controls apply
    /// only to this invocation and are cleared even if the turn returns an
    /// error.
    pub async fn prompt_scoped(
        &self,
        user_text: impl Into<String>,
        scope: SessionRuntimeScope,
    ) -> Result<PromptOutcome> {
        let prompt_guard = self.prompt_lock.clone().lock_owned().await;
        *self.runtime_scope.lock().await = Some(scope);
        let result = self.prompt_unlocked(user_text.into()).await;
        *self.runtime_scope.lock().await = None;
        drop(prompt_guard);
        result
    }

    /// Run a prompt against a child transcript initialized from the current
    /// parent transcript, then restore the parent and append only the child's
    /// final assistant text. Used by `agent: fork` skill invocation.
    pub async fn prompt_forked(
        &self,
        user_text: impl Into<String>,
        scope: Option<SessionRuntimeScope>,
    ) -> Result<PromptOutcome> {
        let _prompt_guard = self.prompt_lock.clone().lock_owned().await;
        let parent_snapshot = self.snapshot_messages().await;
        if let Some(scope) = scope {
            *self.runtime_scope.lock().await = Some(scope);
        }
        let result = self.prompt_unlocked(user_text.into()).await;
        *self.runtime_scope.lock().await = None;
        let child_messages = self.snapshot_messages().await;
        let parent_len = parent_snapshot.len();
        self.restore_messages(parent_snapshot).await;
        let outcome = result?;
        if let Some(final_text) = final_assistant_text(&child_messages[parent_len..]) {
            let mut restored = self.snapshot_messages().await;
            restored.push(Message::Assistant {
                content: final_text,
                tool_calls: Vec::new(),
            });
            self.restore_messages(restored).await;
        }
        Ok(outcome)
    }

    async fn prompt_unlocked(&self, user_text: String) -> Result<PromptOutcome> {
        // Re-arm the cancel token for this prompt invocation.
        let token = {
            let mut guard = self.cancel.lock().await;
            *guard = CancellationToken::new();
            guard.clone()
        };
        {
            *self.active.lock().await = true;
            *self.last_activity_at.lock().await = Instant::now();
        }

        self.messages
            .lock()
            .await
            .push(Message::User { content: user_text });
        let _ = self.tx.send(Event::AgentStart);

        let outcome = tokio::select! {
            biased;
            _ = token.cancelled() => PromptOutcome::Cancelled,
            res = self.run_loop() => {
                res?;
                PromptOutcome::Completed
            }
        };

        {
            *self.active.lock().await = false;
            *self.last_activity_at.lock().await = Instant::now();
        }
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
        let system_prompt = self.system_prompt.read().await.clone();
        let memory_section = {
            let prompt = self.memory_prompt.read().await.clone();
            let controls = self.memory_controls.read().await.clone();
            prompt.and_then(|p| p.render(&controls))
        };
        let combined_prompt = combine_system_prompt(system_prompt, memory_section);
        let history = if let Some(sp) = combined_prompt {
            let mut h = Vec::with_capacity(history.len() + 1);
            h.push(Message::User {
                content: format!("[SYSTEM]\n{sp}"),
            });
            h.extend(history);
            h
        } else {
            history
        };
        let scope = self.runtime_scope.lock().await.clone();
        let specs: Vec<ToolSpec> = self
            .tools
            .values()
            .filter(|t| tool_allowed(t.name(), scope.as_ref()))
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.schema(),
            })
            .collect();
        let model = if let Some(model) = scope.as_ref().and_then(|s| s.model.clone()) {
            model
        } else {
            self.model.read().await.clone()
        };
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
            if !tool_allowed(&call.name, scope.as_ref()) {
                let result = ToolResult {
                    call_id: call.id.clone(),
                    is_error: true,
                    content: format!("tool `{}` denied by skill-scoped tool policy", call.name),
                };
                let _ = self.tx.send(Event::ToolCallEnd(result.clone()));
                self.messages.lock().await.push(Message::ToolResult(result));
                continue;
            }
            let tool = self
                .tools
                .get(&call.name)
                .ok_or_else(|| anyhow!("unknown tool: {}", call.name))?
                .clone();

            // PreToolUse hook: any deny short-circuits the tool entirely.
            let scoped_hooks = self.effective_hooks_for_scope(scope.as_ref());
            let pre_decision = if let Some(h) = &scoped_hooks {
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
            if let Some(h) = &scoped_hooks {
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

fn effective_hooks(
    session_hooks: Option<&Arc<crate::hooks::HookEngine>>,
    scope: Option<&SessionRuntimeScope>,
) -> Option<Arc<crate::hooks::HookEngine>> {
    match (session_hooks, scope.and_then(|s| s.hooks.as_ref())) {
        (Some(base), Some(extra)) => Some(Arc::new(base.merged_with(extra))),
        (Some(base), None) => Some(base.clone()),
        (None, Some(extra)) => Some(extra.clone()),
        (None, None) => None,
    }
}

fn tool_allowed(name: &str, scope: Option<&SessionRuntimeScope>) -> bool {
    let Some(scope) = scope else {
        return true;
    };
    if !scope.allowed_tools.is_empty()
        && !scope
            .allowed_tools
            .iter()
            .any(|decl| tool_decl_allows(decl, name))
    {
        return false;
    }
    !scope
        .disallowed_tools
        .iter()
        .any(|decl| tool_decl_denies(decl, name))
}

fn tool_decl_allows(decl: &str, name: &str) -> bool {
    let decl = decl.trim();
    if decl == "*" {
        return true;
    }
    if decl.contains('(') || decl.contains(')') {
        return false;
    }
    decl.eq_ignore_ascii_case(name)
}

fn tool_decl_denies(decl: &str, name: &str) -> bool {
    let decl = decl.trim();
    if tool_decl_allows(decl, name) {
        return true;
    }
    constrained_tool_decl_head(decl).is_some_and(|head| head.eq_ignore_ascii_case(name))
}

fn constrained_tool_decl_head(decl: &str) -> Option<&str> {
    if !decl.contains('(') && !decl.contains(')') {
        return None;
    }
    let head = decl.split(['(', ')']).next().unwrap_or("").trim();
    (!head.is_empty()).then_some(head)
}

fn combine_system_prompt(base: Option<String>, memory: Option<String>) -> Option<String> {
    match (base, memory) {
        (Some(base), Some(memory)) if !base.trim().is_empty() => {
            Some(format!("{}\n\n{}", base.trim_end(), memory.trim()))
        }
        (Some(base), None) if !base.trim().is_empty() => Some(base),
        (None, Some(memory)) if !memory.trim().is_empty() => Some(memory),
        (Some(_), Some(memory)) if !memory.trim().is_empty() => Some(memory),
        _ => None,
    }
}

fn final_assistant_text(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|msg| match msg {
        Message::Assistant { content, .. } if !content.is_empty() => Some(content.clone()),
        _ => None,
    })
}
