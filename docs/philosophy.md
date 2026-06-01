# Ra Philosophy

Ra is not a command runner waiting for a human supervisor. It is a
Rust-native engineering agent that owns the path from requirement to
verified artifact. Its philosophy is the working discipline that makes
that autonomy practical: the agent owns the method, strict specs come
first, every artifact remains traceable to a requirement, and users do
not need to watch each step for the work to stay accountable.

## Agent-Owned Model

Reliable automation starts when the agent owns the engineering method it
uses. Ra is designed to drive the full process itself rather than asking a
human to feed it every next command.

For OpenSpec projects, that means Ra can discover the `openspec/`
directory, read the active specs and changes, and follow the CLI state
machine non-interactively: create a change, inspect `status`, consume
`instructions --json`, validate with `validate --strict`, implement the
tasks, and archive when the work is complete. On projects without
OpenSpec, Ra can surface the bootstrap path so the repo can adopt the
same convention instead of relying on ad hoc planning.

The same idea applies to Graphify. Ra treats the project graph as an
agent-owned requirement-to-artifact service: it can detect a missing or
stale graph, guide or run a low-cost AST refresh, and use the graph for
intake, planning, impact analysis, verification, and reporting.

Owning the method does not mean reinventing every tool. Ra consumes
OpenSpec, Graphify, ACP, A2A, ATIF, ATOF, MCP, and HCP-flavored config as
explicit contracts. It owns the workflow so it can be responsible for the
outcome, while leaving each protocol and schema as the authority for its
own domain.

## No User Overseer Needed

Ra aims for no human in the loop by default. The user should be able to
state the requirement and step away while the agent advances the work
through planning, implementation, verification, and reporting.

That does not mean Ra is unconstrained. It means the constraints are built
into the process instead of supplied as constant supervision. Sessions are
persisted as ATIF trajectories. Observability events are emitted through
ATOF. Tool calls, prompts, hooks, and turns can be inspected after the
fact. The agent does not need a human overseer because its work remains
auditable.

Autonomy also requires boundaries. Operations that cross durable external
lines, such as publishing packages, force-pushing branches, changing
licenses, or modifying user-level configuration, still require explicit
authorization. Ra's goal is not to remove human judgment from irreversible
decisions. It is to remove the need for a person to babysit reversible
engineering steps that the agent can drive and verify itself.

## End-to-End Requirements to Code Artifacts

Ra treats a requirement as the start of a traceable chain, not as a note
that disappears once coding begins.

The intended path is end to end: requirements become OpenSpec changes,
changes become tasks, tasks become code or documentation artifacts,
verification checks the result, and the final artifact can still be
explained in terms of the requirement that caused it. Graphify extends
that path with a project graph so Ra can ask which files satisfy a
requirement, which requirements a change may affect, and what evidence
should be gathered before calling the work complete.

This is the difference between "the agent edited files" and "the agent
delivered the requested artifact." Ra is built around the latter. A code
change should carry tests or an end-to-end smoke check. A documentation
change should carry the applicable documentation or spec validation. The
work counts when the artifact can be tied back to the requirement and the
verification story is clear.

## Strict Spec First

Ra puts strict specifications before implementation. Specs are not
decorative prose written after the code works; they are the contract that
lets an autonomous agent move safely.

In practice, this means OpenSpec changes are validated before they guide
implementation. Protocol schemas under `spec/` are treated as normative
wire contracts. Configuration schema changes are regenerated from the
Rust types instead of hand-edited. Hooks follow the upstream Claude Code
wire format closely enough that field renames are public API changes, not
local refactors.

Strict does not mean frozen. Specs can evolve, but they evolve through
the same discipline: propose the change, state the requirements, validate
strictly, implement, verify, and archive. When the requirement changes,
the spec changes first. The code follows the contract, not the other way
around.

## Why These Principles Belong Together

The four principles are one system:

- Strict specs make it safe for the agent to own the method.
- Agent-owned methods make end-to-end delivery possible without manual
  step control.
- End-to-end traceability keeps autonomous work accountable.
- Accountability removes the need for a user overseer while preserving
  trust.

Ra is named for the sun god and for "Rust-native agent." Both readings
point at the same ambition: every turn should illuminate the current
context, carry a requirement forward, and leave behind an artifact whose
origin and verification are clear.

Write the spec first. Walk the requirement all the way to the artifact.
Own the result.
