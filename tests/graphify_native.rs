//! Native Graphify support: ensure/update the agent-owned R2A graph, map
//! requirements into impact/verification context, and still expose focused
//! graph query tools through a normal Ra session.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{
    stream::{self, BoxStream},
    StreamExt,
};
use ra::{
    model::{Message, Model, ModelChunk, StopReason, ToolSpec},
    Event, Session,
};
use tempfile::TempDir;

fn write_graph(root: &std::path::Path) {
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("graphify-out")).unwrap();
    fs::write(
        root.join("graphify-out/GRAPH_REPORT.md"),
        "# Graph Report\n",
    )
    .unwrap();
    fs::write(
        root.join("graphify-out/graph.json"),
        r#"{
  "directed": true,
  "nodes": [
    {"id": "auth_service", "label": "AuthService", "source_file": "src/auth.rs", "file_type": "code", "community": 1},
    {"id": "db_pool", "label": "DatabasePool", "source_file": "src/db.rs", "file_type": "code", "community": 1},
    {"id": "auth_test", "label": "AuthService tests", "source_file": "tests/auth_test.rs", "file_type": "code", "community": 1},
    {"id": "login_doc", "label": "Login flow", "source_file": "docs/auth.md", "file_type": "document", "community": 2}
  ],
  "links": [
    {"source": "auth_service", "target": "db_pool", "relation": "uses", "confidence": "EXTRACTED"},
    {"source": "auth_test", "target": "auth_service", "relation": "tests", "confidence": "EXTRACTED"},
    {"source": "login_doc", "target": "auth_service", "relation": "documents", "confidence": "INFERRED"}
  ]
}"#,
    )
    .unwrap();
}

struct OneTurnModel {
    system_prompt_seen: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl Model for OneTurnModel {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> anyhow::Result<BoxStream<'static, ModelChunk>> {
        let system = messages
            .iter()
            .find_map(|m| match m {
                Message::User { content } if content.starts_with("[SYSTEM]\n") => {
                    Some(content.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        *self.system_prompt_seen.lock().unwrap() = Some(system);

        assert!(
            tools.iter().any(|t| t.name == "graphify_query"),
            "graphify_query should be advertised to the model"
        );
        assert!(
            tools.iter().any(|t| t.name == "graphify_ensure"),
            "graphify_ensure should be advertised to the model"
        );
        assert!(
            tools.iter().any(|t| t.name == "graphify_impact"),
            "graphify_impact should be advertised to the model"
        );

        Ok(stream::iter(vec![
            ModelChunk::ToolCall(ra::ToolCall {
                id: "ge-1".into(),
                name: "graphify_ensure".into(),
                input: serde_json::json!({
                    "phase": "intake",
                    "requirement": "tighten auth database login flow verification",
                    "refresh": false
                }),
            }),
            ModelChunk::ToolCall(ra::ToolCall {
                id: "gi-1".into(),
                name: "graphify_impact".into(),
                input: serde_json::json!({
                    "phase": "verify",
                    "requirement": "tighten auth database login flow verification",
                    "changed_files": ["src/auth.rs"],
                    "depth": 2,
                    "max_nodes": 24
                }),
            }),
            ModelChunk::End {
                stop_reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

#[tokio::test]
async fn graphify_project_adds_prompt_section_and_query_tool() {
    let tmp = TempDir::new().unwrap();
    write_graph(tmp.path());

    let project = ra::graphify::discover(&tmp.path().join("src")).unwrap();
    let workflow = ra::graphify::GraphifyWorkflow::from_project(project.clone());
    let bundle = ra::skills::ResourceBundle {
        graphify: Some(workflow.clone()),
        ..Default::default()
    };
    let prompt = bundle.build_system_prompt().unwrap();
    assert!(prompt.contains("# Graphify"));
    assert!(prompt.contains("Graphify R2A Graph Service"));
    assert!(prompt.contains("nodes: 4"));
    assert!(prompt.contains("graphify_impact"));

    let seen = Arc::new(Mutex::new(None));
    let session = Arc::new(Session::new(
        Arc::new(OneTurnModel {
            system_prompt_seen: seen.clone(),
        }),
        ra::graphify::tools_for_workflow(&workflow),
    ));
    session.set_system_prompt(prompt).await;

    let mut rx = session.subscribe();
    let collector = tokio::spawn(async move {
        let mut tool_results = Vec::new();
        while let Ok(event) = rx.recv().await {
            let terminal = matches!(event, Event::AgentEnd);
            if let Event::ToolCallEnd(result) = event {
                tool_results.push(result);
            }
            if terminal {
                break;
            }
        }
        tool_results
    });

    session.prompt("use the graph").await.unwrap();
    let results = collector.await.unwrap();
    assert_eq!(results.len(), 2);
    assert!(!results[0].is_error, "ensure result should be successful");
    assert!(!results[1].is_error, "impact result should be successful");
    assert!(results[0].content.contains("status: ready"));
    assert!(results[1].content.contains("Graphify R2A impact"));
    assert!(results[1].content.contains("AuthService"));
    assert!(results[1].content.contains("tests/auth_test.rs"));
    assert!(results[1].content.contains("Traceability for report"));

    let system = seen.lock().unwrap().clone().unwrap();
    assert!(system.contains("Graphify"));
    assert!(system.contains("requirement -> SWE -> artifact"));
    assert!(system.contains("graphify_path"));
}

#[test]
fn graphify_workflow_missing_graph_keeps_agent_owned_build_path() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();

    let workflow =
        ra::graphify::workflow_from_config(&ra::config::GraphifySection::default(), tmp.path())
            .unwrap();
    assert_eq!(
        workflow.status.kind,
        ra::graphify::GraphifyStatusKind::Missing
    );
    let prompt = workflow.build_system_prompt_section().unwrap();
    assert!(prompt.contains("Graphify R2A Graph Service"));
    assert!(prompt.contains("no graph exists yet"));
    assert!(prompt.contains("graphify_ensure"));
    assert!(prompt.contains("graphify_update"));
    assert!(prompt.contains("Do not require the user to run Graphify first"));

    let tools = ra::graphify::tools_for_workflow(&workflow);
    assert!(tools.iter().any(|t| t.name() == "graphify_ensure"));
    assert!(tools.iter().any(|t| t.name() == "graphify_update"));
    assert!(tools.iter().any(|t| t.name() == "graphify_impact"));
    assert!(tools.iter().any(|t| t.name() == "graphify_query"));
}

#[test]
fn graphify_workflow_marks_stale_graph_and_suggests_refresh() {
    let tmp = TempDir::new().unwrap();
    write_graph(tmp.path());
    std::thread::sleep(Duration::from_millis(25));
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    fs::write(tmp.path().join("src/auth.rs"), "pub fn changed() {}\n").unwrap();

    let workflow =
        ra::graphify::workflow_from_config(&ra::config::GraphifySection::default(), tmp.path())
            .unwrap();
    assert_eq!(
        workflow.status.kind,
        ra::graphify::GraphifyStatusKind::Stale
    );
    let prompt = workflow.build_system_prompt_section().unwrap();
    assert!(prompt.contains("status: stale"));
    assert!(prompt.contains("graphify_update"));
    assert!(prompt.contains("newer input"));
}
