//! Built-in tools shipped with Ra.
//!
//! Each tool is `dyn Tool`-safe and registered at startup. The set is
//! filtered by the `[tools] builtin = [...]` allow-list in ra.toml — an
//! empty allow-list ships every tool whose external dependencies are
//! satisfied (e.g. `grep` only registers when `rg` is on PATH).
//!
//! - `read`  — read a file (ACP fs reverse-call when available)
//! - `write` — write a file
//! - `edit`  — replace a literal string inside a file (Claude Code shape)
//! - `bash`  — run a shell command (ACP terminal reverse-call when available)
//! - `grep`  — wrap ripgrep (rg) for code search
//! - `find`  — wrap fd for file discovery
//! - `ls`    — wrap eza (or exa) for directory listing

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
        match GrepTool::detect() {
            Some(t) => out.push(Arc::new(t)),
            None => eprintln!("[ra::tools] skipping 'grep': ripgrep (rg) not on PATH"),
        }
    }
    if want("find") {
        match FindTool::detect() {
            Some(t) => out.push(Arc::new(t)),
            None => eprintln!("[ra::tools] skipping 'find': fd / fdfind not on PATH"),
        }
    }
    if want("ls") {
        match LsTool::detect() {
            Some(t) => out.push(Arc::new(t)),
            None => eprintln!("[ra::tools] skipping 'ls': eza / exa not on PATH"),
        }
    }
    out
}
