//! ATIF (Agent Trajectory Interchange Format) v1.7 types.
//!
//! Wire-compatible with the Harbor RFC 0001 spec. The full JSON Schema
//! generated from Harbor's pydantic models lives at `spec/atif-v1.7.json`.
//!
//! Design notes:
//! - `serde(skip_serializing_if = "Option::is_none")` everywhere so emitted
//!   files match Harbor's `to_json_dict(exclude_none=True)` output.
//! - `serde(deny_unknown_fields)` is intentionally NOT used: ATIF lets every
//!   object carry an `extra` map, but consumers must still tolerate fields
//!   added in later minor versions.
//! - Multimodal content parts and embedded subagent trajectories are
//!   modeled but unused by Ra today; we keep them in-shape so that future
//!   features (image inputs, planner subagents) round-trip cleanly.

use serde::{Deserialize, Serialize};

/// The full trajectory document, written to one `.json` file per session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    /// `"ATIF-v1.7"` for everything Ra emits.
    pub schema_version: String,
    /// Run-scoped id; OK to share across siblings/continuations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Document-scoped id, required if this trajectory is embedded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    pub agent: Agent,
    pub steps: Vec<Step>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<FinalMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continued_trajectory_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_trajectories: Option<Vec<Trajectory>>,
}

impl Trajectory {
    /// Construct an empty trajectory for a fresh Ra session.
    pub fn empty(session_id: impl Into<String>, agent: Agent) -> Self {
        Self {
            schema_version: "ATIF-v1.7".to_string(),
            session_id: Some(session_id.into()),
            trajectory_id: None,
            agent,
            steps: Vec::new(),
            notes: None,
            final_metrics: None,
            continued_trajectory_ref: None,
            extra: None,
            subagent_trajectories: None,
        }
    }

    /// Append a step, auto-assigning the next `step_id` (1-based).
    pub fn push_step(&mut self, mut step: Step) {
        step.step_id = (self.steps.len() as u64) + 1;
        self.steps.push(step);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

impl Agent {
    pub fn ra(model_name: Option<String>) -> Self {
        Self {
            name: "ra".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            model_name,
            tool_definitions: None,
            extra: None,
        }
    }
}

/// One ordered turn in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// 1-based ordinal. Filled in by `Trajectory::push_step`.
    pub step_id: u64,
    /// ISO 8601 UTC timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub source: StepSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub message: Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
    /// Number of LLM inferences this step represents (default: 1).
    /// 0 means deterministic dispatch (no LLM call); metrics & reasoning
    /// must be absent in that case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<u32>,
    /// True if this step was carried over from a previous context window.
    /// Such steps must be filtered out of SFT training data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_copied_context: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepSource {
    User,
    Agent,
    System,
}

/// `step.message` may be a plain string OR a multimodal content-part array.
/// Untagged so it serializes either way without an explicit discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl Message {
    pub fn text(s: impl Into<String>) -> Self {
        Message::Text(s.into())
    }

    /// Plain-text view; for `Parts` we concatenate every text part.
    pub fn as_text(&self) -> String {
        match self {
            Message::Text(s) => s.clone(),
            Message::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { source: ImageSource },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    pub media_type: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub results: Vec<ObservationResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationResult {
    /// Matches a `tool_calls[*].tool_call_id` from the same step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    /// Tool result body (free-form text or content parts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_trajectory_ref: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_token_ids: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_token_ids: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FinalMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cached_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// Current UTC timestamp in ATIF-friendly ISO 8601 form (e.g.
/// `2025-10-11T10:30:00Z`).
pub fn now_iso8601() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
