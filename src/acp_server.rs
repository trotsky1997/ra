//! ACP server: serve Ra over stdio JSON-RPC, including Phase 2 reverse
//! calls (the client editor hosts our filesystem reads and shell terminals).
//!
//! Agent methods implemented:
//! - `initialize`     — protocol version + capability handshake
//! - `session/new`    — create a Ra Session, attach an `AcpClientHandle`
//! - `session/prompt` — spawn the turn loop, stream `session/update`s
//! - `session/cancel` — notification, signals the session's CancellationToken
//!
//! Tools are wired to call back into the client through `AcpClientHandle`:
//! - `read`  → `fs/read_text_file`
//! - `bash`  → `terminal/create` + `wait_for_exit` + `terminal/output`
//!             + `terminal/release`, gated by `session/request_permission`.

use std::sync::Arc;

use agent_client_protocol::{
    Agent as AcpAgent, ConnectionTo, Dispatch, Error as AcpError, Result as AcpResult, Stdio,
    on_receive_dispatch, on_receive_notification, on_receive_request,
    schema::{
        AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, CreateTerminalRequest,
        InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
        PermissionOption, PermissionOptionId, PermissionOptionKind, PromptRequest, PromptResponse,
        ReadTextFileRequest, ReleaseTerminalRequest, RequestPermissionOutcome,
        RequestPermissionRequest, SessionId, SessionNotification, SessionUpdate, StopReason,
        TerminalOutputRequest, TextContent, ToolCall as AcpToolCall, ToolCallContent, ToolCallId,
        ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
        WaitForTerminalExitRequest, WriteTextFileRequest,
    },
    util,
};
use anyhow::anyhow;
use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::broadcast;
use ulid::Ulid;

use crate::events::Event;
use crate::model::Model;
use crate::session::Session;
use crate::tool_ctx::{ClientHandle, PermissionOutcome, TerminalRunResult};
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

    fn create_session(
        &self,
        id: &str,
        client: Arc<dyn ClientHandle>,
    ) -> Arc<Session> {
        let s = Arc::new(
            Session::new(self.model.clone(), self.tools.clone())
                .with_client(client, id.to_string()),
        );
        self.sessions.insert(id.to_string(), s.clone());
        s
    }

    fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.get(id).map(|r| r.clone())
    }
}

/// Bridge between Ra's `ClientHandle` trait and the ACP `ConnectionTo<Client>`.
///
/// `tokio::spawn` + `block_task().await` is the supported pattern: the
/// surrounding turn-loop already runs in a spawned task, so awaiting an
/// inner JSON-RPC response cannot deadlock the dispatch loop.
struct AcpClientHandle {
    cx: ConnectionTo<agent_client_protocol::Client>,
}

#[async_trait]
impl ClientHandle for AcpClientHandle {
    async fn fs_read_text_file(
        &self,
        session_id: &str,
        path: &str,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> anyhow::Result<String> {
        let mut req = ReadTextFileRequest::new(SessionId::from(session_id.to_string()), path);
        if let Some(l) = line {
            req = req.line(Some(l));
        }
        if let Some(l) = limit {
            req = req.limit(Some(l));
        }
        let resp = self
            .cx
            .send_request(req)
            .block_task()
            .await
            .map_err(|e| anyhow!("fs/read_text_file: {e:?}"))?;
        Ok(resp.content)
    }

    async fn fs_write_text_file(
        &self,
        session_id: &str,
        path: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        let req = WriteTextFileRequest::new(
            SessionId::from(session_id.to_string()),
            path,
            content,
        );
        self.cx
            .send_request(req)
            .block_task()
            .await
            .map_err(|e| anyhow!("fs/write_text_file: {e:?}"))?;
        Ok(())
    }

    async fn run_terminal(
        &self,
        session_id: &str,
        command: &str,
    ) -> anyhow::Result<TerminalRunResult> {
        let sid = SessionId::from(session_id.to_string());

        // Run via /bin/sh -c so the model can pass shell-y commands directly.
        let create_req = CreateTerminalRequest::new(sid.clone(), "/bin/sh")
            .args(vec!["-c".into(), command.into()]);
        let create_resp = self
            .cx
            .send_request(create_req)
            .block_task()
            .await
            .map_err(|e| anyhow!("terminal/create: {e:?}"))?;
        let term_id = create_resp.terminal_id;

        let exit = self
            .cx
            .send_request(WaitForTerminalExitRequest::new(sid.clone(), term_id.clone()))
            .block_task()
            .await
            .map_err(|e| anyhow!("terminal/wait_for_exit: {e:?}"))?;

        let out = self
            .cx
            .send_request(TerminalOutputRequest::new(sid.clone(), term_id.clone()))
            .block_task()
            .await
            .map_err(|e| anyhow!("terminal/output: {e:?}"))?;

        // Best-effort release; drop the error if the host already cleaned up.
        let _ = self
            .cx
            .send_request(ReleaseTerminalRequest::new(sid, term_id))
            .block_task()
            .await;

        Ok(TerminalRunResult {
            exit_code: exit.exit_status.exit_code.map(|n| n as i32),
            output: out.output,
        })
    }

    async fn request_permission(
        &self,
        session_id: &str,
        tool_call_id: &str,
        title: &str,
        _description: &str,
    ) -> anyhow::Result<PermissionOutcome> {
        let options = vec![
            PermissionOption::new(
                PermissionOptionId::from("allow_once".to_string()),
                "Allow once".to_string(),
                PermissionOptionKind::AllowOnce,
            ),
            PermissionOption::new(
                PermissionOptionId::from("allow_always".to_string()),
                "Allow always".to_string(),
                PermissionOptionKind::AllowAlways,
            ),
            PermissionOption::new(
                PermissionOptionId::from("reject_once".to_string()),
                "Reject".to_string(),
                PermissionOptionKind::RejectOnce,
            ),
        ];

        // The permission UI is keyed off ToolCallUpdate, so we surface the
        // title via that field. The tool_call must already exist client-side
        // (the bash tool emits a ToolCallStart before requesting permission).
        let update = ToolCallUpdate::new(
            ToolCallId::from(tool_call_id.to_string()),
            ToolCallUpdateFields::new().title(title.to_string()),
        );

        let req = RequestPermissionRequest::new(
            SessionId::from(session_id.to_string()),
            update,
            options,
        );

        let resp = self
            .cx
            .send_request(req)
            .block_task()
            .await
            .map_err(|e| anyhow!("session/request_permission: {e:?}"))?;

        Ok(match resp.outcome {
            RequestPermissionOutcome::Cancelled => PermissionOutcome::Cancelled,
            RequestPermissionOutcome::Selected(sel) => {
                let id = sel.option_id.to_string();
                if id.starts_with("allow") {
                    PermissionOutcome::Allowed
                } else {
                    PermissionOutcome::Denied
                }
            }
            _ => PermissionOutcome::Denied,
        })
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
            async move |_req: NewSessionRequest, responder, cx: ConnectionTo<agent_client_protocol::Client>| {
                let id = format!("ra_{}", Ulid::new());
                let handle: Arc<dyn ClientHandle> =
                    Arc::new(AcpClientHandle { cx: cx.clone() });
                s_new.create_session(&id, handle);
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

                // Move the actual prompt run into a spawned task so the dispatcher
                // stays free to deliver concurrent messages (cancel / fs / terminal
                // reverse calls). The Responder is owned and can be moved across
                // tasks; we respond once the turn loop finishes.
                tokio::spawn(async move {
                    let prompt_result = session.prompt(user_text).await;
                    let _ = forwarder.await;
                    let stop = match prompt_result {
                        Ok(crate::session::PromptOutcome::Completed) => StopReason::EndTurn,
                        Ok(crate::session::PromptOutcome::Cancelled) => StopReason::Cancelled,
                        Err(e) => {
                            eprintln!("[ra::acp] prompt error: {e:#}");
                            StopReason::EndTurn
                        }
                    };
                    let _ = responder.respond(PromptResponse::new(stop));
                });

                Ok(())
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
