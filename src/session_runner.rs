//! Protocol-neutral runner that wraps a `Session` and produces a stream of
//! `RunnerEvent`s. Each ACP / A2A / future-other server consumes the same
//! events and translates them into its own wire format.
//!
//! The runner owns:
//! - the turn-loop driving (calls `Session::prompt`),
//! - slash-command interception (`/clear`, `/compact`, `/models`, `/mode`),
//! - subscribing to the broadcast bus and translating Ra `Event` →
//!   `RunnerEvent`,
//! - per-prompt observability scope (`nemo_obs::with_task_scope` +
//!   `agent_scope`),
//! - persisting the trajectory at end-of-turn (via the `RunnerHost` trait
//!   so the host stays protocol-neutral),
//! - emitting a final `UsageReport` event with token estimates.

use crate::events::Event as RaEvent;
use crate::model::Message as RaMessage;
use crate::model::Model;
use crate::nemo_obs;
use crate::session::{PromptOutcome, Session, SessionRuntimeScope};
use crate::skills::{SkillRuntimeOptions, SlashTemplate};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::process::Command;

/// Hint to clients about the kind of work a tool does. Maps to ACP `ToolKind`
/// and A2A artifact roles, but stays protocol-neutral here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKindHint {
    Read,
    Search,
    Execute,
    Other,
}

/// Outcome of a single `run_input` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// Turn loop ran to natural completion.
    Completed,
    /// `Session::cancel` was triggered before the loop finished.
    Cancelled,
    /// Anything else: bubble up the error message for the server to translate.
    Failed(String),
}

/// Protocol-neutral event stream. Consumers translate to ACP `SessionUpdate`
/// or A2A `StreamResponse` etc.
#[derive(Debug, Clone)]
pub enum RunnerEvent {
    /// Prompt started running (after slash-command dispatch).
    Started,
    /// Streaming text chunk from the assistant.
    TextDelta(String),
    /// Streaming reasoning/thinking chunk from the assistant.
    ThinkingDelta(String),
    /// Tool call started.
    ToolCallStart {
        id: String,
        name: String,
        input: serde_json::Value,
        title: String,
        kind: ToolKindHint,
    },
    /// Tool call finished. Carries the entire output payload.
    ToolCallEnd {
        id: String,
        is_error: bool,
        content: String,
    },
    /// Session mode flipped (e.g. via /mode slash command). Servers may
    /// re-broadcast this on their wire (ACP CurrentModeUpdate, etc.).
    ModeChanged(String),
    /// Final token-accounting report. Always emitted once per `run_input`
    /// just before `Finished`.
    UsageReport { used: u64, size: u64 },
    /// Final event. Always the last item delivered to the callback.
    Finished(RunOutcome),
}

#[derive(Clone)]
struct PreparedSkillRuntime {
    scope: SessionRuntimeScope,
    stop_hooks: Option<Arc<crate::hooks::HookEngine>>,
    forked: bool,
}

/// Narrow capability surface a `SessionRunner` needs from whatever holds
/// long-lived agent state. Lets the protocol-specific server (ACP,
/// A2A, …) own the actual `SharedState` without leaking ACP types
/// into the runner.
#[async_trait]
pub trait RunnerHost: Send + Sync {
    /// Persist the named session's current trajectory.
    async fn save_session(&self, session_id: &str);

    /// Default context window for the active model, used as `size` in
    /// `UsageReport`.
    fn default_ctx_window(&self) -> u64;

    /// Names + ids of advertised models, used by `/models` slash command.
    fn list_models_for_display(&self) -> Vec<(String, String)>;

    /// Resolve a model id for invocation-scoped skill overrides.
    fn build_model_for_id(&self, _model_id: &str) -> Option<Arc<dyn Model>> {
        None
    }
}

/// Slash command parsed from a user message.
#[derive(Debug)]
struct SlashCommand {
    name: String,
    args: String,
}

const SLASH_COMMANDS: &[&str] = &["clear", "compact", "models", "mode"];

fn parse_slash_command(text: &str, extra_names: &[&str]) -> Option<SlashCommand> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim_start()),
        None => (rest, ""),
    };
    let name_lc = name.to_lowercase();
    let known =
        SLASH_COMMANDS.contains(&name_lc.as_str()) || extra_names.iter().any(|n| *n == name_lc);
    if !known {
        return None;
    }
    Some(SlashCommand {
        name: name_lc,
        args: args.to_string(),
    })
}

/// The actual runner. Cheap to construct; owns no resources of its own
/// beyond Arcs to `Session` and the host.
pub struct SessionRunner {
    session: Arc<Session>,
    session_id: String,
    host: Arc<dyn RunnerHost>,
    /// Map of slash command name → template body. When the user message
    /// is `/<name>` (no args) or `/<name> <args>`, the body is sent to
    /// the LLM as the actual prompt instead. Built from disk via
    /// `crate::skills::load_prompts` and injected from the host.
    prompt_templates: Arc<HashMap<String, SlashTemplate>>,
}

impl SessionRunner {
    pub fn new(session: Arc<Session>, session_id: String, host: Arc<dyn RunnerHost>) -> Self {
        Self {
            session,
            session_id,
            host,
            prompt_templates: Arc::new(HashMap::new()),
        }
    }

    /// Builder: attach a set of prompt templates. The slash dispatcher
    /// will fire any `/<name>` whose name matches a key here, sending
    /// the value as the LLM prompt.
    #[must_use]
    pub fn with_prompt_templates(mut self, templates: Arc<HashMap<String, SlashTemplate>>) -> Self {
        self.prompt_templates = templates;
        self
    }

    /// Drive one user input through the session. Each `RunnerEvent` is
    /// delivered synchronously to `on_event` in emit order. Returns the
    /// final `RunOutcome` (also delivered as `RunnerEvent::Finished`).
    ///
    /// The `on_event` closure runs on whatever task this future runs on;
    /// servers typically `tokio::spawn` the future so the closure may
    /// `cx.send_notification(...)` synchronously.
    pub async fn run_input<F>(&self, user_text: String, mut on_event: F) -> RunOutcome
    where
        F: FnMut(RunnerEvent) + Send,
    {
        let scope_label = format!("session/prompt {}", self.session_id);
        let used_size = self.host.default_ctx_window();

        // The whole prompt body runs inside one Agent-typed observability
        // scope, with `with_task_scope` pinning the task-local stack so
        // nested LLM/tool scopes pop cleanly across thread migrations.
        let result = nemo_obs::with_task_scope(async {
            let _agent = nemo_obs::agent_scope(&scope_label);
            let result = self.run_input_inner(user_text, &mut on_event).await;
            // Emit terminal events while the observability scope is still
            // alive so they appear inside the agent span.
            let used = self.session.estimate_used_tokens().await;
            on_event(RunnerEvent::UsageReport {
                used,
                size: used_size,
            });
            on_event(RunnerEvent::Finished(result.clone()));
            // AgentEnd hook fires last, so log/cleanup tools see the
            // final state (including any TextDelta drained by the bus
            // race in run_input_inner).
            if let Some(hooks) = self.session.hooks() {
                hooks.stop(Some(&self.session_id)).await;
            }
            result
        })
        .await;
        self.host.save_session(&self.session_id).await;
        result
    }

    async fn run_input_inner<F>(&self, user_text: String, on_event: &mut F) -> RunOutcome
    where
        F: FnMut(RunnerEvent) + Send,
    {
        on_event(RunnerEvent::Started);

        let template_names: Vec<&str> = self.prompt_templates.keys().map(|s| s.as_str()).collect();
        let mut effective_text = user_text;
        let mut skill_runtime = None;
        if let Some(cmd) = parse_slash_command(&effective_text, &template_names) {
            // Built-in slash commands run server-side; user-defined prompt
            // templates expand into a fresh prompt that *does* hit the LLM.
            if SLASH_COMMANDS.contains(&cmd.name.as_str()) {
                self.run_slash(&cmd, on_event).await;
                return RunOutcome::Completed;
            }
            if let Some(template) = self.prompt_templates.get(&cmd.name).cloned() {
                skill_runtime = template.runtime.clone();
                if let Some(runtime) = &skill_runtime {
                    match render_dynamic_shell_context(
                        template.body.clone(),
                        runtime,
                        self.session.cwd(),
                    )
                    .await
                    {
                        Ok(rendered) => {
                            let shell_rendered_template = SlashTemplate {
                                body: rendered,
                                ..template
                            };
                            effective_text =
                                render_slash_template(&shell_rendered_template, &cmd.args);
                        }
                        Err(e) => return RunOutcome::Failed(format!("{e:#}")),
                    }
                    if let Some(context) = runtime.prompt_context() {
                        effective_text = format!("[skill context]\n{context}\n\n{effective_text}");
                    }
                } else {
                    effective_text = render_slash_template(&template, &cmd.args);
                }
            }
        }

        let prepared_runtime = match skill_runtime.as_ref() {
            Some(runtime) => match self.runtime_scope_for(runtime).await {
                Ok(prepared) => Some(prepared),
                Err(e) => return RunOutcome::Failed(format!("{e:#}")),
            },
            None => None,
        };
        let scope = prepared_runtime
            .as_ref()
            .map(|prepared| prepared.scope.clone());

        // UserPromptSubmit hooks run after slash expansion so skill-scoped
        // hooks observe the same prompt that will reach the model.
        if let Some(hooks) = self.session.effective_hooks_for_scope(scope.as_ref()) {
            let decision = hooks
                .user_prompt_submit(Some(&self.session_id), &effective_text)
                .await;
            if let Some(reason) = decision.stop.clone() {
                on_event(RunnerEvent::TextDelta(format!(
                    "[stopped by hook] {reason}"
                )));
                hooks.stop(Some(&self.session_id)).await;
                return RunOutcome::Failed(reason);
            }
            if let Some(reason) = decision.block.clone() {
                on_event(RunnerEvent::TextDelta(format!(
                    "[blocked by hook] {reason}"
                )));
                hooks.stop(Some(&self.session_id)).await;
                return RunOutcome::Failed(reason);
            }
            if let Some(extra) = decision.additional_context {
                effective_text = format!("[hook context]\n{extra}\n\n{effective_text}");
            }
        }

        // Subscribe BEFORE prompt() so we don't miss the first events.
        let mut rx = self.session.subscribe();
        let session = self.session.clone();
        let forked = prepared_runtime
            .as_ref()
            .map(|prepared| prepared.forked)
            .unwrap_or(false);
        let prompt_fut: BoxFuture<'_, Result<PromptOutcome>> = Box::pin(async move {
            if forked {
                session.prompt_forked(effective_text, scope).await
            } else if let Some(scope) = scope {
                session.prompt_scoped(effective_text, scope).await
            } else {
                session.prompt(effective_text).await
            }
        });

        // Pump events from the broadcast channel into the callback while
        // the prompt future runs concurrently. Stops when AgentEnd is
        // observed OR when the prompt future resolves.
        let mut outcome = None;
        tokio::pin!(prompt_fut);
        loop {
            tokio::select! {
                biased;
                ev = rx.recv() => {
                    match ev {
                        Ok(RaEvent::TextDelta(s)) => on_event(RunnerEvent::TextDelta(s)),
                        Ok(RaEvent::ThinkingDelta(s)) => on_event(RunnerEvent::ThinkingDelta(s)),
                        Ok(RaEvent::ToolCallStart(c)) => {
                            let kind = tool_kind(&c.name);
                            let title = tool_title(&c.name, &c.input);
                            on_event(RunnerEvent::ToolCallStart {
                                id: c.id.clone(),
                                name: c.name.clone(),
                                input: c.input.clone(),
                                title,
                                kind,
                            });
                        }
                        Ok(RaEvent::ToolCallEnd(r)) => {
                            on_event(RunnerEvent::ToolCallEnd {
                                id: r.call_id,
                                is_error: r.is_error,
                                content: r.content,
                            });
                        }
                        Ok(RaEvent::AgentEnd) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                done = &mut prompt_fut => {
                    outcome = Some(match done {
                        Ok(PromptOutcome::Completed) => RunOutcome::Completed,
                        Ok(PromptOutcome::Cancelled) => RunOutcome::Cancelled,
                        Err(e) => RunOutcome::Failed(format!("{e:#}")),
                    });
                    // Drain any final events the bus has buffered. The
                    // prompt future may have resolved before we'd looped
                    // around to read the last broadcast frames; if we
                    // don't drain them here the consumer never sees them.
                    while let Ok(ev) = rx.try_recv() {
                        match ev {
                            RaEvent::TextDelta(s) => on_event(RunnerEvent::TextDelta(s)),
                            RaEvent::ThinkingDelta(s) => on_event(RunnerEvent::ThinkingDelta(s)),
                            RaEvent::ToolCallStart(c) => {
                                let kind = tool_kind(&c.name);
                                let title = tool_title(&c.name, &c.input);
                                on_event(RunnerEvent::ToolCallStart {
                                    id: c.id.clone(),
                                    name: c.name.clone(),
                                    input: c.input.clone(),
                                    title,
                                    kind,
                                });
                            }
                            RaEvent::ToolCallEnd(r) => {
                                on_event(RunnerEvent::ToolCallEnd {
                                    id: r.call_id,
                                    is_error: r.is_error,
                                    content: r.content,
                                });
                            }
                            RaEvent::AgentEnd => break,
                            _ => {}
                        }
                    }
                    break;
                }
            }
        }
        let outcome = outcome.unwrap_or(RunOutcome::Completed);
        if matches!(outcome, RunOutcome::Completed) {
            if let Some(hooks) = prepared_runtime.and_then(|prepared| prepared.stop_hooks) {
                hooks.stop(Some(&self.session_id)).await;
            }
        }
        outcome
    }

    async fn runtime_scope_for(
        &self,
        runtime: &SkillRuntimeOptions,
    ) -> Result<PreparedSkillRuntime> {
        let model = match runtime.model.as_deref() {
            Some(model_id) => Some(
                self.host
                    .build_model_for_id(model_id)
                    .with_context(|| format!("skill requested unknown model `{model_id}`"))?,
            ),
            None => None,
        };
        let skill_hooks = crate::hooks::HookEngine::from_config(&runtime.hooks);
        let hooks = if skill_hooks.is_empty() {
            None
        } else {
            Some(Arc::new(skill_hooks))
        };
        Ok(PreparedSkillRuntime {
            scope: SessionRuntimeScope {
                model,
                allowed_tools: runtime.allowed_tools.clone(),
                disallowed_tools: runtime.disallowed_tools.clone(),
                hooks: hooks.clone(),
            },
            stop_hooks: hooks,
            forked: runtime.is_fork(),
        })
    }

    async fn run_slash<F>(&self, cmd: &SlashCommand, on_event: &mut F)
    where
        F: FnMut(RunnerEvent) + Send,
    {
        let send = |s: String, ev: &mut F| ev(RunnerEvent::TextDelta(s));
        match cmd.name.as_str() {
            "clear" => {
                self.session.restore_messages(Vec::new()).await;
                send("Session cleared.".into(), on_event);
            }
            "compact" => {
                let snapshot = self.session.snapshot_messages().await;
                let n = snapshot.len();
                let focus = if cmd.args.is_empty() {
                    String::new()
                } else {
                    format!(" focus={}", cmd.args)
                };
                self.session
                    .restore_messages(vec![RaMessage::User {
                        content: format!("[compacted: {n} prior messages elided]{focus}"),
                    }])
                    .await;
                send(format!("Compacted {n} prior messages."), on_event);
            }
            "models" => {
                let mut buf = String::from("Available models:\n");
                for (id, name) in self.host.list_models_for_display() {
                    buf.push_str(&format!("  • {id} — {name}\n"));
                }
                send(buf, on_event);
            }
            "mode" => {
                let valid = ["default", "plan", "ask"];
                let target = cmd.args.split_whitespace().next().unwrap_or("");
                if !valid.contains(&target) {
                    send(
                        format!(
                            "Unknown mode: '{target}'. Choose one of: {}.",
                            valid.join(", ")
                        ),
                        on_event,
                    );
                    return;
                }
                self.session.set_mode(target).await;
                on_event(RunnerEvent::ModeChanged(target.to_string()));
                send(format!("Mode → {target}."), on_event);
            }
            _ => unreachable!("parse_slash_command vetted the name"),
        }
    }
}

fn render_slash_template(template: &SlashTemplate, args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        return template.body.clone();
    }

    let argv = split_slash_args(args);
    let mut rendered = template.body.clone();
    let mut replaced = false;

    for (i, value) in argv.iter().enumerate() {
        let indexed_placeholder = format!("$ARGUMENTS[{i}]");
        if rendered.contains(&indexed_placeholder) {
            rendered = rendered.replace(&indexed_placeholder, value);
            replaced = true;
        }
        let placeholder = format!("${i}");
        if rendered.contains(&placeholder) {
            rendered = rendered.replace(&placeholder, value);
            replaced = true;
        }
    }
    if rendered.contains("$ARGUMENTS") {
        rendered = rendered.replace("$ARGUMENTS", args);
        replaced = true;
    }
    for (i, name) in template.arguments.iter().enumerate() {
        let Some(value) = argv.get(i) else { continue };
        let placeholder = format!("${name}");
        if rendered.contains(&placeholder) {
            rendered = rendered.replace(&placeholder, value);
            replaced = true;
        }
    }

    if replaced {
        rendered
    } else if template.append_arguments_fallback {
        format!("{rendered}\n\nARGUMENTS: {args}")
    } else {
        format!("{rendered}\n\n{args}")
    }
}

fn split_slash_args(args: &str) -> Vec<String> {
    args.split_whitespace().map(ToOwned::to_owned).collect()
}

async fn render_dynamic_shell_context(
    text: String,
    runtime: &SkillRuntimeOptions,
    cwd: &Path,
) -> Result<String> {
    let text = render_fenced_shell_context(text, runtime, cwd).await?;
    render_inline_shell_context(text, runtime, cwd).await
}

async fn render_fenced_shell_context(
    text: String,
    runtime: &SkillRuntimeOptions,
    cwd: &Path,
) -> Result<String> {
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("```!") {
        out.push_str(&rest[..start]);
        let after_marker = &rest[start + 4..];
        let command_start = after_marker
            .strip_prefix("\r\n")
            .map(|s| (s, 2usize))
            .or_else(|| after_marker.strip_prefix('\n').map(|s| (s, 1usize)));
        let (after_newline, consumed_newline) = match command_start {
            Some(pair) => pair,
            None => {
                out.push_str("```!");
                rest = after_marker;
                continue;
            }
        };
        if let Some(end) = after_newline.find("\n```") {
            let command = &after_newline[..end];
            out.push_str(&run_shell_context(command, runtime, cwd).await);
            let after_end = &after_newline[end + 4..];
            rest = if let Some(stripped) = after_end.strip_prefix("\r\n") {
                stripped
            } else if let Some(stripped) = after_end.strip_prefix('\n') {
                stripped
            } else {
                after_end
            };
        } else {
            out.push_str("```!");
            out.push_str(&after_marker[..consumed_newline]);
            rest = after_newline;
        }
    }
    out.push_str(rest);
    Ok(out)
}

async fn render_inline_shell_context(
    text: String,
    runtime: &SkillRuntimeOptions,
    cwd: &Path,
) -> Result<String> {
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("!`") {
        out.push_str(&rest[..start]);
        let after_marker = &rest[start + 2..];
        if let Some(end) = after_marker.find('`') {
            let command = &after_marker[..end];
            out.push_str(&run_shell_context(command, runtime, cwd).await);
            rest = &after_marker[end + 1..];
        } else {
            out.push_str("!`");
            rest = after_marker;
            break;
        }
    }
    out.push_str(rest);
    Ok(out)
}

async fn run_shell_context(command: &str, runtime: &SkillRuntimeOptions, cwd: &Path) -> String {
    let command = command.trim();
    if command.is_empty() {
        return String::new();
    }
    let shell = runtime.shell.as_deref().unwrap_or("");
    let mut cmd = if shell.eq_ignore_ascii_case("bash") {
        let mut c = Command::new("bash");
        c.arg("-lc").arg(command);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(command);
        c
    };
    let output = cmd.current_dir(cwd).output().await;
    match output {
        Ok(output) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.stderr.is_empty() {
                combined.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            if output.status.success() {
                combined
            } else {
                format!(
                    "[shell context command failed: `{command}` exited with {}]\n{}",
                    output.status.code().unwrap_or(-1),
                    combined
                )
            }
        }
        Err(e) => format!("[shell context command failed: `{command}`: {e}]"),
    }
}

fn tool_kind(name: &str) -> ToolKindHint {
    match name {
        "read" | "grep" | "glob" | "ls" | "fuzzy" => ToolKindHint::Read,
        "ast_grep" => ToolKindHint::Search,
        "bash" | "git" | "gh" => ToolKindHint::Execute,
        _ => ToolKindHint::Other,
    }
}

/// Human-readable title for a tool call, surfaced in client UI.
fn tool_title(name: &str, input: &serde_json::Value) -> String {
    match name {
        "read" => input
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("Read {p}"))
            .unwrap_or_else(|| "Read".into()),
        "bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| {
                let s = truncate_chars(c, 60);
                format!("$ {s}")
            })
            .unwrap_or_else(|| "Run shell".into()),
        "ast_grep" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|q| {
                let s = truncate_chars(q, 60);
                format!("ast-grep {s}")
            })
            .unwrap_or_else(|| "ast-grep search".into()),
        "git" | "gh" => input
            .get("args")
            .and_then(|v| v.as_array())
            .map(|args| {
                let raw = std::iter::once(name.to_string())
                    .chain(
                        args.iter()
                            .filter_map(|v| v.as_str())
                            .map(ToString::to_string),
                    )
                    .collect::<Vec<_>>()
                    .join(" ");
                let s = truncate_chars(&raw, 60);
                format!("$ {s}")
            })
            .unwrap_or_else(|| format!("Run {name}")),
        "grep" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|p| format!("Search {p}"))
            .unwrap_or_else(|| "Search".into()),
        "glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|p| format!("Find {p}"))
            .unwrap_or_else(|| "Find files".into()),
        "ls" => input
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("List {p}"))
            .unwrap_or_else(|| "List files".into()),
        "fuzzy" => input
            .get("query")
            .and_then(|v| v.as_str())
            .map(|q| format!("Fuzzy {q}"))
            .unwrap_or_else(|| "Fuzzy filter".into()),
        "apply_patch" => {
            if input
                .get("check_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                "Check patch".into()
            } else {
                "Apply patch".into()
            }
        }
        other => other.to_string(),
    }
}

fn truncate_chars(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(limit).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_title_truncates_multibyte_ast_grep_pattern_safely() {
        let pattern = format!("{}界tail", "a".repeat(59));
        let title = tool_title("ast_grep", &json!({ "pattern": pattern }));

        assert!(title.starts_with("ast-grep "));
        assert!(title.ends_with('…'));
        assert!(title.contains('界'));
    }

    #[test]
    fn tool_title_truncates_multibyte_bash_command_safely() {
        let command = format!("{}界tail", "a".repeat(59));
        let title = tool_title("bash", &json!({ "command": command }));

        assert!(title.starts_with("$ "));
        assert!(title.ends_with('…'));
        assert!(title.contains('界'));
    }

    #[test]
    fn tool_title_truncates_multibyte_native_cli_args_safely() {
        let arg = format!("{}界tail", "a".repeat(55));
        let title = tool_title("git", &json!({ "args": [arg] }));

        assert!(title.starts_with("$ git "));
        assert!(title.ends_with('…'));
        assert!(title.contains('界'));
    }
}
