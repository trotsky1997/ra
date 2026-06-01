//! Integration tests for the TUI session browser and slash-command submit path.
//!
//! These tests exercise the public surface without a real terminal:
//!
//! 1. **Session browser restore** — save a trajectory to disk, then simulate
//!    the Ctrl-R → Enter flow by calling the same store + decode path the TUI
//!    uses, and confirm the session sees the restored history.
//!
//! 2. **Slash command via SessionRunner** — confirm that `/clear` dispatched
//!    through `SessionRunner::run_input` clears the message log, matching the
//!    behaviour the TUI now gets by routing submit through the runner.
//!
//! 3. **Prompt template expansion** — confirm that a user-defined `/greet`
//!    template is expanded to its body before hitting the model, matching the
//!    ACP/A2A behaviour the TUI now shares.

#![cfg(feature = "tui")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use ra::{
    atif_codec, store::SessionStore, Session,
    model::{Message, Model, ModelChunk, StopReason, ToolSpec},
    session_runner::{RunnerHost, RunOutcome, SessionRunner},
};
use tempfile::TempDir;

// ---- Minimal scripted model -----------------------------------------------

struct ScriptedModel {
    turns: Mutex<Vec<Vec<ModelChunk>>>,
    /// Every call records the messages it received.
    seen: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl ScriptedModel {
    fn new(turns: Vec<Vec<ModelChunk>>) -> (Self, Arc<Mutex<Vec<Vec<Message>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (Self { turns: Mutex::new(turns), seen: seen.clone() }, seen)
    }
}

#[async_trait]
impl Model for ScriptedModel {
    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[ToolSpec],
    ) -> anyhow::Result<BoxStream<'static, ModelChunk>> {
        self.seen.lock().unwrap().push(messages.to_vec());
        let chunks = {
            let mut q = self.turns.lock().unwrap();
            if q.is_empty() {
                vec![ModelChunk::End { stop_reason: StopReason::EndTurn }]
            } else {
                q.remove(0)
            }
        };
        Ok(stream::iter(chunks).boxed())
    }
}

// ---- Minimal RunnerHost ---------------------------------------------------

struct NullHost;

#[async_trait]
impl RunnerHost for NullHost {
    async fn save_session(&self, _id: &str) {}
    fn default_ctx_window(&self) -> u64 { 200_000 }
    fn list_models_for_display(&self) -> Vec<(String, String)> { vec![] }
}

// ---- Helpers ---------------------------------------------------------------

fn end_turn() -> Vec<ModelChunk> {
    vec![ModelChunk::End { stop_reason: StopReason::EndTurn }]
}

// ---- Test 1: session browser restore path ---------------------------------

/// Saves a trajectory to disk, then reloads it via the same
/// `store.load → atif_codec::decode → session.restore_messages` path the TUI
/// session browser uses. Confirms the restored session replays history.
#[tokio::test]
async fn session_browser_restore_replays_history() {
    let tmp = TempDir::new().unwrap();
    unsafe { std::env::set_var("RA_HOME", tmp.path()); }

    // Phase 1: build a session with one turn of history.
    let (model_a, _) = ScriptedModel::new(vec![end_turn()]);
    let session_a = Arc::new(Session::new(Arc::new(model_a), vec![]));
    session_a.prompt("original prompt".to_string()).await.unwrap();
    let messages = session_a.snapshot_messages().await;
    assert!(!messages.is_empty());

    // Persist to disk.
    let cwd = std::env::current_dir().unwrap();
    let store = SessionStore::for_cwd(&cwd).unwrap();
    let session_id = "01TUIBROWSER0000000000";
    let traj = atif_codec::encode(session_id, None, &messages);
    store.save(&traj).await.unwrap();

    // Phase 2: simulate the TUI browser restore — load, decode, restore.
    let traj_back = store.load(session_id).await.unwrap();
    let restored = atif_codec::decode(&traj_back);
    assert_eq!(restored.len(), messages.len(), "decode must round-trip");

    let (model_b, seen_b) = ScriptedModel::new(vec![end_turn()]);
    let session_b = Arc::new(Session::new(Arc::new(model_b), vec![]));
    session_b.restore_messages(restored).await;

    // Continue with a new prompt — model must see the full history.
    session_b.prompt("follow up".to_string()).await.unwrap();

    let seen = seen_b.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let history = &seen[0];

    assert!(
        history.iter().any(|m| matches!(m, Message::User { content } if content == "original prompt")),
        "restored history must include the original user message"
    );
    assert!(
        matches!(history.last(), Some(Message::User { content }) if content == "follow up"),
        "last message must be the new prompt"
    );
}

// ---- Test 2: /clear via SessionRunner -------------------------------------

/// Confirms that `/clear` dispatched through `SessionRunner::run_input`
/// clears the session message log — the same path the TUI now uses.
#[tokio::test]
async fn slash_clear_via_runner_clears_history() {
    let (model, _) = ScriptedModel::new(vec![end_turn(), end_turn()]);
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    // Seed some history.
    session.prompt("first message".to_string()).await.unwrap();
    let before = session.snapshot_messages().await;
    assert!(!before.is_empty(), "should have messages before clear");

    // Run /clear through the runner (same path as TUI submit).
    let host = Arc::new(NullHost);
    let runner = SessionRunner::new(session.clone(), "test-session".into(), host);
    let outcome = runner.run_input("/clear".to_string(), |_| {}).await;
    assert_eq!(outcome, RunOutcome::Completed);

    let after = session.snapshot_messages().await;
    assert!(after.is_empty(), "/clear must empty the message log");
}

// ---- Test 3: prompt template expansion via SessionRunner ------------------

/// Confirms that `/greet` expands to its template body before hitting the
/// model — the same expansion the TUI now gets by routing submit through
/// SessionRunner.
#[tokio::test]
async fn prompt_template_expanded_before_model() {
    let (model, seen) = ScriptedModel::new(vec![end_turn()]);
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    let mut templates = HashMap::new();
    templates.insert("greet".to_string(), "Say hello to the user warmly.".to_string());

    let host = Arc::new(NullHost);
    let runner = SessionRunner::new(session.clone(), "test-session".into(), host)
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner.run_input("/greet".to_string(), |_| {}).await;
    assert_eq!(outcome, RunOutcome::Completed);

    let calls = seen.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "model should be called exactly once");
    // The model must see the expanded template body, not the raw "/greet".
    let user_msg = calls[0].iter().find(|m| matches!(m, Message::User { .. }));
    assert!(
        matches!(user_msg, Some(Message::User { content }) if content.contains("Say hello")),
        "model must receive the expanded template body, got: {user_msg:?}"
    );
}

// ---- Test 4: prompt template with args ------------------------------------

/// Confirms that `/greet Alice` appends the args to the template body.
#[tokio::test]
async fn prompt_template_with_args_appended() {
    let (model, seen) = ScriptedModel::new(vec![end_turn()]);
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    let mut templates = HashMap::new();
    templates.insert("greet".to_string(), "Say hello to the user warmly.".to_string());

    let host = Arc::new(NullHost);
    let runner = SessionRunner::new(session.clone(), "test-session".into(), host)
        .with_prompt_templates(Arc::new(templates));

    runner.run_input("/greet Alice".to_string(), |_| {}).await;

    let calls = seen.lock().unwrap().clone();
    let user_msg = calls[0].iter().find(|m| matches!(m, Message::User { .. }));
    assert!(
        matches!(user_msg, Some(Message::User { content }) if content.contains("Alice")),
        "args must be appended to the template body"
    );
}
