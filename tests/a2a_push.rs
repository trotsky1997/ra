//! Integration tests for A2A push notification support.
//!
//! Covers:
//!   1. Agent card advertises push_notifications = true.
//!   2. Push config CRUD via InMemoryPushConfigStore directly.
//!   3. A task that completes sends a push notification to a registered webhook.
//!   4. Clients that never register a push config still receive the normal
//!      streaming response (non-push path is unaffected).

use std::sync::{Arc, Mutex};

use a2a::{
    AgentCapabilities, AgentCard, Message, Part, Role, SendMessageConfiguration,
    SendMessageRequest, StreamResponse, TaskPushNotificationConfig, TaskState,
};
use a2a_client::A2AClient;
use a2a_client::jsonrpc::JsonRpcTransport;
use a2a_server::{
    AgentExecutor, DefaultRequestHandler, ExecutorContext, HttpPushSender, InMemoryPushConfigStore,
    InMemoryTaskStore, PushConfigStore, StaticAgentCard,
    agent_card::agent_card_router, jsonrpc::jsonrpc_router,
};
use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use futures::stream::{self, BoxStream};
use tokio::net::TcpListener;
use ulid::Ulid;

// ---------------------------------------------------------------------------
// Minimal scripted executor
// ---------------------------------------------------------------------------

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, a2a::A2AError>> {
        use a2a::{Task, TaskStatus, TaskStatusUpdateEvent, TaskState as TS};
        use chrono::Utc;

        let task_id = ctx.task_id.clone();
        let context_id = ctx.context_id.clone();

        let working = StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
            task_id: task_id.clone(),
            context_id: context_id.clone(),
            status: TaskStatus {
                state: TS::Working,
                message: None,
                timestamp: Some(Utc::now()),
            },
            metadata: None,
        });

        let done = StreamResponse::Task(Task {
            id: task_id.clone(),
            context_id: context_id.clone(),
            status: TaskStatus {
                state: TS::Completed,
                message: Some(Message {
                    role: Role::Agent,
                    message_id: format!("msg_{}", Ulid::new()),
                    task_id: Some(task_id),
                    context_id: Some(context_id),
                    parts: vec![Part::text("pong".to_string())],
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

        Box::pin(stream::iter(vec![Ok(working), Ok(done)]))
    }

    fn cancel(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, a2a::A2AError>> {
        use a2a::{TaskStatus, TaskStatusUpdateEvent, TaskState as TS};
        use chrono::Utc;
        Box::pin(stream::once(async move {
            Ok(StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                task_id: ctx.task_id,
                context_id: ctx.context_id,
                status: TaskStatus {
                    state: TS::Canceled,
                    message: None,
                    timestamp: Some(Utc::now()),
                },
                metadata: None,
            }))
        }))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_handler() -> Arc<DefaultRequestHandler> {
    Arc::new(
        DefaultRequestHandler::new(EchoExecutor, InMemoryTaskStore::new())
            .with_push_notifications(InMemoryPushConfigStore::new(), HttpPushSender::new(None)),
    )
}

fn make_request(text: &str) -> SendMessageRequest {
    SendMessageRequest {
        message: Message {
            role: Role::User,
            message_id: format!("msg_{}", Ulid::new()),
            task_id: None,
            context_id: None,
            parts: vec![Part::text(text.to_string())],
            metadata: None,
            extensions: None,
            reference_task_ids: None,
        },
        configuration: None,
        metadata: None,
        tenant: None,
    }
}

/// Spin up an axum server on a random port and return its base URL.
async fn spawn_server(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://127.0.0.1:{port}")
}

/// Build an A2AClient pointing at the root of a jsonrpc_router server.
fn make_client(base: &str) -> A2AClient<JsonRpcTransport> {
    // jsonrpc_router mounts at "/", so the endpoint is the base URL itself.
    let http = reqwest13::Client::new();
    A2AClient::new(JsonRpcTransport::new(http, format!("{base}/")))
}

/// Spin up a tiny webhook receiver. Returns (base_url, received_bodies).
async fn spawn_webhook() -> (String, Arc<Mutex<Vec<Vec<u8>>>>) {
    let received: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();

    let app = Router::new()
        .route(
            "/hook",
            axum::routing::post(
                |State(store): State<Arc<Mutex<Vec<Vec<u8>>>>>,
                 body: axum::body::Bytes| async move {
                    store.lock().unwrap().push(body.to_vec());
                    axum::http::StatusCode::OK.into_response()
                },
            ),
        )
        .with_state(received_clone);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://127.0.0.1:{port}"), received)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The agent card must advertise push_notifications = true.
#[tokio::test]
async fn test_agent_card_advertises_push_notifications() {
    let card = AgentCard {
        name: "Ra".to_string(),
        description: "test".to_string(),
        version: "0.0.0".to_string(),
        provider: None,
        capabilities: AgentCapabilities {
            streaming: Some(true),
            push_notifications: Some(true),
            extensions: None,
            extended_agent_card: None,
        },
        skills: vec![],
        default_input_modes: vec!["text/plain".into()],
        default_output_modes: vec!["text/plain".into()],
        supported_interfaces: vec![],
        security_schemes: None,
        security_requirements: None,
        documentation_url: None,
        icon_url: None,
        signatures: None,
    };
    let card_producer = Arc::new(StaticAgentCard::new(card));
    let app = agent_card_router(card_producer);
    let base = spawn_server(app).await;

    let resp = reqwest13::get(format!("{base}/.well-known/agent-card.json"))
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["capabilities"]["pushNotifications"],
        serde_json::Value::Bool(true),
        "agent card must advertise push_notifications=true"
    );
}

/// Push config CRUD: create → get → list → delete.
#[tokio::test]
async fn test_push_config_crud() {
    let store = InMemoryPushConfigStore::new();

    // create
    let saved = store
        .save(TaskPushNotificationConfig {
            task_id: "task-1".to_string(),
            url: "http://example.com/hook".to_string(),
            id: None,
            token: Some("tok".to_string()),
            authentication: None,
            tenant: None,
        })
        .await
        .unwrap();
    let config_id = saved.id.clone().expect("id must be auto-assigned");

    // get
    let got = store.get("task-1", &config_id).await.unwrap();
    assert_eq!(got.url, "http://example.com/hook");
    assert_eq!(got.token.as_deref(), Some("tok"));

    // list
    let list = store.list("task-1").await.unwrap();
    assert_eq!(list.len(), 1);

    // delete
    store.delete("task-1", &config_id).await.unwrap();
    let list = store.list("task-1").await.unwrap();
    assert_eq!(list.len(), 0);
}

/// A task that completes must POST a push notification to the registered webhook.
#[tokio::test]
async fn test_push_notification_delivered_on_task_completion() {
    let (webhook_url, received) = spawn_webhook().await;

    let handler = make_handler();
    let base = spawn_server(jsonrpc_router(handler)).await;
    let client = make_client(&base);

    // Send a message with a push config attached inline.
    let hook_url = format!("{webhook_url}/hook");
    let mut req = make_request("ping");
    req.configuration = Some(SendMessageConfiguration {
        accepted_output_modes: None,
        task_push_notification_config: Some(TaskPushNotificationConfig {
            task_id: String::new(), // server fills this in
            url: hook_url,
            id: None,
            token: None,
            authentication: None,
            tenant: None,
        }),
        history_length: None,
        return_immediately: None,
    });

    let resp = client.send_message(&req).await.unwrap();
    match resp {
        a2a::SendMessageResponse::Task(task) => {
            assert_eq!(task.status.state, TaskState::Completed);
        }
        other => panic!("expected Task response, got {other:?}"),
    }

    // Give the push sender a moment to fire (it runs in a background task).
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let bodies = received.lock().unwrap();
    assert!(
        !bodies.is_empty(),
        "webhook should have received at least one push notification"
    );

    // At least one notification should carry a completed-state event.
    // The push payload uses protojson enum format: "TASK_STATE_COMPLETED".
    let has_completed = bodies.iter().any(|b| {
        let text = String::from_utf8_lossy(b);
        text.contains("TASK_STATE_COMPLETED") || text.contains("completed")
    });
    assert!(
        has_completed,
        "at least one push event should carry completed state; got: {:?}",
        bodies.iter().map(|b| String::from_utf8_lossy(b).to_string()).collect::<Vec<_>>()
    );
}

/// Clients that never register a push config still get the normal response.
#[tokio::test]
async fn test_non_push_client_still_works() {
    let handler = make_handler();
    let base = spawn_server(jsonrpc_router(handler)).await;
    let client = make_client(&base);

    let resp = client.send_message(&make_request("hello")).await.unwrap();
    match resp {
        a2a::SendMessageResponse::Task(task) => {
            assert_eq!(task.status.state, TaskState::Completed);
        }
        other => panic!("expected Task response, got {other:?}"),
    }
}
