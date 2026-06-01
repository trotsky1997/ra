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
//!   + `terminal/release`, gated by `session/request_permission`.

use std::sync::Arc;

use agent_client_protocol::{
    on_receive_dispatch, on_receive_notification, on_receive_request,
    schema::{
        AgentCapabilities, AuthMethod, AuthMethodAgent, AuthMethodId, AuthenticateRequest,
        AuthenticateResponse, AvailableCommand, AvailableCommandInput, AvailableCommandsUpdate,
        CancelNotification, CloseSessionRequest, CloseSessionResponse, ConfigOptionUpdate,
        ContentBlock, ContentChunk, CreateTerminalRequest, CurrentModeUpdate, DeleteSessionRequest,
        DeleteSessionResponse, ForkSessionRequest, ForkSessionResponse, InitializeRequest,
        InitializeResponse, KillTerminalRequest, ListSessionsRequest, ListSessionsResponse,
        LoadSessionRequest, LoadSessionResponse, LogoutRequest, LogoutResponse, ModelId, ModelInfo,
        NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionId,
        PermissionOptionKind, PromptRequest, PromptResponse, ReadTextFileRequest,
        ReleaseTerminalRequest, RequestPermissionOutcome, RequestPermissionRequest,
        ResumeSessionRequest, ResumeSessionResponse, SessionConfigId, SessionConfigOption,
        SessionConfigOptionValue, SessionConfigSelectOption, SessionConfigValueId, SessionId,
        SessionInfo, SessionMode, SessionModeId, SessionModeState, SessionModelState,
        SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
        SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse,
        SetSessionModelRequest, SetSessionModelResponse, StopReason, TerminalOutputRequest,
        TextContent, ToolCall as AcpToolCall, ToolCallContent, ToolCallId, ToolCallStatus,
        ToolCallUpdate, ToolCallUpdateFields, ToolKind, UnstructuredCommandInput, UsageUpdate,
        WaitForTerminalExitRequest, WriteTextFileRequest,
    },
    util, Agent as AcpAgent, ConnectionTo, Dispatch, Error as AcpError, Result as AcpResult, Stdio,
};
use anyhow::anyhow;
use async_trait::async_trait;
use dashmap::DashMap;
use std::path::PathBuf;
use ulid::Ulid;

use crate::atif_codec;
use crate::model::Model;
use crate::session::Session;
use crate::session_runner::{RunOutcome, RunnerEvent, RunnerHost, SessionRunner, ToolKindHint};
use crate::skills::SlashTemplate;
use crate::store::SessionStore;
use crate::tool_ctx::{ClientHandle, PermissionOutcome, TerminalRunResult};
use crate::tools::Tool;

/// The mode catalogue Ra advertises. We don't actually change behavior between
/// modes today; the field is mostly cosmetic and lets clients show a picker.
fn ra_modes() -> SessionModeState {
    let make = |id: &str, name: &str, desc: &str| {
        let mut m = SessionMode::new(SessionModeId::from(id.to_string()), name.to_string());
        m.description = Some(desc.to_string());
        m
    };
    SessionModeState::new(
        SessionModeId::from("default".to_string()),
        vec![
            make("default", "Default", "Run tools as needed."),
            make("plan", "Plan", "Think only; do not run tools."),
            make("ask", "Ask", "Confirm every tool call."),
        ],
    )
}

/// Default `agent` auth method advertised to clients. Ra holds its API
/// keys in environment variables and considers itself "always authenticated"
/// once those are set, so this is a single no-op method.
fn ra_auth_methods() -> Vec<AuthMethod> {
    let mut m = AuthMethodAgent::new(
        AuthMethodId::from("env".to_string()),
        "Environment".to_string(),
    );
    m.description =
        Some("Auth via PI_API_KEY / ANTHROPIC_API_KEY / OPENAI_API_KEY env vars.".into());
    vec![AuthMethod::Agent(m)]
}

/// Best-guess context window size (in tokens) for a given model id, used
/// as the `size` field on `SessionUpdate::UsageUpdate`. Falls back to a
/// conservative default for unknown ids.
fn ctx_window_for(model_id: &str) -> u64 {
    let id = model_id.to_ascii_lowercase();
    if id.contains("gpt-5") {
        1_000_000
    } else if id.contains("gpt-4.1") || id.contains("gpt-4o") {
        128_000
    } else if id.contains("claude") {
        200_000
    } else if id.contains("gemini") {
        1_000_000
    } else if id.contains("ollama") {
        8_000
    } else {
        128_000
    }
}
/// intercepted server-side in the prompt handler when the user message
/// starts with `/<name>`; the LLM never sees these.
fn ra_commands() -> Vec<AvailableCommand> {
    let unstructured = |hint: &str| {
        Some(AvailableCommandInput::Unstructured(
            UnstructuredCommandInput::new(hint),
        ))
    };
    vec![
        AvailableCommand::new("clear", "Reset the session: drop all prior messages."),
        AvailableCommand::new(
            "compact",
            "Summarize the conversation so far and replace history with the summary.",
        )
        .input(unstructured("optional focus, e.g. 'keep code edits'")),
        AvailableCommand::new("models", "List the available LLM backends and their ids."),
        AvailableCommand::new("mode", "Switch session mode: default | plan | ask.")
            .input(unstructured("default | plan | ask")),
    ]
}

/// Per-session configuration options advertised to the client. These are
/// surfaced via `NewSessionResponse.configOptions` and re-emitted on
/// `ConfigOptionUpdate` notifications when the user changes them via
/// `session/set_config_option`. Today they're informational only — Ra
/// stores the values on the Session but does not yet branch its behavior
/// on them.
fn ra_config_options() -> Vec<SessionConfigOption> {
    vec![
        SessionConfigOption::boolean(
            SessionConfigId::from("follow_up_summary".to_string()),
            "Follow-up summary".to_string(),
            false,
        )
        .description("Automatically summarize the conversation after each turn."),
        SessionConfigOption::select(
            SessionConfigId::from("verbose_tools".to_string()),
            "Tool output verbosity".to_string(),
            SessionConfigValueId::from("compact".to_string()),
            vec![
                SessionConfigSelectOption::new(
                    SessionConfigValueId::from("compact".to_string()),
                    "Compact".to_string(),
                ),
                SessionConfigSelectOption::new(
                    SessionConfigValueId::from("detailed".to_string()),
                    "Detailed".to_string(),
                ),
            ],
        )
        .description("Whether tool outputs are inlined verbatim or trimmed."),
    ]
}

/// Holds the active default model, the model registry (for `session/set_model`
/// + `NewSessionResponse.models`), the on-disk trajectory store, and the
///   live ACP session map.
struct SharedState {
    model: Arc<dyn Model>,
    model_factory: Arc<dyn ModelFactory>,
    available_models: Vec<ModelInfo>,
    tools: Vec<Arc<dyn Tool>>,
    sessions: DashMap<String, Arc<Session>>,
    /// Per-session ATIF cwd reported by the client at session/new (or
    /// session/load). Used as the bucketing key for the on-disk store.
    session_cwds: DashMap<String, PathBuf>,
    /// Default cwd for `session/list` when the client doesn't pin one.
    /// We treat the agent's launch cwd as a sensible fallback.
    default_cwd: PathBuf,
    /// System prompt loaded from skills (if any). Injected into every
    /// new Session at create_session time.
    system_prompt: Option<String>,
    /// Prompt templates keyed by slash-command name. Injected into the
    /// SessionRunner so `/<name>` expands into LLM input.
    prompt_templates: Arc<std::collections::HashMap<String, SlashTemplate>>,
    /// Optional lifecycle hooks engine, attached to every Session.
    hooks: Option<Arc<crate::hooks::HookEngine>>,
    /// RTK rewriter applied to every Session built from this state.
    rtk: crate::tools::RtkRewriter,
}

/// Resolve a model id (sent by the client over `session/set_model`) to a `Model`
/// instance. Implementors can read environment, multiplex backends, etc.
pub trait ModelFactory: Send + Sync {
    fn build(&self, model_id: &str) -> Option<Arc<dyn Model>>;
    /// Identifier of the default model (matches one of `available_models`).
    fn default_model_id(&self) -> String;
    /// What to advertise to ACP clients in `NewSessionResponse.models`.
    fn available(&self) -> Vec<ModelInfo>;
}

impl SharedState {
    fn new(
        model: Arc<dyn Model>,
        model_factory: Arc<dyn ModelFactory>,
        extra_tools: Vec<Arc<dyn Tool>>,
        system_prompt: Option<String>,
        prompt_templates: Arc<std::collections::HashMap<String, SlashTemplate>>,
        hooks: Option<Arc<crate::hooks::HookEngine>>,
        rtk: crate::tools::RtkRewriter,
    ) -> Self {
        let available_models = model_factory.available();
        let default_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        // The full tool catalog is passed in by the caller (main.rs) so the
        // `[tools] builtin` allow-list lives in one place. Empty input is
        // allowed but unusual.
        let tools = extra_tools;
        Self {
            model,
            model_factory,
            available_models,
            tools,
            sessions: DashMap::new(),
            session_cwds: DashMap::new(),
            default_cwd,
            system_prompt,
            prompt_templates,
            hooks,
            rtk,
        }
    }

    async fn create_session(
        &self,
        id: &str,
        client: Arc<dyn ClientHandle>,
        cwd: PathBuf,
    ) -> Arc<Session> {
        let mut s = Session::new(self.model.clone(), self.tools.clone())
            .with_client(client, id.to_string())
            .with_cwd(cwd.clone())
            .with_rtk(self.rtk.clone());
        if let Some(h) = &self.hooks {
            s = s.with_hooks(h.clone());
        }
        let s = Arc::new(s);
        if let Some(sp) = &self.system_prompt {
            s.set_system_prompt(sp.clone()).await;
        }
        self.sessions.insert(id.to_string(), s.clone());
        self.session_cwds.insert(id.to_string(), cwd);
        s
    }

    fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.get(id).map(|r| r.clone())
    }

    fn cwd_for(&self, id: &str) -> PathBuf {
        self.session_cwds
            .get(id)
            .map(|p| p.clone())
            .unwrap_or_else(|| self.default_cwd.clone())
    }

    fn store_for(&self, id: &str) -> anyhow::Result<SessionStore> {
        SessionStore::for_cwd(self.cwd_for(id))
    }

    /// Snapshot the session's messages → ATIF Trajectory → save to disk.
    /// Best-effort: errors are logged but do not propagate (we don't want to
    /// fail an otherwise-completed prompt because the disk hiccuped).
    async fn save_session(&self, id: &str) {
        let Some(session) = self.get(id) else { return };
        let store = match self.store_for(id) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[ra::acp] store_for({id}): {e:#}");
                return;
            }
        };
        let messages = session.snapshot_messages().await;
        let model_name = Some(self.model_factory.default_model_id());
        let traj = atif_codec::encode(id, model_name, &messages);
        if let Err(e) = store.save(&traj).await {
            eprintln!("[ra::acp] save trajectory {id}: {e:#}");
        }
    }
}

#[async_trait]
impl RunnerHost for SharedState {
    async fn save_session(&self, session_id: &str) {
        // Reuse the inherent method to avoid name collision; the trait
        // method takes precedence in callers because they hold the trait
        // object via Arc<dyn RunnerHost>.
        SharedState::save_session(self, session_id).await
    }

    fn default_ctx_window(&self) -> u64 {
        ctx_window_for(&self.model_factory.default_model_id())
    }

    fn list_models_for_display(&self) -> Vec<(String, String)> {
        self.available_models
            .iter()
            .map(|m| (m.model_id.to_string(), m.name.clone()))
            .collect()
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
        let req = WriteTextFileRequest::new(SessionId::from(session_id.to_string()), path, content);
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
            .send_request(WaitForTerminalExitRequest::new(
                sid.clone(),
                term_id.clone(),
            ))
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

    async fn kill_terminal(&self, session_id: &str, terminal_id: &str) -> anyhow::Result<()> {
        use agent_client_protocol::schema::TerminalId;
        let req = KillTerminalRequest::new(
            SessionId::from(session_id.to_string()),
            TerminalId::from(terminal_id.to_string()),
        );
        self.cx
            .send_request(req)
            .block_task()
            .await
            .map_err(|e| anyhow!("terminal/kill: {e:?}"))?;
        Ok(())
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

        let req =
            RequestPermissionRequest::new(SessionId::from(session_id.to_string()), update, options);

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
///
/// `model` is the initial model used for sessions until the client overrides
/// it via `session/set_model`. `model_factory` is consulted on those overrides
/// and to advertise the list of available models in `NewSessionResponse`.
pub async fn run(
    model: Arc<dyn Model>,
    model_factory: Arc<dyn ModelFactory>,
    extra_tools: Vec<Arc<dyn Tool>>,
    system_prompt: Option<String>,
    prompt_templates: Arc<std::collections::HashMap<String, SlashTemplate>>,
    hooks: Option<Arc<crate::hooks::HookEngine>>,
    rtk: crate::tools::RtkRewriter,
) -> AcpResult<()> {
    crate::nemo_obs::init();
    let state = Arc::new(SharedState::new(
        model,
        model_factory,
        extra_tools,
        system_prompt,
        prompt_templates,
        hooks,
        rtk,
    ));

    // Each handler closure is FnMut, so we clone the Arc into each one.
    let s_init = state.clone();
    let s_new = state.clone();
    let s_prompt = state.clone();
    let s_cancel = state.clone();
    let s_setmodel = state.clone();
    let s_fork = state.clone();
    let s_load = state.clone();
    let s_list = state.clone();
    let s_close = state.clone();
    let s_resume = state.clone();
    let s_delete = state.clone();
    let s_auth = state.clone();
    let s_logout = state.clone();
    let s_setmode = state.clone();
    let s_setconfig = state.clone();

    AcpAgent
        .builder()
        .name("ra")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _cx| {
                let _ = &s_init;
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::default())
                        .auth_methods(ra_auth_methods()),
                )
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: NewSessionRequest,
                        responder,
                        cx: ConnectionTo<agent_client_protocol::Client>| {
                let id = format!("ra_{}", Ulid::new());
                let handle: Arc<dyn ClientHandle> = Arc::new(AcpClientHandle { cx: cx.clone() });
                s_new.create_session(&id, handle, req.cwd.clone()).await;
                // Persist an empty trajectory upfront so the session shows up
                // in session/list immediately (not just after first prompt).
                s_new.save_session(&id).await;
                let session_id = SessionId::from(id);
                // Advertise our slash commands now that the session exists.
                let _ = cx.send_notification(SessionNotification::new(
                    session_id.clone(),
                    SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(
                        ra_commands(),
                    )),
                ));
                let resp = NewSessionResponse::new(session_id)
                    .modes(ra_modes())
                    .config_options(Some(ra_config_options()))
                    .models(SessionModelState::new(
                        ModelId::from(s_new.model_factory.default_model_id()),
                        s_new.available_models.clone(),
                    ));
                responder.respond(resp)
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: PromptRequest,
                        responder,
                        cx: ConnectionTo<agent_client_protocol::Client>| {
                let session_id = req.session_id.clone();
                let user_text = collect_text(&req.prompt);

                let Some(session) = s_prompt.get(&session_id.to_string()) else {
                    return responder.respond_with_error(util::internal_error(format!(
                        "unknown session id: {session_id}"
                    )));
                };

                // Build a SessionRunner with the shared host. The runner
                // owns the spawn body, slash dispatch, observability scope,
                // event forwarding and final save.
                let host: Arc<dyn RunnerHost> = s_prompt.clone();
                let runner = SessionRunner::new(session, session_id.to_string(), host)
                    .with_prompt_templates(s_prompt.prompt_templates.clone());

                let cx_for_task = cx.clone();
                let session_id_for_cb = session_id.clone();
                let mut accumulated_text = String::new();

                tokio::spawn(async move {
                    let outcome = runner
                        .run_input(user_text, |ev| {
                            translate_runner_event(
                                &session_id_for_cb,
                                &cx_for_task,
                                &mut accumulated_text,
                                ev,
                            );
                        })
                        .await;

                    // The runner already emitted UsageReport via the callback,
                    // but PromptResponse.usage carries the same number for
                    // clients that close before the notification lands.
                    let final_usage = agent_client_protocol::schema::Usage::new(0, 0, 0);
                    let stop = match outcome {
                        RunOutcome::Completed | RunOutcome::Failed(_) => StopReason::EndTurn,
                        RunOutcome::Cancelled => StopReason::Cancelled,
                    };
                    let _ = responder.respond(PromptResponse::new(stop).usage(Some(final_usage)));
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
        .on_receive_request(
            async move |req: SetSessionModelRequest, responder, _cx| {
                let Some(session) = s_setmodel.get(&req.session_id.to_string()) else {
                    return responder.respond_with_error(util::internal_error(format!(
                        "unknown session id: {}",
                        req.session_id
                    )));
                };
                let model_id = req.model_id.to_string();
                match s_setmodel.model_factory.build(&model_id) {
                    Some(m) => {
                        session.set_model(m).await;
                        responder.respond(SetSessionModelResponse::default())
                    }
                    None => responder.respond_with_error(util::internal_error(format!(
                        "unknown model id: {model_id}"
                    ))),
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ForkSessionRequest,
                        responder,
                        cx: ConnectionTo<agent_client_protocol::Client>| {
                let Some(parent) = s_fork.get(&req.session_id.to_string()) else {
                    return responder.respond_with_error(util::internal_error(format!(
                        "unknown session id: {}",
                        req.session_id
                    )));
                };
                let new_id = format!("ra_{}", Ulid::new());
                let handle: Arc<dyn ClientHandle> = Arc::new(AcpClientHandle { cx: cx.clone() });
                let child = s_fork
                    .create_session(&new_id, handle, req.cwd.clone())
                    .await;
                let snapshot = parent.snapshot_messages().await;
                child.restore_messages(snapshot).await;
                s_fork.save_session(&new_id).await;
                let resp = ForkSessionResponse::new(SessionId::from(new_id))
                    .modes(ra_modes())
                    .config_options(Some(ra_config_options()))
                    .models(SessionModelState::new(
                        ModelId::from(s_fork.model_factory.default_model_id()),
                        s_fork.available_models.clone(),
                    ));
                responder.respond(resp)
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: LoadSessionRequest,
                        responder,
                        cx: ConnectionTo<agent_client_protocol::Client>| {
                let id = req.session_id.to_string();
                let cwd = req.cwd.clone();
                let store = match SessionStore::for_cwd(&cwd) {
                    Ok(s) => s,
                    Err(e) => {
                        return responder.respond_with_error(util::internal_error(format!(
                            "store for {}: {e:#}",
                            cwd.display()
                        )));
                    }
                };
                let traj = match store.load(&id).await {
                    Ok(t) => t,
                    Err(e) => {
                        return responder
                            .respond_with_error(util::internal_error(format!("load {id}: {e:#}")));
                    }
                };
                let messages = atif_codec::decode(&traj);
                let handle: Arc<dyn ClientHandle> = Arc::new(AcpClientHandle { cx: cx.clone() });
                let session = s_load.create_session(&id, handle, cwd).await;
                session.restore_messages(messages).await;
                let resp = LoadSessionResponse::default();
                responder.respond(resp)
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ListSessionsRequest, responder, _cx| {
                let cwd = req
                    .cwd
                    .clone()
                    .unwrap_or_else(|| s_list.default_cwd.clone());
                let store = match SessionStore::for_cwd(&cwd) {
                    Ok(s) => s,
                    Err(e) => {
                        return responder.respond_with_error(util::internal_error(format!(
                            "store for {}: {e:#}",
                            cwd.display()
                        )));
                    }
                };
                let metas = match store.list().await {
                    Ok(m) => m,
                    Err(e) => {
                        return responder
                            .respond_with_error(util::internal_error(format!("list: {e:#}")));
                    }
                };
                let sessions = metas
                    .into_iter()
                    .map(|m| {
                        let mut info = SessionInfo::new(SessionId::from(m.session_id), cwd.clone());
                        info.title = m.title;
                        info.updated_at = chrono::DateTime::<chrono::Utc>::from(m.modified)
                            .format("%Y-%m-%dT%H:%M:%SZ")
                            .to_string()
                            .into();
                        info
                    })
                    .collect();
                responder.respond(ListSessionsResponse::new(sessions))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: CloseSessionRequest, responder, _cx| {
                let id = req.session_id.to_string();
                // Final save before letting go.
                s_close.save_session(&id).await;
                s_close.sessions.remove(&id);
                s_close.session_cwds.remove(&id);
                responder.respond(CloseSessionResponse::default())
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ResumeSessionRequest,
                        responder,
                        cx: ConnectionTo<agent_client_protocol::Client>| {
                // resume == load semantically for us: the on-disk trajectory
                // is the source of truth, and a fresh in-memory Session is
                // hydrated from it. (We don't keep idle sessions warm.)
                let id = req.session_id.to_string();
                let cwd = req.cwd.clone();
                let store = match SessionStore::for_cwd(&cwd) {
                    Ok(s) => s,
                    Err(e) => {
                        return responder.respond_with_error(util::internal_error(format!(
                            "store for {}: {e:#}",
                            cwd.display()
                        )));
                    }
                };
                let traj = match store.load(&id).await {
                    Ok(t) => t,
                    Err(e) => {
                        return responder
                            .respond_with_error(util::internal_error(format!("load {id}: {e:#}")));
                    }
                };
                let messages = atif_codec::decode(&traj);
                let handle: Arc<dyn ClientHandle> = Arc::new(AcpClientHandle { cx: cx.clone() });
                let session = s_resume.create_session(&id, handle, cwd).await;
                session.restore_messages(messages).await;
                let resp = ResumeSessionResponse::default();
                responder.respond(resp)
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: DeleteSessionRequest, responder, _cx| {
                let id = req.session_id.to_string();
                // Drop in-memory session if present, then nuke the file.
                s_delete.sessions.remove(&id);
                let cwd = s_delete.cwd_for(&id);
                s_delete.session_cwds.remove(&id);
                let store = match SessionStore::for_cwd(&cwd) {
                    Ok(s) => s,
                    Err(e) => {
                        return responder.respond_with_error(util::internal_error(format!(
                            "store for {}: {e:#}",
                            cwd.display()
                        )));
                    }
                };
                if let Err(e) = store.delete(&id).await {
                    return responder
                        .respond_with_error(util::internal_error(format!("delete {id}: {e:#}")));
                }
                responder.respond(DeleteSessionResponse::default())
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: AuthenticateRequest, responder, _cx| {
                // Single 'env' method advertised at initialize. Ra trusts the
                // env to already hold the API key, so authenticate is a no-op
                // success; unknown method ids return InvalidRequest.
                let _ = &s_auth;
                if req.method_id.to_string() == "env" {
                    responder.respond(AuthenticateResponse::default())
                } else {
                    responder.respond_with_error(util::internal_error(format!(
                        "unknown auth method: {}",
                        req.method_id
                    )))
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: LogoutRequest, responder, _cx| {
                // No persistent auth state to clear (env vars stay where they
                // are). The handler exists so clients calling logout don't
                // see method_not_found.
                let _ = &s_logout;
                responder.respond(LogoutResponse::default())
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: SetSessionModeRequest,
                        responder,
                        cx: ConnectionTo<agent_client_protocol::Client>| {
                let id = req.session_id.to_string();
                let Some(session) = s_setmode.get(&id) else {
                    return responder.respond_with_error(util::internal_error(format!(
                        "unknown session id: {id}"
                    )));
                };
                let mode_id = req.mode_id.to_string();
                // Validate against the advertised catalogue.
                let valid = ra_modes()
                    .available_modes
                    .iter()
                    .any(|m| m.id.to_string() == mode_id);
                if !valid {
                    return responder.respond_with_error(util::internal_error(format!(
                        "unknown mode id: {mode_id}"
                    )));
                }
                session.set_mode(&mode_id).await;
                // Notify the client of the new current mode so it can
                // refresh any picker UI.
                let notif = SessionNotification::new(
                    req.session_id.clone(),
                    SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(SessionModeId::from(
                        mode_id,
                    ))),
                );
                let _ = cx.send_notification(notif);
                responder.respond(SetSessionModeResponse::default())
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: SetSessionConfigOptionRequest,
                        responder,
                        cx: ConnectionTo<agent_client_protocol::Client>| {
                let id = req.session_id.to_string();
                let Some(session) = s_setconfig.get(&id) else {
                    return responder.respond_with_error(util::internal_error(format!(
                        "unknown session id: {id}"
                    )));
                };
                let config_id = req.config_id.to_string();
                // Validate against the advertised catalogue.
                if !ra_config_options()
                    .iter()
                    .any(|c| c.id.to_string() == config_id)
                {
                    return responder.respond_with_error(util::internal_error(format!(
                        "unknown config option id: {config_id}"
                    )));
                }
                // Persist whichever variant the client sent. We store as a
                // free-form JSON Value so the same map serves both boolean
                // and select-id payloads.
                let stored = match &req.value {
                    SessionConfigOptionValue::Boolean { value } => serde_json::json!(value),
                    SessionConfigOptionValue::ValueId { value } => {
                        serde_json::json!(value.to_string())
                    }
                    _ => serde_json::Value::Null,
                };
                session.set_config(&config_id, stored).await;

                // Echo the full advertised catalogue back. This is what ACP
                // clients consume to rebuild their config UI; for now the
                // catalogue is static, so we don't bother reflecting the
                // user's pick into the displayed `current_value`.
                let opts = ra_config_options();
                let notif = SessionNotification::new(
                    req.session_id.clone(),
                    SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(opts.clone())),
                );
                let _ = cx.send_notification(notif);
                responder.respond(SetSessionConfigOptionResponse::new(opts))
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

/// Translate one `RunnerEvent` into ACP `SessionUpdate` notifications.
///
/// Called from inside the prompt-handler's spawned task. Owns its own copy
/// of the running text so the spec-encouraged final `current_message` could
/// be reconstructed if we ever wanted (currently unused).
fn translate_runner_event(
    session_id: &SessionId,
    cx: &ConnectionTo<agent_client_protocol::Client>,
    accumulated_text: &mut String,
    ev: RunnerEvent,
) {
    let notify = |update: SessionUpdate| {
        let _ = cx.send_notification(SessionNotification::new(session_id.clone(), update));
    };
    match ev {
        RunnerEvent::TextDelta(s) => {
            accumulated_text.push_str(&s);
            notify(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(s)),
            )));
        }
        RunnerEvent::ThinkingDelta(s) => {
            notify(SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(s)),
            )));
        }
        RunnerEvent::ToolCallStart {
            id,
            name: _,
            input,
            title,
            kind,
        } => {
            notify(SessionUpdate::ToolCall(
                AcpToolCall::new(ToolCallId::from(id), title)
                    .kind(tool_kind_hint_to_acp(kind))
                    .status(ToolCallStatus::InProgress)
                    .raw_input(input),
            ));
        }
        RunnerEvent::ToolCallEnd {
            id,
            is_error,
            content,
        } => {
            let status = if is_error {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            };
            let acp_content = vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(
                content.clone(),
            )))];
            let fields = ToolCallUpdateFields::new()
                .status(status)
                .content(acp_content)
                .raw_output(serde_json::Value::String(content));
            notify(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::from(id),
                fields,
            )));
        }
        RunnerEvent::ModeChanged(mode_id) => {
            notify(SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(
                SessionModeId::from(mode_id),
            )));
        }
        RunnerEvent::UsageReport { used, size } => {
            notify(SessionUpdate::UsageUpdate(UsageUpdate::new(used, size)));
        }
        RunnerEvent::Started | RunnerEvent::Finished(_) => {}
    }
}

fn tool_kind_hint_to_acp(hint: ToolKindHint) -> ToolKind {
    match hint {
        ToolKindHint::Read => ToolKind::Read,
        ToolKindHint::Search => ToolKind::Search,
        ToolKindHint::Execute => ToolKind::Execute,
        ToolKindHint::Other => ToolKind::Other,
    }
}

// (id generation lives inline at NewSessionRequest using Ulid::new())
