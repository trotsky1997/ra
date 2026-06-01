# Ra spec collection

Authoritative schemas / specs for every protocol Ra speaks or persists to.

> **Looking for something else?** The project status and what's next
> live in [`../ROADMAP.md`](../ROADMAP.md); build commands and project
> conventions are in [`../CLAUDE.md`](../CLAUDE.md); the runtime tool
> reference is [`tools.md`](tools.md).

| File                              | What                                                    | Origin                                                    | Validated |
|-----------------------------------|---------------------------------------------------------|-----------------------------------------------------------|-----------|
| **ACP — Agent Client Protocol**   |                                                         |                                                           |           |
| `acp-v1.json`                     | JSON Schema, stable v1                                  | https://github.com/agentclientprotocol/agent-client-protocol `schema/schema.json` | ✓ upstream |
| `acp-v1-unstable.json`            | JSON Schema with all `unstable_*` features              | upstream `schema/schema.unstable.json`                    | ✓ upstream |
| `acp-meta.json`                   | Method-id index (stable)                                | upstream `schema/meta.json`                               | ✓ upstream |
| `acp-meta-unstable.json`          | Method-id index incl. unstable                          | upstream `schema/meta.unstable.json`                      | ✓ upstream |
| **A2A — Agent-to-Agent**          |                                                         |                                                           |           |
| `a2a.proto`                       | Canonical proto3 spec                                   | https://github.com/a2aproject/A2A `specification/a2a.proto` | source-of-truth |
| `a2a-v1.json`                     | JSON Schema bundle (47 defs)                            | generated locally via `protoc-gen-jsonschema` (bufbuild)  | ✓ matches `a2a.proto` |
| `a2a-build-README.md`             | Build pipeline notes                                    | upstream `specification/json/README.md`                   | reference |
| **ATIF — trajectory at rest**     |                                                         |                                                           |           |
| `atif-v1.7.json`                  | JSON Schema, all 11 `$defs` for Harbor RFC 0001 v1.7   | generated from Harbor's pydantic models via `model_json_schema()` | ✓ produced trajectories validate |
| **ATOF — telemetry events**       |                                                         |                                                           |           |
| `atof-v0.1.mdx`                   | Human-readable spec doc                                 | NeMo Relay `docs/observability-plugin/atof.mdx`           | source narrative |
| `atof-v0.1.json`                  | JSON Schema for Scope + Mark events                     | hand-derived from `nemo-relay::api::event` (no upstream JsonSchema derive) | ✓ all 4 samples validate |
| `atof-v0.1-samples.jsonl`         | Real captures from a live Ra prompt run                 | `~/.local/share/ra/obs/atof-<pid>.jsonl`                  | conformance set |
| **graniet/llm — client SDK**      |                                                         |                                                           |           |
| `graniet-llm-NOTES.md`            | Crate is not a protocol; pointer to backend specs       | hand-written                                              | n/a |
| **HCP — Ra runtime config**       |                                                         |                                                           |           |
| `hcp.json`                        | Upstream HCP RFC-0002 schema (Pi coding-agent)          | https://github.com/trotsky1997/hcp-sdk                    | reference |
| `hcp-RFC.md`                      | RFC-0002 narrative                                      | upstream `docs/rfcs/0002-...md`                           | reference |
| `ra-config.schema.json`           | Ra's own JSON Schema (HCP-flavored, factored down)      | generated via `cargo run --bin gen-schema`                | ✓ schemars derive |
| `ra.toml.example`                 | Annotated example config covering every section         | hand-written                                              | tracks code |
| **Tools — built-in catalog**      |                                                         |                                                           |           |
| `tools.md`                        | Reference for `read`/`write`/`edit`/`bash` | hand-written                                        | tracks `src/tools/` |
| **Hooks — Claude Code wire**      |                                                         |                                                           |           |
| (no local schema)                 | Wire format follows the upstream Claude Code hooks spec | https://code.claude.com/docs/en/hooks.md                  | reference |
| **OpenSpec — spec-driven dev**    |                                                         |                                                           |           |
| (no local schema)                 | `openspec/` directory convention Ra discovers (`src/openspec.rs`) | https://github.com/Fission-AI/OpenSpec          | reference |

## Regenerating

### A2A
```bash
# Toolchain: protoc, jq, go-installed protoc-gen-jsonschema (bufbuild fork).
go install github.com/bufbuild/protoschema-plugins/cmd/protoc-gen-jsonschema@latest
git clone https://github.com/googleapis/googleapis /tmp/googleapis
git clone https://github.com/a2aproject/A2A /tmp/A2A-spec
GOOGLEAPIS_DIR=/tmp/googleapis bash /tmp/A2A-spec/scripts/proto_to_json_schema.sh \
    spec/a2a-v1.json
```

### ATIF
```bash
git clone https://github.com/harbor-framework/harbor /tmp/harbor
python -m venv /tmp/venv && /tmp/venv/bin/pip install pydantic
/tmp/venv/bin/pip install -e /tmp/harbor --no-deps
/tmp/venv/bin/python -c \
    "import json; from harbor.models.trajectories.trajectory import Trajectory; \
     print(json.dumps(Trajectory.model_json_schema(), indent=2))" \
    > spec/atif-v1.7.json
```

### ATOF
The schema is hand-maintained because `nemo-relay::api::event::Event` does
not derive `schemars::JsonSchema`. To regenerate samples:
```bash
RA_OBS_BACKEND=file ra acp <<< '...'
cp ~/.local/share/ra/obs/atof-*.jsonl spec/atof-v0.1-samples.jsonl
```
Then re-validate:
```bash
python -c "import json; from jsonschema import Draft202012Validator; \
    s=json.load(open('spec/atof-v0.1.json')); \
    [Draft202012Validator(s).validate(json.loads(l)) for l in open('spec/atof-v0.1-samples.jsonl')]"
```

### ACP
Pulled directly from the upstream `agent-client-protocol-schema` repo;
re-clone and copy when bumping protocol version:
```bash
cp /tmp/acp/schema/{schema.json,schema.unstable.json,meta.json,meta.unstable.json} spec/
```

### Ra runtime config
The `ra-config.schema.json` is generated from the live Rust types
(`schemars` derive on `RaConfig` and friends) — re-run after touching
`src/config.rs`:
```bash
cargo run --bin gen-schema > spec/ra-config.schema.json
```
