use crate::events::Event;
use anyhow::{Context, Result};
use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use tokio::sync::broadcast;

/// 工具的统一接口。dyn-safe：不绑定 associated type，参数走 serde_json::Value。
///
/// 真实工具一般会定义自己的 Params struct + #[derive(JsonSchema, Deserialize)]，
/// 然后在 schema() 里用 schema_for!(Params) 自动生成 JSON Schema。
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    /// 返回 JSON Schema，描述本工具的入参。模型层会把这个塞给 LLM。
    fn schema(&self) -> serde_json::Value;

    /// 真正执行。tx 用来在执行过程中广播 Update 事件（比如 bash 的流式 stdout）。
    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        tx: &broadcast::Sender<Event>,
    ) -> Result<String>;
}

// ---------- ReadTool ----------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// File path to read (absolute or relative to cwd).
    pub path: String,
}

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read a file from the filesystem."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(ReadParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        _tx: &broadcast::Sender<Event>,
    ) -> Result<String> {
        let params: ReadParams =
            serde_json::from_value(input).context("invalid params for read")?;
        let bytes = tokio::fs::read(&params.path)
            .await
            .with_context(|| format!("read {}", params.path))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

// ---------- BashTool ----------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BashParams {
    /// Shell command to execute via /bin/sh -c.
    pub command: String,
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Run a shell command and return its combined stdout/stderr."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(BashParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        tx: &broadcast::Sender<Event>,
    ) -> Result<String> {
        let params: BashParams =
            serde_json::from_value(input).context("invalid params for bash")?;

        let output = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&params.command)
            .output()
            .await
            .with_context(|| format!("spawn `{}`", params.command))?;

        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.stderr.is_empty() {
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
        }

        // 演示一下 ToolCallUpdate 的广播用法
        let _ = tx.send(Event::ToolCallUpdate {
            id: call_id.to_string(),
            chunk: format!("[exit={}]", output.status.code().unwrap_or(-1)),
        });

        Ok(combined)
    }
}
