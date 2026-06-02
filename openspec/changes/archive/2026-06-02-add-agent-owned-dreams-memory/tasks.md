## 1. Dreams Policy Layer

- [x] 1.1 Add agent-owned Dreams scheduling decisions, skip reasons, scheduler, input selection, job state, and output adoption gate.
- [x] 1.2 Keep Dreams outside `MemoryEntryLifecycle` and reuse existing `decide_use` policy for output adoption.

## 2. Memory Config

- [x] 2.1 Add `min_sessions_between_dreams` to memory policy/config with default value 10 and runtime mapping.
- [x] 2.2 Regenerate the Ra config schema and update the example config.

## 3. Tests And Docs

- [x] 3.1 Add unit tests for dream skip branches, input filtering/cap behavior, completed/non-completed adoption, and config default/parsing.
- [x] 3.2 Add developer documentation explaining the Dreams generated-state/source-evidence boundary and no-live-API scope.
- [x] 3.3 Run relevant tests and OpenSpec validation, then archive the accepted change.
