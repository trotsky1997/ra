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
    on_receive_dispatch, on_receive_request,
    schema::{
        AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
        NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionId,
        SessionNotification, SessionUpdate, StopReason, TextContent,
    },
    util,
};
use dashmap::DashMap;
use tokio::sync::broadcast;

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
                let id = format!("ra_{}", new_id());
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
                    Ok(()) => StopReason::EndTurn,
                    Err(e) => {
                        eprintln!("[ra::acp] prompt error: {e:#}");
                        StopReason::EndTurn
                    }
                };
                responder.respond(PromptResponse::new(stop))
            },
            on_receive_request!(),
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
                Ok(Event::ToolCallStart(_))
                | Ok(Event::ToolCallEnd(_))
                | Ok(Event::ToolCallUpdate { .. }) => {
                    // Phase 2: map to SessionUpdate::ToolCall / ToolCallUpdate.
                    // For now we silently swallow them; the tool result still appears
                    // in the next AgentMessageChunk because the model summarizes it.
                }
                Ok(Event::AgentEnd) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
}

fn new_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!(
        "{:x}",
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    )
}
