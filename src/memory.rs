//! Codex-style local memory runtime.
//!
//! `memory_entry` remains the pure lifecycle/policy model. This module owns
//! generated memory files, redaction, prompt rendering, and the lightweight
//! generation pipeline that calls the lifecycle decisions before writing state.

use crate::config::{MemorySection, RaConfig};
use crate::memory_entry::{
    decide_generation, decide_use, GenerationDecision, MemoryCandidate, MemoryEntry, MemoryPolicy,
    SuppressionReason, UseDecision,
};
use crate::model::Message;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const MEMORY_SCHEMA_VERSION: &str = "ra.memory.v1";

#[derive(Debug, Clone)]
pub struct MemorySystem {
    section: MemorySection,
}

impl MemorySystem {
    pub fn from_config(config: &RaConfig) -> Self {
        Self {
            section: config.memory.clone(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.section.enabled
    }

    pub fn runtime_for_cwd(&self, cwd: impl AsRef<Path>) -> Result<MemoryRuntime> {
        let store = MemoryStore::for_cwd(cwd, self.section.dir.as_deref())?;
        Ok(MemoryRuntime {
            policy: policy_from_config(&self.section),
            store,
            max_prompt_memories: self.section.max_prompt_memories,
        })
    }
}

#[derive(Debug, Clone)]
pub struct MemoryRuntime {
    policy: MemoryPolicy,
    store: MemoryStore,
    max_prompt_memories: usize,
}

impl MemoryRuntime {
    pub fn policy(&self) -> &MemoryPolicy {
        &self.policy
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }

    pub async fn load_prompt(&self) -> Result<Option<MemoryPrompt>> {
        let artifacts = self.store.load_all().await?;
        if artifacts.is_empty() {
            return Ok(None);
        }
        Ok(Some(MemoryPrompt {
            policy: self.policy.clone(),
            artifacts,
            max_entries: self.max_prompt_memories,
        }))
    }

    pub async fn generate_from_session(
        &self,
        input: MemoryGenerationInput,
    ) -> Result<MemoryGenerationOutcome> {
        if self.store.source_session_exists(&input.session_id).await? {
            return Ok(MemoryGenerationOutcome {
                decision: GenerationDecision::Skipped {
                    reason: SuppressionReason::EntryNotDurable,
                },
                artifact: None,
                path: None,
            });
        }
        let mut policy = policy_for_thread(&self.policy, &input.controls);
        policy.use_memories = self.policy.use_memories;

        let candidate = MemoryCandidate {
            session_duration: input.session_duration,
            idle_for: input.idle_for,
            is_active: input.is_active,
            has_external_context: input.controls.has_external_context,
            rate_limit_remaining_percent: input.rate_limit_remaining_percent,
            redaction_applied: false,
        };
        let decision = decide_generation(&policy, &candidate);
        if !matches!(decision, GenerationDecision::Allowed { .. }) {
            return Ok(MemoryGenerationOutcome {
                decision,
                artifact: None,
                path: None,
            });
        }

        let Some(draft) = LocalMemoryExtractor.extract(&input.session_id, &input.messages) else {
            return Ok(MemoryGenerationOutcome {
                decision,
                artifact: None,
                path: None,
            });
        };
        let (content, redaction_applied) = redact_draft(draft);
        let entry = MemoryEntry::generated(redaction_applied);
        let artifact = MemoryArtifact::generated(
            input.session_id,
            self.store.cwd_hash().to_string(),
            entry,
            content,
        );
        let path = self.store.save(&artifact).await?;
        Ok(MemoryGenerationOutcome {
            decision: GenerationDecision::Allowed {
                entry: MemoryEntry::generated(redaction_applied),
            },
            artifact: Some(artifact),
            path: Some(path),
        })
    }
}

pub async fn load_prompt_for_cwd(
    system: Option<&MemorySystem>,
    cwd: impl AsRef<Path>,
) -> Option<MemoryPrompt> {
    let system = system?;
    if !system.is_enabled() {
        return None;
    }
    let runtime = match system.runtime_for_cwd(cwd) {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("[ra::memory] runtime: {e:#}");
            return None;
        }
    };
    match runtime.load_prompt().await {
        Ok(prompt) => prompt,
        Err(e) => {
            eprintln!("[ra::memory] load prompt: {e:#}");
            None
        }
    }
}

pub async fn generate_for_session(
    system: Option<&MemorySystem>,
    cwd: impl AsRef<Path>,
    session_id: &str,
    session: Arc<crate::session::Session>,
    rate_limit_remaining_percent: Option<u8>,
) {
    let Some(system) = system else { return };
    if !system.is_enabled() {
        return;
    }
    let cwd = cwd.as_ref().to_path_buf();
    let session_id = session_id.to_string();
    let outcome = generate_for_session_once(
        system,
        &cwd,
        &session_id,
        &session,
        rate_limit_remaining_percent,
    )
    .await;
    if let Some(GenerationDecision::Pending { remaining_idle, .. }) = outcome {
        let system = system.clone();
        let session = session.clone();
        tokio::spawn(async move {
            tokio::time::sleep(remaining_idle).await;
            let _ = generate_for_session_once(
                &system,
                &cwd,
                &session_id,
                &session,
                rate_limit_remaining_percent,
            )
            .await;
        });
    }
}

async fn generate_for_session_once(
    system: &MemorySystem,
    cwd: &Path,
    session_id: &str,
    session: &Arc<crate::session::Session>,
    rate_limit_remaining_percent: Option<u8>,
) -> Option<GenerationDecision> {
    let runtime = match system.runtime_for_cwd(cwd) {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("[ra::memory] runtime: {e:#}");
            return None;
        }
    };
    let timing = session.memory_timing().await;
    let messages = session.snapshot_messages().await;
    if messages.is_empty() {
        return None;
    }
    let input = MemoryGenerationInput {
        session_id: session_id.to_string(),
        messages,
        session_duration: timing.session_duration,
        idle_for: timing.idle_for,
        is_active: timing.is_active,
        rate_limit_remaining_percent,
        controls: session.memory_controls().await,
    };
    match runtime.generate_from_session(input).await {
        Ok(outcome) => {
            if let Some(path) = outcome.path {
                eprintln!("[ra::memory] saved {}", path.display());
            }
            Some(outcome.decision)
        }
        Err(e) => {
            eprintln!("[ra::memory] generate: {e:#}");
            None
        }
    }
}

pub fn policy_from_config(section: &MemorySection) -> MemoryPolicy {
    MemoryPolicy {
        memories_enabled: section.enabled,
        region_available: section.region_available,
        generate_memories: section.generate_memories,
        use_memories: section.use_memories,
        disable_on_external_context: section.disable_on_external_context,
        min_idle_before_generation: Duration::from_secs(section.min_idle_before_generation_secs),
        min_session_duration: Duration::from_secs(section.min_session_duration_secs),
        min_rate_limit_remaining_percent: section.min_rate_limit_remaining_percent,
        min_sessions_between_dreams: section.min_sessions_between_dreams,
    }
}

pub fn policy_for_thread(base: &MemoryPolicy, controls: &MemoryThreadControls) -> MemoryPolicy {
    let mut policy = base.clone();
    policy.use_memories = base.use_memories && controls.use_memories;
    policy.generate_memories = base.generate_memories && controls.generate_memories;
    policy
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryThreadControls {
    pub use_memories: bool,
    pub generate_memories: bool,
    pub has_external_context: bool,
}

impl Default for MemoryThreadControls {
    fn default() -> Self {
        Self {
            use_memories: true,
            generate_memories: true,
            has_external_context: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemoryPrompt {
    policy: MemoryPolicy,
    artifacts: Vec<MemoryArtifact>,
    max_entries: usize,
}

impl MemoryPrompt {
    pub fn render(&self, controls: &MemoryThreadControls) -> Option<String> {
        let policy = policy_for_thread(&self.policy, controls);
        let mut rendered = Vec::new();
        for artifact in self.artifacts.iter().filter(|a| a.entry.durable) {
            let entry = MemoryEntry {
                durable: artifact.entry.durable,
                redaction_applied: artifact.entry.redaction_applied,
            };
            match decide_use(&policy, &entry, controls.has_external_context) {
                UseDecision::Active => rendered.push(render_artifact(artifact)),
                UseDecision::Suppressed { .. } => {}
            }
            if rendered.len() >= self.max_entries {
                break;
            }
        }
        if rendered.is_empty() {
            return None;
        }

        let guidance = MemoryEntry::generated(false).guidance();
        let mut out = String::from("# Local Memories\n\n");
        out.push_str(
            "These are generated local memories for stable preferences, recurring workflows, \
             stacks, project conventions, and known pitfalls. They are inspectable generated \
             state, not the primary control surface.\n\n",
        );
        out.push_str(&format!(
            "Required team guidance remains authoritative in {}.\n\n",
            guidance.authoritative_team_guidance
        ));
        out.push_str(&rendered.join("\n\n"));
        Some(out)
    }

    pub fn artifacts(&self) -> &[MemoryArtifact] {
        &self.artifacts
    }
}

fn render_artifact(artifact: &MemoryArtifact) -> String {
    let mut out = format!("## Memory {}\n", artifact.id);
    if let Some(summary) = &artifact.content.summary {
        out.push_str(&format!("- summary: {}\n", summary.trim()));
    }
    append_list(&mut out, "facts", &artifact.content.facts);
    append_list(&mut out, "preferences", &artifact.content.preferences);
    append_list(&mut out, "workflows", &artifact.content.workflows);
    append_list(&mut out, "pitfalls", &artifact.content.pitfalls);
    if artifact.entry.redaction_applied {
        out.push_str("- note: secret-like values were redacted before storage\n");
    }
    out.trim_end().to_string()
}

fn append_list(out: &mut String, label: &str, values: &[String]) {
    for value in values {
        out.push_str(&format!("- {label}: {}\n", value.trim()));
    }
}

#[derive(Debug, Clone)]
pub struct MemoryGenerationInput {
    pub session_id: String,
    pub messages: Vec<Message>,
    pub session_duration: Duration,
    pub idle_for: Duration,
    pub is_active: bool,
    pub rate_limit_remaining_percent: Option<u8>,
    pub controls: MemoryThreadControls,
}

#[derive(Debug, Clone)]
pub struct MemoryGenerationOutcome {
    pub decision: GenerationDecision,
    pub artifact: Option<MemoryArtifact>,
    pub path: Option<PathBuf>,
}

impl MemoryGenerationOutcome {
    pub fn suppression_reason(&self) -> Option<SuppressionReason> {
        match &self.decision {
            GenerationDecision::Skipped { reason } => Some(*reason),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryArtifact {
    pub schema_version: String,
    pub id: String,
    pub created_at: String,
    pub cwd_hash: String,
    pub source_session_id: String,
    pub entry: StoredMemoryEntry,
    pub content: MemoryContent,
}

impl MemoryArtifact {
    pub fn generated(
        source_session_id: String,
        cwd_hash: String,
        entry: MemoryEntry,
        content: MemoryContent,
    ) -> Self {
        Self {
            schema_version: MEMORY_SCHEMA_VERSION.to_string(),
            id: ulid::Ulid::new().to_string(),
            created_at: crate::atif::now_iso8601(),
            cwd_hash,
            source_session_id,
            entry: StoredMemoryEntry {
                durable: entry.durable,
                redaction_applied: entry.redaction_applied,
            },
            content,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMemoryEntry {
    pub durable: bool,
    pub redaction_applied: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferences: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflows: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pitfalls: Vec<String>,
}

impl MemoryContent {
    fn is_empty(&self) -> bool {
        self.summary.as_deref().unwrap_or("").trim().is_empty()
            && self.facts.is_empty()
            && self.preferences.is_empty()
            && self.workflows.is_empty()
            && self.pitfalls.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct MemoryStore {
    bucket: PathBuf,
    cwd_hash: String,
}

impl MemoryStore {
    pub fn for_cwd(cwd: impl AsRef<Path>, configured_dir: Option<&str>) -> Result<Self> {
        let cwd = cwd.as_ref();
        let cwd_hash = crate::store::cwd_hash(cwd);
        let root = match configured_dir {
            Some(dir) => RaConfig::expand_path(dir, cwd),
            None => ra_home()?.join("memories"),
        };
        let bucket = root.join(&cwd_hash);
        std::fs::create_dir_all(&bucket)
            .with_context(|| format!("create_dir_all {}", bucket.display()))?;
        Ok(Self { bucket, cwd_hash })
    }

    pub fn bucket(&self) -> &Path {
        &self.bucket
    }

    pub fn cwd_hash(&self) -> &str {
        &self.cwd_hash
    }

    pub fn path_for(&self, id: &str) -> PathBuf {
        self.bucket.join(format!("{id}.json"))
    }

    pub async fn save(&self, artifact: &MemoryArtifact) -> Result<PathBuf> {
        let bucket = self.bucket.clone();
        let target = self.path_for(&artifact.id);
        let json = serde_json::to_vec_pretty(artifact).context("serialize memory artifact")?;
        tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            let mut tmp = tempfile::NamedTempFile::new_in(&bucket)
                .with_context(|| format!("tempfile in {}", bucket.display()))?;
            std::io::Write::write_all(tmp.as_file_mut(), &json)?;
            tmp.as_file_mut().sync_all().ok();
            tmp.persist(&target)
                .map_err(|e| anyhow::anyhow!("persist: {e}"))?;
            Ok(target)
        })
        .await
        .context("save memory spawn_blocking")?
    }

    pub async fn load_all(&self) -> Result<Vec<MemoryArtifact>> {
        let bucket = self.bucket.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<MemoryArtifact>> {
            let mut out = Vec::new();
            let dir = match std::fs::read_dir(&bucket) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
                Err(e) => return Err(e).context("read_dir"),
            };
            let mut files = Vec::new();
            for entry in dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                files.push((modified, path));
            }
            files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
            for (_, path) in files {
                let bytes =
                    std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
                let artifact: MemoryArtifact = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {}", path.display()))?;
                if artifact.schema_version == MEMORY_SCHEMA_VERSION {
                    out.push(artifact);
                }
            }
            Ok(out)
        })
        .await
        .context("load memories spawn_blocking")?
    }

    pub async fn source_session_exists(&self, session_id: &str) -> Result<bool> {
        Ok(self
            .load_all()
            .await?
            .iter()
            .any(|artifact| artifact.source_session_id == session_id))
    }
}

fn ra_home() -> Result<PathBuf> {
    match std::env::var_os("RA_HOME") {
        Some(p) => Ok(PathBuf::from(p)),
        None => Ok(dirs::data_local_dir()
            .context("dirs::data_local_dir returned None")?
            .join("ra")),
    }
}

trait MemoryExtractor {
    fn extract(&self, session_id: &str, messages: &[Message]) -> Option<MemoryContent>;
}

struct LocalMemoryExtractor;

impl MemoryExtractor for LocalMemoryExtractor {
    fn extract(&self, _session_id: &str, messages: &[Message]) -> Option<MemoryContent> {
        let mut content = MemoryContent::default();
        for message in messages {
            let Message::User { content: text } = message else {
                continue;
            };
            for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                let lower = line.to_ascii_lowercase();
                if lower.contains("password")
                    || lower.contains("api key")
                    || lower.contains("token")
                    || lower.contains("secret")
                {
                    push_unique(&mut content.facts, line);
                } else if lower.contains("prefer")
                    || lower.contains("preference")
                    || lower.contains("remember")
                {
                    push_unique(&mut content.preferences, line);
                } else if lower.contains("workflow")
                    || lower.contains("always run")
                    || lower.contains("usually run")
                {
                    push_unique(&mut content.workflows, line);
                } else if lower.contains("stack")
                    || lower.contains("convention")
                    || lower.contains("project uses")
                {
                    push_unique(&mut content.facts, line);
                } else if lower.contains("pitfall")
                    || lower.contains("gotcha")
                    || lower.contains("avoid")
                {
                    push_unique(&mut content.pitfalls, line);
                }
            }
        }
        if content.is_empty() {
            None
        } else {
            let mut summary_parts = Vec::new();
            if !content.preferences.is_empty() {
                summary_parts.push("stable preferences");
            }
            if !content.workflows.is_empty() {
                summary_parts.push("recurring workflows");
            }
            if !content.facts.is_empty() {
                summary_parts.push("project facts");
            }
            if !content.pitfalls.is_empty() {
                summary_parts.push("known pitfalls");
            }
            content.summary = Some(format!("Captured {}.", summary_parts.join(", ")));
            Some(content)
        }
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() || values.iter().any(|existing| existing == value) {
        return;
    }
    values.push(value.to_string());
}

fn redact_draft(draft: MemoryContent) -> (MemoryContent, bool) {
    let mut redacted = false;
    let summary = draft.summary.map(|s| {
        let (value, did) = redact_text(&s);
        redacted |= did;
        value
    });
    let mut redact_vec = |values: Vec<String>| {
        values
            .into_iter()
            .map(|value| {
                let (value, did) = redact_text(&value);
                redacted |= did;
                value
            })
            .collect()
    };
    (
        MemoryContent {
            summary,
            facts: redact_vec(draft.facts),
            preferences: redact_vec(draft.preferences),
            workflows: redact_vec(draft.workflows),
            pitfalls: redact_vec(draft.pitfalls),
        },
        redacted,
    )
}

pub fn redact_text(input: &str) -> (String, bool) {
    let mut out = input.to_string();
    let mut changed = false;
    let patterns = [
        (
            r"(?i)(api[_ -]?key|access[_ -]?token|auth[_ -]?token|token|secret|password)\s*[:=]\s*([^\s,;]+)",
            "$1=<redacted>",
        ),
        (r"sk-[A-Za-z0-9_-]{12,}", "<redacted-secret>"),
        (r"ghp_[A-Za-z0-9_]{12,}", "<redacted-secret>"),
    ];
    for (pattern, replacement) in patterns {
        let re = regex::Regex::new(pattern).expect("valid memory redaction regex");
        let next = re.replace_all(&out, replacement).to_string();
        if next != out {
            changed = true;
            out = next;
        }
    }
    (out, changed)
}
