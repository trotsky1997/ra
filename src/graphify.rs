//! Native [Graphify](https://github.com/safishamsi/graphify) support.
//!
//! Graphify writes a durable `graphify-out/graph.json` file using
//! NetworkX's node-link JSON shape (`nodes` plus `links`; some raw
//! outputs use `edges`). Ra treats that file as an agent-owned R2A graph
//! service: sessions can ensure freshness, build/update the low-cost AST
//! graph, map requirements and changed files into impact/verification
//! context, and then use native query/path/explain reads. We
//! intentionally do not shell out to the `graphify` CLI for reads; the
//! graph file is the stable API.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::process::Command;

use crate::tool_ctx::ToolCtx;
use crate::tools::Tool;

const DEFAULT_GRAPHIFY_OUT: &str = "graphify-out";
const MAX_GRAPH_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_DEPTH: usize = 2;
const DEFAULT_MAX_NODES: usize = 24;
const DEFAULT_MAX_NEIGHBORS: usize = 20;
const DEFAULT_UPDATE_TIMEOUT_SECS: u64 = 600;
const MAX_FRESHNESS_SCAN_FILES: usize = 8_000;
const MAX_TOOL_OUTPUT_CHARS: usize = 12_000;
const SKIP_FRESHNESS_DIRS: &[&str] = &[
    ".git",
    "graphify-out",
    "target",
    "node_modules",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".next",
    "dist",
    "build",
];

/// A discovered Graphify project rooted at the directory that owns
/// `graphify-out/`.
#[derive(Debug, Clone)]
pub struct GraphifyProject {
    pub root: PathBuf,
    pub graph_path: PathBuf,
    pub report_path: Option<PathBuf>,
    pub html_path: Option<PathBuf>,
    pub stats: GraphStats,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub hyperedge_count: usize,
    pub community_count: usize,
    pub file_types: Vec<(String, usize)>,
}

/// The agent-owned Graphify service state for one Ra session. Unlike
/// `GraphifyProject`, this exists even when the graph is missing or stale,
/// so the agent can ensure/update it during the R2A flow.
#[derive(Debug, Clone)]
pub struct GraphifyWorkflow {
    pub root: PathBuf,
    pub graph_path: PathBuf,
    pub project: Option<GraphifyProject>,
    pub status: GraphifyStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphifyStatus {
    pub kind: GraphifyStatusKind,
    pub detail: Option<String>,
    pub newest_input: Option<PathBuf>,
    pub scanned_files: usize,
    pub scan_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphifyStatusKind {
    Missing,
    Ready,
    Stale,
    Invalid,
}

impl GraphifyStatusKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Ready => "ready",
            Self::Stale => "stale",
            Self::Invalid => "invalid",
        }
    }
}

impl GraphifyProject {
    /// Render the progressive-disclosure prompt section for callers that
    /// already have a parsed project. The workflow wrapper is the primary
    /// prompt surface because it also covers missing/stale graphs.
    pub fn build_system_prompt_section(&self) -> Option<String> {
        if self.stats.node_count == 0 {
            return None;
        }
        GraphifyWorkflow::from_project(self.clone()).build_system_prompt_section()
    }
}

impl GraphifyWorkflow {
    pub fn from_project(project: GraphifyProject) -> Self {
        let loaded: Result<GraphifyProject> = Ok(project.clone());
        let status = build_status(&project.root, &project.graph_path, Some(&loaded));
        Self {
            root: project.root.clone(),
            graph_path: project.graph_path.clone(),
            project: Some(project),
            status,
        }
    }

    /// R2A prompt section. The model should treat Graphify as a project
    /// context-maintenance service, not as a user-operated command line.
    pub fn build_system_prompt_section(&self) -> Option<String> {
        let mut buf = String::new();
        buf.push_str("# Graphify R2A Graph Service\n\n");
        buf.push_str(
            "Graphify is Ra's project semantic graph for the requirement -> SWE -> artifact \
             workflow. Use it as shared project memory: ensure freshness at requirement intake, \
             query impact before planning, use paths/explanations while editing, derive \
             verification candidates from the impact subgraph, and include traceability in the \
             delivery summary. Do not require the user to run Graphify first.\n\n",
        );
        buf.push_str(&format!(
            "- root: `{}`\n- graph: `{}`\n- status: {}\n",
            self.root.display(),
            self.graph_path.display(),
            self.status.kind.as_str()
        ));

        match self.status.kind {
            GraphifyStatusKind::Ready => {
                if let Some(project) = &self.project {
                    buf.push_str(&format!(
                        "- nodes: {}\n- edges: {}\n",
                        project.stats.node_count, project.stats.edge_count
                    ));
                    if project.stats.hyperedge_count > 0 {
                        buf.push_str(&format!(
                            "- hyperedges: {}\n",
                            project.stats.hyperedge_count
                        ));
                    }
                    if project.stats.community_count > 0 {
                        buf.push_str(&format!(
                            "- communities: {}\n",
                            project.stats.community_count
                        ));
                    }
                    if !project.stats.file_types.is_empty() {
                        let parts = project
                            .stats
                            .file_types
                            .iter()
                            .map(|(kind, count)| format!("{kind}={count}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        buf.push_str(&format!("- file types: {parts}\n"));
                    }
                }
            }
            GraphifyStatusKind::Stale => {
                if let Some(newest) = &self.status.newest_input {
                    buf.push_str(&format!("- newer input: `{}`\n", newest.display()));
                }
                buf.push_str("- before relying on the graph for current-code answers, call `graphify_ensure` with `refresh=true` or `graphify_update`.\n");
            }
            GraphifyStatusKind::Missing => {
                buf.push_str("- no graph exists yet; call `graphify_ensure` to get the build path, or `graphify_update` to build the low-cost AST graph when appropriate.\n");
            }
            GraphifyStatusKind::Invalid => {
                if let Some(detail) = &self.status.detail {
                    buf.push_str(&format!("- parse/load problem: {detail}\n"));
                }
                buf.push_str(
                    "- call `graphify_update` to regenerate before graph-backed planning.\n",
                );
            }
        }

        buf.push_str(
            "\nR2A usage:\n\
             - Intake/planning: call `graphify_ensure`, then `graphify_impact` with the requirement.\n\
             - Implementation: use `graphify_query`, `graphify_path`, and `graphify_explain` to stay inside the affected subgraph.\n\
             - Verification/report: call `graphify_impact` with changed files to derive tests/docs and traceability.\n\
             - If Graphify is not installed, the tools return an install command and a no-shell command plan.\n\n",
        );
        Some(buf)
    }
}

/// Resolve a Graphify graph from config and current working directory.
pub fn from_config(cfg: &crate::config::GraphifySection, cwd: &Path) -> Option<GraphifyProject> {
    workflow_from_config(cfg, cwd).and_then(|workflow| workflow.project)
}

/// Resolve the agent-owned Graphify workflow from config and cwd. When
/// enabled, this returns a workflow even if the graph is missing, so the
/// agent can ensure/update it inside the session.
pub fn workflow_from_config(
    cfg: &crate::config::GraphifySection,
    cwd: &Path,
) -> Option<GraphifyWorkflow> {
    if !cfg.enabled {
        return None;
    }
    if let Some(path) = &cfg.path {
        let path = resolve_config_path(path, cwd);
        let root = root_for_graph_path(&path, cwd);
        return Some(workflow_from_target(root, path));
    }
    if !cfg.discover {
        return None;
    }
    let root = find_project_root(cwd);
    let graph_path = find_graph_path(cwd).unwrap_or_else(|| default_graph_path_for_root(&root));
    Some(workflow_from_target(
        root_for_graph_path(&graph_path, &root),
        graph_path,
    ))
}

/// Locate the nearest `graphify-out/graph.json`, walking cwd up to the
/// git root, and parse a lightweight project summary.
pub fn discover(cwd: &Path) -> Option<GraphifyProject> {
    let graph_path = find_graph_path(cwd)?;
    match load_project_from_graph_path(&graph_path) {
        Ok(project) => Some(project),
        Err(e) => {
            eprintln!("[ra::graphify] {}: {e:#}", graph_path.display());
            None
        }
    }
}

/// Build native Graphify tools bound to this project's graph path.
pub fn tools_for_project(project: &GraphifyProject) -> Vec<Arc<dyn Tool>> {
    tools_for_workflow(&GraphifyWorkflow::from_project(project.clone()))
}

/// Build Graphify R2A tools bound to this workflow. The ensure/update/impact
/// tools are present even when the graph does not exist yet.
pub fn tools_for_workflow(workflow: &GraphifyWorkflow) -> Vec<Arc<dyn Tool>> {
    let root = workflow.root.clone();
    let graph_path = workflow.graph_path.clone();
    vec![
        Arc::new(GraphifyEnsureTool {
            root: root.clone(),
            graph_path: graph_path.clone(),
        }),
        Arc::new(GraphifyImpactTool {
            root: root.clone(),
            graph_path: graph_path.clone(),
        }),
        Arc::new(GraphifyUpdateTool {
            root: root.clone(),
            graph_path: graph_path.clone(),
        }),
        Arc::new(GraphifyQueryTool {
            root: root.clone(),
            graph_path: graph_path.clone(),
        }),
        Arc::new(GraphifyPathTool {
            root: root.clone(),
            graph_path: graph_path.clone(),
        }),
        Arc::new(GraphifyExplainTool { root, graph_path }),
    ]
}

/// Convenience helper for startup paths.
pub fn tools_from_config(cfg: &crate::config::GraphifySection, cwd: &Path) -> Vec<Arc<dyn Tool>> {
    workflow_from_config(cfg, cwd)
        .as_ref()
        .map(tools_for_workflow)
        .unwrap_or_default()
}

fn resolve_config_path(raw: &str, cwd: &Path) -> PathBuf {
    let expanded = shellexpand::tilde(raw).to_string();
    let mut path = PathBuf::from(expanded);
    if !path.is_absolute() {
        path = cwd.join(path);
    }
    if path.is_dir() {
        path.join("graph.json")
    } else {
        path
    }
}

fn workflow_from_target(root: PathBuf, graph_path: PathBuf) -> GraphifyWorkflow {
    let project_result = if graph_path.is_file() {
        Some(load_project_from_graph_path(&graph_path))
    } else {
        None
    };
    let project = project_result
        .as_ref()
        .and_then(|result| result.as_ref().ok().cloned());
    let status = build_status(&root, &graph_path, project_result.as_ref());
    GraphifyWorkflow {
        root,
        graph_path,
        project,
        status,
    }
}

fn root_for_graph_path(graph_path: &Path, fallback: &Path) -> PathBuf {
    graph_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| fallback.to_path_buf())
}

fn default_graph_path_for_root(root: &Path) -> PathBuf {
    let out_dir = std::env::var("GRAPHIFY_OUT").unwrap_or_else(|_| DEFAULT_GRAPHIFY_OUT.into());
    let out_path = PathBuf::from(out_dir);
    if out_path.is_absolute() {
        out_path.join("graph.json")
    } else {
        root.join(out_path).join("graph.json")
    }
}

fn find_project_root(cwd: &Path) -> PathBuf {
    let mut current = cwd.to_path_buf();
    let mut depth = 0usize;
    loop {
        if current.join(".git").exists() {
            return current;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
        depth += 1;
        if depth > 16 {
            break;
        }
    }
    cwd.to_path_buf()
}

fn find_graph_path(cwd: &Path) -> Option<PathBuf> {
    let out_dir = std::env::var("GRAPHIFY_OUT").unwrap_or_else(|_| DEFAULT_GRAPHIFY_OUT.into());
    let mut current = cwd.to_path_buf();
    let mut depth = 0usize;
    loop {
        let candidate = current.join(&out_dir).join("graph.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        if current.join(".git").exists() {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
        depth += 1;
        if depth > 16 {
            break;
        }
    }
    None
}

fn load_project_from_graph_path(graph_path: &Path) -> Result<GraphifyProject> {
    let graph = load_graph(graph_path)?;
    let out_dir = graph_path
        .parent()
        .ok_or_else(|| anyhow!("graph path has no parent"))?;
    let root = out_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| out_dir.to_path_buf());
    let report_path = existing_file(&out_dir.join("GRAPH_REPORT.md"));
    let html_path = existing_file(&out_dir.join("graph.html"));
    Ok(GraphifyProject {
        root,
        graph_path: graph_path.to_path_buf(),
        report_path,
        html_path,
        stats: graph.stats(),
    })
}

fn existing_file(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

fn build_status(
    root: &Path,
    graph_path: &Path,
    project_result: Option<&Result<GraphifyProject>>,
) -> GraphifyStatus {
    if !graph_path.is_file() {
        return GraphifyStatus {
            kind: GraphifyStatusKind::Missing,
            detail: None,
            newest_input: None,
            scanned_files: 0,
            scan_truncated: false,
        };
    }

    if let Some(Err(e)) = project_result {
        return GraphifyStatus {
            kind: GraphifyStatusKind::Invalid,
            detail: Some(e.to_string()),
            newest_input: None,
            scanned_files: 0,
            scan_truncated: false,
        };
    }

    let graph_mtime = std::fs::metadata(graph_path)
        .and_then(|m| m.modified())
        .ok();
    let scan = scan_newest_input(root, graph_path);
    let stale = match (graph_mtime, scan.newest_mtime) {
        (Some(graph), Some(input)) => input > graph,
        _ => false,
    };
    GraphifyStatus {
        kind: if stale {
            GraphifyStatusKind::Stale
        } else {
            GraphifyStatusKind::Ready
        },
        detail: None,
        newest_input: scan.newest_input,
        scanned_files: scan.scanned_files,
        scan_truncated: scan.truncated,
    }
}

struct FreshnessScan {
    newest_input: Option<PathBuf>,
    newest_mtime: Option<SystemTime>,
    scanned_files: usize,
    truncated: bool,
}

fn scan_newest_input(root: &Path, graph_path: &Path) -> FreshnessScan {
    let graph_out = graph_path.parent().map(Path::to_path_buf);
    let mut newest_input = None;
    let mut newest_mtime = None;
    let mut scanned_files = 0usize;
    let mut truncated = false;
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if scanned_files >= MAX_FRESHNESS_SCAN_FILES {
                truncated = true;
                break;
            }
            let path = entry.path();
            if graph_out
                .as_ref()
                .is_some_and(|out| path == *out || path.starts_with(out))
            {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if should_skip_freshness_dir(&path) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !ft.is_file() || path == graph_path {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let Ok(mtime) = meta.modified() else {
                continue;
            };
            scanned_files += 1;
            if newest_mtime.map_or(true, |current| mtime > current) {
                newest_mtime = Some(mtime);
                newest_input = Some(path);
            }
        }
        if truncated {
            break;
        }
    }

    FreshnessScan {
        newest_input,
        newest_mtime,
        scanned_files,
        truncated,
    }
}

fn should_skip_freshness_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| SKIP_FRESHNESS_DIRS.iter().any(|skip| name == *skip))
}

fn graph_unavailable_message(root: &Path, graph_path: &Path, err: &anyhow::Error) -> String {
    format!(
        "Graphify graph is not ready: {err:#}\n\
         Root: `{}`\n\
         Expected graph: `{}`\n\n\
         R2A path:\n\
         - call `graphify_ensure` to check install/freshness and decide whether to refresh;\n\
         - call `graphify_update` to build or refresh the low-cost AST graph;\n\
         - then retry this graph-backed query.",
        root.display(),
        graph_path.display()
    )
}

fn maybe_stale_prefix(root: &Path, graph_path: &Path) -> String {
    let workflow = workflow_from_target(root.to_path_buf(), graph_path.to_path_buf());
    if workflow.status.kind != GraphifyStatusKind::Stale {
        return String::new();
    }
    let mut out = String::from(
        "Warning: Graphify graph appears stale; consider `graphify_ensure` with `refresh=true` before relying on this for current-code answers.\n",
    );
    if let Some(newest) = workflow.status.newest_input {
        out.push_str(&format!("Newest input: `{}`\n\n", newest.display()));
    } else {
        out.push('\n');
    }
    out
}

fn truncate_tool_output(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw).into_owned();
    if text.chars().count() <= MAX_TOOL_OUTPUT_CHARS {
        return text;
    }
    let clipped = text.chars().take(MAX_TOOL_OUTPUT_CHARS).collect::<String>();
    format!("{clipped}\n... truncated ...")
}

fn format_command(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./:=+".contains(c))
            {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_ensure(
    workflow: &GraphifyWorkflow,
    phase: Option<&str>,
    requirement: Option<&str>,
) -> String {
    let cli = which::which("graphify").ok();
    let mut out = String::new();
    out.push_str("Graphify R2A graph status\n");
    out.push_str(&format!("- phase: {}\n", phase.unwrap_or("intake")));
    if let Some(req) = requirement.filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("- requirement: {req}\n"));
    }
    out.push_str(&format!(
        "- root: `{}`\n- graph: `{}`\n- status: {}\n",
        workflow.root.display(),
        workflow.graph_path.display(),
        workflow.status.kind.as_str()
    ));
    if let Some(project) = &workflow.project {
        out.push_str(&format!(
            "- graph size: {} node(s), {} edge(s)\n",
            project.stats.node_count, project.stats.edge_count
        ));
    }
    if workflow.status.scan_truncated {
        out.push_str(&format!(
            "- freshness scan: truncated after {} file(s)\n",
            workflow.status.scanned_files
        ));
    }
    if let Some(newest) = &workflow.status.newest_input {
        out.push_str(&format!("- newest input: `{}`\n", newest.display()));
    }
    if let Some(detail) = &workflow.status.detail {
        out.push_str(&format!("- detail: {detail}\n"));
    }
    match cli {
        Some(ref path) => out.push_str(&format!("- graphify CLI: `{}`\n", path.display())),
        None => out.push_str("- graphify CLI: not found\n"),
    }

    let update_cmd = vec![
        "graphify".to_string(),
        "update".to_string(),
        workflow.root.display().to_string(),
        "--no-cluster".to_string(),
    ];
    let extract_cmd = vec![
        "graphify".to_string(),
        "extract".to_string(),
        workflow.root.display().to_string(),
        "--backend".to_string(),
        "<backend>".to_string(),
    ];

    out.push_str("\nRecommended R2A path:\n");
    match workflow.status.kind {
        GraphifyStatusKind::Ready => {
            out.push_str("- graph is ready; call `graphify_impact` before plan/verify/report.\n");
            out.push_str("- use `graphify_query`, `graphify_path`, and `graphify_explain` while editing to stay within the affected subgraph.\n");
        }
        GraphifyStatusKind::Stale => {
            out.push_str("- graph is stale; call `graphify_update` with default mode before current-code planning.\n");
            out.push_str(&format!(
                "- equivalent low-cost command: `{}`\n",
                format_command(&update_cmd)
            ));
        }
        GraphifyStatusKind::Missing => {
            out.push_str("- graph is missing; build the low-cost AST graph with `graphify_update` when graph context would reduce repo-wide reading.\n");
            out.push_str(&format!(
                "- equivalent low-cost command: `{}`\n",
                format_command(&update_cmd)
            ));
        }
        GraphifyStatusKind::Invalid => {
            out.push_str("- graph cannot be parsed; regenerate with `graphify_update` before using graph-backed impact analysis.\n");
            out.push_str(&format!(
                "- equivalent low-cost command: `{}`\n",
                format_command(&update_cmd)
            ));
        }
    }
    if cli.is_none() {
        out.push_str("- install path: `uv tool install graphifyy` (or `pipx install graphifyy`), then retry `graphify_update`.\n");
    }
    out.push_str("- privacy/cost boundary: default `graphify_update` is AST-only and local; semantic extraction may send project material to the selected backend.\n");
    out.push_str(&format!(
        "- semantic path, only when needed and configured: `{}`\n",
        format_command(&extract_cmd)
    ));
    out
}

async fn run_graphify_update(
    root: &Path,
    graph_path: &Path,
    params: GraphifyUpdateParams,
) -> Result<String> {
    let Some(bin) = which::which("graphify").ok() else {
        return Ok(format!(
            "Graphify CLI is not installed, so no command was run.\n\
             Install with `uv tool install graphifyy` or `pipx install graphifyy`, then retry.\n\
             Default local build command: `{}`",
            format_command(&[
                "graphify".into(),
                "update".into(),
                root.display().to_string(),
                "--no-cluster".into(),
            ])
        ));
    };

    let mut mode = params.mode.unwrap_or_else(|| "auto".into()).to_lowercase();
    if mode == "auto" {
        mode = if params.backend.is_some() || params.model.is_some() {
            "extract".into()
        } else {
            "update".into()
        };
    }
    let mut args = Vec::new();
    match mode.as_str() {
        "update" => {
            args.push("update".to_string());
            args.push(root.display().to_string());
            if params.force.unwrap_or(false) {
                args.push("--force".into());
            }
            if params.no_cluster.unwrap_or(true) {
                args.push("--no-cluster".into());
            }
        }
        "extract" => {
            args.push("extract".to_string());
            args.push(root.display().to_string());
            if let Some(backend) = params.backend {
                args.push("--backend".into());
                args.push(backend);
            }
            if let Some(model) = params.model {
                args.push("--model".into());
                args.push(model);
            }
            if params.no_cluster.unwrap_or(false) {
                args.push("--no-cluster".into());
            }
        }
        "cluster-only" | "cluster_only" => {
            args.push("cluster-only".to_string());
            args.push(root.display().to_string());
            args.push("--graph".into());
            args.push(graph_path.display().to_string());
            if params.no_viz.unwrap_or(true) {
                args.push("--no-viz".into());
            }
        }
        other => {
            return Ok(format!(
                "Unsupported Graphify update mode `{other}`. Use `auto`, `update`, `extract`, or `cluster-only`."
            ));
        }
    }

    let display_args = std::iter::once("graphify".to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();
    let timeout_secs = params
        .timeout_sec
        .unwrap_or(DEFAULT_UPDATE_TIMEOUT_SECS)
        .clamp(5, 60 * 60);

    let mut cmd = Command::new(bin);
    cmd.args(&args)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(out_dir) = graph_path.parent() {
        cmd.env("GRAPHIFY_OUT", out_dir);
    }

    let waited = tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output()).await;
    let output = match waited {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Ok(format!(
                "Failed to run `{}`: {e:#}",
                format_command(&display_args)
            ));
        }
        Err(_) => {
            return Ok(format!(
                "Timed out after {timeout_secs}s running `{}`.",
                format_command(&display_args)
            ));
        }
    };

    let refreshed = workflow_from_target(root.to_path_buf(), graph_path.to_path_buf());
    let mut out = String::new();
    out.push_str(&format!("Command: `{}`\n", format_command(&display_args)));
    out.push_str(&format!("Exit status: {}\n", output.status));
    out.push_str(&format!(
        "Graph status after command: {}\n",
        refreshed.status.kind.as_str()
    ));
    if !output.stdout.is_empty() {
        out.push_str("\nstdout:\n");
        out.push_str(&truncate_tool_output(&output.stdout));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    if !output.stderr.is_empty() {
        out.push_str("\nstderr:\n");
        out.push_str(&truncate_tool_output(&output.stderr));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    if !output.status.success() {
        out.push_str("\nGraphify did not complete successfully; keep using normal Ra file tools as fallback and retry after fixing the command output above.\n");
    }
    Ok(out)
}

// ---------- native tools -------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphifyEnsureParams {
    /// R2A phase: intake, plan, implement, verify, or report.
    #[serde(default)]
    pub phase: Option<String>,
    /// Requirement or task summary the agent is about to work on.
    #[serde(default)]
    pub requirement: Option<String>,
    /// If true, run Graphify to build/refresh when the graph is missing,
    /// stale, or invalid. Default false: report an executable path only.
    #[serde(default)]
    pub refresh: Option<bool>,
    /// If true, use full semantic extraction (`graphify extract`) rather
    /// than the default AST-only update path. This may use an LLM backend.
    #[serde(default)]
    pub semantic: Option<bool>,
    /// Optional Graphify LLM backend for semantic extraction.
    #[serde(default)]
    pub backend: Option<String>,
    /// Optional timeout for a refresh/build command. Default 600 seconds.
    #[serde(default)]
    pub timeout_sec: Option<u64>,
}

pub struct GraphifyEnsureTool {
    root: PathBuf,
    graph_path: PathBuf,
}

#[async_trait]
impl Tool for GraphifyEnsureTool {
    fn name(&self) -> &str {
        "graphify_ensure"
    }

    fn description(&self) -> &str {
        "Ensure Ra's Graphify R2A graph is available and fresh; optionally build or refresh it."
    }

    fn schema(&self) -> Value {
        serde_json::to_value(schema_for!(GraphifyEnsureParams)).unwrap()
    }

    async fn execute(&self, _call_id: &str, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("graphify_ensure");
        let params: GraphifyEnsureParams =
            serde_json::from_value(input).context("invalid params for graphify_ensure")?;
        let mut workflow = workflow_from_target(self.root.clone(), self.graph_path.clone());
        let mut out = render_ensure(
            &workflow,
            params.phase.as_deref(),
            params.requirement.as_deref(),
        );

        if params.refresh.unwrap_or(false) && workflow.status.kind != GraphifyStatusKind::Ready {
            let semantic = params.semantic.unwrap_or(false) || params.backend.is_some();
            let update_params = GraphifyUpdateParams {
                mode: Some(if semantic {
                    "extract".into()
                } else {
                    "update".into()
                }),
                backend: params.backend,
                model: None,
                force: None,
                no_cluster: Some(!semantic),
                no_viz: None,
                timeout_sec: params.timeout_sec,
            };
            out.push_str("\n\nRefresh result:\n");
            out.push_str(&run_graphify_update(&self.root, &self.graph_path, update_params).await?);
            workflow = workflow_from_target(self.root.clone(), self.graph_path.clone());
            out.push_str("\n\nPost-refresh status:\n");
            out.push_str(&render_ensure(
                &workflow,
                params.phase.as_deref(),
                params.requirement.as_deref(),
            ));
        }

        Ok(out)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphifyImpactParams {
    /// R2A phase: intake, plan, implement, verify, or report.
    #[serde(default)]
    pub phase: Option<String>,
    /// Requirement, bug, or artifact goal to map into the project graph.
    #[serde(default)]
    pub requirement: Option<String>,
    /// Files already touched or expected to change; used for verification
    /// and traceability impact queries.
    #[serde(default)]
    pub changed_files: Vec<String>,
    /// Traversal depth from matched requirement/change seeds. Default 2.
    #[serde(default)]
    pub depth: Option<usize>,
    /// Maximum nodes to include in the impact subgraph. Default 32.
    #[serde(default)]
    pub max_nodes: Option<usize>,
}

pub struct GraphifyImpactTool {
    root: PathBuf,
    graph_path: PathBuf,
}

#[async_trait]
impl Tool for GraphifyImpactTool {
    fn name(&self) -> &str {
        "graphify_impact"
    }

    fn description(&self) -> &str {
        "Map a requirement or file change set through the Graphify graph for R2A planning, verification, and traceability."
    }

    fn schema(&self) -> Value {
        serde_json::to_value(schema_for!(GraphifyImpactParams)).unwrap()
    }

    async fn execute(&self, _call_id: &str, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("graphify_impact");
        let params: GraphifyImpactParams =
            serde_json::from_value(input).context("invalid params for graphify_impact")?;
        let workflow = workflow_from_target(self.root.clone(), self.graph_path.clone());
        if workflow.status.kind != GraphifyStatusKind::Ready
            && workflow.status.kind != GraphifyStatusKind::Stale
        {
            let mut out = String::from("Graphify impact is not available yet.\n\n");
            out.push_str(&render_ensure(
                &workflow,
                params.phase.as_deref(),
                params.requirement.as_deref(),
            ));
            return Ok(out);
        }

        let graph = match load_graph(&self.graph_path) {
            Ok(graph) => graph,
            Err(e) => return Ok(graph_unavailable_message(&self.root, &self.graph_path, &e)),
        };

        let mut out = maybe_stale_prefix(&self.root, &self.graph_path);
        out.push_str(&graph.impact(
            params.phase.as_deref().unwrap_or("intake"),
            params.requirement.as_deref(),
            &params.changed_files,
            params.depth.unwrap_or(DEFAULT_DEPTH),
            params.max_nodes.unwrap_or(32),
        ));
        Ok(out)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphifyUpdateParams {
    /// auto, update, extract, or cluster-only. Default auto chooses AST-only
    /// update unless semantic/backend is requested.
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional Graphify LLM backend for semantic extraction.
    #[serde(default)]
    pub backend: Option<String>,
    /// Optional model override for semantic extraction.
    #[serde(default)]
    pub model: Option<String>,
    /// Pass --force to Graphify update after refactors that legitimately
    /// shrink the graph.
    #[serde(default)]
    pub force: Option<bool>,
    /// Skip clustering/report generation. Default true for the low-cost
    /// update path to avoid hidden LLM/cost surfaces.
    #[serde(default)]
    pub no_cluster: Option<bool>,
    /// For cluster-only, skip graph.html generation. Default true.
    #[serde(default)]
    pub no_viz: Option<bool>,
    /// Timeout in seconds. Default 600.
    #[serde(default)]
    pub timeout_sec: Option<u64>,
}

pub struct GraphifyUpdateTool {
    root: PathBuf,
    graph_path: PathBuf,
}

#[async_trait]
impl Tool for GraphifyUpdateTool {
    fn name(&self) -> &str {
        "graphify_update"
    }

    fn description(&self) -> &str {
        "Build or refresh Ra's Graphify graph using the installed graphify CLI, with AST-only update as the default low-cost path."
    }

    fn schema(&self) -> Value {
        serde_json::to_value(schema_for!(GraphifyUpdateParams)).unwrap()
    }

    async fn execute(&self, _call_id: &str, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("graphify_update");
        let params: GraphifyUpdateParams =
            serde_json::from_value(input).context("invalid params for graphify_update")?;
        run_graphify_update(&self.root, &self.graph_path, params).await
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphifyQueryParams {
    /// Natural-language question or identifier to search for in the graph.
    pub question: String,
    /// Traversal depth from the best-matching seed nodes. Default 2.
    #[serde(default)]
    pub depth: Option<usize>,
    /// Maximum nodes to include in the returned subgraph. Default 24.
    #[serde(default)]
    pub max_nodes: Option<usize>,
}

pub struct GraphifyQueryTool {
    root: PathBuf,
    graph_path: PathBuf,
}

#[async_trait]
impl Tool for GraphifyQueryTool {
    fn name(&self) -> &str {
        "graphify_query"
    }

    fn description(&self) -> &str {
        "Query the discovered Graphify code knowledge graph and return a focused subgraph."
    }

    fn schema(&self) -> Value {
        serde_json::to_value(schema_for!(GraphifyQueryParams)).unwrap()
    }

    async fn execute(&self, _call_id: &str, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("graphify_query");
        let params: GraphifyQueryParams =
            serde_json::from_value(input).context("invalid params for graphify_query")?;
        let graph = match load_graph(&self.graph_path) {
            Ok(graph) => graph,
            Err(e) => return Ok(graph_unavailable_message(&self.root, &self.graph_path, &e)),
        };
        let mut out = maybe_stale_prefix(&self.root, &self.graph_path);
        out.push_str(&graph.query(
            &params.question,
            params.depth.unwrap_or(DEFAULT_DEPTH),
            params.max_nodes.unwrap_or(DEFAULT_MAX_NODES),
        ));
        Ok(out)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphifyPathParams {
    /// Source node label, node id, or identifier fragment.
    pub source: String,
    /// Target node label, node id, or identifier fragment.
    pub target: String,
}

pub struct GraphifyPathTool {
    root: PathBuf,
    graph_path: PathBuf,
}

#[async_trait]
impl Tool for GraphifyPathTool {
    fn name(&self) -> &str {
        "graphify_path"
    }

    fn description(&self) -> &str {
        "Find the shortest relationship path between two Graphify graph nodes."
    }

    fn schema(&self) -> Value {
        serde_json::to_value(schema_for!(GraphifyPathParams)).unwrap()
    }

    async fn execute(&self, _call_id: &str, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("graphify_path");
        let params: GraphifyPathParams =
            serde_json::from_value(input).context("invalid params for graphify_path")?;
        let graph = match load_graph(&self.graph_path) {
            Ok(graph) => graph,
            Err(e) => return Ok(graph_unavailable_message(&self.root, &self.graph_path, &e)),
        };
        let mut out = maybe_stale_prefix(&self.root, &self.graph_path);
        out.push_str(&graph.path(&params.source, &params.target));
        Ok(out)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphifyExplainParams {
    /// Node label, node id, or identifier fragment.
    pub node: String,
    /// Maximum neighbor relationships to list. Default 20.
    #[serde(default)]
    pub max_neighbors: Option<usize>,
}

pub struct GraphifyExplainTool {
    root: PathBuf,
    graph_path: PathBuf,
}

#[async_trait]
impl Tool for GraphifyExplainTool {
    fn name(&self) -> &str {
        "graphify_explain"
    }

    fn description(&self) -> &str {
        "Explain a Graphify graph node and its strongest neighboring relationships."
    }

    fn schema(&self) -> Value {
        serde_json::to_value(schema_for!(GraphifyExplainParams)).unwrap()
    }

    async fn execute(&self, _call_id: &str, input: Value, _ctx: &ToolCtx) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("graphify_explain");
        let params: GraphifyExplainParams =
            serde_json::from_value(input).context("invalid params for graphify_explain")?;
        let graph = match load_graph(&self.graph_path) {
            Ok(graph) => graph,
            Err(e) => return Ok(graph_unavailable_message(&self.root, &self.graph_path, &e)),
        };
        let mut out = maybe_stale_prefix(&self.root, &self.graph_path);
        out.push_str(&graph.explain(
            &params.node,
            params.max_neighbors.unwrap_or(DEFAULT_MAX_NEIGHBORS),
        ));
        Ok(out)
    }
}

// ---------- graph loading ------------------------------------------------

#[derive(Debug, Clone)]
struct GraphifyGraph {
    path: PathBuf,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    id_to_idx: HashMap<String, usize>,
    adjacency: HashMap<String, Vec<usize>>,
    hyperedge_count: usize,
}

#[derive(Debug, Clone)]
struct Node {
    id: String,
    label: String,
    source_file: Option<String>,
    source_location: Option<String>,
    file_type: Option<String>,
    community: Option<String>,
}

#[derive(Debug, Clone)]
struct Edge {
    source: String,
    target: String,
    relation: Option<String>,
    confidence: Option<String>,
    source_file: Option<String>,
}

fn load_graph(path: &Path) -> Result<GraphifyGraph> {
    if !path.is_file() {
        return Err(anyhow!("graph file not found"));
    }
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.len() > MAX_GRAPH_BYTES {
        return Err(anyhow!(
            "graph file is too large ({} bytes; max {})",
            meta.len(),
            MAX_GRAPH_BYTES
        ));
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let data: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    let nodes_v = data
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("graph JSON missing `nodes` array"))?;

    let mut nodes = Vec::new();
    for node in nodes_v {
        let Some(id) = scalar_to_string(node.get("id")) else {
            continue;
        };
        let label = scalar_to_string(node.get("label")).unwrap_or_else(|| id.clone());
        nodes.push(Node {
            id,
            label,
            source_file: scalar_to_string(node.get("source_file")),
            source_location: scalar_to_string(node.get("source_location")),
            file_type: scalar_to_string(node.get("file_type")),
            community: scalar_to_string(node.get("community")),
        });
    }

    let id_to_idx = nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| (node.id.clone(), idx))
        .collect::<HashMap<_, _>>();

    let links_v = data
        .get("links")
        .or_else(|| data.get("edges"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("graph JSON missing `links` or `edges` array"))?;

    let mut edges = Vec::new();
    for link in links_v {
        let source_v = link.get("source").or_else(|| link.get("from"));
        let target_v = link.get("target").or_else(|| link.get("to"));
        let Some(source) = endpoint_to_id(source_v, &nodes) else {
            continue;
        };
        let Some(target) = endpoint_to_id(target_v, &nodes) else {
            continue;
        };
        if !id_to_idx.contains_key(&source) || !id_to_idx.contains_key(&target) {
            continue;
        }
        edges.push(Edge {
            source,
            target,
            relation: scalar_to_string(link.get("relation"))
                .or_else(|| scalar_to_string(link.get("label"))),
            confidence: scalar_to_string(link.get("confidence")),
            source_file: scalar_to_string(link.get("source_file")),
        });
    }

    let mut adjacency: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, edge) in edges.iter().enumerate() {
        adjacency.entry(edge.source.clone()).or_default().push(idx);
        adjacency.entry(edge.target.clone()).or_default().push(idx);
    }

    let hyperedge_count = data
        .get("hyperedges")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    Ok(GraphifyGraph {
        path: path.to_path_buf(),
        nodes,
        edges,
        id_to_idx,
        adjacency,
        hyperedge_count,
    })
}

impl GraphifyGraph {
    fn stats(&self) -> GraphStats {
        let mut file_types: BTreeMap<String, usize> = BTreeMap::new();
        let mut communities = HashSet::new();
        for node in &self.nodes {
            if let Some(kind) = &node.file_type {
                if !kind.is_empty() {
                    *file_types.entry(kind.clone()).or_default() += 1;
                }
            }
            if let Some(community) = &node.community {
                if !community.is_empty() && community != "null" {
                    communities.insert(community.clone());
                }
            }
        }
        GraphStats {
            node_count: self.nodes.len(),
            edge_count: self.edges.len(),
            hyperedge_count: self.hyperedge_count,
            community_count: communities.len(),
            file_types: file_types.into_iter().collect(),
        }
    }

    fn query(&self, question: &str, depth: usize, max_nodes: usize) -> String {
        let seeds = self.pick_seeds(question, 3);
        if seeds.is_empty() {
            return format!(
                "No Graphify nodes matched `{}` in `{}`.",
                question,
                self.path.display()
            );
        }

        let max_nodes = max_nodes.clamp(1, 100);
        let depth = depth.min(5);
        let (visited, edge_ids, truncated) = self.bfs(&seeds, depth, max_nodes);

        let mut out = String::new();
        out.push_str(&format!("Graphify query: `{question}`\n"));
        out.push_str(&format!("Graph: `{}`\n", self.path.display()));
        out.push_str("\nSeeds:\n");
        for seed in &seeds {
            if let Some(node) = self.node(seed) {
                out.push_str(&format!("- {}\n", self.format_node(node)));
            }
        }

        out.push_str(&format!(
            "\nSubgraph: {} node(s), {} edge(s), depth {}\n",
            visited.len(),
            edge_ids.len(),
            depth
        ));
        if truncated {
            out.push_str(&format!("- truncated at {} node(s)\n", max_nodes));
        }

        out.push_str("\nNodes:\n");
        for id in &visited {
            if let Some(node) = self.node(id) {
                out.push_str(&format!("- {}\n", self.format_node(node)));
            }
        }

        if !edge_ids.is_empty() {
            out.push_str("\nEdges:\n");
            for idx in edge_ids.iter().take(80) {
                if let Some(edge) = self.edges.get(*idx) {
                    out.push_str(&format!("- {}\n", self.format_edge(edge)));
                }
            }
            if edge_ids.len() > 80 {
                out.push_str(&format!("- ... {} more edge(s)\n", edge_ids.len() - 80));
            }
        }

        out
    }

    fn impact(
        &self,
        phase: &str,
        requirement: Option<&str>,
        changed_files: &[String],
        depth: usize,
        max_nodes: usize,
    ) -> String {
        let mut query_parts = Vec::new();
        if let Some(req) = requirement.filter(|s| !s.trim().is_empty()) {
            query_parts.push(req.to_string());
        }
        query_parts.extend(
            changed_files
                .iter()
                .filter(|s| !s.trim().is_empty())
                .cloned(),
        );
        if query_parts.is_empty() {
            return "Graphify impact needs a requirement, changed_files, or both to seed the R2A impact query.".into();
        }

        let query = query_parts.join(" ");
        let seeds = self.pick_seeds(&query, 6);
        if seeds.is_empty() {
            return format!(
                "No Graphify nodes matched the R2A seed `{query}` in `{}`.\n\
                 Keep the requirement as a traceability seed, use normal Ra file tools for initial discovery, then refresh Graphify after identifying concrete files.",
                self.path.display()
            );
        }

        let max_nodes = max_nodes.clamp(1, 120);
        let depth = depth.min(5);
        let (visited, edge_ids, truncated) = self.bfs(&seeds, depth, max_nodes);
        let mut source_files: BTreeMap<String, usize> = BTreeMap::new();
        let mut test_files: BTreeMap<String, usize> = BTreeMap::new();
        let mut doc_files: BTreeMap<String, usize> = BTreeMap::new();
        let mut communities: BTreeMap<String, usize> = BTreeMap::new();

        for id in &visited {
            let Some(node) = self.node(id) else {
                continue;
            };
            if let Some(community) = &node.community {
                if !community.is_empty() && community != "null" {
                    *communities.entry(community.clone()).or_default() += 1;
                }
            }
            let Some(file) = node.source_file.as_ref().filter(|s| !s.is_empty()) else {
                continue;
            };
            match classify_source_file(file) {
                SourceFileKind::Test => *test_files.entry(file.clone()).or_default() += 1,
                SourceFileKind::Doc => *doc_files.entry(file.clone()).or_default() += 1,
                SourceFileKind::Source => *source_files.entry(file.clone()).or_default() += 1,
            }
        }

        let mut out = String::new();
        out.push_str("Graphify R2A impact\n");
        out.push_str(&format!("- phase: {phase}\n"));
        if let Some(req) = requirement.filter(|s| !s.trim().is_empty()) {
            out.push_str(&format!("- requirement: {req}\n"));
        }
        if !changed_files.is_empty() {
            out.push_str(&format!("- changed files: {}\n", changed_files.join(", ")));
        }
        out.push_str(&format!(
            "- subgraph: {} node(s), {} edge(s), depth {}\n",
            visited.len(),
            edge_ids.len(),
            depth
        ));
        if truncated {
            out.push_str(&format!("- truncated at {} node(s)\n", max_nodes));
        }

        out.push_str("\nRequirement/change seeds:\n");
        for seed in &seeds {
            if let Some(node) = self.node(seed) {
                out.push_str(&format!("- {}\n", self.format_node(node)));
            }
        }

        out.push_str("\nPlanning focus:\n");
        push_ranked_files(&mut out, &source_files, 18, "source file(s)");
        if !communities.is_empty() {
            let parts = communities
                .iter()
                .take(8)
                .map(|(community, count)| format!("{community}({count})"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("- risk communities: {parts}\n"));
        }

        out.push_str("\nImplementation guidance:\n");
        for idx in edge_ids.iter().take(24) {
            if let Some(edge) = self.edges.get(*idx) {
                out.push_str(&format!("- {}\n", self.format_edge(edge)));
            }
        }
        if edge_ids.len() > 24 {
            out.push_str(&format!(
                "- ... {} more relationship(s)\n",
                edge_ids.len() - 24
            ));
        }

        out.push_str("\nVerification candidates:\n");
        if test_files.is_empty() {
            out.push_str("- no explicit test nodes in the impact subgraph; choose tests that cover the planning-focus files and add missing coverage if behavior changes.\n");
        } else {
            push_ranked_files(&mut out, &test_files, 12, "test file(s)");
        }
        if !doc_files.is_empty() {
            push_ranked_files(&mut out, &doc_files, 8, "doc/artifact file(s)");
        }

        out.push_str("\nTraceability for report:\n");
        out.push_str("- requirement -> seed nodes -> planning-focus files -> tests/docs -> final touched files\n");
        out.push_str("- refresh Graphify after code/doc edits so the next agent run inherits the updated graph.\n");
        out
    }

    fn explain(&self, query: &str, max_neighbors: usize) -> String {
        let Some(node_id) = self.pick_seeds(query, 1).into_iter().next() else {
            return format!(
                "No Graphify node matched `{}` in `{}`.",
                query,
                self.path.display()
            );
        };
        let Some(node) = self.node(&node_id) else {
            return format!("No Graphify node matched `{query}`.");
        };

        let mut out = String::new();
        out.push_str(&format!("Node: {}\n", node.label));
        out.push_str(&format!("- id: `{}`\n", node.id));
        if let Some(source) = &node.source_file {
            out.push_str(&format!("- source: `{source}`"));
            if let Some(loc) = &node.source_location {
                out.push_str(&format!(" {loc}"));
            }
            out.push('\n');
        }
        if let Some(kind) = &node.file_type {
            out.push_str(&format!("- file_type: {kind}\n"));
        }
        if let Some(community) = &node.community {
            out.push_str(&format!("- community: {community}\n"));
        }

        let mut edge_ids = self.adjacency.get(&node_id).cloned().unwrap_or_default();
        edge_ids.sort_by_key(|idx| {
            self.edges
                .get(*idx)
                .map(|e| std::cmp::Reverse(self.edge_neighbor_degree(&node_id, e)))
                .unwrap_or(std::cmp::Reverse(0))
        });

        let limit = max_neighbors.clamp(1, 100);
        out.push_str(&format!("\nNeighbors (showing up to {limit}):\n"));
        for idx in edge_ids.iter().take(limit) {
            if let Some(edge) = self.edges.get(*idx) {
                out.push_str(&format!("- {}\n", self.format_edge_from(&node_id, edge)));
            }
        }
        if edge_ids.is_empty() {
            out.push_str("- none\n");
        } else if edge_ids.len() > limit {
            out.push_str(&format!("- ... {} more\n", edge_ids.len() - limit));
        }
        out
    }

    fn path(&self, source_query: &str, target_query: &str) -> String {
        let Some(source_id) = self.pick_seeds(source_query, 1).into_iter().next() else {
            return format!("No source node matched `{source_query}`.");
        };
        let Some(target_id) = self.pick_seeds(target_query, 1).into_iter().next() else {
            return format!("No target node matched `{target_query}`.");
        };
        if source_id == target_id {
            let label = self
                .node(&source_id)
                .map(|n| n.label.clone())
                .unwrap_or(source_id);
            return format!("Both queries resolved to the same node: {label}.");
        }

        let Some((node_path, edge_path)) = self.shortest_path(&source_id, &target_id) else {
            let source = self.node_label(&source_id);
            let target = self.node_label(&target_id);
            return format!("No path found between `{source}` and `{target}`.");
        };

        let mut out = String::new();
        out.push_str(&format!(
            "Shortest Graphify path ({} hop(s)):\n",
            node_path.len().saturating_sub(1)
        ));
        for (idx, node_id) in node_path.iter().enumerate() {
            let label = self.node_label(node_id);
            if idx == 0 {
                out.push_str(&format!("- {label}\n"));
                continue;
            }
            let edge = &self.edges[edge_path[idx - 1]];
            out.push_str(&format!(
                "  {}\n",
                self.format_edge_between(&node_path[idx - 1], edge)
            ));
            out.push_str(&format!("- {label}\n"));
        }
        out
    }

    fn pick_seeds(&self, query: &str, max: usize) -> Vec<String> {
        let terms = tokens(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let mut scored = Vec::new();
        for node in &self.nodes {
            let score = score_node(node, &terms);
            if score > 0 {
                scored.push((score, node.id.clone()));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored.into_iter().take(max).map(|(_, id)| id).collect()
    }

    fn bfs(
        &self,
        seeds: &[String],
        depth: usize,
        max_nodes: usize,
    ) -> (Vec<String>, Vec<usize>, bool) {
        let mut visited: HashSet<String> = HashSet::new();
        let mut ordered_nodes = Vec::new();
        let mut seen_edges: HashSet<usize> = HashSet::new();
        let mut ordered_edges = Vec::new();
        let mut queue = VecDeque::new();

        for seed in seeds {
            if visited.insert(seed.clone()) {
                ordered_nodes.push(seed.clone());
                queue.push_back((seed.clone(), 0usize));
            }
        }

        let mut truncated = false;
        while let Some((node_id, dist)) = queue.pop_front() {
            if dist >= depth {
                continue;
            }
            let edge_ids = self.adjacency.get(&node_id).cloned().unwrap_or_default();
            for edge_idx in edge_ids {
                if seen_edges.insert(edge_idx) {
                    ordered_edges.push(edge_idx);
                }
                let Some(edge) = self.edges.get(edge_idx) else {
                    continue;
                };
                let neighbor = if edge.source == node_id {
                    &edge.target
                } else {
                    &edge.source
                };
                if !visited.contains(neighbor) {
                    if ordered_nodes.len() >= max_nodes {
                        truncated = true;
                        continue;
                    }
                    visited.insert(neighbor.clone());
                    ordered_nodes.push(neighbor.clone());
                    queue.push_back((neighbor.clone(), dist + 1));
                }
            }
        }

        (ordered_nodes, ordered_edges, truncated)
    }

    fn shortest_path(&self, source: &str, target: &str) -> Option<(Vec<String>, Vec<usize>)> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut prev: HashMap<String, (String, usize)> = HashMap::new();
        let mut queue = VecDeque::new();
        seen.insert(source.to_string());
        queue.push_back(source.to_string());

        while let Some(node_id) = queue.pop_front() {
            if node_id == target {
                break;
            }
            for edge_idx in self.adjacency.get(&node_id).cloned().unwrap_or_default() {
                let edge = self.edges.get(edge_idx)?;
                let neighbor = if edge.source == node_id {
                    &edge.target
                } else {
                    &edge.source
                };
                if seen.insert(neighbor.clone()) {
                    prev.insert(neighbor.clone(), (node_id.clone(), edge_idx));
                    queue.push_back(neighbor.clone());
                }
            }
        }

        if !seen.contains(target) {
            return None;
        }

        let mut nodes = vec![target.to_string()];
        let mut edges = Vec::new();
        let mut current = target.to_string();
        while current != source {
            let (parent, edge_idx) = prev.get(&current)?.clone();
            edges.push(edge_idx);
            nodes.push(parent.clone());
            current = parent;
        }
        nodes.reverse();
        edges.reverse();
        Some((nodes, edges))
    }

    fn node(&self, id: &str) -> Option<&Node> {
        self.id_to_idx.get(id).and_then(|idx| self.nodes.get(*idx))
    }

    fn node_label(&self, id: &str) -> String {
        self.node(id)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn format_node(&self, node: &Node) -> String {
        let mut out = format!("{} (`{}`)", node.label, node.id);
        if let Some(source) = &node.source_file {
            out.push_str(&format!(" - `{source}`"));
            if let Some(loc) = &node.source_location {
                out.push_str(&format!(" {loc}"));
            }
        }
        if let Some(kind) = &node.file_type {
            out.push_str(&format!(" [{kind}]"));
        }
        out
    }

    fn format_edge(&self, edge: &Edge) -> String {
        let source = self.node_label(&edge.source);
        let target = self.node_label(&edge.target);
        let relation = edge.relation.as_deref().unwrap_or("related_to");
        let confidence = edge
            .confidence
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(|s| format!(" [{s}]"))
            .unwrap_or_default();
        let mut out = format!("{source} --{relation}{confidence}--> {target}");
        if let Some(file) = &edge.source_file {
            out.push_str(&format!(" (`{file}`)"));
        }
        out
    }

    fn format_edge_from(&self, node_id: &str, edge: &Edge) -> String {
        let relation = edge.relation.as_deref().unwrap_or("related_to");
        let confidence = edge
            .confidence
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(|s| format!(" [{s}]"))
            .unwrap_or_default();
        if edge.source == node_id {
            format!(
                "--{relation}{confidence}--> {}",
                self.node_label(&edge.target)
            )
        } else {
            format!(
                "<--{relation}{confidence}-- {}",
                self.node_label(&edge.source)
            )
        }
    }

    fn format_edge_between(&self, from: &str, edge: &Edge) -> String {
        let relation = edge.relation.as_deref().unwrap_or("related_to");
        let confidence = edge
            .confidence
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(|s| format!(" [{s}]"))
            .unwrap_or_default();
        if edge.source == from {
            format!("--{relation}{confidence}-->")
        } else {
            format!("<--{relation}{confidence}--")
        }
    }

    fn edge_neighbor_degree(&self, node_id: &str, edge: &Edge) -> usize {
        let neighbor = if edge.source == node_id {
            &edge.target
        } else {
            &edge.source
        };
        self.adjacency.get(neighbor).map_or(0, Vec::len)
    }
}

enum SourceFileKind {
    Source,
    Test,
    Doc,
}

fn classify_source_file(file: &str) -> SourceFileKind {
    let lower = file.to_lowercase();
    if lower.contains("/test")
        || lower.contains("\\test")
        || lower.contains("_test.")
        || lower.contains(".test.")
        || lower.contains("_spec.")
        || lower.contains(".spec.")
        || lower.starts_with("test")
    {
        SourceFileKind::Test
    } else if lower.starts_with("docs/")
        || lower.contains("/docs/")
        || lower.ends_with(".md")
        || lower.ends_with(".mdx")
        || lower.ends_with(".rst")
        || lower.ends_with(".txt")
    {
        SourceFileKind::Doc
    } else {
        SourceFileKind::Source
    }
}

fn push_ranked_files(out: &mut String, files: &BTreeMap<String, usize>, limit: usize, label: &str) {
    if files.is_empty() {
        out.push_str(&format!("- no {label} found in the impact subgraph\n"));
        return;
    }
    let mut ranked = files.iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (file, count) in ranked.iter().take(limit) {
        out.push_str(&format!("- `{file}` ({count} node(s))\n"));
    }
    if ranked.len() > limit {
        out.push_str(&format!("- ... {} more {label}\n", ranked.len() - limit));
    }
}

fn scalar_to_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn endpoint_to_id(value: Option<&Value>, nodes: &[Node]) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => {
            if let Some(idx) = n.as_u64().and_then(|v| usize::try_from(v).ok()) {
                if let Some(node) = nodes.get(idx) {
                    return Some(node.id.clone());
                }
            }
            Some(n.to_string())
        }
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '/'))
        .map(str::trim)
        .filter(|t| t.chars().count() >= 2)
        .map(ToOwned::to_owned)
        .collect()
}

fn score_node(node: &Node, terms: &[String]) -> usize {
    let label = node.label.to_lowercase();
    let id = node.id.to_lowercase();
    let source = node.source_file.as_deref().unwrap_or("").to_lowercase();
    let mut score = 0usize;
    for term in terms {
        if label == *term || id == *term {
            score += 1000;
        } else if label.starts_with(term) || id.starts_with(term) {
            score += 100;
        } else if label.contains(term) || id.contains(term) {
            score += 10;
        }
        if source.contains(term) {
            score += 3;
        }
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_graph(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            r#"{
  "directed": true,
  "nodes": [
    {"id": "auth_service", "label": "AuthService", "source_file": "src/auth.rs", "file_type": "code", "community": 1},
    {"id": "db_pool", "label": "DatabasePool", "source_file": "src/db.rs", "file_type": "code", "community": 1},
    {"id": "login_doc", "label": "Login flow", "source_file": "docs/auth.md", "file_type": "document", "community": 2}
  ],
  "links": [
    {"source": "auth_service", "target": "db_pool", "relation": "uses", "confidence": "EXTRACTED"},
    {"source": "login_doc", "target": "auth_service", "relation": "documents", "confidence": "INFERRED"}
  ],
  "hyperedges": [{"label": "auth", "nodes": ["auth_service", "db_pool", "login_doc"]}]
}"#,
        )
        .unwrap();
    }

    #[test]
    fn loads_stats_and_prompt_section() {
        let tmp = tempfile::tempdir().unwrap();
        let graph_path = tmp.path().join("graphify-out/graph.json");
        write_graph(&graph_path);
        std::fs::write(
            tmp.path().join("graphify-out/GRAPH_REPORT.md"),
            "# Report\n",
        )
        .unwrap();

        let project = load_project_from_graph_path(&graph_path).unwrap();
        assert_eq!(project.stats.node_count, 3);
        assert_eq!(project.stats.edge_count, 2);
        assert_eq!(project.stats.hyperedge_count, 1);
        assert_eq!(project.stats.community_count, 2);
        assert!(project.report_path.is_some());

        let section = project.build_system_prompt_section().unwrap();
        assert!(section.contains("# Graphify"));
        assert!(section.contains("graphify_query"));
        assert!(section.contains("nodes: 3"));
        assert!(section.contains("file types: code=2, document=1"));
    }

    #[test]
    fn query_path_and_explain_are_graph_native() {
        let tmp = tempfile::tempdir().unwrap();
        let graph_path = tmp.path().join("graphify-out/graph.json");
        write_graph(&graph_path);
        let graph = load_graph(&graph_path).unwrap();

        let query = graph.query("auth database", 2, 10);
        assert!(query.contains("AuthService"));
        assert!(query.contains("DatabasePool"));
        assert!(query.contains("--uses [EXTRACTED]-->"));

        let path = graph.path("Login", "DatabasePool");
        assert!(path.contains("Login flow"));
        assert!(path.contains("AuthService"));
        assert!(path.contains("DatabasePool"));

        let explain = graph.explain("AuthService", 10);
        assert!(explain.contains("Node: AuthService"));
        assert!(explain.contains("src/auth.rs"));
        assert!(explain.contains("DatabasePool"));
    }

    #[test]
    fn discovery_walks_up_to_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let graph_path = tmp.path().join("graphify-out/graph.json");
        write_graph(&graph_path);
        let nested = tmp.path().join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();

        let project = discover(&nested).expect("should find graphify graph walking up");
        assert_eq!(project.graph_path, graph_path);
        assert_eq!(project.stats.node_count, 3);
    }
}
