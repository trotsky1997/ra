use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use ra::{
    model::{Message, Model, ModelChunk, StopReason, ToolSpec},
    session_runner::{RunOutcome, RunnerHost, SessionRunner},
    skills::SlashTemplate,
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
        SlashTemplate::skill(
            "Deploy using the project release checklist.".to_string(),
            Vec::new(),
        ),
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
        matches!(user_msg, Some(Message::User { content }) if content.contains("release checklist") && content.contains("ARGUMENTS: staging")),
        "skill body and args must be sent to the model, got: {user_msg:?}"
    );
}

#[tokio::test]
async fn skill_slash_template_replaces_arguments_placeholders() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel { seen: seen.clone() };
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    let mut templates = HashMap::new();
    templates.insert(
        "migrate".to_string(),
        SlashTemplate::skill(
            "Migrate $component from $0 to $ARGUMENTS[1]. Raw: $ARGUMENTS.".to_string(),
            vec!["component".to_string()],
        ),
    );

    let runner = SessionRunner::new(session, "test-session".into(), Arc::new(NullHost))
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner
        .run_input("/migrate SearchBar React Vue".to_string(), |_| {})
        .await;
    assert_eq!(outcome, RunOutcome::Completed);

    let calls = seen.lock().unwrap().clone();
    let user_msg = calls[0].iter().find(|m| matches!(m, Message::User { .. }));
    assert!(
        matches!(user_msg, Some(Message::User { content }) if content.contains("Migrate SearchBar from SearchBar to React. Raw: SearchBar React Vue.")),
        "skill placeholders must be replaced before model call, got: {user_msg:?}"
    );
}

#[tokio::test]
async fn prompt_template_with_args_keeps_plain_append_behavior() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel { seen: seen.clone() };
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    let mut templates = HashMap::new();
    templates.insert(
        "greet".to_string(),
        SlashTemplate::prompt("Say hello.".to_string()),
    );

    let runner = SessionRunner::new(session, "test-session".into(), Arc::new(NullHost))
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner.run_input("/greet Alice".to_string(), |_| {}).await;
    assert_eq!(outcome, RunOutcome::Completed);

    let calls = seen.lock().unwrap().clone();
    let user_msg = calls[0].iter().find(|m| matches!(m, Message::User { .. }));
    assert!(
        matches!(user_msg, Some(Message::User { content }) if content == "Say hello.\n\nAlice"),
        "prompt templates should keep legacy append behavior, got: {user_msg:?}"
    );
}
