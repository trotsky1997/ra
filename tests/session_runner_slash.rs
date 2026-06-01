use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use ra::{
    config::{Hook, HooksSection},
    events::ToolCall,
    model::{Message, Model, ModelChunk, StopReason, ToolSpec},
    session_runner::{RunOutcome, RunnerHost, SessionRunner},
    skills::{SkillAgentMode, SkillRuntimeOptions, SlashTemplate},
    tool_ctx::ToolCtx,
    Session, Tool,
};
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;

struct ScriptedModel {
    seen: Arc<Mutex<Vec<Vec<Message>>>>,
    seen_tools: Arc<Mutex<Vec<Vec<String>>>>,
    chunks: Vec<ModelChunk>,
}

#[async_trait]
impl Model for ScriptedModel {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> anyhow::Result<BoxStream<'static, ModelChunk>> {
        self.seen.lock().unwrap().push(messages.to_vec());
        self.seen_tools
            .lock()
            .unwrap()
            .push(tools.iter().map(|t| t.name.clone()).collect());
        Ok(stream::iter(self.chunks.clone()).boxed())
    }
}

impl ScriptedModel {
    fn end_turn(seen: Arc<Mutex<Vec<Vec<Message>>>>) -> Self {
        Self {
            seen,
            seen_tools: Arc::new(Mutex::new(Vec::new())),
            chunks: vec![ModelChunk::End {
                stop_reason: StopReason::EndTurn,
            }],
        }
    }
}

struct NamedModel {
    name: &'static str,
    seen: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Model for NamedModel {
    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSpec],
    ) -> anyhow::Result<BoxStream<'static, ModelChunk>> {
        self.seen.lock().unwrap().push(self.name);
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

struct ModelHost {
    override_model: Arc<dyn Model>,
}

#[async_trait]
impl RunnerHost for ModelHost {
    async fn save_session(&self, _session_id: &str) {}

    fn default_ctx_window(&self) -> u64 {
        200_000
    }

    fn list_models_for_display(&self) -> Vec<(String, String)> {
        vec![]
    }

    fn build_model_for_id(&self, model_id: &str) -> Option<Arc<dyn Model>> {
        (model_id == "review-model").then(|| self.override_model.clone())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct EchoParams {
    text: String,
}

#[derive(Clone)]
struct EchoTool {
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "test echo tool"
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(EchoParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> anyhow::Result<String> {
        let params: EchoParams = serde_json::from_value(input)?;
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{}", self.name, params.text));
        Ok(params.text)
    }
}

fn skill_template(body: &str, runtime: SkillRuntimeOptions) -> SlashTemplate {
    SlashTemplate::skill(body.to_string(), Vec::new(), runtime)
}

#[tokio::test]
async fn skill_slash_template_with_args_expands_before_model() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel::end_turn(seen.clone());
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    let mut templates = HashMap::new();
    templates.insert(
        "deploy".to_string(),
        SlashTemplate::skill(
            "Deploy using the project release checklist.".to_string(),
            Vec::new(),
            SkillRuntimeOptions::default(),
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
    let model = ScriptedModel::end_turn(seen.clone());
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    let mut templates = HashMap::new();
    templates.insert(
        "migrate".to_string(),
        SlashTemplate::skill(
            "Migrate $component from $0 to $ARGUMENTS[1]. Raw: $ARGUMENTS.".to_string(),
            vec!["component".to_string()],
            SkillRuntimeOptions::default(),
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
    let model = ScriptedModel::end_turn(seen.clone());
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

#[tokio::test]
async fn skill_slash_template_renders_dynamic_shell_context() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel::end_turn(seen.clone());
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    let mut templates = HashMap::new();
    templates.insert(
        "inspect".to_string(),
        skill_template(
            "Inline: !`printf inline-context`\nBlock:\n```!\nprintf fenced-context\n```",
            SkillRuntimeOptions::default(),
        ),
    );

    let runner = SessionRunner::new(session, "test-session".into(), Arc::new(NullHost))
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner.run_input("/inspect".to_string(), |_| {}).await;
    assert_eq!(outcome, RunOutcome::Completed);

    let calls = seen.lock().unwrap().clone();
    let user_msg = calls[0].iter().find(|m| matches!(m, Message::User { .. }));
    assert!(
        matches!(user_msg, Some(Message::User { content }) if content.contains("Inline: inline-context") && content.contains("fenced-context")),
        "dynamic shell context should be rendered, got: {user_msg:?}"
    );
}

#[tokio::test]
async fn skill_slash_template_marks_failed_dynamic_shell_context() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel::end_turn(seen.clone());
    let session = Arc::new(Session::new(Arc::new(model), vec![]));

    let mut templates = HashMap::new();
    templates.insert(
        "inspect".to_string(),
        skill_template("Before !`exit 7` after", SkillRuntimeOptions::default()),
    );

    let runner = SessionRunner::new(session, "test-session".into(), Arc::new(NullHost))
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner.run_input("/inspect".to_string(), |_| {}).await;
    assert_eq!(outcome, RunOutcome::Completed);

    let calls = seen.lock().unwrap().clone();
    let user_msg = calls[0].iter().find(|m| matches!(m, Message::User { .. }));
    assert!(
        matches!(user_msg, Some(Message::User { content }) if content.contains("shell context command failed") && content.contains("exited with 7")),
        "failed shell context should be visible, got: {user_msg:?}"
    );
}

#[tokio::test]
async fn skill_scoped_allowed_tools_limit_model_specs() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_tools = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel {
        seen: seen.clone(),
        seen_tools: seen_tools.clone(),
        chunks: vec![ModelChunk::End {
            stop_reason: StopReason::EndTurn,
        }],
    };
    let session = Arc::new(Session::new(
        Arc::new(model),
        vec![
            Arc::new(EchoTool {
                name: "read",
                log: Arc::new(Mutex::new(Vec::new())),
            }),
            Arc::new(EchoTool {
                name: "bash",
                log: Arc::new(Mutex::new(Vec::new())),
            }),
        ],
    ));

    let mut templates = HashMap::new();
    templates.insert(
        "review".to_string(),
        skill_template(
            "Review.",
            SkillRuntimeOptions {
                allowed_tools: vec!["read".to_string()],
                ..SkillRuntimeOptions::default()
            },
        ),
    );

    let runner = SessionRunner::new(session, "test-session".into(), Arc::new(NullHost))
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner.run_input("/review".to_string(), |_| {}).await;
    assert_eq!(outcome, RunOutcome::Completed);

    assert_eq!(seen_tools.lock().unwrap()[0], vec!["read".to_string()]);
}

#[tokio::test]
async fn skill_scoped_disallowed_tools_block_execution() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel {
        seen,
        seen_tools: Arc::new(Mutex::new(Vec::new())),
        chunks: vec![
            ModelChunk::ToolCall(ToolCall {
                id: "call-1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({ "text": "blocked" }),
            }),
            ModelChunk::End {
                stop_reason: StopReason::EndTurn,
            },
        ],
    };
    let session = Arc::new(Session::new(
        Arc::new(model),
        vec![Arc::new(EchoTool {
            name: "bash",
            log: log.clone(),
        })],
    ));

    let mut templates = HashMap::new();
    templates.insert(
        "audit".to_string(),
        skill_template(
            "Audit.",
            SkillRuntimeOptions {
                disallowed_tools: vec!["bash".to_string()],
                ..SkillRuntimeOptions::default()
            },
        ),
    );

    let mut events = Vec::new();
    let runner = SessionRunner::new(session, "test-session".into(), Arc::new(NullHost))
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner
        .run_input("/audit".to_string(), |ev| events.push(ev))
        .await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(log.lock().unwrap().is_empty(), "denied tool must not run");
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            ra::session_runner::RunnerEvent::ToolCallEnd {
                is_error: true,
                content,
                ..
            } if content.contains("denied by skill-scoped tool policy")
        )),
        "denied tool should produce an error ToolCallEnd: {events:?}"
    );
}

#[tokio::test]
async fn skill_scoped_hooks_are_temporary() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel {
        seen,
        seen_tools: Arc::new(Mutex::new(Vec::new())),
        chunks: vec![
            ModelChunk::ToolCall(ToolCall {
                id: "call-1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({ "text": "maybe" }),
            }),
            ModelChunk::End {
                stop_reason: StopReason::EndTurn,
            },
        ],
    };
    let session = Arc::new(Session::new(
        Arc::new(model),
        vec![Arc::new(EchoTool {
            name: "bash",
            log: log.clone(),
        })],
    ));

    let mut hooks = HooksSection::default();
    hooks.pre_tool_use.push(Hook {
        matcher: "bash".to_string(),
        command: "printf '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"skill hook\"}}'".to_string(),
        timeout: 5.0,
        run_async: false,
    });
    let mut templates = HashMap::new();
    templates.insert(
        "guarded".to_string(),
        skill_template(
            "Guarded.",
            SkillRuntimeOptions {
                hooks,
                ..SkillRuntimeOptions::default()
            },
        ),
    );

    let mut events = Vec::new();
    let runner = SessionRunner::new(session, "test-session".into(), Arc::new(NullHost))
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner
        .run_input("/guarded".to_string(), |ev| events.push(ev))
        .await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(
        log.lock().unwrap().is_empty(),
        "hook-denied tool must not run"
    );
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            ra::session_runner::RunnerEvent::ToolCallEnd {
                is_error: true,
                content,
                ..
            } if content.contains("skill hook")
        )),
        "skill hook denial should surface in tool result: {events:?}"
    );
}

#[tokio::test]
async fn skill_scoped_model_override_is_temporary() {
    let seen_models = Arc::new(Mutex::new(Vec::new()));
    let default_model = Arc::new(NamedModel {
        name: "default",
        seen: seen_models.clone(),
    });
    let override_model = Arc::new(NamedModel {
        name: "review",
        seen: seen_models.clone(),
    });
    let session = Arc::new(Session::new(default_model, vec![]));

    let mut templates = HashMap::new();
    templates.insert(
        "review".to_string(),
        skill_template(
            "Review.",
            SkillRuntimeOptions {
                model: Some("review-model".to_string()),
                ..SkillRuntimeOptions::default()
            },
        ),
    );
    let host = Arc::new(ModelHost { override_model });
    let runner = SessionRunner::new(session.clone(), "test-session".into(), host)
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner.run_input("/review".to_string(), |_| {}).await;
    assert_eq!(outcome, RunOutcome::Completed);
    session.prompt("plain".to_string()).await.unwrap();

    assert_eq!(&*seen_models.lock().unwrap(), &["review", "default"]);
}

#[tokio::test]
async fn forked_skill_does_not_keep_internal_user_prompt() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel {
        seen: seen.clone(),
        seen_tools: Arc::new(Mutex::new(Vec::new())),
        chunks: vec![
            ModelChunk::TextDelta("fork result".to_string()),
            ModelChunk::End {
                stop_reason: StopReason::EndTurn,
            },
        ],
    };
    let session = Arc::new(Session::new(Arc::new(model), vec![]));
    session
        .prompt("parent prompt".to_string())
        .await
        .expect("seed parent");

    let mut templates = HashMap::new();
    templates.insert(
        "fork-review".to_string(),
        skill_template(
            "Internal fork prompt.",
            SkillRuntimeOptions {
                agent: Some(SkillAgentMode::Fork),
                ..SkillRuntimeOptions::default()
            },
        ),
    );
    let runner = SessionRunner::new(session.clone(), "test-session".into(), Arc::new(NullHost))
        .with_prompt_templates(Arc::new(templates));

    let outcome = runner.run_input("/fork-review".to_string(), |_| {}).await;
    assert_eq!(outcome, RunOutcome::Completed);

    let messages = session.snapshot_messages().await;
    assert!(
        !messages.iter().any(|msg| matches!(
            msg,
            Message::User { content } if content.contains("Internal fork prompt")
        )),
        "fork prompt should not remain in parent transcript: {messages:?}"
    );
    assert!(
        messages.iter().any(|msg| matches!(
            msg,
            Message::Assistant { content, .. } if content.contains("fork result")
        )),
        "fork result should be appended to parent transcript: {messages:?}"
    );
}
