//! End-to-end check that Session correctly drives:
//!   1. **Streaming text** — multiple `TextDelta` events arrive in order
//!      while the model is still emitting.
//!   2. **Parallel tool calls** — when one turn produces several
//!      `ToolCall` chunks, every tool runs and every `ToolResult` comes
//!      back inside that turn, before the next turn starts.
//!   3. **Multi-turn tool loop** — Session keeps re-prompting the model
//!      while the previous turn ends in `StopReason::ToolUse`, until a
//!      turn ends in `EndTurn`.
//!
//! We drive a `ScriptedModel` that returns a pre-baked sequence of
//! `Vec<ModelChunk>`, one per turn, so the test deterministically
//! controls every shape Session has to handle.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use ra::{
    Event, Session, ToolCall, ToolResult,
    model::{Message, Model, ModelChunk, StopReason, ToolSpec},
    tool_ctx::ToolCtx,
    Tool,
};
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;

// -- Scripted model ---------------------------------------------------------

/// A model whose `stream` impl pops the next pre-baked response off a
/// shared queue. Each call gets its own delay between chunks so the
/// stream is genuinely staggered (i.e. real `TextDelta` interleaving,
/// not one cat-blob).
struct ScriptedModel {
    turns: Mutex<Vec<Vec<ModelChunk>>>,
}

impl ScriptedModel {
    fn new(turns: Vec<Vec<ModelChunk>>) -> Self {
        Self { turns: Mutex::new(turns) }
    }
}

#[async_trait]
impl Model for ScriptedModel {
    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSpec],
    ) -> anyhow::Result<BoxStream<'static, ModelChunk>> {
        let chunks = self
            .turns
            .lock()
            .unwrap()
            .pop_front_compat()
            .expect("ScriptedModel: ran out of turns");
        let s = stream::iter(chunks).then(|c| async move {
            // Tiny delay so streamed chunks land on the broadcast bus
            // in distinct ticks — otherwise the `TextDelta` interleaving
            // assertion is meaningless.
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            c
        });
        Ok(s.boxed())
    }
}

trait PopFront<T> {
    fn pop_front_compat(&mut self) -> Option<T>;
}
impl<T> PopFront<T> for Vec<T> {
    fn pop_front_compat(&mut self) -> Option<T> {
        if self.is_empty() {
            None
        } else {
            Some(self.remove(0))
        }
    }
}

// -- Trivial in-process tool ------------------------------------------------

/// A tool that records every invocation in a shared counter. Used so the
/// test can assert "all parallel calls actually ran" without depending
/// on the local filesystem.
#[derive(Clone, Default)]
struct CallLog(Arc<Mutex<Vec<(String, String)>>>);

impl CallLog {
    fn new() -> Self {
        Self::default()
    }
    fn snapshot(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().clone()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct EchoParams {
    text: String,
}

struct EchoTool {
    log: CallLog,
}

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str { "echo" }
    fn description(&self) -> &str { "echo back the input, recording it in the test log" }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(EchoParams)).unwrap()
    }
    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> anyhow::Result<String> {
        let p: EchoParams = serde_json::from_value(input)?;
        self.log.0.lock().unwrap().push((call_id.to_string(), p.text.clone()));
        Ok(format!("echoed: {}", p.text))
    }
}

// -- Helpers ----------------------------------------------------------------

fn text_chunks(s: &str) -> Vec<ModelChunk> {
    s.chars()
        .collect::<Vec<_>>()
        .chunks(3)
        .map(|c| ModelChunk::TextDelta(c.iter().collect::<String>()))
        .collect()
}

fn tool_call(id: &str, name: &str, input: serde_json::Value) -> ModelChunk {
    ModelChunk::ToolCall(ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        input,
    })
}

// -- The actual test --------------------------------------------------------

#[tokio::test]
async fn streaming_text_parallel_tools_multi_turn() {
    // Turn 1: stream a chunked sentence, then fire THREE tool calls
    //         in the same turn (parallel-tools branch), end with ToolUse.
    let turn1: Vec<ModelChunk> = text_chunks("planning... ")
        .into_iter()
        .chain(std::iter::once(ModelChunk::TextDelta("ok now run all three tools.".into())))
        .chain([
            tool_call("c-alpha", "echo", serde_json::json!({ "text": "alpha" })),
            tool_call("c-beta",  "echo", serde_json::json!({ "text": "beta" })),
            tool_call("c-gamma", "echo", serde_json::json!({ "text": "gamma" })),
            ModelChunk::End { stop_reason: StopReason::ToolUse },
        ])
        .collect();

    // Turn 2: model has seen the three tool results, asks for one more.
    let turn2: Vec<ModelChunk> = text_chunks("two of three look right; ")
        .into_iter()
        .chain([
            tool_call("c-delta", "echo", serde_json::json!({ "text": "delta" })),
            ModelChunk::End { stop_reason: StopReason::ToolUse },
        ])
        .collect();

    // Turn 3: natural end.
    let turn3: Vec<ModelChunk> = text_chunks("done.")
        .into_iter()
        .chain(std::iter::once(ModelChunk::End { stop_reason: StopReason::EndTurn }))
        .collect();

    let log = CallLog::new();
    let session = Arc::new(Session::new(
        Arc::new(ScriptedModel::new(vec![turn1, turn2, turn3])),
        vec![Arc::new(EchoTool { log: log.clone() })],
    ));

    // Subscribe BEFORE prompting so we capture every event.
    let mut rx = session.subscribe();
    let collected = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Ok(ev) = rx.recv().await {
            let terminal = matches!(ev, Event::AgentEnd);
            events.push(ev);
            if terminal {
                break;
            }
        }
        events
    });

    let outcome = session.prompt("kick off".to_string()).await.expect("prompt ok");
    assert_eq!(format!("{outcome:?}"), "Completed");
    let events = collected.await.expect("event collector finished");

    // ----- 1. Streaming text -----
    // Multiple TextDeltas should arrive across the run, and the
    // concatenation should reproduce what we scripted.
    let text_deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| if let Event::TextDelta(s) = e { Some(s.as_str()) } else { None })
        .collect();
    assert!(
        text_deltas.len() >= 6,
        "expected the model output to land in many deltas, got {}: {:?}",
        text_deltas.len(),
        text_deltas
    );
    let joined: String = text_deltas.concat();
    assert!(joined.starts_with("planning..."), "joined={joined:?}");
    assert!(joined.contains("ok now run all three tools."), "joined={joined:?}");
    assert!(joined.contains("two of three look right;"), "joined={joined:?}");
    assert!(joined.ends_with("done."), "joined={joined:?}");

    // ----- 2. Parallel tool calls within turn 1 -----
    // All three Start events for turn 1 must appear before any End event
    // for turn 1's calls (Session emits Start as soon as the chunk lands,
    // then runs the calls in order).
    let starts: Vec<&str> = events
        .iter()
        .filter_map(|e| if let Event::ToolCallStart(c) = e { Some(c.id.as_str()) } else { None })
        .collect();
    let ends: Vec<&ToolResult> = events
        .iter()
        .filter_map(|e| if let Event::ToolCallEnd(r) = e { Some(r) } else { None })
        .collect();

    assert_eq!(
        starts,
        vec!["c-alpha", "c-beta", "c-gamma", "c-delta"],
        "ToolCallStart order across both turns"
    );

    // The first turn's three Starts must precede any turn-1 End event.
    let first_end_idx = events
        .iter()
        .position(|e| matches!(e, Event::ToolCallEnd(_)))
        .expect("at least one ToolCallEnd");
    let third_start_idx = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Event::ToolCallStart(c) if c.id.starts_with("c-")))
        .nth(2)
        .expect("at least three Starts")
        .0;
    assert!(
        third_start_idx < first_end_idx,
        "all three turn-1 Starts must arrive before the first End \
         (parallel-tools dispatch). third_start_idx={third_start_idx}, \
         first_end_idx={first_end_idx}"
    );

    // Every call should have exactly one End and no errors.
    assert_eq!(ends.len(), 4, "one End per scripted call");
    assert!(ends.iter().all(|r| !r.is_error), "no tool errors expected");
    let log = log.snapshot();
    assert_eq!(
        log,
        vec![
            ("c-alpha".into(), "alpha".into()),
            ("c-beta".into(),  "beta".into()),
            ("c-gamma".into(), "gamma".into()),
            ("c-delta".into(), "delta".into()),
        ],
        "every parallel call must actually have run, in dispatch order"
    );

    // ----- 3. Multi-turn tool loop -----
    // Three TurnStart / TurnEnd pairs (one per scripted turn).
    let turn_starts = events.iter().filter(|e| matches!(e, Event::TurnStart)).count();
    let turn_ends = events.iter().filter(|e| matches!(e, Event::TurnEnd)).count();
    assert_eq!(turn_starts, 3, "expected exactly three turns");
    assert_eq!(turn_ends, 3, "expected exactly three turn ends");

    // And the last event before AgentEnd must be the last TurnEnd.
    let last_turn_end_idx = events
        .iter()
        .rposition(|e| matches!(e, Event::TurnEnd))
        .expect("a TurnEnd somewhere");
    let agent_end_idx = events
        .iter()
        .rposition(|e| matches!(e, Event::AgentEnd))
        .expect("an AgentEnd");
    assert!(last_turn_end_idx < agent_end_idx, "AgentEnd should be last");
}
