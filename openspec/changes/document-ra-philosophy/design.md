# Design

## Placement

The philosophy statement will live in `docs/philosophy.md`. It is longer
than a README bullet but directly connected to the product identity, so the
README will link it from the introduction instead of inlining the full text.

## Content Shape

The document will be written as durable English project copy, not as an
issue-response translation. It will keep the four requested concepts as
top-level sections:

- Agent-Owned Model
- No User Overseer Needed
- End-to-End Requirements to Code Artifacts
- Strict Spec First

The conclusion will explain how those principles reinforce each other:
strict specs let the agent own the method; agent-owned methods and
traceability remove the need for step-by-step user oversight.

## Non-Goals

- Do not change code, schemas, or runtime defaults.
- Do not duplicate the full README feature catalog.
- Do not introduce generated documentation tooling.
