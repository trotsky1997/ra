//! Built-in tools shipped with Ra.
//!
//! Each tool is `dyn Tool`-safe and registered at startup. The set is
//! filtered by the `[tools] builtin = [...]` allow-list in ra.toml.
//! An empty allow-list ships every built-in tool.
//!
//! - `read`  — read a file (ACP fs reverse-call when available)
//! - `write` — write a file
//! - `edit`  — replace a literal string inside a file (Claude Code shape)
//! - `bash`  — run a shell command (ACP terminal reverse-call when available)

mod core;
mod fs;
mod rtk;

pub use core::{BashTool, ReadTool};
pub use fs::{EditTool, WriteTool};
pub use rtk::RtkRewriter;

use std::sync::Arc;

/// The unified tool trait. Re-exported so callers can `use ra::Tool`.
pub use self::core::Tool;

/// Build the default tool set, honouring the `[tools] builtin = [...]`
/// allow-list. An empty allow-list = ship every built-in tool.
pub fn default_builtins(allowlist: &[String]) -> Vec<Arc<dyn Tool>> {
    const KNOWN_BUILTINS: &[&str] = &["read", "write", "edit", "bash"];

    if !allowlist.is_empty() {
        for name in allowlist {
            if !KNOWN_BUILTINS.contains(&name.as_str()) {
                eprintln!(
                    "[ra::tools] unknown builtin tool '{name}' in [tools] builtin allow-list; ignoring"
                );
            }
        }
    }

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
    out
}
