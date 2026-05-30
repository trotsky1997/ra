//! Phase 1 ACP server: serve Ra over stdio JSON-RPC.
//!
//! Implements the baseline 3 agent methods:
//! - `initialize`     — protocol version + capability handshake
//! - `session/new`    — create a Ra Session
//! - `session/prompt` — run the turn loop, stream session/update notifications
//!
//! Other methods are routed to `on_receive_dispatch` and answered with
//! method_not_found. Phase 2 will add the client-side reverse calls
//! (`fs/read_text_file`, `terminal/*`) and `session/request_permission`.

use std::sync::Arc;

use agent_client_protocol::{
    Agent as AcpAgent, ConnectionTo, Dispatch, Error as AcpError, Result as AcpResult, Stdio,
    on_receive_dispatch, on_receive_notification, on_receive_request,
    schema::{
        AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, InitializeRequest,
        InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse,
        SessionId, SessionNotification, SessionUpdate, StopReason, TextContent,
        ToolCall as AcpToolCall, ToolCallContent, ToolCallId, ToolCallStatus, ToolCallUpdate,
        ToolCallUpdateFields, ToolKind,
    },
    util,
};
use dashmap::DashMap;
use tokio::sync::broadcast;
use ulid::Ulid;

use crate::events::Event;
use crate::model::Model;
use crate::session::Session;
use crate::tools::{BashTool, ReadTool, Tool};

/// Build the agent's session map and the model factory used by every new session.
///
/// We keep one `Session` per ACP session id, all sharing the same `Model` instance
/// and the same set of tools. Tools and model are cheap clones (Arc).
struct SharedState {
    model: Arc<dyn Model>,
    tools: Vec<Arc<dyn Tool>>,
    sessions: DashMap<String, Arc<Session>>,
}

impl SharedState {
    fn new(model: Arc<dyn Model>) -> Self {
        Self {
            model,
            tools: vec![Arc::new(ReadTool), Arc::new(BashTool)],
            sessions: DashMap::new(),
        }
    }

    fn create_session(&self, id: &str) -> Arc<Session> {
        let s = Arc::new(Session::new(self.model.clone(), self.tools.clone()));
        self.sessions.insert(id.to_string(), s.clone());
        s
    }

    fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.get(id).map(|r| r.clone())
    }
}

/// Run the ACP server on stdio. Blocks until the client closes stdin.
pub async fn run(model: Arc<dyn Model>) -> AcpResult<()> {
    let state = Arc::new(SharedState::new(model));

    // Each handler closure is FnMut, so we clone the Arc into each one.
    let s_init = state.clone();
    let s_new = state.clone();
    let s_prompt = state.clone();
    let s_cancel = state.clone();

    AcpAgent
        .builder()
        .name("ra")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _cx| {
                let _ = &s_init; // keep the Arc captured even if we don't read it yet
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::default()),
                )
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: NewSessionRequest, responder, _cx| {
                let id = format!("ra_{}", Ulid::new());
                s_new.create_session(&id);
                responder.respond(NewSessionResponse::new(SessionId::from(id)))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: PromptRequest, responder, cx: ConnectionTo<agent_client_protocol::Client>| {
                let session_id = req.session_id.clone();
                let user_text = collect_text(&req.prompt);

                let Some(session) = s_prompt.get(&session_id.to_string()) else {
                    return responder.respond_with_error(util::internal_error(format!(
                        "unknown session id: {session_id}"
                    )));
                };

                // Subscribe BEFORE dispatching prompt, so we don't miss the first events.
                let rx = session.subscribe();
                let forwarder = spawn_event_forwarder(rx, session_id.clone(), cx.clone());

                // Run the turn loop. This drains the model + tools and emits events into the
                // broadcast channel; the forwarder task converts each one into session/update
                // and pushes it to the client.
                let prompt_result = session.prompt(user_text).await;

                // Wait for the forwarder to drain remaining events (it stops on AgentEnd).
                let _ = forwarder.await;

                let stop = match prompt_result {
                    Ok(crate::session::PromptOutcome::Completed) => StopReason::EndTurn,
                    Ok(crate::session::PromptOutcome::Cancelled) => StopReason::Cancelled,
                    Err(e) => {
                        eprintln!("[ra::acp] prompt error: {e:#}");
                        StopReason::EndTurn
                    }
                };
                responder.respond(PromptResponse::new(stop))
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            async move |notif: CancelNotification, _cx| {
                if let Some(s) = s_cancel.get(&notif.session_id.to_string()) {
                    s.cancel().await;
                }
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_dispatch(
            async move |msg: Dispatch, cx: ConnectionTo<agent_client_protocol::Client>| {
                msg.respond_with_error(AcpError::method_not_found(), cx)
            },
            on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
}

fn collect_text(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for b in blocks {
        if let ContentBlock::Text(t) = b {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&t.text);
        }
    }
    out
}

/// Bridge Ra's broadcast<Event> to ACP's session/update notifications.
///
/// Returns a JoinHandle that finishes when AgentEnd is observed (or the channel closes).
fn spawn_event_forwarder(
    mut rx: broadcast::Receiver<Event>,
    session_id: SessionId,
    cx: ConnectionTo<agent_client_protocol::Client>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Event::TextDelta(s)) => {
                    let notif = SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(
                            ContentBlock::Text(TextContent::new(s)),
                        )),
                    );
                    let _ = cx.send_notification(notif);
                }
                Ok(Event::ThinkingDelta(s)) => {
                    let notif = SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                            ContentBlock::Text(TextContent::new(s)),
                        )),
                    );
                    let _ = cx.send_notification(notif);
                }
                Ok(Event::ToolCallStart(c)) => {
                    let kind = tool_kind_for(&c.name);
                    let title = tool_title(&c.name, &c.input);
                    let notif = SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::ToolCall(
                            AcpToolCall::new(ToolCallId::from(c.id.clone()), title)
                                .kind(kind)
                                .status(ToolCallStatus::InProgress)
                                .raw_input(c.input.clone()),
                        ),
                    );
                    let _ = cx.send_notification(notif);
                }
                Ok(Event::ToolCallUpdate { .. }) => {
                    // intermediate progress (exit code etc) — Phase 2: emit
                    // ToolCallUpdateFields with `content` patch. Skipped for now.
                }
                Ok(Event::ToolCallEnd(r)) => {
                    let status = if r.is_error {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    };
                    let content = vec![ToolCallContent::from(ContentBlock::Text(
                        TextContent::new(r.content.clone()),
                    ))];
                    let raw_output = serde_json::Value::String(r.content.clone());
                    let fields = ToolCallUpdateFields::new()
                        .status(status)
                        .content(content)
                        .raw_output(raw_output);
                    let notif = SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                            ToolCallId::from(r.call_id.clone()),
                            fields,
                        )),
                    );
                    let _ = cx.send_notification(notif);
                }
                Ok(Event::AgentEnd) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
}

/// Map our internal tool name → ACP `ToolKind`. The kind drives client-side
/// icon / treatment ("read" vs "execute" gets very different UI).
fn tool_kind_for(name: &str) -> ToolKind {
    match name {
        "read" => ToolKind::Read,
        "bash" => ToolKind::Execute,
        _ => ToolKind::Other,
    }
}

/// Build a short human-readable title for a tool call, surfaced as the
/// first line in the client UI.
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

// (id generation lives inline at NewSessionRequest using Ulid::new())
