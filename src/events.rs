use serde::{Deserialize, Serialize};

/// 一次 tool 调用的描述（assistant 发起）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// 一次 tool 调用的结果（喂回给 assistant）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub is_error: bool,
    pub content: String,
}

/// 流式事件，对应 TS SDK 里 session.subscribe(cb) 收到的 union 类型。
#[derive(Debug, Clone)]
pub enum Event {
    /// 一次 prompt() 开始
    AgentStart,
    /// 一个 turn（一次 LLM 响应 + 它要求的 tool 调用）开始
    TurnStart,

    /// assistant 文本流增量
    TextDelta(String),
    /// assistant 思考流增量（如果模型支持）
    ThinkingDelta(String),

    /// assistant 决定调用某个工具
    ToolCallStart(ToolCall),
    /// 工具执行过程中的增量输出（比如 bash stdout 行）
    ToolCallUpdate { id: String, chunk: String },
    /// 工具执行结束
    ToolCallEnd(ToolResult),

    /// 一个 turn 结束（assistant 没再要求调用工具就结束）
    TurnEnd,
    /// 一次 prompt() 全部跑完
    AgentEnd,

    /// 出错（不致命，会广播出来给 UI）
    Error(String),
}
