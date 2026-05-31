//! Filesystem mutation tools: `write`, `edit`.
//!
//! Both prefer the ACP host's `fs/write_text_file` reverse call when
//! available so the editor stays in control of disk I/O. They fall back
//! to direct tokio fs operations when no host is connected (CLI mode,
//! A2A serve mode, etc.).

use crate::tool_ctx::ToolCtx;
use crate::tools::core::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;

// ---------- WriteTool ------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteParams {
    /// File path to write (absolute, or relative to the agent's cwd).
    /// Parent directories are created if missing.
    pub path: String,
    /// New file contents. The file is overwritten in full.
    pub content: String,
}

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "Write a text file. Overwrites any existing file at the path. \
         Prefer `edit` for in-place modifications to keep diffs small."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(WriteParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("write");
        let params: WriteParams =
            serde_json::from_value(input).context("invalid params for write")?;

        // Reverse-call the ACP host first.
        if let (Some(client), Some(sid)) = (&ctx.client, &ctx.session_id) {
            match client
                .fs_write_text_file(sid, &params.path, &params.content)
                .await
            {
                Ok(()) => return Ok(format!("wrote {} ({} bytes)", params.path, params.content.len())),
                Err(e) => {
                    eprintln!(
                        "[ra::tools::write] reverse fs/write_text_file failed: {e:#}; \
                         falling back to local write"
                    );
                }
            }
        }

        if let Some(parent) = std::path::Path::new(&params.path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("mkdir -p {}", parent.display()))?;
            }
        }
        tokio::fs::write(&params.path, &params.content)
            .await
            .with_context(|| format!("write {}", params.path))?;
        Ok(format!(
            "wrote {} ({} bytes)",
            params.path,
            params.content.len()
        ))
    }
}

// ---------- EditTool -------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditParams {
    /// File path to edit.
    pub path: String,
    /// Exact text to replace. Must appear verbatim in the file.
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// When true, replace every occurrence. When false (default), require
    /// `old_string` to be unique and replace exactly one match.
    #[serde(default)]
    pub replace_all: bool,
}

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Replace a literal substring in a text file. By default the \
         substring must be unique; pass `replace_all=true` to swap every \
         occurrence. Errors if the substring is missing or non-unique."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(EditParams)).unwrap()
    }

    async fn execute(
        &self,
        _call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("edit");
        let params: EditParams =
            serde_json::from_value(input).context("invalid params for edit")?;

        let original = read_via_host_or_disk(ctx, &params.path).await?;

        if params.old_string == params.new_string {
            return Err(anyhow::anyhow!(
                "old_string and new_string are identical; nothing to do"
            ));
        }
        if params.old_string.is_empty() {
            return Err(anyhow::anyhow!("old_string must not be empty"));
        }

        let occurrences = original.matches(&params.old_string).count();
        if occurrences == 0 {
            return Err(anyhow::anyhow!(
                "old_string not found in {}",
                params.path
            ));
        }
        if !params.replace_all && occurrences > 1 {
            return Err(anyhow::anyhow!(
                "old_string matches {occurrences} places in {}; \
                 add more context to make it unique or set replace_all=true",
                params.path
            ));
        }

        let updated = if params.replace_all {
            original.replace(&params.old_string, &params.new_string)
        } else {
            original.replacen(&params.old_string, &params.new_string, 1)
        };

        write_via_host_or_disk(ctx, &params.path, &updated).await?;

        let replaced = if params.replace_all { occurrences } else { 1 };
        Ok(format!(
            "edited {} ({} replacement{})",
            params.path,
            replaced,
            if replaced == 1 { "" } else { "s" }
        ))
    }
}

async fn read_via_host_or_disk(ctx: &ToolCtx, path: &str) -> Result<String> {
    if let (Some(client), Some(sid)) = (&ctx.client, &ctx.session_id) {
        if let Ok(text) = client.fs_read_text_file(sid, path, None, None).await {
            return Ok(text);
        }
    }
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read {path}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn write_via_host_or_disk(ctx: &ToolCtx, path: &str, content: &str) -> Result<()> {
    if let (Some(client), Some(sid)) = (&ctx.client, &ctx.session_id) {
        if client.fs_write_text_file(sid, path, content).await.is_ok() {
            return Ok(());
        }
    }
    tokio::fs::write(path, content)
        .await
        .with_context(|| format!("write {path}"))?;
    Ok(())
}
