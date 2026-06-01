//! Round-trip a Session through ATIF on disk and confirm a resumed
//! Session keeps the original conversation context.
//!
//! Steps:
//!   1. Spin up a fresh Session driven by a `ScriptedModel`. Prompt it
//!      so the message log holds [User, Assistant, ToolResult, ...].
//!   2. Encode to ATIF, save via `SessionStore`.
//!   3. Make a NEW Session, load the trajectory back, hand it a model
//!      that ASSERTS the history it sees on the next turn includes the
//!      original user message and tool result.
//!   4. Run another prompt and check the assertion never tripped.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use ra::{
    atif_codec,
    model::{Message, Model, ModelChunk, StopReason, ToolSpec},
    store::SessionStore,
    tool_ctx::ToolCtx,
    Session, Tool, ToolCall,
};
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use tempfile::TempDir;

// -- A scripted model with extra introspection -----------------------------

struct ScriptedModel {
    turns: Mutex<Vec<Vec<ModelChunk>>>,
    /// Snapshot of the message history seen at each call (in order).
    seen: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl ScriptedModel {
    fn new(turns: Vec<Vec<ModelChunk>>) -> (Self, Arc<Mutex<Vec<Vec<Message>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                turns: Mutex::new(turns),
                seen: seen.clone(),
            },
            seen,
        )
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
                vec![ModelChunk::End {
                    stop_reason: StopReason::EndTurn,
                }]
            } else {
                q.remove(0)
            }
        };
        Ok(stream::iter(chunks).boxed())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct EchoParams {
    text: String,
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echo back the input"
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(EchoParams)).unwrap()
    }
    async fn execute(
        &self,
        _id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> anyhow::Result<String> {
        let p: EchoParams = serde_json::from_value(input)?;
        Ok(format!("echoed: {}", p.text))
    }
}

#[tokio::test]
async fn resume_preserves_message_log_through_disk() {
    // Direct the on-disk store at a private temp dir so the test never
    // touches the user's $RA_HOME.
    let tmp = TempDir::new().expect("tempdir");
    // SAFETY: tests are single-threaded with respect to env in this process.
    unsafe {
        std::env::set_var("RA_HOME", tmp.path());
    }

    // ---- Phase 1: original session, ends in a tool call + result ----
    let turn1 = vec![
        ModelChunk::TextDelta("calling echo".into()),
        ModelChunk::ToolCall(ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            input: serde_json::json!({ "text": "from-original" }),
        }),
        ModelChunk::End {
            stop_reason: StopReason::ToolUse,
        },
    ];
    let turn2 = vec![
        ModelChunk::TextDelta("ok done".into()),
        ModelChunk::End {
            stop_reason: StopReason::EndTurn,
        },
    ];
    let (model_a, _seen_a) = ScriptedModel::new(vec![turn1, turn2]);
    let session_a = Arc::new(Session::new(Arc::new(model_a), vec![Arc::new(EchoTool)]));
    session_a
        .prompt("kick off original".to_string())
        .await
        .expect("prompt");
    let original_messages = session_a.snapshot_messages().await;
    assert!(
        original_messages.len() >= 4,
        "expected at least User/Assistant/ToolResult/Assistant"
    );

    // ---- Persist ----
    let cwd = std::env::current_dir().expect("cwd");
    let store = SessionStore::for_cwd(&cwd).expect("store");
    let session_id = "01TESTRESUME0000000000";
    let traj = atif_codec::encode(session_id, Some("test-model".into()), &original_messages);
    let saved_path = store.save(&traj).await.expect("save");
    assert!(
        saved_path.exists(),
        "trajectory file must be on disk: {saved_path:?}"
    );

    // Drop the original Session — only disk remains.
    drop(session_a);

    // ---- Phase 2: fresh Session, hydrated from disk ----
    let (model_b, seen_b) = ScriptedModel::new(vec![vec![
        ModelChunk::TextDelta("continuing...".into()),
        ModelChunk::End {
            stop_reason: StopReason::EndTurn,
        },
    ]]);
    let session_b = Arc::new(Session::new(Arc::new(model_b), vec![Arc::new(EchoTool)]));

    // Reload from disk and seed the new Session.
    let traj_back = store.load(session_id).await.expect("load");
    let restored = atif_codec::decode(&traj_back);
    assert_eq!(
        restored.len(),
        original_messages.len(),
        "decoded message count must match what we saved"
    );
    session_b.restore_messages(restored).await;

    // ---- Continue with a new prompt ----
    session_b
        .prompt("follow up".to_string())
        .await
        .expect("prompt");

    // The model must have seen, on its single call this phase, the FULL
    // history: every original message plus the new user turn.
    let seen = seen_b.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "ScriptedModel should be called exactly once");
    let history_seen_by_model = &seen[0];

    // Newest message is the just-submitted user prompt.
    assert!(
        matches!(
            history_seen_by_model.last(),
            Some(Message::User { content }) if content == "follow up"
        ),
        "tail must be the new user prompt"
    );

    // The original user message must still be there.
    let saw_original_user = history_seen_by_model
        .iter()
        .any(|m| matches!(m, Message::User { content } if content == "kick off original"));
    assert!(
        saw_original_user,
        "resumed session must replay the original user prompt"
    );

    // The original ToolResult must still be there.
    let saw_original_tool_result = history_seen_by_model.iter().any(
        |m| matches!(m, Message::ToolResult(r) if r.content.contains("echoed: from-original")),
    );
    assert!(
        saw_original_tool_result,
        "resumed session must replay the original tool result"
    );

    // Total length: original messages + 1 new user turn.
    assert_eq!(
        history_seen_by_model.len(),
        original_messages.len() + 1,
        "model should see the saved log plus exactly one new user message"
    );
}
