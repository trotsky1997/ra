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
    /// Maximum bytes in the returned JSON. Matches are dropped from the end if
    /// needed to preserve valid JSON.
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

        let output = run_ast_grep(&binary, &invocation).await?;
        let code = output.status.code().unwrap_or(-1);
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
) -> Result<std::process::Output> {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(&invocation.args);
    cmd.stdin(Stdio::null());
    cmd.output()
        .await
        .with_context(|| format!("spawn `{}`", invocation.summary()))
}

fn format_ast_grep_output(
    output: &std::process::Output,
    invocation: &AstGrepInvocation,
) -> Result<String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().unwrap_or(-1);

    if !(output.status.success() || code == 1) {
        let mut combined = String::new();
        if !stdout.trim().is_empty() {
            combined.push_str(stdout.trim_end());
        }
        if !stderr.trim().is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(stderr.trim_end());
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

    let mut matches = Vec::new();
    for (idx, line) in stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let value: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("parse ast-grep JSON stream line {}", idx + 1))?;
        matches.push(value);
    }

    let total_matches = matches.len();
    if invocation.max_matches > 0 && matches.len() > invocation.max_matches {
        matches.truncate(invocation.max_matches);
    }

    build_result_json(
        matches,
        total_matches,
        code,
        stderr.trim(),
        invocation.max_output_bytes,
    )
}

fn build_result_json(
    mut matches: Vec<serde_json::Value>,
    total_matches: usize,
    exit_code: i32,
    stderr: &str,
    max_output_bytes: usize,
) -> Result<String> {
    let mut truncated = matches.len() < total_matches;

    loop {
        let value = json!({
            "matches": matches,
            "total_matches": total_matches,
            "truncated": truncated,
            "exit_code": exit_code,
            "stderr": if stderr.is_empty() { serde_json::Value::Null } else { json!(stderr) },
        });
        let rendered = serde_json::to_string_pretty(&value)?;
        if max_output_bytes == 0 || rendered.len() <= max_output_bytes || matches.is_empty() {
            return Ok(rendered);
        }
        matches.pop();
        truncated = true;
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

fn default_max_matches() -> usize {
    DEFAULT_MAX_MATCHES
}

fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

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
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: Vec::new(),
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
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: br#"{"file":"a.rs","text":"foo(1)"}
{"file":"b.rs","text":"foo(2)"}
"#
            .to_vec(),
            stderr: Vec::new(),
        };

        let rendered = format_ast_grep_output(&output, &invocation).unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["matches"].as_array().unwrap().len(), 1);
        assert_eq!(value["total_matches"], json!(2));
        assert_eq!(value["truncated"], json!(true));
    }
}
