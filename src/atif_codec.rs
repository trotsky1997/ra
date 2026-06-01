//! Bridge: Ra's `Message` history ↔ ATIF v1.7 `Step` array.
//!
//! Ra stores conversation as a flat `Vec<Message>` where each `ToolResult`
//! is its own entry. ATIF stores it as `Vec<Step>` where every tool call
//! lives on the **same** Agent step that issued it (alongside an
//! `observation.results[*]` array that mirrors the call ids).
//!
//! Encoding therefore folds successive `ToolResult`s back onto the most
//! recent agent step. Decoding splits them out again.

use crate::atif::{
    self, Agent, Message as AtifMessage, Observation, ObservationResult, Step, StepSource,
    ToolCall as AtifToolCall, Trajectory,
};
use crate::events::{ToolCall as RaToolCall, ToolResult};
use crate::model::Message as RaMessage;

/// Build an ATIF `Trajectory` from a Ra session's message log.
pub fn encode(
    session_id: impl Into<String>,
    model_name: Option<String>,
    messages: &[RaMessage],
) -> Trajectory {
    let mut traj = Trajectory::empty(session_id, Agent::ra(model_name));

    for m in messages {
        match m {
            RaMessage::User { content } => {
                traj.push_step(Step {
                    step_id: 0, // overwritten by push_step
                    timestamp: Some(atif::now_iso8601()),
                    source: StepSource::User,
                    model_name: None,
                    reasoning_effort: None,
                    message: AtifMessage::text(content),
                    reasoning_content: None,
                    tool_calls: None,
                    observation: None,
                    metrics: None,
                    extra: None,
                    llm_call_count: None,
                    is_copied_context: None,
                });
            }
            RaMessage::Assistant {
                content,
                tool_calls,
            } => {
                let calls = if tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        tool_calls
                            .iter()
                            .map(|c| AtifToolCall {
                                tool_call_id: c.id.clone(),
                                function_name: c.name.clone(),
                                arguments: c.input.clone(),
                                extra: None,
                            })
                            .collect(),
                    )
                };
                traj.push_step(Step {
                    step_id: 0,
                    timestamp: Some(atif::now_iso8601()),
                    source: StepSource::Agent,
                    model_name: traj.agent.model_name.clone(),
                    reasoning_effort: None,
                    message: AtifMessage::text(content),
                    reasoning_content: None,
                    tool_calls: calls,
                    observation: None, // filled by trailing ToolResults
                    metrics: None,
                    extra: None,
                    llm_call_count: None,
                    is_copied_context: None,
                });
            }
            RaMessage::ToolResult(r) => {
                // Fold onto the most recent agent step. The Ra session
                // invariant guarantees an Assistant step precedes any
                // ToolResult; if the invariant is violated we silently
                // append a synthetic Agent step so we never lose data.
                let needs_synth = traj
                    .steps
                    .last()
                    .map(|s| s.source != StepSource::Agent)
                    .unwrap_or(true);
                if needs_synth {
                    traj.push_step(Step {
                        step_id: 0,
                        timestamp: Some(atif::now_iso8601()),
                        source: StepSource::Agent,
                        model_name: None,
                        reasoning_effort: None,
                        message: AtifMessage::text(""),
                        reasoning_content: None,
                        tool_calls: None,
                        observation: None,
                        metrics: None,
                        extra: None,
                        llm_call_count: None,
                        is_copied_context: None,
                    });
                }
                let last = traj.steps.last_mut().expect("at least one agent step");
                let obs = last.observation.get_or_insert(Observation {
                    results: Vec::new(),
                });
                obs.results.push(ObservationResult {
                    source_call_id: Some(r.call_id.clone()),
                    content: Some(AtifMessage::text(&r.content)),
                    subagent_trajectory_ref: None,
                    extra: if r.is_error {
                        Some(serde_json::json!({"is_error": true}))
                    } else {
                        None
                    },
                });
            }
        }
    }

    traj
}

/// Convert an ATIF `Trajectory` back into Ra's flat message log.
pub fn decode(traj: &Trajectory) -> Vec<RaMessage> {
    let mut out = Vec::new();
    for step in &traj.steps {
        match step.source {
            StepSource::User => {
                out.push(RaMessage::User {
                    content: step.message.as_text(),
                });
            }
            StepSource::Agent => {
                let calls: Vec<RaToolCall> = step
                    .tool_calls
                    .as_ref()
                    .map(|tcs| {
                        tcs.iter()
                            .map(|c| RaToolCall {
                                id: c.tool_call_id.clone(),
                                name: c.function_name.clone(),
                                input: c.arguments.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(RaMessage::Assistant {
                    content: step.message.as_text(),
                    tool_calls: calls,
                });
                if let Some(obs) = &step.observation {
                    for r in &obs.results {
                        let is_error = r
                            .extra
                            .as_ref()
                            .and_then(|e| e.get("is_error"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        out.push(RaMessage::ToolResult(ToolResult {
                            call_id: r.source_call_id.clone().unwrap_or_default(),
                            is_error,
                            content: r.content.as_ref().map(|m| m.as_text()).unwrap_or_default(),
                        }));
                    }
                }
            }
            StepSource::System => {
                // System messages aren't part of the conversational state we
                // hand back to the model; skip them. (They'd be useful for
                // SFT export but Ra doesn't currently emit any.)
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ToolCall as RaToolCall;

    #[test]
    fn roundtrip_text_only() {
        let msgs = vec![
            RaMessage::User {
                content: "hi".into(),
            },
            RaMessage::Assistant {
                content: "hello".into(),
                tool_calls: vec![],
            },
        ];
        let traj = encode("sess1", Some("m".into()), &msgs);
        let back = decode(&traj);
        assert_eq!(back.len(), msgs.len());
        assert!(matches!(&back[0], RaMessage::User { content } if content == "hi"));
        assert!(matches!(&back[1], RaMessage::Assistant { content, .. } if content == "hello"));
    }

    #[test]
    fn roundtrip_with_tool() {
        let msgs = vec![
            RaMessage::User {
                content: "run ls".into(),
            },
            RaMessage::Assistant {
                content: "running".into(),
                tool_calls: vec![RaToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command":"ls"}),
                }],
            },
            RaMessage::ToolResult(ToolResult {
                call_id: "c1".into(),
                is_error: false,
                content: "Cargo.toml\n".into(),
            }),
            RaMessage::Assistant {
                content: "done".into(),
                tool_calls: vec![],
            },
        ];
        let traj = encode("sess2", None, &msgs);
        // 3 ATIF steps: user / agent (with obs) / agent
        assert_eq!(traj.steps.len(), 3);
        assert_eq!(traj.steps[1].tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(traj.steps[1].observation.as_ref().unwrap().results.len(), 1);
        // Round-trip the structure
        let back = decode(&traj);
        assert_eq!(back.len(), msgs.len());
    }
}
