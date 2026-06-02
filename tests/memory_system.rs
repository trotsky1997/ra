use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use ra::config::RaConfig;
use ra::memory::{
    load_prompt_for_cwd, policy_for_thread, policy_from_config, redact_text, MemoryArtifact,
    MemoryContent, MemoryGenerationInput, MemoryStore, MemorySystem, MemoryThreadControls,
};
use ra::memory_entry::{GenerationDecision, MemoryEntry, SuppressionReason};
use ra::model::{Message, Model, ModelChunk, StopReason, ToolSpec};
use ra::Session;
use tokio::sync::Mutex as AsyncMutex;

fn env_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

fn enabled_config() -> RaConfig {
    toml::from_str(
        r#"
version = 1

[memory]
enabled = true
min_idle_before_generation_secs = 0
min_session_duration_secs = 0
min_rate_limit_remaining_percent = 20
max_prompt_memories = 5
"#,
    )
    .unwrap()
}

struct SeenModel {
    seen: std::sync::Arc<Mutex<Vec<Vec<Message>>>>,
}

#[async_trait]
impl Model for SeenModel {
    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[ToolSpec],
    ) -> anyhow::Result<BoxStream<'static, ModelChunk>> {
        self.seen.lock().unwrap().push(messages.to_vec());
        Ok(stream::iter([ModelChunk::End {
            stop_reason: StopReason::EndTurn,
        }])
        .boxed())
    }
}

#[test]
fn memory_config_maps_to_lifecycle_policy_and_thread_controls() {
    let cfg = enabled_config();
    let policy = policy_from_config(&cfg.memory);
    assert!(policy.memories_enabled);
    assert!(policy.use_memories);
    assert!(policy.generate_memories);
    assert_eq!(policy.min_rate_limit_remaining_percent, 20);

    let thread_policy = policy_for_thread(
        &policy,
        &MemoryThreadControls {
            use_memories: false,
            generate_memories: true,
            has_external_context: false,
        },
    );
    assert!(!thread_policy.use_memories);
    assert!(thread_policy.generate_memories);
}

#[tokio::test]
async fn memory_store_defaults_under_ra_home_and_roundtrips_artifact() {
    let _guard = env_lock().lock().await;
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("RA_HOME", tmp.path());
    }

    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let store = MemoryStore::for_cwd(&cwd, None).unwrap();
    assert!(store.bucket().starts_with(tmp.path().join("memories")));

    let artifact = MemoryArtifact::generated(
        "session-1".into(),
        store.cwd_hash().to_string(),
        MemoryEntry::generated(true),
        MemoryContent {
            summary: Some("Captured stable preferences.".into()),
            preferences: vec!["Prefer cargo test before review.".into()],
            ..MemoryContent::default()
        },
    );
    let path = store.save(&artifact).await.unwrap();
    assert!(path.exists());

    let loaded = store.load_all().await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].source_session_id, "session-1");
    assert!(loaded[0].entry.redaction_applied);
    assert_eq!(loaded[0].content.preferences.len(), 1);
}

#[tokio::test]
async fn disabled_memory_does_not_create_state_when_loading_prompt() {
    let _guard = env_lock().lock().await;
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("RA_HOME", tmp.path());
    }
    let cfg: RaConfig = toml::from_str("version = 1\n").unwrap();
    let system = MemorySystem::from_config(&cfg);
    assert!(load_prompt_for_cwd(Some(&system), tmp.path())
        .await
        .is_none());
    assert!(!tmp.path().join("memories").exists());
}

#[tokio::test]
async fn prompt_context_respects_use_and_external_context_suppression() {
    let _guard = env_lock().lock().await;
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("RA_HOME", tmp.path());
    }

    let cfg: RaConfig = toml::from_str(
        r#"
version = 1

[memory]
enabled = true
disable_on_external_context = true
"#,
    )
    .unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let system = MemorySystem::from_config(&cfg);
    let runtime = system.runtime_for_cwd(&cwd).unwrap();
    let artifact = MemoryArtifact::generated(
        "session-1".into(),
        runtime.store().cwd_hash().to_string(),
        MemoryEntry::generated(false),
        MemoryContent {
            summary: Some("Captured project facts.".into()),
            facts: vec!["Project uses Rust stable.".into()],
            ..MemoryContent::default()
        },
    );
    runtime.store().save(&artifact).await.unwrap();

    let prompt = runtime.load_prompt().await.unwrap().unwrap();
    let rendered = prompt.render(&MemoryThreadControls::default()).unwrap();
    assert!(rendered.contains("# Local Memories"));
    assert!(rendered.contains("Project uses Rust stable."));
    assert!(rendered.contains("AGENTS.md or checked-in documentation"));

    assert!(prompt
        .render(&MemoryThreadControls {
            use_memories: false,
            ..MemoryThreadControls::default()
        })
        .is_none());
    assert!(prompt
        .render(&MemoryThreadControls {
            has_external_context: true,
            ..MemoryThreadControls::default()
        })
        .is_none());
}

#[tokio::test]
async fn generation_pipeline_applies_gates_and_redaction() {
    let _guard = env_lock().lock().await;
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("RA_HOME", tmp.path());
    }

    let cfg = enabled_config();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let runtime = MemorySystem::from_config(&cfg)
        .runtime_for_cwd(&cwd)
        .unwrap();
    let messages = vec![Message::User {
        content: "Remember prefer concise Rust tests. api_key=sk-1234567890abcdef".into(),
    }];

    let low_rate = runtime
        .generate_from_session(MemoryGenerationInput {
            session_id: "low-rate".into(),
            messages: messages.clone(),
            session_duration: Duration::from_secs(120),
            idle_for: Duration::from_secs(120),
            is_active: false,
            rate_limit_remaining_percent: Some(19),
            controls: MemoryThreadControls::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        low_rate.suppression_reason(),
        Some(SuppressionReason::RateLimitTooLow)
    );
    assert!(low_rate.path.is_none());

    let active = runtime
        .generate_from_session(MemoryGenerationInput {
            session_id: "active".into(),
            messages: messages.clone(),
            session_duration: Duration::from_secs(120),
            idle_for: Duration::from_secs(120),
            is_active: true,
            rate_limit_remaining_percent: Some(80),
            controls: MemoryThreadControls::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        active.suppression_reason(),
        Some(SuppressionReason::SessionActive)
    );

    let generated = runtime
        .generate_from_session(MemoryGenerationInput {
            session_id: "eligible".into(),
            messages,
            session_duration: Duration::from_secs(120),
            idle_for: Duration::from_secs(120),
            is_active: false,
            rate_limit_remaining_percent: Some(80),
            controls: MemoryThreadControls::default(),
        })
        .await
        .unwrap();
    assert!(matches!(
        generated.decision,
        GenerationDecision::Allowed { .. }
    ));
    let artifact = generated.artifact.unwrap();
    assert!(artifact.entry.redaction_applied);
    assert!(artifact
        .content
        .preferences
        .iter()
        .any(|value| value.contains("<redacted>")));
    assert!(generated.path.unwrap().exists());
}

#[test]
fn redaction_masks_common_secret_shapes() {
    let (out, changed) = redact_text("token=ghp_abcdefghijklmnopqrstuvwxyz password=hunter2");
    assert!(changed);
    assert!(!out.contains("hunter2"));
    assert!(!out.contains("ghp_abcdefghijklmnopqrstuvwxyz"));
}

#[tokio::test]
async fn session_injects_memory_context_and_thread_can_suppress_it() {
    let _guard = env_lock().lock().await;
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("RA_HOME", tmp.path());
    }

    let cfg = enabled_config();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let runtime = MemorySystem::from_config(&cfg)
        .runtime_for_cwd(&cwd)
        .unwrap();
    let artifact = MemoryArtifact::generated(
        "session-1".into(),
        runtime.store().cwd_hash().to_string(),
        MemoryEntry::generated(false),
        MemoryContent {
            facts: vec!["Project convention: run cargo test --test memory_system.".into()],
            ..MemoryContent::default()
        },
    );
    runtime.store().save(&artifact).await.unwrap();
    let prompt = runtime.load_prompt().await.unwrap();

    let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
    let session =
        Session::new(Arc::new(SeenModel { seen: seen.clone() }), Vec::new()).with_cwd(&cwd);
    session.set_memory_prompt(prompt).await;
    session.prompt("first".to_string()).await.unwrap();

    let first = seen.lock().unwrap()[0].clone();
    assert!(matches!(
        &first[0],
        Message::User { content }
            if content.contains("# Local Memories")
                && content.contains("Project convention: run cargo test --test memory_system.")
    ));

    session.set_memory_use_enabled(false).await;
    session.prompt("second".to_string()).await.unwrap();
    let second = seen.lock().unwrap()[1].clone();
    assert!(matches!(
        &second[0],
        Message::User { content } if !content.contains("# Local Memories")
    ));
}
