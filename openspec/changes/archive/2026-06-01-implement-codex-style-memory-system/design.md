## Context

Parent change `abstract-memory-entry-lifecycle` added a pure
`memory_entry` module for generation and use decisions. Runtime code still has
no `[memory]` config, no generated memory file layout, no prompt loading, and
no end-of-turn generation pass. Existing session trajectories are saved under
`RA_HOME/sessions/<cwd-hash>/`; generated memories should live under the same
Ra home/state root as inspectable generated state, but they must not become the
primary place for required project guidance.

## Goals / Non-Goals

**Goals:**

- Wire a disabled-by-default memory feature through config and generated JSON
  schema.
- Persist, list, and load durable local memories from the Ra home/state root.
- Add per-session controls for using existing memories and contributing future
  memories without mutating global config.
- Inject eligible durable memories into system prompt context while respecting
  external-context suppression.
- Run a deterministic background-style generation pass after saved sessions,
  gated by the existing `memory_entry` lifecycle decisions.
- Redact likely secrets from generated memory fields.

**Non-Goals:**

- Implement high-quality model-backed extraction in this change.
- Make memory files the authoritative control surface for team rules.
- Add UI for browsing or editing memories.
- Add cross-machine sync or remote memory services.

## Decisions

1. Add a new `memory` runtime module rather than expanding
   `memory_entry.rs`.

   The lifecycle module remains pure policy. The new module owns storage,
   redaction, prompt rendering, and generation orchestration, and calls
   `decide_generation` / `decide_use` for the policy decision. Alternative:
   fold runtime behavior into `memory_entry.rs`; rejected because it would
   blur the parent abstraction and make lifecycle unit tests depend on IO.

2. Store generated files as JSON under `<RA_HOME>/memories/<cwd-hash>/`.

   Reusing the cwd bucket shape keeps local project memories separated like
   trajectories while avoiding path leakage. Each file is inspectable and
   atomic-write persisted. Alternative: append to a single TOML/Markdown file;
   rejected because generated state should not look like the user's primary
   hand-edited config surface.

3. Use a narrow `MemoryExtractor` trait with a deterministic local extractor.

   The trait leaves room for model-backed extraction later. The default
   extractor creates conservative facts from stable-looking user turns and is
   sufficient to test the generation pipeline. Alternative: call the LLM during
   save; rejected for this scoped change because model extraction quality is
   explicitly allowed to be stubbed.

4. Attach memory runtime options to `Session` and generation to
   `RunnerHost::save_session`.

   Existing ACP/A2A/TUI paths already save through `SessionRunner`, so adding a
   hook near save keeps generation tied to completed turns. CLI print/resume
   paths save inline and will call the same helper after persistence.
   Alternative: spawn an always-on scheduler; rejected because active-session
   tracking and shutdown semantics would be larger than necessary.

5. Treat external context suppression as a caller-provided boolean with config
   aliases.

   This gives ACP/A2A/TUI/CLI a simple control point and maps existing
   suppression aliases into `MemoryPolicy::disable_on_external_context`.
   Current callers default to no external context unless they explicitly know
   otherwise.

## Risks / Trade-offs

- Deterministic extraction may generate fewer memories than a model-backed
  extractor -> keep the interface narrow and the pipeline testable so model
  extraction can replace it later.
- End-of-turn generation is synchronous enough to run during session save ->
  keep it lightweight, skip when rate-limit headroom is too low, and avoid LLM
  calls in this change.
- Secret redaction is heuristic -> redact common key/token/password shapes and
  record `redaction_applied`; do not claim perfect data-loss prevention.
- Memory context may compete with other system prompt resources -> render a
  small bounded section and allow thread-level/global suppression.
