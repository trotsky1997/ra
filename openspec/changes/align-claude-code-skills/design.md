## Context

Ra currently loads `SKILL.md` files into `ResourceBundle` and includes only skill names, descriptions, and paths in the startup system prompt. It also has an existing slash-command path for prompt templates. Claude Code's current skills documentation treats skills and custom commands as one surface: a skill creates `/skill-name`, supports optional frontmatter, and separates model invocation from user invocation.

## Goals / Non-Goals

**Goals:**

- Make Ra discover common Claude Code skill directories without extra config.
- Make Ra tolerate current Claude Code frontmatter shape.
- Keep startup context small by preserving progressive disclosure for model-visible skills.
- Reuse the existing slash-command prompt-template dispatcher for direct skill invocation.
- Preserve existing Ra-native and universal skills layouts.

**Non-Goals:**

- Do not execute dynamic context injection placeholders.
- Do not implement skill-scoped model, effort, hook, allowed-tools, or disallowed-tools semantics.
- Do not implement forked subagent execution.
- Do not add filesystem watching or on-demand nested discovery.

## Decisions

### Derive command name from location

Claude Code derives the typed command from the skill path, while `name` is a display label. Ra should store both `command_name` and `display_name`, where `command_name` is the parent directory name of `SKILL.md` and `display_name` falls back to it.

Alternative considered: keep using frontmatter `name` as the key. This would preserve existing Ra internals but remain incompatible with Claude Code skills that omit `name` or use it only as a label.

### Reuse prompt templates for direct invocation

`ResourceBundle::prompt_map` already feeds a shared slash-command dispatcher used by ACP, A2A, and TUI. Add user-invocable skill bodies to the same map as typed `SlashTemplate` values so the runner can preserve legacy prompt-template behavior while rendering Claude Code skill arguments.

Alternative considered: add a separate Skill tool and runtime state. That is closer to Claude Code's internal architecture but larger than needed for the current behavior contract.

### Render skill arguments in the runner

Skill templates should replace `$ARGUMENTS`, positional `$0`/`$1` placeholders, and named placeholders declared by frontmatter `arguments`. If a skill receives arguments but contains no placeholders, append `ARGUMENTS: <raw args>` to match Claude Code's direct invocation fallback. Plain prompt templates keep the legacy behavior of appending raw args without the `ARGUMENTS:` label.

Alternative considered: perform argument rendering while building the resource bundle. That cannot work because rendering needs the user's per-invocation arguments.

### Walk project skills from cwd to repository root

Default project discovery should include `.claude/skills` and `.agents/skills` directories from the launch cwd up through the repository root, mirroring the existing AGENTS.md walking pattern. Global `~/.claude/skills` and `~/.agents/skills` remain single fixed globs.

Alternative considered: rely on `./.claude/skills/**/SKILL.md` only. That misses repo-root skills when Ra is launched from nested package directories.

### Model catalog filters out disabled skills

`disable-model-invocation: true` should prevent a skill from appearing in the system prompt catalog, while still allowing direct invocation unless `user-invocable: false` is also set. This keeps automatic invocation and manual invocation separate.

Alternative considered: skip disabled skills entirely. That would incorrectly remove direct `/skill-name` workflows.

### Parse additional fields without enforcing them

Fields such as `allowed-tools`, `disallowed-tools`, `model`, `effort`, `context`, and `agent` should be accepted by YAML parsing so valid Claude Code skills do not fail, but this change should not imply runtime enforcement.

Alternative considered: reject unsupported fields to avoid silent behavior gaps. That would make Ra less compatible with existing skill files.

## Risks / Trade-offs

- Advanced Claude Code skills may load but not get every runtime behavior. Mitigation: document unsupported features as out of scope.
- Duplicate slash names can occur between prompt templates and skills. Mitigation: keep prompt templates taking precedence unless tests show a stronger local expectation.
- Adding `.claude/skills` to default discovery can load more project content into the catalog. Mitigation: only descriptions enter the system prompt, and authors can set `disable-model-invocation: true`.
