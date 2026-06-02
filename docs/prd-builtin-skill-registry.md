# PRD: Builtin Skill Registry And npx Skills Adapter

## Overview / Problem Statement

Ra can already discover and invoke project and personal `SKILL.md` files, but it does not yet have a first-class lifecycle for Ra-shipped builtin skills. It also does not expose the package-management workflow users expect from `npx skills`: adding, listing, finding, removing, and updating skills from remote or local sources.

Ra should not reimplement the `npx skills` ecosystem manager. Instead, Ra should provide a small adapter that forwards skill-management commands to `npx skills`, while keeping Ra's runtime registry responsible for resolving which installed, configured, and builtin skills are active for a session.

## Goals & Success Metrics

- Ra ships a deterministic builtin skill registry that can be loaded without network access.
- Ra can load a local git-backed skill registry and records its HEAD revision as skill provenance.
- Project and global installed skills can shadow Ra builtin skills by command name.
- Users can manage external skills through `ra skills ...` with behavior aligned to `npx skills`.
- Project-level installs default to a path Ra already discovers.
- Global installs are discoverable by Ra after installation.
- Tests cover registry precedence, builtin disable/include/exclude behavior, and `npx skills` command forwarding argument construction.

## User Personas & Stories

- As a Ra user, I want useful builtin skills available on a fresh install without manually copying skill files.
- As a project maintainer, I want committed project skills to override generic builtin guidance for the same workflow.
- As a user of the open skills ecosystem, I want to run `ra skills add vercel-labs/agent-skills` and get the same installation semantics as `npx skills`.
- As an operator, I want to disable builtin skills without deleting or modifying Ra-shipped files.

## Functional Requirements

| Priority | Requirement |
| --- | --- |
| Must | Add a builtin skill registry loaded from Ra-shipped skill definitions. |
| Must | Treat builtin skills as virtual or embedded source entries, not as installed user files. |
| Must | Resolve active skills with deterministic precedence: explicit config paths, project skills, global skills, local registry skills, then builtin skills. |
| Must | Allow project or global skills to shadow builtin skills with the same slash command name. |
| Must | Expose only the resolved active winner in the model-facing catalog and slash-command map. |
| Must | Add config controls to enable/disable builtin skills and include/exclude builtin names. |
| Must | Support an optional local registry path that is treated as a git checkout and skipped if it is not versioned by git. |
| Must | Record the registry HEAD commit for each skill loaded from the local registry. |
| Must | Preserve progressive disclosure: builtin skill bodies must not be dumped into the startup system prompt. |
| Must | Support `ra skills add/list/find/remove/update/init` by forwarding to `npx skills`. |
| Must | When forwarding `ra skills add` without an explicit `--agent`, default to `--agent codex` so installs land in `.agents/skills/` for project scope. |
| Must | Add global Codex discovery for `~/.codex/skills/**/SKILL.md`, matching `npx skills` global Codex install location. |
| Must | Keep explicit user `--agent` arguments intact and do not inject a default agent when the user already supplied one. |
| Should | Pin the forwarded package invocation to a known `skills` npm version or make the version configurable. |
| Should | Expose `ra skills list --builtin` to inspect Ra-shipped builtin skills and shadowing status. |
| Should | Report shadowed builtin skills in diagnostics or JSON list output. |
| Could | Provide `ra skills disable <name>` as a config-editing helper that writes a builtin exclude entry. |
| Could | Support an environment variable equivalent to `INSTALL_INTERNAL_SKILLS` for internal builtin skills. |
| Won't | Reimplement remote source parsing, package install, update, or removal logic already provided by `npx skills`. |

## Non-Functional Requirements

- Keep session startup deterministic and offline-capable.
- Keep installed skill management separate from runtime skill resolution.
- Avoid writing builtin skills into project or home directories unless explicitly requested by a future export command.
- Preserve existing `.ra/skills`, `.agents/skills`, and `.claude/skills` discovery behavior.
- Avoid surprising command rewriting: only add Ra defaults when the user did not specify the corresponding `npx skills` option.

## Design Considerations

`npx skills` is the ecosystem manager. It owns source formats, install/update/remove behavior, project/global scopes, agent target paths, and interactive flows. Ra should use it as a subprocess for management commands.

Ra's runtime registry has two local sources. The builtin source is embedded with Ra and provides fallback skills. The local registry source is a git checkout, analogous to the GitHub repositories used by `npx skills`, so skill versions are managed by commits, branches, tags, and ordinary git operations. Builtin and registry entries should behave like ordinary skills after resolution, including `disable-model-invocation`, `user-invocable`, runtime tool policy, hooks, model overrides, shell context, and fork behavior.

## Technical Considerations

The implementation should extend `src/skills.rs` with a `SkillSource` or equivalent provenance field so resolved entries can distinguish explicit, project, global, registry, and builtin sources. Registry-sourced skills should also carry the registry HEAD revision. `ResourceBundle::prompt_map` and `build_system_prompt` should operate on the resolved active list, not every discovered duplicate.

Builtin definitions can live in a repo directory such as `skills/.system/<name>/SKILL.md` and be embedded at compile time with `include_str!`, or be loaded from a packaged runtime directory. Embedding is safer for single-binary installs; materializing to a read-only cache path may be useful so the existing `read` tool can load full builtin skill bodies during progressive disclosure.

The optional local registry should default to `~/.ra/skill-registry` when the directory exists. It must be a git checkout; Ra should not silently treat an unversioned directory as a registry. Ra should not fetch, pull, checkout, or mutate the registry during normal startup. Version changes happen through git commands or future explicit registry-management commands.

The `ra skills` adapter should shell out to:

```bash
npx --yes skills@<pinned-version> <subcommand> ...
```

For `ra skills add`, if no `--agent`/`-a` appears in user arguments, append `--agent codex`. For all other arguments, preserve ordering and values. The adapter should stream stdout/stderr and return the child exit status.

## Timeline & Milestones

| Milestone | Owner | Target |
| --- | --- | --- |
| PRD and issue draft | Agent | Before implementation |
| Registry data model, git provenance, and precedence tests | Agent | First implementation PR |
| Builtin config and discovery integration | Agent | First implementation PR |
| `ra skills` passthrough adapter | Agent | Second implementation PR if scope grows |
| Documentation and CLI examples | Agent | Before merge |

## Open Questions & Risks

- Whether builtin skill bodies should be embedded only, materialized to cache for `read`, or exposed through a virtual resource reader.
- Whether future registry management should use `git` directly or redirect to a skills ecosystem command when the source is remote.
- Whether `ra skills list` should default to Ra's resolved registry view or `npx skills list` project view. A pragmatic split is `ra skills list` passthrough and `ra skills registry` for Ra's resolved view.
- `npx skills` is an npm dependency at command time. Users without Node/npm need a clear error and may still rely on builtin and manually installed skills.
- A pinned `skills` version improves reproducibility but may lag ecosystem behavior. A config override can handle this without making default behavior unstable.

## Appendix

Reference behavior: `skills@1.5.9` (`npx skills`) supports `add`, `remove`, `list`, `find`, `update`, `init`, project/global scopes, `--agent`, `--skill`, `--copy`, `--all`, and Codex install paths of `.agents/skills/` for project scope and `~/.codex/skills/` for global scope.
