use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use ra::{
    model::{Message, Model, ModelChunk, StopReason, ToolSpec},
    session_runner::{RunOutcome, RunnerHost, SessionRunner},
    Session,
};

struct ScriptedModel {
    seen: Arc<Mutex<Vec<Vec<Message>>>>,
}

#[async_trait]
impl Model for ScriptedModel {
    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[ToolSpec],
    ) -> anyhow::Result<BoxStream<'static, ModelChunk>> {
        self.seen.lock().unwrap().push(messages.to_vec());
        Ok(stream::iter(vec![ModelChunk::End {
            stop_reason: StopReason::EndTurn,
        }])
        .boxed())
    }
}

struct NullHost;

#[async_trait]
impl RunnerHost for NullHost {
    async fn save_session(&self, _session_id: &str) {}

    fn default_ctx_window(&self) -> u64 {
        200_000
    }

    fn list_models_for_display(&self) -> Vec<(String, String)> {
        vec![]
    }
}

#[tokio::test]
async fn skill_slash_template_with_args_expands_before_model() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel { seen: seen.clone() };
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    let mut templates = HashMap::new();
    templates.insert(
        "deploy".to_string(),
        "Deploy using the project release checklist.".to_string(),
    );

    let runner = SessionRunner::new(session, "test-session".into(), Arc::new(NullHost))
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner
        .run_input("/deploy staging".to_string(), |_| {})
        .await;
    assert_eq!(outcome, RunOutcome::Completed);

    let calls = seen.lock().unwrap().clone();
    let user_msg = calls[0].iter().find(|m| matches!(m, Message::User { .. }));
    assert!(
        matches!(user_msg, Some(Message::User { content }) if content.contains("release checklist") && content.contains("staging")),
        "skill body and args must be sent to the model, got: {user_msg:?}"
    );
}
