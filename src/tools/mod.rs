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
//! - `git`   — run native git with argv-safe arguments
//! - `gh`    — run native GitHub CLI with argv-safe arguments

mod cli;
mod core;
mod fs;
mod rtk;

pub use cli::{GhTool, GitTool};
pub use core::{BashTool, ReadTool};
pub use fs::{EditTool, WriteTool};
pub use rtk::RtkRewriter;

use std::sync::Arc;

/// The unified tool trait. Re-exported so callers can `use ra::Tool`.
pub use self::core::Tool;

/// Build the default tool set, honouring the `[tools] builtin = [...]`
/// allow-list. An empty allow-list = ship every built-in tool.
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
    if want("git") {
        out.push(Arc::new(GitTool));
    }
    if want("gh") {
        out.push(Arc::new(GhTool));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin_names(allowlist: &[&str]) -> Vec<String> {
        let allowlist = allowlist.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        default_builtins(&allowlist)
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    #[test]
    fn default_catalog_includes_native_cli_tools() {
        let names = builtin_names(&[]);
        assert!(names.contains(&"git".to_string()));
        assert!(names.contains(&"gh".to_string()));
    }

    #[test]
    fn allowlist_can_select_native_cli_tools() {
        assert_eq!(builtin_names(&["git", "gh"]), vec!["git", "gh"]);
    }
}
