## 1. Config And Storage

- [x] 1.1 Add `[memory]` config fields, defaults, aliases, and schema/example coverage.
- [x] 1.2 Implement local memory artifact types, cwd-bucketed storage paths, atomic persistence, and loading.
- [x] 1.3 Implement redaction and generated-state guidance helpers.

## 2. Runtime Integration

- [x] 2.1 Map config and thread controls into `MemoryPolicy` using the existing lifecycle abstraction.
- [x] 2.2 Load eligible durable memories into system prompt context with global, thread, and external-context suppression.
- [x] 2.3 Add the generation pipeline with idle, active, short-lived, rate-limit, and redaction gates.
- [x] 2.4 Wire memory runtime into CLI, resume, TUI, ACP, and A2A session save/load paths.

## 3. Tests And Validation

- [x] 3.1 Add focused tests for config gating, aliases, storage paths, persistence, and prompt loading.
- [x] 3.2 Add focused tests for thread use/generation controls, idle/rate-limit skipping, active/short-lived skipping, and redaction.
- [x] 3.3 Run formatting, targeted tests, OpenSpec validation, and full available validation.
