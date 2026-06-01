//! Built-in tools shipped with Ra.
//!
//! Each tool is `dyn Tool`-safe and registered at startup. The set is
//! filtered by the `[tools] builtin = [...]` allow-list in ra.toml — an
//! empty allow-list ships every tool.
//!
//! - `read`  — read a file (ACP fs reverse-call when available)
//! - `write` — write a file
//! - `edit`  — replace a literal string inside a file (Claude Code shape)
//! - `bash`  — run a shell command (ACP terminal reverse-call when available)
//! - `grep`  — search file contents (pure Rust, no external binary)
//! - `find`  — find files by name (pure Rust, no external binary)
//! - `ls`    — list directory contents (pure Rust, no external binary)

mod core;
mod fs;
mod rtk;
mod search;

pub use core::{BashTool, ReadTool};
pub use fs::{EditTool, WriteTool};
pub use rtk::RtkRewriter;
pub use search::{FindTool, GrepTool, LsTool};

use std::sync::Arc;

/// The unified tool trait. Re-exported so callers can `use ra::Tool`.
pub use self::core::Tool;

/// Build the default tool set, honouring the `[tools] builtin = [...]`
/// allow-list. An empty allow-list = ship every tool whose external
/// dependencies are present.
///
/// Each external-binary tool (`grep`/`find`/`ls`) probes for its binary
/// on PATH at startup; missing binaries are logged once and the tool
/// is silently dropped from the registry.
pub fn default_builtins(allowlist: &[String]) -> Vec<Arc<dyn Tool>> {
    let want = |name: &str| allowlist.is_empty() || allowlist.iter().any(|n| n == name);

    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    if want("read") {
        out.push(Arc::new(ReadTool));
    }
    if want("write") {
        out.push(Arc::new(WriteTool));
    }
    if want("edit") {
        out.push(Arc::new(EditTool));
    }
    if want("bash") {
        out.push(Arc::new(BashTool));
    }
    if want("grep") {
        out.push(Arc::new(GrepTool));
    }
    if want("find") {
        out.push(Arc::new(FindTool));
    }
    if want("ls") {
        out.push(Arc::new(LsTool));
    }
    out
}
