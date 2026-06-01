## Context

Ra's current `Skill` model already tolerates advanced Claude Code fields during YAML parsing, but it discards them. `ResourceBundle::prompt_map` converts user-invocable skills into `SlashTemplate` values consumed by `SessionRunner`, and the runner sends the rendered template through `Session::prompt`. `Session` owns the model, tool catalog, message log, cwd, and hook engine.

## Goals / Non-Goals

**Goals:**

- Preserve advanced skill frontmatter in the runtime template.
- Render dynamic shell context deterministically from the session cwd.
- Apply model/tool/hook settings only to the directly invoked skill turn.
- Implement a practical forked skill execution mode that isolates parent transcript history from internal skill turns.
- Keep existing non-skill prompt-template behavior unchanged.

**Non-Goals:**

- Do not implement provider-specific model effort controls.
- Do not add live skill rediscovery while a session is already running.
- Do not reinterpret Claude Code permissions beyond tool-name and simple `Tool(pattern)` declarations.

## Decisions

### Store Runtime Options On Skill Templates

`Skill` and `SlashTemplate` will carry a `SkillRuntimeOptions` struct containing optional model, effort, shell, agent mode, allowed/disallowed tool declarations, and hooks. Prompt templates keep `None`, so legacy prompt commands remain unaffected.

### Render Shell Context In The Runner

Dynamic context syntax is prompt rendering, not model behavior. The runner already owns direct slash invocation and has access to session cwd, so it will replace inline `` !`command` `` and fenced ` ```! ` blocks after argument substitution and before calling the session.

Unsuccessful shell commands should become readable error markers in the rendered prompt. This preserves debuggability and avoids unexpectedly aborting the entire skill invocation.

### Use Scoped Session Runtime State

`Session` will expose a scoped invocation method that takes an optional runtime override. During that invocation, it will:

- temporarily select an override model when provided,
- filter advertised tool specs and executable tool lookup using an allow/deny policy,
- merge skill hooks with session hooks,
- restore the previous runtime state after completion.

The scope is held in the async prompt path and is not persisted into the message log.

### Extend RunnerHost For Model Resolution

`RunnerHost` already abstracts host behavior needed by `SessionRunner`. Add a `build_model_for_id` method with a default `None` implementation so ACP can resolve model IDs via its existing factory, while tests and other hosts can opt in without changing protocol code.

Unknown model IDs should fail the skill invocation visibly. Silent fallback to the default model would violate the skill author's explicit runtime policy.

### Forked Skill Execution Uses Transcript Snapshot Isolation

For `agent: fork`, the runner will run the rendered skill prompt against a child session state initialized from the parent transcript snapshot. After the child finishes, the parent transcript is restored to its pre-skill state and receives only the final assistant text produced by the child. Tool calls and intermediate skill messages remain isolated from the parent transcript.

This matches the operational need for fork/subagent behavior with Ra's current single-session architecture and avoids protocol-specific session creation in the shared runner.

## Risks / Trade-offs

- Tool declarations in Claude Code can include richer permission patterns than Ra tool names. The first implementation will enforce by normalized tool name and `Tool(pattern)` prefix, which covers the current Ra tool-spec surface.
- Forked execution cannot perfectly emulate Claude Code internals without a public transcript contract. Tests will lock Ra's defined behavior: parent snapshot in, final assistant result out.
- Shell context injection executes local commands before the model call. That is expected for Claude Code-compatible skills and is contained to direct skill invocation.
