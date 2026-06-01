//! Structural search tools.
//!
//! `ast_grep` is intentionally read-only: it exposes ast-grep's one-shot
//! structural search path, while code mutation remains owned by `write`/`edit`
//! so interactive frontends can keep reviewing file changes in one place.

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
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

const DEFAULT_MAX_MATCHES: usize = 50;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 100_000;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AstGrepParams {
    /// AST pattern to match, for example `if ($COND) { $BODY }`.
    pub pattern: String,
    /// Files or directories to search. Defaults to the current directory.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Language of the pattern query, for example `rust`, `python`,
    /// `typescript`, or `tsx`. ast-grep infers from file extensions when
    /// omitted.
    #[serde(default)]
    pub lang: Option<String>,
    /// AST kind inside `pattern` that should be used as the actual matcher.
    #[serde(default)]
    pub selector: Option<String>,
    /// Pattern match strictness. ast-grep defaults to `smart` when omitted.
    #[serde(default)]
    pub strictness: Option<AstGrepStrictness>,
    /// Include or exclude globs. Prefix with `!` to exclude. Later globs win.
    #[serde(default)]
    pub globs: Vec<String>,
    /// Follow symbolic links while traversing directories.
    #[serde(default)]
    pub follow: bool,
    /// Show this many lines around each match. Conflicts with `before`/`after`.
    #[serde(default)]
    pub context: Option<u32>,
    /// Show this many lines before each match. Conflicts with `context`.
    #[serde(default)]
    pub before: Option<u32>,
    /// Show this many lines after each match. Conflicts with `context`.
    #[serde(default)]
    pub after: Option<u32>,
    /// Maximum matches to include in the returned JSON. Set to 0 for no
    /// match-count cap.
    #[serde(default = "default_max_matches")]
    pub max_matches: usize,
    /// Maximum bytes in the returned JSON. Search stops before adding a match
    /// that would exceed this budget.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AstGrepStrictness {
    Cst,
    Smart,
    Ast,
    Relaxed,
    Signature,
    Template,
}

impl AstGrepStrictness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cst => "cst",
            Self::Smart => "smart",
            Self::Ast => "ast",
            Self::Relaxed => "relaxed",
            Self::Signature => "signature",
            Self::Template => "template",
        }
    }
}

pub struct AstGrepTool;

#[async_trait]
impl Tool for AstGrepTool {
    fn name(&self) -> &str {
        "ast_grep"
    }

    fn description(&self) -> &str {
        "Search code structurally with ast-grep and return JSON matches. \
         Read-only; use edit/write for any follow-up changes."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(AstGrepParams)).unwrap()
    }

    async fn execute(
        &self,
        call_id: &str,
        input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<String> {
        let _scope = crate::nemo_obs::tool_scope("ast_grep");
        let params: AstGrepParams =
            serde_json::from_value(input).context("invalid params for ast_grep")?;
        let invocation = AstGrepInvocation::from_params(params)?;
        let binary = find_ast_grep_binary()?;

        let _ = ctx.events.send(Event::ToolCallUpdate {
            id: call_id.to_string(),
            chunk: format!("[ast-grep] {}", invocation.summary()),
        });

        let output = run_ast_grep(&binary, &invocation, &ctx.cwd).await?;
        let code = output.exit_code;
        let _ = ctx.events.send(Event::ToolCallUpdate {
            id: call_id.to_string(),
            chunk: format!("[exit={code}]"),
        });

        format_ast_grep_output(&output, &invocation)
    }
}

#[derive(Debug)]
struct AstGrepInvocation {
    args: Vec<String>,
    max_matches: usize,
    max_output_bytes: usize,
}

impl AstGrepInvocation {
    fn from_params(params: AstGrepParams) -> Result<Self> {
        validate_query(&params)?;

        let mut args = vec!["run".to_string(), "--json=stream".to_string()];
        args.push("--pattern".into());
        args.push(params.pattern);
        if let Some(lang) = params.lang.filter(|l| !l.trim().is_empty()) {
            args.push("--lang".into());
            args.push(lang);
        }
        if let Some(selector) = params.selector.filter(|s| !s.trim().is_empty()) {
            args.push("--selector".into());
            args.push(selector);
        }
        if let Some(strictness) = params.strictness {
            args.push("--strictness".into());
            args.push(strictness.as_str().into());
        }
        if params.follow {
            args.push("--follow".into());
        }
        for glob in params.globs.into_iter().filter(|g| !g.trim().is_empty()) {
            args.push("--globs".into());
            args.push(glob);
        }
        if let Some(n) = params.context {
            args.push("--context".into());
            args.push(n.to_string());
        } else {
            if let Some(n) = params.before {
                args.push("--before".into());
                args.push(n.to_string());
            }
            if let Some(n) = params.after {
                args.push("--after".into());
                args.push(n.to_string());
            }
        }

        let paths: Vec<String> = params
            .paths
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect();
        if paths.is_empty() {
            args.push(".".into());
        } else {
            args.extend(paths);
        }

        Ok(Self {
            args,
            max_matches: params.max_matches,
            max_output_bytes: params.max_output_bytes,
        })
    }

    fn summary(&self) -> String {
        shell_words("ast-grep", &self.args)
    }
}

fn validate_query(params: &AstGrepParams) -> Result<()> {
    if params.pattern.trim().is_empty() {
        return Err(anyhow!("ast_grep requires a non-empty pattern"));
    }

    if params.context.is_some() && (params.before.is_some() || params.after.is_some()) {
        return Err(anyhow!("context conflicts with before/after"));
    }
    Ok(())
}

fn find_ast_grep_binary() -> Result<PathBuf> {
    which::which("ast-grep")
        .or_else(|_| which::which("sg"))
        .context("ast-grep not found on PATH; install `ast-grep` or `sg` to use ast_grep")
}

async fn run_ast_grep(
    binary: &Path,
    invocation: &AstGrepInvocation,
    cwd: &Path,
) -> Result<AstGrepOutput> {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(&invocation.args);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn `{}` in {}", invocation.summary(), cwd.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("ast-grep stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("ast-grep stderr was not piped"))?;

    let stderr_limit = if invocation.max_output_bytes == 0 {
        DEFAULT_MAX_OUTPUT_BYTES
    } else {
        invocation.max_output_bytes.min(DEFAULT_MAX_OUTPUT_BYTES)
    };
    let stderr_task = tokio::spawn(async move { read_limited(stderr, stderr_limit).await });

    let mut collector = MatchCollector::new(invocation.max_matches, invocation.max_output_bytes);
    collector
        .collect(BufReader::new(stdout))
        .await
        .context("read ast-grep stdout")?;

    if collector.hit_budget {
        let _ = child.start_kill();
    }

    let status = child
        .wait()
        .await
        .with_context(|| format!("wait `{}`", invocation.summary()))?;
    let stderr = stderr_task
        .await
        .context("join ast-grep stderr reader")?
        .context("read ast-grep stderr")?;

    Ok(AstGrepOutput {
        exit_code: status.code().unwrap_or(-1),
        matches: collector.matches,
        observed_matches: collector.observed_matches,
        truncated: collector.hit_budget,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn format_ast_grep_output(
    output: &AstGrepOutput,
    invocation: &AstGrepInvocation,
) -> Result<String> {
    let code = output.exit_code;

    if !(code == 0 || code == 1 || output.truncated) {
        let mut combined = String::new();
        if !output.stderr.trim().is_empty() {
            combined.push_str(output.stderr.trim_end());
        }
        return Err(anyhow!(
            "ast-grep exited with status {code}: {}",
            if combined.is_empty() {
                "<no output>"
            } else {
                combined.as_str()
            }
        ));
    }

    build_result_json(
        output.matches.clone(),
        output.observed_matches,
        code,
        output.stderr.trim(),
        invocation.max_output_bytes,
        output.truncated,
    )
}

#[derive(Debug)]
struct AstGrepOutput {
    exit_code: i32,
    matches: Vec<serde_json::Value>,
    observed_matches: usize,
    truncated: bool,
    stderr: String,
}

struct MatchCollector {
    max_matches: usize,
    max_output_bytes: usize,
    matches: Vec<serde_json::Value>,
    observed_matches: usize,
    hit_budget: bool,
}

impl MatchCollector {
    fn new(max_matches: usize, max_output_bytes: usize) -> Self {
        Self {
            max_matches,
            max_output_bytes,
            matches: Vec::new(),
            observed_matches: 0,
            hit_budget: false,
        }
    }

    async fn collect<R>(&mut self, reader: R) -> Result<()>
    where
        R: AsyncBufRead + Unpin,
    {
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            self.observed_matches += 1;
            let value: serde_json::Value = serde_json::from_str(&line).with_context(|| {
                format!("parse ast-grep JSON stream line {}", self.observed_matches)
            })?;
            if self.can_accept(&value)? {
                self.matches.push(value);
                continue;
            }
            self.hit_budget = true;
            break;
        }
        Ok(())
    }

    fn can_accept(&self, value: &serde_json::Value) -> Result<bool> {
        if self.max_matches > 0 && self.matches.len() >= self.max_matches {
            return Ok(false);
        }
        if self.max_output_bytes == 0 {
            return Ok(true);
        }

        let mut probe = self.matches.clone();
        probe.push(value.clone());
        let rendered = render_result_json(&probe, self.observed_matches, -1, "", true)?;
        Ok(rendered.len() <= self.max_output_bytes)
    }
}

fn build_result_json(
    matches: Vec<serde_json::Value>,
    total_matches: usize,
    exit_code: i32,
    stderr: &str,
    max_output_bytes: usize,
    already_truncated: bool,
) -> Result<String> {
    let truncated = already_truncated || matches.len() < total_matches;
    let rendered = render_result_json(&matches, total_matches, exit_code, stderr, truncated)?;
    if max_output_bytes == 0 || rendered.len() <= max_output_bytes || matches.is_empty() {
        return Ok(rendered);
    }

    let value = json!({
        "matches": [],
        "total_matches": total_matches,
        "truncated": true,
        "exit_code": exit_code,
        "stderr": if stderr.is_empty() { serde_json::Value::Null } else { json!(stderr) },
    });
    serde_json::to_string_pretty(&value).map_err(Into::into)
}

fn render_result_json(
    matches: &[serde_json::Value],
    total_matches: usize,
    exit_code: i32,
    stderr: &str,
    truncated: bool,
) -> Result<String> {
    let value = json!({
        "matches": matches,
        "total_matches": total_matches,
        "truncated": truncated,
        "exit_code": exit_code,
        "stderr": if stderr.is_empty() { serde_json::Value::Null } else { json!(stderr) },
    });
    serde_json::to_string_pretty(&value).map_err(Into::into)
}

async fn read_limited<R>(mut reader: R, limit: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 8192];
    let mut truncated = false;

    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }

        let remaining = limit.saturating_sub(buf.len());
        if remaining > 0 {
            let keep = n.min(remaining);
            buf.extend_from_slice(&chunk[..keep]);
            truncated |= keep < n;
        } else {
            truncated = true;
        }
    }

    if truncated {
        if !buf.ends_with(b"\n") {
            buf.push(b'\n');
        }
        buf.extend_from_slice(b"[stderr truncated]\n");
    }
    Ok(buf)
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

fn default_max_matches() -> usize {
    DEFAULT_MAX_MATCHES
}

fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn params(pattern: &str) -> AstGrepParams {
        AstGrepParams {
            pattern: pattern.into(),
            paths: Vec::new(),
            lang: None,
            selector: None,
            strictness: None,
            globs: Vec::new(),
            follow: false,
            context: None,
            before: None,
            after: None,
            max_matches: DEFAULT_MAX_MATCHES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    #[test]
    fn builds_ast_grep_run_args() {
        let mut params = params("foo($A)");
        params.lang = Some("rust".into());
        params.strictness = Some(AstGrepStrictness::Ast);
        params.globs = vec!["src/**/*.rs".into(), "!target/**".into()];
        params.context = Some(2);
        params.paths = vec!["src".into()];

        let invocation = AstGrepInvocation::from_params(params).unwrap();

        assert_eq!(
            invocation.args,
            vec![
                "run",
                "--json=stream",
                "--pattern",
                "foo($A)",
                "--lang",
                "rust",
                "--strictness",
                "ast",
                "--globs",
                "src/**/*.rs",
                "--globs",
                "!target/**",
                "--context",
                "2",
                "src",
            ]
        );
    }

    #[test]
    fn defaults_to_current_directory() {
        let invocation = AstGrepInvocation::from_params(params("foo($A)")).unwrap();
        assert_eq!(invocation.args.last().unwrap(), ".");
    }

    #[test]
    fn rejects_empty_pattern() {
        assert!(AstGrepInvocation::from_params(params("")).is_err());
    }

    #[test]
    fn rejects_conflicting_context_options() {
        let mut params = params("foo($A)");
        params.context = Some(1);
        params.before = Some(1);
        assert!(AstGrepInvocation::from_params(params).is_err());
    }

    #[test]
    fn no_matches_exit_code_is_successful_empty_json() {
        let invocation = AstGrepInvocation::from_params(params("foo($A)")).unwrap();
        let output = AstGrepOutput {
            exit_code: 1,
            matches: Vec::new(),
            observed_matches: 0,
            truncated: false,
            stderr: String::new(),
        };

        let rendered = format_ast_grep_output(&output, &invocation).unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["matches"], json!([]));
        assert_eq!(value["total_matches"], json!(0));
        assert_eq!(value["truncated"], json!(false));
    }

    #[test]
    fn limits_matches_and_preserves_valid_json() {
        let mut invocation = AstGrepInvocation::from_params(params("foo($A)")).unwrap();
        invocation.max_matches = 1;
        let output = AstGrepOutput {
            exit_code: 0,
            matches: vec![json!({"file":"a.rs","text":"foo(1)"})],
            observed_matches: 2,
            truncated: true,
            stderr: String::new(),
        };

        let rendered = format_ast_grep_output(&output, &invocation).unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["matches"].as_array().unwrap().len(), 1);
        assert_eq!(value["total_matches"], json!(2));
        assert_eq!(value["truncated"], json!(true));
    }

    #[tokio::test]
    async fn collector_stops_when_match_budget_is_reached() {
        let input = br#"{"file":"a.rs","text":"foo(1)"}
{"file":"b.rs","text":"foo(2)"}
{"file":"c.rs","text":"foo(3)"}
"#;
        let mut collector = MatchCollector::new(1, 0);

        collector
            .collect(BufReader::new(Cursor::new(input)))
            .await
            .unwrap();

        assert!(collector.hit_budget);
        assert_eq!(collector.observed_matches, 2);
        assert_eq!(
            collector.matches,
            vec![json!({"file":"a.rs","text":"foo(1)"})]
        );
    }

    #[tokio::test]
    async fn collector_stops_when_output_budget_is_reached() {
        let first = json!({"file":"a.rs","text":"foo(1)"});
        let second = json!({"file":"b.rs","text":"foo(2)"});
        let third = json!({"file":"c.rs","text":"foo(3)"});
        let input = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap(),
            serde_json::to_string(&third).unwrap()
        );
        let budget = render_result_json(&[first.clone()], 1, -1, "", true)
            .unwrap()
            .len();
        let mut collector = MatchCollector::new(0, budget);

        collector
            .collect(BufReader::new(Cursor::new(input)))
            .await
            .unwrap();

        assert!(collector.hit_budget);
        assert_eq!(collector.observed_matches, 2);
        assert_eq!(collector.matches, vec![first]);
    }

    #[tokio::test]
    async fn run_ast_grep_uses_session_cwd_for_relative_paths() {
        let Ok(binary) = find_ast_grep_binary() else {
            return;
        };
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("sample.rs"), "fn target() {}\n")
            .await
            .unwrap();

        let mut params = params("fn target() {}");
        params.lang = Some("rust".into());
        params.paths = vec!["sample.rs".into()];
        let invocation = AstGrepInvocation::from_params(params).unwrap();
        let output = run_ast_grep(&binary, &invocation, tmp.path())
            .await
            .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.observed_matches, 1);
        assert!(output.matches[0]["file"]
            .as_str()
            .unwrap()
            .ends_with("sample.rs"));
    }
}
