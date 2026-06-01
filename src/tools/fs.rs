//! Filesystem mutation tools: `write`, `edit`.
//!
//! Both prefer the ACP host's `fs/write_text_file` reverse call when
//! available so the editor stays in control of disk I/O. They fall back
//! to direct tokio fs operations when no host is connected (CLI mode,
//! A2A serve mode, etc.).

use crate::tool_ctx::{FileChange, FileChangeDecision, ToolCtx};
use crate::tools::core::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use schemars::{JsonSchema, schema_for};
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
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("write");
        let params: WriteParams =
            serde_json::from_value(input).context("invalid params for write")?;

        if ctx.file_approver.is_some() {
            let old_content = read_via_host_or_disk(ctx, &params.path).await.ok();
            approve_if_needed(
                ctx,
                FileChange {
                    call_id: call_id.to_string(),
                    tool_name: "write".into(),
                    path: params.path.clone(),
                    diff: unified_diff(&params.path, old_content.as_deref(), &params.content),
                    summary: if old_content.is_some() {
                        format!("overwrite {}", params.path)
                    } else {
                        format!("create {}", params.path)
                    },
                    old_content,
                    new_content: params.content.clone(),
                },
            )
            .await?;
        }

        // Reverse-call the ACP host first.
        if let (Some(client), Some(sid)) = (&ctx.client, &ctx.session_id) {
            match client
                .fs_write_text_file(sid, &params.path, &params.content)
                .await
            {
                Ok(()) => {
                    return Ok(format!(
                        "wrote {} ({} bytes)",
                        params.path,
                        params.content.len()
                    ));
                }
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
        call_id: &str,
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
            return Err(anyhow::anyhow!("old_string not found in {}", params.path));
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

        let replaced = if params.replace_all { occurrences } else { 1 };
        if ctx.file_approver.is_some() {
            approve_if_needed(
                ctx,
                FileChange {
                    call_id: call_id.to_string(),
                    tool_name: "edit".into(),
                    path: params.path.clone(),
                    diff: unified_diff(&params.path, Some(&original), &updated),
                    summary: format!(
                        "edit {} ({} replacement{})",
                        params.path,
                        replaced,
                        if replaced == 1 { "" } else { "s" }
                    ),
                    old_content: Some(original.clone()),
                    new_content: updated.clone(),
                },
            )
            .await?;
        }

        write_via_host_or_disk(ctx, &params.path, &updated).await?;

        Ok(format!(
            "edited {} ({} replacement{})",
            params.path,
            replaced,
            if replaced == 1 { "" } else { "s" }
        ))
    }
}

async fn approve_if_needed(ctx: &ToolCtx, change: FileChange) -> Result<()> {
    let Some(approver) = &ctx.file_approver else {
        return Ok(());
    };
    match approver.approve_file_change(change).await? {
        FileChangeDecision::Accept => Ok(()),
        FileChangeDecision::Reject => Err(anyhow::anyhow!("file change rejected by user")),
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

fn unified_diff(path: &str, old: Option<&str>, new: &str) -> String {
    let old_label = if old.is_some() {
        format!("a/{path}")
    } else {
        "/dev/null".to_string()
    };
    let new_label = format!("b/{path}");
    let old = old.unwrap_or("");
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);

    let mut out = String::new();
    out.push_str(&format!("--- {old_label}\n"));
    out.push_str(&format!("+++ {new_label}\n"));
    out.push_str(&format!(
        "@@ -1,{} +1,{} @@\n",
        old_lines.len(),
        new_lines.len()
    ));
    if old_lines.len().saturating_mul(new_lines.len()) > 250_000 {
        out.push_str(&format!(
            "\\ large diff omitted from inline review ({} old lines, {} new lines)\n",
            old_lines.len(),
            new_lines.len()
        ));
        return out;
    }
    let ops = diff_ops(&old_lines, &new_lines);
    for op in ops {
        match op {
            DiffOp::Equal(line) => {
                push_diff_line(&mut out, ' ', line);
            }
            DiffOp::Delete(line) => {
                push_diff_line(&mut out, '-', line);
            }
            DiffOp::Insert(line) => {
                push_diff_line(&mut out, '+', line);
            }
        }
    }
    out
}

fn push_diff_line(out: &mut String, prefix: char, line: &str) {
    out.push(prefix);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
    }
}

#[derive(Debug, Clone, Copy)]
enum DiffOp<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

fn diff_ops<'a>(old: &'a [&'a str], new: &'a [&'a str]) -> Vec<DiffOp<'a>> {
    let n = old.len();
    let m = new.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            out.push(DiffOp::Equal(old[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(DiffOp::Delete(old[i]));
            i += 1;
        } else {
            out.push(DiffOp::Insert(new[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(DiffOp::Delete(old[i]));
        i += 1;
    }
    while j < m {
        out.push(DiffOp::Insert(new[j]));
        j += 1;
    }
    out
}

fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split_inclusive('\n').collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_ctx::FileChangeApprover;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    struct StaticApprover {
        decision: FileChangeDecision,
        seen: Arc<Mutex<Vec<FileChange>>>,
    }

    #[async_trait]
    impl FileChangeApprover for StaticApprover {
        async fn approve_file_change(&self, change: FileChange) -> Result<FileChangeDecision> {
            self.seen.lock().unwrap().push(change);
            Ok(self.decision)
        }
    }

    fn ctx_with_approver(decision: FileChangeDecision) -> (ToolCtx, Arc<Mutex<Vec<FileChange>>>) {
        let (events, _) = tokio::sync::broadcast::channel(16);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let approver = StaticApprover {
            decision,
            seen: seen.clone(),
        };
        let mut ctx = ToolCtx::local(events);
        ctx.file_approver = Some(Arc::new(approver));
        (ctx, seen)
    }

    #[tokio::test]
    async fn write_reject_does_not_touch_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sample.txt");
        tokio::fs::write(&path, "old\n").await.unwrap();
        let (ctx, seen) = ctx_with_approver(FileChangeDecision::Reject);

        let err = WriteTool
            .execute(
                "call-write",
                serde_json::json!({
                    "path": path.to_string_lossy(),
                    "content": "new\n"
                }),
                &ctx,
            )
            .await
            .expect_err("rejected write should error");

        assert!(err.to_string().contains("rejected"));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "old\n");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].diff.contains("-old\n"));
        assert!(seen[0].diff.contains("+new\n"));
    }

    #[tokio::test]
    async fn edit_accept_writes_after_decision() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sample.txt");
        tokio::fs::write(&path, "alpha\nbeta\n").await.unwrap();
        let (ctx, seen) = ctx_with_approver(FileChangeDecision::Accept);

        let output = EditTool
            .execute(
                "call-edit",
                serde_json::json!({
                    "path": path.to_string_lossy(),
                    "old_string": "beta\n",
                    "new_string": "gamma\n"
                }),
                &ctx,
            )
            .await
            .expect("accepted edit should write");

        assert!(output.contains("edited"));
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "alpha\ngamma\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].tool_name, "edit");
        assert!(seen[0].diff.contains("-beta\n"));
        assert!(seen[0].diff.contains("+gamma\n"));
    }
}
