use crate::events::{ToolCall, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 对话历史中一条消息（user / assistant / tool）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        content: String,
    },
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    ToolResult(ToolResult),
}

/// 模型流出来的增量。一个 turn 由若干 chunk 拼成，最后必须有 End。
#[derive(Debug, Clone)]
pub enum ModelChunk {
    TextDelta(String),
    ThinkingDelta(String),
    /// 模型决定调用工具。骨架版一次只发一个，真实实现可以发多个。
    ToolCall(ToolCall),
    /// 本 turn 的模型输出结束。stop_reason 让上层判断要不要继续 loop。
    End {
        stop_reason: StopReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// 自然结束，不再调用工具
    EndTurn,
    /// 至少发起了一次 tool_call，等结果回来再继续
    ToolUse,
}

#[async_trait]
pub trait Model: Send + Sync {
    /// 给定完整历史 + 当前可用工具，开始流式生成。返回的 stream 必须以 End chunk 收尾。
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, ModelChunk>>;
}

/// 工具元信息，喂给模型层做 function calling 注册用。
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ---------- MockModel ----------

/// 一个完全本地的假模型，方便不挂 API key 也能看到完整 turn loop。
///
/// 规则：
/// - 用户最后一条消息以 `bash:`  开头  → 发起 bash 调用
/// - 用户最后一条消息以 `git:`   开头  → 发起 git 调用
/// - 用户最后一条消息以 `gh:`    开头  → 发起 gh 调用
/// - 用户最后一条消息以 `read:`  开头  → 发起 read 调用
/// - `write:<path>|<content>`           → 发起 write 调用
/// - `edit:<path>|<old>|<new>`          → 发起 edit 调用
/// - 上一条是 ToolResult                → 把结果转述一下并结束
/// - 否则                                → 按字符流式回显
pub struct MockModel;

#[async_trait]
impl Model for MockModel {
    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, ModelChunk>> {
        let last = messages.last().cloned();

        let chunks: Vec<ModelChunk> = match last {
            Some(Message::ToolResult(tr)) => {
                let summary = format!(
                    "Tool returned ({} bytes). Here's the head:\n{}\n",
                    tr.content.len(),
                    tr.content.chars().take(200).collect::<String>()
                );
                split_text_chunks(&summary)
                    .into_iter()
                    .chain(std::iter::once(ModelChunk::End {
                        stop_reason: StopReason::EndTurn,
                    }))
                    .collect()
            }
            Some(Message::User { content }) if content.starts_with("bash:") => {
                let cmd = content.trim_start_matches("bash:").trim().to_string();
                vec![
                    ModelChunk::TextDelta("Running shell...\n".into()),
                    ModelChunk::ToolCall(ToolCall {
                        id: format!("call_{}", rand_id()),
                        name: "bash".into(),
                        input: serde_json::json!({ "command": cmd }),
                    }),
                    ModelChunk::End {
                        stop_reason: StopReason::ToolUse,
                    },
                ]
            }
            Some(Message::User { content }) if content.starts_with("git:") => {
                let args = split_cli_args(content.trim_start_matches("git:").trim());
                vec![
                    ModelChunk::TextDelta("Running git...\n".into()),
                    ModelChunk::ToolCall(ToolCall {
                        id: format!("call_{}", rand_id()),
                        name: "git".into(),
                        input: serde_json::json!({ "args": args }),
                    }),
                    ModelChunk::End {
                        stop_reason: StopReason::ToolUse,
                    },
                ]
            }
            Some(Message::User { content }) if content.starts_with("gh:") => {
                let args = split_cli_args(content.trim_start_matches("gh:").trim());
                vec![
                    ModelChunk::TextDelta("Running gh...\n".into()),
                    ModelChunk::ToolCall(ToolCall {
                        id: format!("call_{}", rand_id()),
                        name: "gh".into(),
                        input: serde_json::json!({ "args": args }),
                    }),
                    ModelChunk::End {
                        stop_reason: StopReason::ToolUse,
                    },
                ]
            }
            Some(Message::User { content }) if content.starts_with("read:") => {
                let path = content.trim_start_matches("read:").trim().to_string();
                vec![
                    ModelChunk::TextDelta(format!("Reading {}...\n", path)),
                    ModelChunk::ToolCall(ToolCall {
                        id: format!("call_{}", rand_id()),
                        name: "read".into(),
                        input: serde_json::json!({ "path": path }),
                    }),
                    ModelChunk::End {
                        stop_reason: StopReason::ToolUse,
                    },
                ]
            }
            Some(Message::User { content }) if content.starts_with("write:") => {
                // write:<path>|<content>
                let body = content.trim_start_matches("write:").trim_start();
                let (path, text) = body.split_once('|').unwrap_or((body, ""));
                vec![
                    ModelChunk::TextDelta(format!("Writing {}...\n", path)),
                    ModelChunk::ToolCall(ToolCall {
                        id: format!("call_{}", rand_id()),
                        name: "write".into(),
                        input: serde_json::json!({
                            "path": path.trim(),
                            "content": text,
                        }),
                    }),
                    ModelChunk::End {
                        stop_reason: StopReason::ToolUse,
                    },
                ]
            }
            Some(Message::User { content }) if content.starts_with("edit:") => {
                // edit:<path>|<old>|<new>
                let body = content.trim_start_matches("edit:").trim_start();
                let mut parts = body.splitn(3, '|');
                let path = parts.next().unwrap_or("").trim();
                let old = parts.next().unwrap_or("");
                let new_s = parts.next().unwrap_or("");
                vec![
                    ModelChunk::TextDelta(format!("Editing {}...\n", path)),
                    ModelChunk::ToolCall(ToolCall {
                        id: format!("call_{}", rand_id()),
                        name: "edit".into(),
                        input: serde_json::json!({
                            "path": path,
                            "old_string": old,
                            "new_string": new_s,
                        }),
                    }),
                    ModelChunk::End {
                        stop_reason: StopReason::ToolUse,
                    },
                ]
            }
            Some(Message::User { content }) => {
                let reply = format!("You said: {}\n", content);
                split_text_chunks(&reply)
                    .into_iter()
                    .chain(std::iter::once(ModelChunk::End {
                        stop_reason: StopReason::EndTurn,
                    }))
                    .collect()
            }
            _ => vec![ModelChunk::End {
                stop_reason: StopReason::EndTurn,
            }],
        };

        // 加点延迟模拟真实流式
        let s = stream::iter(chunks).then(|c| async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            c
        });

        Ok(s.boxed())
    }
}

fn split_text_chunks(s: &str) -> Vec<ModelChunk> {
    s.chars()
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| ModelChunk::TextDelta(c.iter().collect()))
        .collect()
}

fn split_cli_args(s: &str) -> Vec<String> {
    s.split_whitespace().map(ToString::to_string).collect()
}

fn rand_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!(
        "{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn first_tool_call(input: &str) -> ToolCall {
        let messages = [Message::User {
            content: input.to_string(),
        }];
        let mut stream = MockModel.stream(&messages, &[]).await.unwrap();
        while let Some(chunk) = stream.next().await {
            if let ModelChunk::ToolCall(call) = chunk {
                return call;
            }
        }
        panic!("expected tool call for {input}");
    }

    #[tokio::test]
    async fn mock_model_maps_git_prefix_to_native_tool() {
        let call = first_tool_call("git:status --short").await;

        assert_eq!(call.name, "git");
        assert_eq!(
            call.input,
            serde_json::json!({ "args": ["status", "--short"] })
        );
    }

    #[tokio::test]
    async fn mock_model_maps_gh_prefix_to_native_tool() {
        let call = first_tool_call("gh:pr view --json title").await;

        assert_eq!(call.name, "gh");
        assert_eq!(
            call.input,
            serde_json::json!({ "args": ["pr", "view", "--json", "title"] })
        );
    }
}
