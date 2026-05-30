//! Protocol-neutral runner that wraps a `Session` and produces a stream of
//! `RunnerEvent`s. Each ACP / A2A / future-other server consumes the same
//! events and translates them into its own wire format.
//!
//! The runner owns:
//! - the turn-loop driving (calls `Session::prompt`),
//! - slash-command interception (`/clear`, `/compact`, `/models`, `/mode`),
//! - subscribing to the broadcast bus and translating Ra `Event` →
//!   `RunnerEvent`,
//! - per-prompt observability scope (`nemo_obs::with_task_scope` +
//!   `agent_scope`),
//! - persisting the trajectory at end-of-turn (via the `RunnerHost` trait
//!   so the host stays protocol-neutral),
//! - emitting a final `UsageReport` event with token estimates.

use crate::events::Event as RaEvent;
use crate::model::Message as RaMessage;
use crate::nemo_obs;
use crate::session::{PromptOutcome, Session};
use anyhow::Result;
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::sync::Arc;

/// Hint to clients about the kind of work a tool does. Maps to ACP `ToolKind`
/// and A2A artifact roles, but stays protocol-neutral here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKindHint {
    Read,
    Execute,
    Other,
}

/// Outcome of a single `run_input` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// Turn loop ran to natural completion.
    Completed,
    /// `Session::cancel` was triggered before the loop finished.
    Cancelled,
    /// Anything else: bubble up the error message for the server to translate.
    Failed(String),
}

/// Protocol-neutral event stream. Consumers translate to ACP `SessionUpdate`
/// or A2A `StreamResponse` etc.
#[derive(Debug, Clone)]
pub enum RunnerEvent {
    /// Prompt started running (after slash-command dispatch).
    Started,
    /// Streaming text chunk from the assistant.
    TextDelta(String),
    /// Streaming reasoning/thinking chunk from the assistant.
    ThinkingDelta(String),
    /// Tool call started.
    ToolCallStart {
        id: String,
        name: String,
        input: serde_json::Value,
        title: String,
        kind: ToolKindHint,
    },
    /// Tool call finished. Carries the entire output payload.
    ToolCallEnd {
        id: String,
        is_error: bool,
        content: String,
    },
    /// Session mode flipped (e.g. via /mode slash command). Servers may
    /// re-broadcast this on their wire (ACP CurrentModeUpdate, etc.).
    ModeChanged(String),
    /// Final token-accounting report. Always emitted once per `run_input`
    /// just before `Finished`.
    UsageReport { used: u64, size: u64 },
    /// Final event. Always the last item delivered to the callback.
    Finished(RunOutcome),
}

/// Narrow capability surface a `SessionRunner` needs from whatever holds
/// long-lived agent state. Lets the protocol-specific server (ACP,
/// A2A, …) own the actual `SharedState` without leaking ACP types
/// into the runner.
#[async_trait]
pub trait RunnerHost: Send + Sync {
    /// Persist the named session's current trajectory.
    async fn save_session(&self, session_id: &str);

    /// Default context window for the active model, used as `size` in
    /// `UsageReport`.
    fn default_ctx_window(&self) -> u64;

    /// Names + ids of advertised models, used by `/models` slash command.
    fn list_models_for_display(&self) -> Vec<(String, String)>;
}

/// Slash command parsed from a user message.
#[derive(Debug)]
struct SlashCommand {
    name: String,
    args: String,
}

const SLASH_COMMANDS: &[&str] = &["clear", "compact", "models", "mode"];

fn parse_slash_command(text: &str) -> Option<SlashCommand> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim_start()),
        None => (rest, ""),
    };
    let name_lc = name.to_lowercase();
    if !SLASH_COMMANDS.contains(&name_lc.as_str()) {
        return None;
    }
    Some(SlashCommand { name: name_lc, args: args.to_string() })
}

/// The actual runner. Cheap to construct; owns no resources of its own
/// beyond Arcs to `Session` and the host.
pub struct SessionRunner {
    session: Arc<Session>,
    session_id: String,
    host: Arc<dyn RunnerHost>,
}

impl SessionRunner {
    pub fn new(session: Arc<Session>, session_id: String, host: Arc<dyn RunnerHost>) -> Self {
        Self { session, session_id, host }
    }

    /// Drive one user input through the session. Each `RunnerEvent` is
    /// delivered synchronously to `on_event` in emit order. Returns the
    /// final `RunOutcome` (also delivered as `RunnerEvent::Finished`).
    ///
    /// The `on_event` closure runs on whatever task this future runs on;
    /// servers typically `tokio::spawn` the future so the closure may
    /// `cx.send_notification(...)` synchronously.
    pub async fn run_input<F>(&self, user_text: String, mut on_event: F) -> RunOutcome
    where
        F: FnMut(RunnerEvent) + Send,
    {
        let scope_label = format!("session/prompt {}", self.session_id);
        let used_size = self.host.default_ctx_window();

        // The whole prompt body runs inside one Agent-typed observability
        // scope, with `with_task_scope` pinning the task-local stack so
        // nested LLM/tool scopes pop cleanly across thread migrations.
        let result = nemo_obs::with_task_scope(async {
            let _agent = nemo_obs::agent_scope(&scope_label);
            let result = self.run_input_inner(user_text, &mut on_event).await;
            // Emit terminal events while the observability scope is still
            // alive so they appear inside the agent span.
            let used = self.session.estimate_used_tokens().await;
            on_event(RunnerEvent::UsageReport { used, size: used_size });
            on_event(RunnerEvent::Finished(result.clone()));
            result
        })
        .await;
        self.host.save_session(&self.session_id).await;
        result
    }

    async fn run_input_inner<F>(&self, user_text: String, on_event: &mut F) -> RunOutcome
    where
        F: FnMut(RunnerEvent) + Send,
    {
        on_event(RunnerEvent::Started);
        if let Some(cmd) = parse_slash_command(&user_text) {
            self.run_slash(&cmd, on_event).await;
            return RunOutcome::Completed;
        }

        // Subscribe BEFORE prompt() so we don't miss the first events.
        let mut rx = self.session.subscribe();
        let session = self.session.clone();
        let prompt_fut: BoxFuture<'_, Result<PromptOutcome>> =
            Box::pin(async move { session.prompt(user_text).await });

        // Pump events from the broadcast channel into the callback while
        // the prompt future runs concurrently. Stops when AgentEnd is
        // observed OR when the prompt future resolves.
        let mut outcome = None;
        tokio::pin!(prompt_fut);
        loop {
            tokio::select! {
                biased;
                ev = rx.recv() => {
                    match ev {
                        Ok(RaEvent::TextDelta(s)) => on_event(RunnerEvent::TextDelta(s)),
                        Ok(RaEvent::ThinkingDelta(s)) => on_event(RunnerEvent::ThinkingDelta(s)),
                        Ok(RaEvent::ToolCallStart(c)) => {
                            let kind = match c.name.as_str() {
                                "read" => ToolKindHint::Read,
                                "bash" => ToolKindHint::Execute,
                                _ => ToolKindHint::Other,
                            };
                            let title = tool_title(&c.name, &c.input);
                            on_event(RunnerEvent::ToolCallStart {
                                id: c.id.clone(),
                                name: c.name.clone(),
                                input: c.input.clone(),
                                title,
                                kind,
                            });
                        }
                        Ok(RaEvent::ToolCallEnd(r)) => {
                            on_event(RunnerEvent::ToolCallEnd {
                                id: r.call_id,
                                is_error: r.is_error,
                                content: r.content,
                            });
                        }
                        Ok(RaEvent::AgentEnd) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                done = &mut prompt_fut => {
                    outcome = Some(match done {
                        Ok(PromptOutcome::Completed) => RunOutcome::Completed,
                        Ok(PromptOutcome::Cancelled) => RunOutcome::Cancelled,
                        Err(e) => RunOutcome::Failed(format!("{e:#}")),
                    });
                    // Drain any final events the bus has buffered. The
                    // prompt future may have resolved before we'd looped
                    // around to read the last broadcast frames; if we
                    // don't drain them here the consumer never sees them.
                    while let Ok(ev) = rx.try_recv() {
                        match ev {
                            RaEvent::TextDelta(s) => on_event(RunnerEvent::TextDelta(s)),
                            RaEvent::ThinkingDelta(s) => on_event(RunnerEvent::ThinkingDelta(s)),
                            RaEvent::ToolCallStart(c) => {
                                let kind = match c.name.as_str() {
                                    "read" => ToolKindHint::Read,
                                    "bash" => ToolKindHint::Execute,
                                    _ => ToolKindHint::Other,
                                };
                                let title = tool_title(&c.name, &c.input);
                                on_event(RunnerEvent::ToolCallStart {
                                    id: c.id.clone(),
                                    name: c.name.clone(),
                                    input: c.input.clone(),
                                    title,
                                    kind,
                                });
                            }
                            RaEvent::ToolCallEnd(r) => {
                                on_event(RunnerEvent::ToolCallEnd {
                                    id: r.call_id,
                                    is_error: r.is_error,
                                    content: r.content,
                                });
                            }
                            RaEvent::AgentEnd => break,
                            _ => {}
                        }
                    }
                    break;
                }
            }
        }
        outcome.unwrap_or(RunOutcome::Completed)
    }

    async fn run_slash<F>(&self, cmd: &SlashCommand, on_event: &mut F)
    where
        F: FnMut(RunnerEvent) + Send,
    {
        let send = |s: String, ev: &mut F| ev(RunnerEvent::TextDelta(s));
        match cmd.name.as_str() {
            "clear" => {
                self.session.restore_messages(Vec::new()).await;
                send("Session cleared.".into(), on_event);
            }
            "compact" => {
                let snapshot = self.session.snapshot_messages().await;
                let n = snapshot.len();
                let focus = if cmd.args.is_empty() {
                    String::new()
                } else {
                    format!(" focus={}", cmd.args)
                };
                self.session
                    .restore_messages(vec![RaMessage::User {
                        content: format!("[compacted: {n} prior messages elided]{focus}"),
                    }])
                    .await;
                send(format!("Compacted {n} prior messages."), on_event);
            }
            "models" => {
                let mut buf = String::from("Available models:\n");
                for (id, name) in self.host.list_models_for_display() {
                    buf.push_str(&format!("  • {id} — {name}\n"));
                }
                send(buf, on_event);
            }
            "mode" => {
                let valid = ["default", "plan", "ask"];
                let target = cmd.args.split_whitespace().next().unwrap_or("");
                if !valid.contains(&target) {
                    send(
                        format!(
                            "Unknown mode: '{target}'. Choose one of: {}.",
                            valid.join(", ")
                        ),
                        on_event,
                    );
                    return;
                }
                self.session.set_mode(target).await;
                on_event(RunnerEvent::ModeChanged(target.to_string()));
                send(format!("Mode → {target}."), on_event);
            }
            _ => unreachable!("parse_slash_command vetted the name"),
        }
    }
}

/// Human-readable title for a tool call, surfaced in client UI.
fn tool_title(name: &str, input: &serde_json::Value) -> String {
    match name {
        "read" => input
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("Read {p}"))
            .unwrap_or_else(|| "Read".into()),
        "bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| {
                let mut s = c.to_string();
                if s.len() > 60 {
                    s.truncate(60);
                    s.push('…');
                }
                format!("$ {s}")
            })
            .unwrap_or_else(|| "Run shell".into()),
        other => other.to_string(),
    }
}
