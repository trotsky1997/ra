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
//! - `ast_grep` — structural code search via ast-grep
//! - `git`   — run native git with argv-safe arguments
//! - `gh`    — run native GitHub CLI with argv-safe arguments
//! - `jq`    — run jq filters with argv-safe stdin
//! - `mise`  — run mise tasks and tests with argv-safe arguments
//! - `just`  — run just recipes with argv-safe arguments
//! - `wrkflw` — validate/run GitHub Actions workflows locally
//! - `mergiraf` — syntax-aware merge conflict resolution
//! - `grep`  — structured text search
//! - `glob`  — structured file discovery
//! - `ls`    — structured directory listing
//! - `fuzzy` — non-interactive fuzzy ranking/filtering
//! - `apply_patch` — controlled `git apply` wrapper
//! - `lsp`   — openlsp code intelligence (diagnostics, hover, references, …)
//! - `webfetch_fetch` — fetch one web page as Markdown via webfetch-cli
//! - `webfetch_crawl` — crawl bounded documentation via webfetch-cli
//! - `openspec` — drive the agent-own OpenSpec SDD loop via the openspec CLI
//! - `tmux_run` — run commands in persistent tmux sessions
//! - `tmux_send` — send input to tmux panes
//! - `tmux_capture` — capture tmux pane output
//! - `tmux_kill` — kill Ra-owned tmux targets
//! - `tmux_listen` — poll tmux panes for new output
//! - `tmux_wait` — block until a tmux event or timeout

mod cli;
mod core;
mod extended;
mod fs;
mod jq;
mod lsp;
mod mergiraf;
mod openspec;
mod rtk;
mod search;
mod task_workflow;
mod tmux;
mod webfetch;

pub use cli::{GhTool, GitTool};
pub use core::{BashTool, ReadTool};
pub use extended::{ApplyPatchTool, FuzzyTool, GlobTool, GrepTool, LsTool};
pub use fs::{EditTool, WriteTool};
pub use jq::JqTool;
pub use lsp::{resolve_openlsp_binary, LspTool};
pub use mergiraf::MergirafTool;
pub use openspec::OpenSpecTool;
pub use rtk::RtkRewriter;
pub use search::AstGrepTool;
pub use task_workflow::{JustTool, MiseTool, WrkflwTool};
pub use tmux::{
    TmuxCaptureTool, TmuxKillTool, TmuxListenTool, TmuxRunTool, TmuxSendTool, TmuxWaitTool,
};
pub use webfetch::{WebfetchCrawlTool, WebfetchFetchTool};

use crate::config::OpenlspSection;
use std::sync::Arc;

/// The unified tool trait. Re-exported so callers can `use ra::Tool`.
pub use self::core::Tool;

/// Build the default tool set, honouring the `[tools] builtin = [...]`
/// allow-list. An empty allow-list = ship every built-in tool.
/// `openlsp_cfg` is used to resolve the lsp tool binary; pass
/// `&OpenlspSection::default()` when no config is available.
pub fn default_builtins(allowlist: &[String]) -> Vec<Arc<dyn Tool>> {
    default_builtins_with_cfg(allowlist, &OpenlspSection::default())
}

/// Like `default_builtins` but accepts an explicit `OpenlspSection` so
/// callers that have loaded a config can pass it through.
pub fn default_builtins_with_cfg(
    allowlist: &[String],
    openlsp_cfg: &OpenlspSection,
) -> Vec<Arc<dyn Tool>> {
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
    if want("ast_grep") {
        out.push(Arc::new(AstGrepTool));
    }
    if want("git") {
        out.push(Arc::new(GitTool));
    }
    if want("gh") {
        out.push(Arc::new(GhTool));
    }
    if want("jq") {
        out.push(Arc::new(JqTool));
    }
    if want("mise") {
        out.push(Arc::new(MiseTool));
    }
    if want("just") {
        out.push(Arc::new(JustTool));
    }
    if want("wrkflw") {
        out.push(Arc::new(WrkflwTool));
    }
    if want("mergiraf") {
        out.push(Arc::new(MergirafTool));
    }
    if want("grep") {
        out.push(Arc::new(GrepTool));
    }
    if want("glob") {
        out.push(Arc::new(GlobTool));
    }
    if want("ls") {
        out.push(Arc::new(LsTool));
    }
    if want("fuzzy") {
        out.push(Arc::new(FuzzyTool));
    }
    if want("apply_patch") {
        out.push(Arc::new(ApplyPatchTool));
    }
    if want("webfetch_fetch") {
        out.push(Arc::new(WebfetchFetchTool));
    }
    if want("webfetch_crawl") {
        out.push(Arc::new(WebfetchCrawlTool));
    }
    if want("openspec") {
        out.push(Arc::new(OpenSpecTool));
    }
    if want("tmux_run") {
        out.push(Arc::new(TmuxRunTool));
    }
    if want("tmux_send") {
        out.push(Arc::new(TmuxSendTool));
    }
    if want("tmux_capture") {
        out.push(Arc::new(TmuxCaptureTool));
    }
    if want("tmux_kill") {
        out.push(Arc::new(TmuxKillTool));
    }
    if want("tmux_listen") {
        out.push(Arc::new(TmuxListenTool));
    }
    if want("tmux_wait") {
        out.push(Arc::new(TmuxWaitTool));
    }
    if want("lsp") {
        if let Some(binary) = resolve_openlsp_binary(openlsp_cfg) {
            out.push(Arc::new(LspTool {
                binary,
                workspace_root: openlsp_cfg.workspace_root.clone(),
                timeout_secs: openlsp_cfg.timeout,
            }));
        }
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
        assert!(names.contains(&"ast_grep".to_string()));
        assert!(names.contains(&"git".to_string()));
        assert!(names.contains(&"gh".to_string()));
    }

    #[test]
    fn default_catalog_includes_extended_tools() {
        let names = builtin_names(&[]);
        for name in ["grep", "glob", "ls", "fuzzy", "apply_patch"] {
            assert!(
                names.contains(&name.to_string()),
                "missing {name}: {names:?}"
            );
        }
    }

    #[test]
    fn allowlist_can_select_native_cli_tools() {
        assert_eq!(builtin_names(&["git", "gh"]), vec!["git", "gh"]);
    }

    #[test]
    fn default_catalog_includes_jq_tool() {
        let names = builtin_names(&[]);
        assert!(names.contains(&"jq".to_string()));
    }

    #[test]
    fn allowlist_can_select_jq_exactly() {
        assert_eq!(builtin_names(&["jq"]), vec!["jq"]);
    }

    #[test]
    fn default_catalog_includes_task_workflow_tools() {
        let names = builtin_names(&[]);
        for name in ["mise", "just", "wrkflw", "mergiraf"] {
            assert!(
                names.contains(&name.to_string()),
                "missing {name}: {names:?}"
            );
        }
    }

    #[test]
    fn allowlist_can_select_task_workflow_tools_exactly() {
        assert_eq!(builtin_names(&["mise"]), vec!["mise"]);
        assert_eq!(builtin_names(&["just"]), vec!["just"]);
        assert_eq!(builtin_names(&["wrkflw"]), vec!["wrkflw"]);
        assert_eq!(builtin_names(&["mergiraf"]), vec!["mergiraf"]);
    }

    #[test]
    fn allowlist_can_select_extended_tools_exactly() {
        assert_eq!(builtin_names(&["grep"]), vec!["grep"]);
    }

    #[test]
    fn default_catalog_includes_webfetch_tools() {
        let names = builtin_names(&[]);
        assert!(names.contains(&"webfetch_fetch".to_string()));
        assert!(names.contains(&"webfetch_crawl".to_string()));
    }

    #[test]
    fn allowlist_can_select_webfetch_tools_exactly() {
        assert_eq!(builtin_names(&["webfetch_fetch"]), vec!["webfetch_fetch"]);
    }

    #[test]
    fn default_catalog_includes_openspec_tool() {
        let names = builtin_names(&[]);
        assert!(names.contains(&"openspec".to_string()));
    }

    #[test]
    fn allowlist_can_select_openspec_exactly() {
        assert_eq!(builtin_names(&["openspec"]), vec!["openspec"]);
    }

    #[test]
    fn default_catalog_includes_tmux_tools() {
        let names = builtin_names(&[]);
        for name in [
            "tmux_run",
            "tmux_send",
            "tmux_capture",
            "tmux_kill",
            "tmux_listen",
            "tmux_wait",
        ] {
            assert!(
                names.contains(&name.to_string()),
                "missing {name}: {names:?}"
            );
        }
    }

    #[test]
    fn allowlist_can_select_tmux_tools_exactly() {
        assert_eq!(builtin_names(&["tmux_capture"]), vec!["tmux_capture"]);
    }

    #[test]
    fn lsp_absent_when_disabled_in_config() {
        let cfg = OpenlspSection {
            enabled: false,
            binary: Some("true".to_string()),
            workspace_root: None,
            timeout: 30.0,
        };
        let allowlist: Vec<String> = vec![];
        let names: Vec<String> = default_builtins_with_cfg(&allowlist, &cfg)
            .into_iter()
            .map(|t| t.name().to_string())
            .collect();
        assert!(!names.contains(&"lsp".to_string()));
    }

    #[test]
    fn lsp_present_when_binary_resolves() {
        let cfg = OpenlspSection {
            enabled: true,
            binary: Some("true".to_string()),
            workspace_root: None,
            timeout: 30.0,
        };
        let allowlist: Vec<String> = vec![];
        let names: Vec<String> = default_builtins_with_cfg(&allowlist, &cfg)
            .into_iter()
            .map(|t| t.name().to_string())
            .collect();
        assert!(names.contains(&"lsp".to_string()));
    }

    #[test]
    fn allowlist_excludes_lsp_when_not_listed() {
        let cfg = OpenlspSection {
            enabled: true,
            binary: Some("true".to_string()),
            workspace_root: None,
            timeout: 30.0,
        };
        let allowlist = vec!["read".to_string(), "bash".to_string()];
        let names: Vec<String> = default_builtins_with_cfg(&allowlist, &cfg)
            .into_iter()
            .map(|t| t.name().to_string())
            .collect();
        assert!(!names.contains(&"lsp".to_string()));
    }
}
