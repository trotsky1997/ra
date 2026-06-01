//! Native webfetch-cli wrappers.
//!
//! The tools intentionally keep the upstream project as the implementation of
//! web fetching and crawling. Ra owns typed parameters, argv-safe spawning,
//! structured missing-dependency guidance, and output bounding.

use crate::events::Event;
use crate::tool_ctx::ToolCtx;
use crate::tools::core::Tool;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

const WEBFETCH_PACKAGE: &str = "github:trotsky1997/webfetch-cli";
const WEBFETCH_BINARY: &str = "webfetch-cli";
const DEFAULT_FETCH_MAX_OUTPUT_BYTES: usize = 100_000;
const DEFAULT_CRAWL_MAX_OUTPUT_BYTES: usize = 200_000;
const DEFAULT_STDERR_BYTES: usize = 32_000;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WebfetchFetchParams {
    /// URL to fetch. http(s) URLs are supported; bare hosts are normalized by
    /// webfetch-cli.
    pub url: String,
    /// Output mode: `toc-only`, `path-only`, or `all`.
    #[serde(default)]
    pub output: Option<FetchOutputMode>,
    /// Per-attempt timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Directory where webfetch-cli writes `.md/`. Defaults to the session cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Skip hosted Markdown services and fetch the target URL directly.
    #[serde(default)]
    pub raw_only: bool,
    /// Maximum bytes in Ra's returned JSON envelope. Applies even when
    /// `output` is `all`.
    #[serde(default = "default_fetch_max_output_bytes")]
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FetchOutputMode {
    TocOnly,
    PathOnly,
    All,
}

impl FetchOutputMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::TocOnly => "toc-only",
            Self::PathOnly => "path-only",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WebfetchCrawlParams {
    /// Root URL to crawl.
    pub url: String,
    /// Allowed hostname suffix. Defaults to the root URL hostname.
    #[serde(default)]
    pub parent_domain: Option<String>,
    /// Link distance from the root page.
    #[serde(default)]
    pub max_hops: Option<u32>,
    /// Maximum pages to fetch.
    #[serde(default)]
    pub max_pages: Option<u32>,
    /// Maximum followed links per page.
    #[serde(default)]
    pub max_links: Option<u32>,
    /// Number of pages fetched concurrently.
    #[serde(default)]
    pub concurrency: Option<u32>,
    /// Keep query-string URLs in crawl scope.
    #[serde(default)]
    pub allow_query: bool,
    /// Output mode: `summary`, `path-only`, or `all`.
    #[serde(default)]
    pub output: Option<CrawlOutputMode>,
    /// Per-attempt timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Directory where webfetch-cli writes `.md/`. Defaults to the session cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Skip hosted Markdown services and fetch target URLs directly.
    #[serde(default)]
    pub raw_only: bool,
    /// Ask webfetch-cli to print crawl progress to stderr. Ra returns stderr in
    /// a bounded JSON field.
    #[serde(default)]
    pub verbose: bool,
    /// Maximum bytes in Ra's returned JSON envelope. Applies even when
    /// `output` is `all`.
    #[serde(default = "default_crawl_max_output_bytes")]
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CrawlOutputMode {
    Summary,
    PathOnly,
    All,
}

impl CrawlOutputMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::PathOnly => "path-only",
            Self::All => "all",
        }
    }
}

pub struct WebfetchFetchTool;

pub struct WebfetchCrawlTool;

#[async_trait]
impl Tool for WebfetchFetchTool {
    fn name(&self) -> &str {
        "webfetch_fetch"
    }

    fn description(&self) -> &str {
        "Fetch one web page as Markdown through webfetch-cli, cache it under \
         `.md/`, and return bounded JSON. Requires npm; missing npm returns \
         structured install guidance."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(WebfetchFetchParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("webfetch_fetch");
        let params: WebfetchFetchParams =
            serde_json::from_value(input).context("invalid params for webfetch_fetch")?;
        let invocation = WebfetchInvocation::fetch(params)?;
        execute_webfetch(call_id, invocation, ctx).await
    }
}

#[async_trait]
impl Tool for WebfetchCrawlTool {
    fn name(&self) -> &str {
        "webfetch_crawl"
    }

    fn description(&self) -> &str {
        "Crawl a bounded documentation subtree through webfetch-cli, mirror \
         Markdown under `.md/`, and return bounded JSON. Requires npm; \
         missing npm returns structured install guidance."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(WebfetchCrawlParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("webfetch_crawl");
        let params: WebfetchCrawlParams =
            serde_json::from_value(input).context("invalid params for webfetch_crawl")?;
        let invocation = WebfetchInvocation::crawl(params)?;
        execute_webfetch(call_id, invocation, ctx).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebfetchCommand {
    Fetch,
    Crawl,
}

impl WebfetchCommand {
    fn tool_name(self) -> &'static str {
        match self {
            Self::Fetch => "webfetch_fetch",
            Self::Crawl => "webfetch_crawl",
        }
    }
}

#[derive(Debug, Clone)]
struct WebfetchInvocation {
    command: WebfetchCommand,
    webfetch_args: Vec<String>,
    max_output_bytes: usize,
    stream_stderr: bool,
}

impl WebfetchInvocation {
    fn fetch(params: WebfetchFetchParams) -> Result<Self> {
        validate_url(&params.url, "webfetch_fetch")?;

        let output = params.output.unwrap_or(FetchOutputMode::TocOnly);
        let mut args = vec!["fetch".to_string(), params.url];
        args.push("--output".into());
        args.push(output.as_str().into());
        if let Some(timeout) = params.timeout_ms {
            args.push("--timeout".into());
            args.push(timeout.to_string());
        }
        if let Some(cwd) = non_empty(params.cwd) {
            args.push("--cwd".into());
            args.push(cwd);
        }
        if params.raw_only {
            args.push("--raw-only".into());
        }
        args.push("--json".into());

        Ok(Self {
            command: WebfetchCommand::Fetch,
            webfetch_args: args,
            max_output_bytes: params.max_output_bytes,
            stream_stderr: false,
        })
    }

    fn crawl(params: WebfetchCrawlParams) -> Result<Self> {
        validate_url(&params.url, "webfetch_crawl")?;

        let output = params.output.unwrap_or(CrawlOutputMode::Summary);
        let mut args = vec!["crawl".to_string(), params.url];
        if let Some(parent_domain) = non_empty(params.parent_domain) {
            args.push("--parent-domain".into());
            args.push(parent_domain);
        }
        if let Some(max_hops) = params.max_hops {
            args.push("--max-hops".into());
            args.push(max_hops.to_string());
        }
        if let Some(max_pages) = params.max_pages {
            args.push("--max-pages".into());
            args.push(max_pages.to_string());
        }
        if let Some(max_links) = params.max_links {
            args.push("--max-links".into());
            args.push(max_links.to_string());
        }
        if let Some(concurrency) = params.concurrency {
            args.push("--concurrency".into());
            args.push(concurrency.to_string());
        }
        if params.allow_query {
            args.push("--allow-query".into());
        }
        args.push("--output".into());
        args.push(output.as_str().into());
        if let Some(timeout) = params.timeout_ms {
            args.push("--timeout".into());
            args.push(timeout.to_string());
        }
        if let Some(cwd) = non_empty(params.cwd) {
            args.push("--cwd".into());
            args.push(cwd);
        }
        if params.raw_only {
            args.push("--raw-only".into());
        }
        if params.verbose {
            args.push("--verbose".into());
        }
        args.push("--json".into());

        Ok(Self {
            command: WebfetchCommand::Crawl,
            webfetch_args: args,
            max_output_bytes: params.max_output_bytes,
            stream_stderr: params.verbose,
        })
    }

    fn npm_exec_args(&self) -> Vec<String> {
        let mut args = vec![
            "exec".to_string(),
            "--yes".to_string(),
            "--package".to_string(),
            WEBFETCH_PACKAGE.to_string(),
            "--".to_string(),
            WEBFETCH_BINARY.to_string(),
        ];
        args.extend(self.webfetch_args.iter().cloned());
        args
    }

    fn summary(&self) -> String {
        shell_words("npm", &self.npm_exec_args())
    }
}

async fn execute_webfetch(
    call_id: &str,
    invocation: WebfetchInvocation,
    ctx: &ToolCtx,
) -> Result<String> {
    let npm = match find_npm_binary() {
        Some(path) => path,
        None => {
            let command = invocation.summary();
            let rendered = missing_npm_json(invocation.command.tool_name(), &command);
            return Ok(rendered);
        }
    };

    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[webfetch] {}", invocation.summary()),
    });

    let output = run_webfetch(&npm, &invocation, &ctx.cwd, call_id, ctx).await?;
    let exit_code = output.exit_code;
    let _ = ctx.events.send(Event::ToolCallUpdate {
        id: call_id.to_string(),
        chunk: format!("[exit={exit_code}]"),
    });

    format_webfetch_output(&invocation, output)
}

fn find_npm_binary() -> Option<PathBuf> {
    which::which("npm").ok()
}

async fn run_webfetch(
    npm: &Path,
    invocation: &WebfetchInvocation,
    cwd: &Path,
    call_id: &str,
    ctx: &ToolCtx,
) -> Result<WebfetchOutput> {
    let mut child = Command::new(npm)
        .args(invocation.npm_exec_args())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn `{}` in {}", invocation.summary(), cwd.display()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("webfetch-cli stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("webfetch-cli stderr was not piped"))?;

    let stdout_task = tokio::spawn(async move {
        let mut reader = stdout;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&bytes).into_owned())
    });

    let stream_stderr = invocation.stream_stderr;
    let events = ctx.events.clone();
    let id = call_id.to_string();
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        let mut text = String::new();
        while let Some(line) = reader.next_line().await? {
            if stream_stderr {
                let _ = events.send(Event::ToolCallUpdate {
                    id: id.clone(),
                    chunk: format!("[webfetch] {line}"),
                });
            }
            text.push_str(&line);
            text.push('\n');
        }
        Ok::<_, std::io::Error>(text)
    });

    let status = child
        .wait()
        .await
        .with_context(|| format!("wait `{}`", invocation.summary()))?;
    let stdout = stdout_task
        .await
        .context("join webfetch stdout reader")?
        .context("read webfetch stdout")?;
    let stderr = stderr_task
        .await
        .context("join webfetch stderr reader")?
        .context("read webfetch stderr")?;

    Ok(WebfetchOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

#[derive(Debug, Clone)]
struct WebfetchOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn format_webfetch_output(
    invocation: &WebfetchInvocation,
    output: WebfetchOutput,
) -> Result<String> {
    let stdout = output.stdout;
    let mut stderr = output.stderr;
    let ok = output.exit_code == 0;
    let stderr_truncated = trim_to_char_budget(&mut stderr, DEFAULT_STDERR_BYTES);

    let base = json!({
        "ok": ok,
        "tool": invocation.command.tool_name(),
        "command": {
            "program": "npm",
            "args": invocation.npm_exec_args(),
            "display": invocation.summary(),
        },
        "exit_code": output.exit_code,
        "stdout": stdout.clone(),
        "stderr": if stderr.is_empty() { serde_json::Value::Null } else { json!(stderr.clone()) },
        "truncated": stderr_truncated,
    });

    bounded_output_json(
        base,
        &stdout,
        stderr,
        invocation.max_output_bytes,
        stderr_truncated,
    )
}

fn bounded_output_json(
    mut value: serde_json::Value,
    stdout: &str,
    stderr: String,
    max_output_bytes: usize,
    _already_truncated: bool,
) -> Result<String> {
    let budget = max_output_bytes;
    let mut rendered = serde_json::to_string_pretty(&value)?;
    if budget == 0 || rendered.len() <= budget {
        return Ok(rendered);
    }

    let min_stdout_chars = 0;
    let mut low = min_stdout_chars;
    let mut high = stdout.chars().count();
    let mut best = min_stdout_chars;

    while low <= high {
        let mid = low + (high - low) / 2;
        let probe_stdout = take_chars(stdout, mid);
        value["stdout"] = json!(with_truncation_marker(&probe_stdout));
        value["stderr"] = if stderr.is_empty() {
            serde_json::Value::Null
        } else {
            json!(stderr.clone())
        };
        value["truncated"] = json!(true);
        let probe = serde_json::to_string_pretty(&value)?;
        if probe.len() <= budget {
            best = mid;
            low = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }

    value["stdout"] = json!(with_truncation_marker(&take_chars(stdout, best)));
    rendered = serde_json::to_string_pretty(&value)?;
    if rendered.len() <= budget || stderr.is_empty() {
        return Ok(rendered);
    }

    let mut stderr_trimmed = stderr;
    trim_to_char_budget(&mut stderr_trimmed, DEFAULT_STDERR_BYTES.min(1024));
    value["stderr"] = json!(stderr_trimmed);
    rendered = serde_json::to_string_pretty(&value)?;
    if rendered.len() <= budget || best == 0 {
        return Ok(rendered);
    }

    value["stdout"] = json!(with_truncation_marker(""));
    value["truncated"] = json!(true);
    serde_json::to_string_pretty(&value).map_err(Into::into)
}

fn missing_npm_json(tool: &str, command: &str) -> String {
    serde_json::to_string_pretty(&json!({
        "ok": false,
        "tool": tool,
        "error": {
            "kind": "missing_npm",
            "message": "npm was not found on PATH, so webfetch-cli could not be run.",
            "install": [
                "Install Node.js 20 or newer, which includes npm.",
                "After npm is available, rerun this tool; Ra will execute webfetch-cli through npm exec without a global install."
            ]
        },
        "command": {
            "program": "npm",
            "display": command
        },
        "exit_code": null,
        "stdout": "",
        "stderr": null,
        "truncated": false
    }))
    .expect("missing npm JSON is serializable")
}

fn validate_url(url: &str, tool: &str) -> Result<()> {
    if url.trim().is_empty() {
        return Err(anyhow!("{tool} requires a non-empty url"));
    }
    Ok(())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn trim_to_char_budget(text: &mut String, max_bytes: usize) -> bool {
    if text.len() <= max_bytes {
        return false;
    }

    let mut end = 0;
    for (idx, ch) in text.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }

    text.truncate(end);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("[truncated]\n");
    true
}

fn take_chars(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

fn with_truncation_marker(text: &str) -> String {
    if text.is_empty() {
        "[truncated]\n".to_string()
    } else if text.ends_with('\n') {
        format!("{text}[truncated]\n")
    } else {
        format!("{text}\n[truncated]\n")
    }
}

fn shell_words(binary: &str, args: &[String]) -> String {
    std::iter::once(binary.to_string())
        .chain(args.iter().map(|arg| shell_word(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_word(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '_' | '-' | '=' | ':'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn default_fetch_max_output_bytes() -> usize {
    DEFAULT_FETCH_MAX_OUTPUT_BYTES
}

fn default_crawl_max_output_bytes() -> usize {
    DEFAULT_CRAWL_MAX_OUTPUT_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch_params(url: &str) -> WebfetchFetchParams {
        WebfetchFetchParams {
            url: url.into(),
            output: None,
            timeout_ms: None,
            cwd: None,
            raw_only: false,
            max_output_bytes: DEFAULT_FETCH_MAX_OUTPUT_BYTES,
        }
    }

    fn crawl_params(url: &str) -> WebfetchCrawlParams {
        WebfetchCrawlParams {
            url: url.into(),
            parent_domain: None,
            max_hops: None,
            max_pages: None,
            max_links: None,
            concurrency: None,
            allow_query: false,
            output: None,
            timeout_ms: None,
            cwd: None,
            raw_only: false,
            verbose: false,
            max_output_bytes: DEFAULT_CRAWL_MAX_OUTPUT_BYTES,
        }
    }

    #[test]
    fn fetch_builds_npm_exec_args() {
        let mut params = fetch_params("https://example.com/docs");
        params.output = Some(FetchOutputMode::All);
        params.timeout_ms = Some(60_000);
        params.cwd = Some(" scratch ".into());
        params.raw_only = true;

        let invocation = WebfetchInvocation::fetch(params).unwrap();

        assert_eq!(
            invocation.npm_exec_args(),
            vec![
                "exec",
                "--yes",
                "--package",
                WEBFETCH_PACKAGE,
                "--",
                WEBFETCH_BINARY,
                "fetch",
                "https://example.com/docs",
                "--output",
                "all",
                "--timeout",
                "60000",
                "--cwd",
                "scratch",
                "--raw-only",
                "--json",
            ]
        );
    }

    #[test]
    fn crawl_builds_npm_exec_args() {
        let mut params = crawl_params("https://docs.example.com/start");
        params.parent_domain = Some("example.com".into());
        params.max_hops = Some(1);
        params.max_pages = Some(10);
        params.max_links = Some(20);
        params.concurrency = Some(2);
        params.allow_query = true;
        params.output = Some(CrawlOutputMode::All);
        params.timeout_ms = Some(45_000);
        params.cwd = Some("/tmp/webfetch".into());
        params.raw_only = true;
        params.verbose = true;

        let invocation = WebfetchInvocation::crawl(params).unwrap();

        assert_eq!(
            invocation.npm_exec_args(),
            vec![
                "exec",
                "--yes",
                "--package",
                WEBFETCH_PACKAGE,
                "--",
                WEBFETCH_BINARY,
                "crawl",
                "https://docs.example.com/start",
                "--parent-domain",
                "example.com",
                "--max-hops",
                "1",
                "--max-pages",
                "10",
                "--max-links",
                "20",
                "--concurrency",
                "2",
                "--allow-query",
                "--output",
                "all",
                "--timeout",
                "45000",
                "--cwd",
                "/tmp/webfetch",
                "--raw-only",
                "--verbose",
                "--json",
            ]
        );
    }

    #[test]
    fn rejects_empty_url() {
        assert!(WebfetchInvocation::fetch(fetch_params("")).is_err());
        assert!(WebfetchInvocation::crawl(crawl_params(" ")).is_err());
    }

    #[test]
    fn missing_npm_response_is_structured_json() {
        let rendered = missing_npm_json("webfetch_fetch", "npm exec --yes ...");
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["error"]["kind"], "missing_npm");
        assert!(value["error"]["install"][0]
            .as_str()
            .unwrap()
            .contains("Node.js"));
    }

    #[test]
    fn output_budget_preserves_valid_json_and_marks_truncated() {
        let invocation = WebfetchInvocation::fetch(fetch_params("https://example.com")).unwrap();
        let output = WebfetchOutput {
            exit_code: 0,
            stdout: "x".repeat(10_000),
            stderr: String::new(),
        };
        let mut invocation = invocation;
        invocation.max_output_bytes = 600;

        let rendered = format_webfetch_output(&invocation, output).unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(value["truncated"], json!(true));
        assert!(value["stdout"].as_str().unwrap().contains("[truncated]"));
        assert!(
            rendered.len() <= 600,
            "len={} output={rendered}",
            rendered.len()
        );
    }

    #[test]
    fn trim_to_char_budget_does_not_split_multibyte_chars() {
        let mut text = "aaa界tail".to_string();
        assert!(trim_to_char_budget(&mut text, 6));
        assert!(text.starts_with("aaa"));
        assert!(text.contains("[truncated]"));
    }
}
