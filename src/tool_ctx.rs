use crate::events::Event;
use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Per-call execution context handed to `Tool::execute`. Carries:
/// - the broadcast event sender (for `ToolCallUpdate` chunks),
/// - an optional ACP client handle so tools can call back into the host
///   editor (`fs/read_text_file`, `terminal/*`, `session/request_permission`),
/// - the session id, required by every reverse RPC,
/// - the session cwd for local tools that evaluate relative paths,
/// - an optional file-change approval hook used by interactive frontends,
/// - an optional [`RtkRewriter`](crate::tools::RtkRewriter) so shell commands
///   can pre-route through `rtk rewrite` for token compression.
#[derive(Clone)]
pub struct ToolCtx {
    pub events: broadcast::Sender<Event>,
    pub client: Option<Arc<dyn ClientHandle>>,
    pub session_id: Option<String>,
    pub cwd: PathBuf,
    pub file_approver: Option<Arc<dyn FileChangeApprover>>,
    pub rtk: crate::tools::RtkRewriter,
}

impl ToolCtx {
    /// Local-only context (used by the print-mode CLI). No reverse calls available.
    pub fn local(events: broadcast::Sender<Event>) -> Self {
        Self {
            events,
            client: None,
            session_id: None,
            cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
            file_approver: None,
            rtk: crate::tools::RtkRewriter::default(),
        }
    }
}

/// Candidate text change prepared by a filesystem mutation tool.
#[derive(Debug, Clone)]
pub struct FileChange {
    pub call_id: String,
    pub tool_name: String,
    pub path: String,
    pub old_content: Option<String>,
    pub new_content: String,
    pub diff: String,
    pub summary: String,
}

/// Decision returned by an interactive frontend for a prepared file change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeDecision {
    Accept,
    Reject,
}

/// Optional approval hook for file writes. Non-interactive sessions leave this
/// unset, preserving the existing direct-write behavior.
#[async_trait]
pub trait FileChangeApprover: Send + Sync {
    async fn approve_file_change(&self, change: FileChange) -> Result<FileChangeDecision>;
}

/// A description of how the host should execute a single shell command.
/// Returned by `ClientHandle::run_terminal` when reverse-call is available.
#[derive(Debug, Clone)]
pub struct TerminalRunResult {
    pub exit_code: Option<i32>,
    /// Combined stdout/stderr text harvested from the host terminal.
    pub output: String,
}

/// Permission request outcome, mirroring ACP's `RequestPermissionOutcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionOutcome {
    Allowed,
    Denied,
    /// User cancelled (closed dialog without choosing).
    Cancelled,
}

/// Trait implemented by the ACP server adapter. The Ra core never depends on
/// the ACP crate directly; it only sees this interface.
#[async_trait]
pub trait ClientHandle: Send + Sync {
    /// Read a text file from the host editor's filesystem view.
    async fn fs_read_text_file(
        &self,
        session_id: &str,
        path: &str,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> Result<String>;

    /// Write a text file via the host editor.
    async fn fs_write_text_file(&self, session_id: &str, path: &str, content: &str) -> Result<()>;

    /// Run a shell command via the host terminal and wait for it to exit.
    /// Bundles `terminal/create` + `wait_for_exit` + `terminal/output`
    /// + `terminal/release` into one logical operation.
    async fn run_terminal(&self, session_id: &str, command: &str) -> Result<TerminalRunResult>;

    /// Kill a still-running terminal previously created via `run_terminal`.
    /// Maps to ACP `terminal/kill`. Default implementation returns an error
    /// so existing handles compile; the ACP-backed handle overrides.
    async fn kill_terminal(&self, _session_id: &str, _terminal_id: &str) -> Result<()> {
        Err(anyhow::anyhow!(
            "kill_terminal not supported by this ClientHandle"
        ))
    }

    /// Ask the host to confirm a tool invocation. `tool_call_id` correlates
    /// with the previously emitted `ToolCall` notification; `title` and
    /// `description` are surfaced in the host's permission dialog.
    async fn request_permission(
        &self,
        session_id: &str,
        tool_call_id: &str,
        title: &str,
        description: &str,
    ) -> Result<PermissionOutcome>;
}
