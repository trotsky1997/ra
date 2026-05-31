//! A2A server: serve Ra as a remote Agent2Agent agent.
//!
//! Exposes three transport bindings backed by the same `RaExecutor`:
//!   - HTTP JSON-RPC at `:HTTP_PORT/jsonrpc`
//!   - HTTP REST at `:HTTP_PORT/rest`
//!   - gRPC at `:GRPC_PORT`
//!
//! The agent card is reachable at `:HTTP_PORT/.well-known/agent-card.json`.
//!
//! Auth: optional Bearer token. Set `RA_A2A_TOKEN` to require it; clients
//! must send `Authorization: Bearer <token>`. Unset (default) means no
//! authentication, suitable for localhost / behind a reverse proxy.
//!
//! All three transports share one `DefaultRequestHandler` and a single
//! in-process Ra `SharedState`. A2A `Task`s map 1:1 to Ra `Session`s.

use std::sync::Arc;

use a2a::{
    AgentCapabilities, AgentCard, AgentInterface, AgentProvider, AgentSkill, A2AError,
    HttpAuthSecurityScheme, Message, Part, PartContent, Role, SecurityRequirement,
    SecurityScheme, StreamResponse, Task, TaskState, TaskStatus, TaskStatusUpdateEvent,
    TRANSPORT_PROTOCOL_GRPC, TRANSPORT_PROTOCOL_HTTP_JSON, TRANSPORT_PROTOCOL_JSONRPC,
};
use a2a_grpc::GrpcHandler;
use a2a_pb::proto::a2a_service_server::A2aServiceServer;
use a2a_server::{
    AgentExecutor, DefaultRequestHandler, ExecutorContext, InMemoryTaskStore, StaticAgentCard,
    agent_card::agent_card_router, jsonrpc::jsonrpc_router, rest::rest_router,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use futures::stream::{self, BoxStream};
use std::future::IntoFuture;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Server as TonicServer;
use ulid::Ulid;

use crate::acp_server::ModelFactory;
use crate::model::Model;
use crate::nemo_obs;
use crate::session::Session;
use crate::session_runner::{RunOutcome, RunnerEvent, RunnerHost, SessionRunner};
use crate::store::SessionStore;
use crate::tools::Tool;

/// Top-level A2A server state. Mirrors `acp_server::SharedState` but
/// trimmed to only what the A2A path needs (no AcpClientHandle wiring,
/// no per-session cwd map — A2A clients don't have a host filesystem
/// to delegate back to).
pub struct A2aState {
    model: Arc<dyn Model>,
    model_factory: Arc<dyn ModelFactory>,
    tools: Vec<Arc<dyn Tool>>,
    /// Map task_id → live Ra Session. A2A `Task` ↔ Ra `Session`.
    sessions: DashMap<String, Arc<Session>>,
    cwd: std::path::PathBuf,
    /// System prompt seeded into every new Session, mirroring acp_server.
    system_prompt: Option<String>,
    /// Slash command templates handed to the SessionRunner.
    prompt_templates: Arc<std::collections::HashMap<String, String>>,
    /// Optional lifecycle hooks attached to every Session.
    hooks: Option<Arc<crate::hooks::HookEngine>>,
}

impl A2aState {
    pub fn new(
        model: Arc<dyn Model>,
        model_factory: Arc<dyn ModelFactory>,
        extra_tools: Vec<Arc<dyn Tool>>,
        system_prompt: Option<String>,
        prompt_templates: Arc<std::collections::HashMap<String, String>>,
        hooks: Option<Arc<crate::hooks::HookEngine>>,
    ) -> Self {
        // Full tool catalog supplied by the caller (main.rs); see
        // `tools::default_builtins` for the allow-list filter and missing
        // external-binary detection.
        let tools = extra_tools;
        Self {
            model,
            model_factory,
            tools,
            sessions: DashMap::new(),
            cwd: std::env::current_dir().unwrap_or_else(|_| "/".into()),
            system_prompt,
            prompt_templates,
            hooks,
        }
    }

    async fn get_or_create(&self, task_id: &str) -> Arc<Session> {
        if let Some(s) = self.sessions.get(task_id) {
            return s.clone();
        }
        let mut s = Session::new(self.model.clone(), self.tools.clone());
        if let Some(h) = &self.hooks {
            s = s.with_hooks(h.clone());
        }
        let s = Arc::new(s);
        if let Some(sp) = &self.system_prompt {
            s.set_system_prompt(sp.clone()).await;
        }
        // Resume: if an ATIF trajectory exists on disk for this task id,
        // hydrate the message log from it. Lets A2A clients reconnect to
        // a prior task and keep the model's context, or pick up where a
        // crashed `ra serve` left off.
        if let Ok(store) = SessionStore::for_cwd(&self.cwd) {
            if store.path_for(task_id).exists() {
                match store.load(task_id).await {
                    Ok(traj) => {
                        let messages = crate::atif_codec::decode(&traj);
                        if !messages.is_empty() {
                            eprintln!(
                                "[ra::a2a] resumed session {task_id} ({} messages from disk)",
                                messages.len()
                            );
                            s.restore_messages(messages).await;
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[ra::a2a] could not load saved session {task_id}: {e:#}; \
                             starting fresh"
                        );
                    }
                }
            }
        }
        self.sessions.insert(task_id.to_string(), s.clone());
        s
    }
}

#[async_trait]
impl RunnerHost for A2aState {
    async fn save_session(&self, session_id: &str) {
        let Some(session) = self.sessions.get(session_id).map(|r| r.clone()) else {
            return;
        };
        let store = match SessionStore::for_cwd(&self.cwd) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[ra::a2a] store: {e:#}");
                return;
            }
        };
        let messages = session.snapshot_messages().await;
        let model_name = Some(self.model_factory.default_model_id());
        let traj = crate::atif_codec::encode(session_id, model_name, &messages);
        if let Err(e) = store.save(&traj).await {
            eprintln!("[ra::a2a] save trajectory {session_id}: {e:#}");
        }
    }

    fn default_ctx_window(&self) -> u64 {
        // 200k is a safe-ish midpoint; A2A doesn't surface UsageUpdate
        // anyway, and our tiktoken estimate is approximate.
        match self.model_factory.default_model_id().to_lowercase().as_str() {
            id if id.contains("gpt-5") => 1_000_000,
            id if id.contains("gemini") => 1_000_000,
            id if id.contains("claude") => 200_000,
            _ => 128_000,
        }
    }

    fn list_models_for_display(&self) -> Vec<(String, String)> {
        // Slash command UX: `/models` would surface this but A2A users
        // typically don't type slash commands, so we stay terse.
        vec![(self.model_factory.default_model_id(), "Default".into())]
    }
}

/// The A2A executor. Each incoming `SendMessage` / `SendStreamingMessage`
/// routes here.
struct RaExecutor {
    state: Arc<A2aState>,
}

impl AgentExecutor for RaExecutor {
    fn execute(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let task_id = ctx.task_id.clone();
        let context_id = ctx.context_id.clone();
        let user_text = collect_text(&ctx.message);
        let state = self.state.clone();

        let (tx, rx) = mpsc::channel::<Result<StreamResponse, A2AError>>(64);

        // Emit a Working status as soon as we accept the task, then
        // run the prompt in a background task so the stream returns
        // immediately.
        let working = StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
            task_id: task_id.clone(),
            context_id: context_id.clone(),
            status: TaskStatus {
                state: TaskState::Working,
                message: None,
                timestamp: Some(Utc::now()),
            },
            metadata: None,
        });
        let tx_for_working = tx.clone();
        tokio::spawn(async move {
            let _ = tx_for_working.send(Ok(working)).await;
        });

        let task_id_for_run = task_id.clone();
        let context_id_for_run = context_id.clone();
        tokio::spawn(async move {
            let session = state.get_or_create(&task_id_for_run).await;
            let host: Arc<dyn RunnerHost> = state.clone();
            let runner = SessionRunner::new(session, task_id_for_run.to_string(), host)
                .with_prompt_templates(state.prompt_templates.clone());

            let mut accumulated = String::new();
            let outcome = runner
                .run_input(user_text, |ev| match ev {
                    RunnerEvent::TextDelta(s) | RunnerEvent::ThinkingDelta(s) => {
                        accumulated.push_str(&s);
                    }
                    _ => {}
                })
                .await;

            // A2A's wire model is task-grained, not chunk-grained: many
            // clients prefer a single final Task object. We therefore
            // assemble one consolidated agent message at end-of-turn
            // rather than streaming each TextDelta individually. Future
            // work could send TaskStatusUpdate events with partial
            // messages for true streaming UX.
            let final_state = match outcome {
                RunOutcome::Completed => TaskState::Completed,
                RunOutcome::Cancelled => TaskState::Canceled,
                RunOutcome::Failed(_) => TaskState::Failed,
            };
            let final_task = StreamResponse::Task(Task {
                id: task_id_for_run.clone(),
                context_id: context_id_for_run.clone(),
                status: TaskStatus {
                    state: final_state,
                    message: Some(Message {
                        role: Role::Agent,
                        message_id: format!("msg_{}", Ulid::new()),
                        task_id: Some(task_id_for_run),
                        context_id: Some(context_id_for_run),
                        parts: vec![Part::text(accumulated)],
                        metadata: None,
                        extensions: None,
                        reference_task_ids: None,
                    }),
                    timestamp: Some(Utc::now()),
                },
                artifacts: None,
                history: None,
                metadata: None,
            });
            let _ = tx.send(Ok(final_task)).await;
        });

        Box::pin(ReceiverStream::new(rx))
    }

    fn cancel(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let task_id = ctx.task_id.clone();
        let context_id = ctx.context_id.clone();
        let state = self.state.clone();

        Box::pin(stream::once(async move {
            // Cancel the live Ra session, if any.
            if let Some(session) = state.sessions.get(&task_id.to_string()).map(|r| r.clone()) {
                session.cancel().await;
            }
            Ok(StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                task_id,
                context_id,
                status: TaskStatus {
                    state: TaskState::Canceled,
                    message: None,
                    timestamp: Some(Utc::now()),
                },
                metadata: None,
            }))
        }))
    }
}

fn collect_text(msg: &Option<Message>) -> String {
    let Some(m) = msg else { return String::new() };
    let mut out = String::new();
    for p in &m.parts {
        if let PartContent::Text(t) = &p.content {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(t);
        }
    }
    out
}

// --- auth helpers ----------------------------------------------------------

async fn bearer_middleware(
    expected: Arc<String>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> std::result::Result<axum::response::Response, axum::http::StatusCode> {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = header.strip_prefix("Bearer ").unwrap_or("").trim();
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}

fn check_grpc_bearer(
    expected: &Option<String>,
    req: tonic::Request<()>,
) -> std::result::Result<tonic::Request<()>, tonic::Status> {
    let Some(expected) = expected else {
        return Ok(req);
    };
    let header = req
        .metadata()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = header.strip_prefix("Bearer ").unwrap_or("").trim();
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return Err(tonic::Status::unauthenticated(
            "missing or invalid bearer token",
        ));
    }
    Ok(req)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn build_card(http_port: u16, grpc_port: u16, require_bearer: bool) -> AgentCard {
    AgentCard {
        name: "Ra".to_string(),
        description: "Rust-native agent. ACP-native, A2A-compatible. Speaks bash and read tools.".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        provider: Some(AgentProvider {
            organization: "Ra".to_string(),
            url: "https://github.com/trotsky1997/ra".to_string(),
        }),
        capabilities: AgentCapabilities {
            streaming: Some(true),
            push_notifications: Some(false),
            extensions: None,
            extended_agent_card: None,
        },
        skills: vec![
            AgentSkill {
                id: "prompt".to_string(),
                name: "Free-form prompt".to_string(),
                description: "Send a natural-language prompt; Ra will plan, call read/bash tools as needed, and reply.".to_string(),
                tags: vec!["chat".into(), "code".into()],
                examples: Some(vec!["What's the kernel version?".into()]),
                input_modes: None,
                output_modes: None,
                security_requirements: None,
            },
            AgentSkill {
                id: "bash".to_string(),
                name: "Run shell command".to_string(),
                description: "Tool: ask Ra to run a shell command in a sandboxed terminal.".to_string(),
                tags: vec!["execute".into(), "shell".into()],
                examples: Some(vec!["Use bash to print uname".into()]),
                input_modes: None,
                output_modes: None,
                security_requirements: None,
            },
            AgentSkill {
                id: "read".to_string(),
                name: "Read file".to_string(),
                description: "Tool: ask Ra to read a file from the filesystem.".to_string(),
                tags: vec!["fs".into(), "read".into()],
                examples: Some(vec!["Read Cargo.toml".into()]),
                input_modes: None,
                output_modes: None,
                security_requirements: None,
            },
        ],
        default_input_modes: vec!["text/plain".into()],
        default_output_modes: vec!["text/plain".into()],
        supported_interfaces: vec![
            AgentInterface::new(
                format!("http://localhost:{http_port}/jsonrpc"),
                TRANSPORT_PROTOCOL_JSONRPC,
            ),
            AgentInterface::new(
                format!("http://localhost:{http_port}/rest"),
                TRANSPORT_PROTOCOL_HTTP_JSON,
            ),
            AgentInterface::new(
                format!("http://localhost:{grpc_port}"),
                TRANSPORT_PROTOCOL_GRPC,
            ),
        ],
        security_schemes: if require_bearer {
            let mut schemes = std::collections::HashMap::new();
            schemes.insert(
                "bearer".to_string(),
                SecurityScheme::HttpAuth(HttpAuthSecurityScheme {
                    scheme: "bearer".to_string(),
                    description: Some(
                        "Send `Authorization: Bearer <token>` on every request \
                         (token expected in the env var configured at server start)."
                            .to_string(),
                    ),
                    bearer_format: Some("opaque".to_string()),
                }),
            );
            Some(schemes)
        } else {
            None
        },
        security_requirements: if require_bearer {
            let mut req: SecurityRequirement = std::collections::HashMap::new();
            req.insert("bearer".to_string(), vec![]);
            Some(vec![req])
        } else {
            None
        },
        documentation_url: None,
        icon_url: None,
        signatures: None,
    }
}

/// Run both HTTP and gRPC servers concurrently. Returns when either exits.
pub async fn run(
    model: Arc<dyn Model>,
    model_factory: Arc<dyn ModelFactory>,
    http_port: u16,
    grpc_port: u16,
    extra_tools: Vec<Arc<dyn Tool>>,
    system_prompt: Option<String>,
    prompt_templates: Arc<std::collections::HashMap<String, String>>,
    hooks: Option<Arc<crate::hooks::HookEngine>>,
    bearer_token: Option<String>,
) -> Result<()> {
    nemo_obs::init();

    let state = Arc::new(A2aState::new(
        model,
        model_factory,
        extra_tools,
        system_prompt,
        prompt_templates,
        hooks,
    ));
    let executor = RaExecutor { state: state.clone() };
    let handler = Arc::new(DefaultRequestHandler::new(executor, InMemoryTaskStore::new()));

    let require_bearer = bearer_token.is_some();
    let card = build_card(http_port, grpc_port, require_bearer);
    let card_producer = Arc::new(StaticAgentCard::new(card));

    // Public router: agent-card endpoint stays unauthenticated so
    // discovery still works.
    let public = agent_card_router(card_producer);

    // Protected routers: gated by Bearer middleware when configured.
    let mut protected = axum::Router::new()
        .nest("/jsonrpc", jsonrpc_router(handler.clone()))
        .nest("/rest", rest_router(handler.clone()));
    if let Some(tok) = bearer_token.clone() {
        let expected = Arc::new(tok);
        protected = protected.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let expected = expected.clone();
                async move { bearer_middleware(expected, req, next).await }
            },
        ));
    }

    let app = public.merge(protected);

    let grpc_service = A2aServiceServer::new(GrpcHandler::new(handler));
    let grpc_token = bearer_token.clone();
    let grpc_service = tonic::service::interceptor::InterceptedService::new(
        grpc_service,
        move |req: tonic::Request<()>| -> std::result::Result<tonic::Request<()>, tonic::Status> {
            check_grpc_bearer(&grpc_token, req)
        },
    );

    let http_addr = format!("0.0.0.0:{http_port}");
    let grpc_addr = format!("0.0.0.0:{grpc_port}");
    let http_listener = TcpListener::bind(&http_addr).await
        .with_context(|| format!("bind {http_addr}"))?;
    let grpc_listener = TcpListener::bind(&grpc_addr).await
        .with_context(|| format!("bind {grpc_addr}"))?;

    eprintln!("[ra::a2a] agent card:  http://localhost:{http_port}/.well-known/agent-card.json");
    eprintln!("[ra::a2a] JSON-RPC:    http://localhost:{http_port}/jsonrpc");
    eprintln!("[ra::a2a] REST:        http://localhost:{http_port}/rest");
    eprintln!("[ra::a2a] gRPC:        http://localhost:{grpc_port}");
    if require_bearer {
        eprintln!("[ra::a2a] auth: Bearer required on /jsonrpc, /rest, gRPC");
    }

    tokio::select! {
        result = axum::serve(http_listener, app).into_future() => {
            result.context("HTTP server")?;
        }
        result = TonicServer::builder()
            .add_service(grpc_service)
            .serve_with_incoming(TcpListenerStream::new(grpc_listener)) => {
            result.context("gRPC server")?;
        }
    }
    Ok(())
}
