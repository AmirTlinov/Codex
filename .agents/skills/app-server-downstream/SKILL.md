---
name: app-server-downstream
description: Downstream workflow for codex app-server and protocol changes. Use when touching app-server behavior, JSON-RPC APIs, generated schemas, or extension-facing transport surfaces.
---

# App-server downstream

## Use when

- the task changes `codex app-server`;
- the task changes app-server protocol types or JSON-RPC methods;
- the task affects IDE/extension-facing thread, turn, command, fs, or config
  APIs.

## Read first

1. `codex-rs/app-server/README.md`
2. the owning files in:
   - `codex-rs/app-server-protocol/src/protocol/`
   - `codex-rs/app-server/`
   - `codex-rs/app-server-client/` when relevant
3. root `AGENTS.md` app-server rules

## Downstream policy

- Do new API work in app-server v2 unless there is a hard compatibility reason
  not to.
- Keep wire shape changes explicit, documented, and schema-backed.
- Prefer narrow endpoint or field additions over broad transport rewrites.
- If the goal can be reached through config, skills, MCP, or wrapper behavior,
  prefer that before protocol churn.

## Validation

When protocol shapes change:

```bash
cd codex-rs
just write-app-server-schema
cargo test -p codex-app-server-protocol
```

When runtime server behavior changes, run the owning crate tests too:

```bash
cd codex-rs
cargo test -p codex-app-server
```

And always update the app-server docs if behavior changed:

- `codex-rs/app-server/README.md`

## Done looks like

- protocol/runtime change is narrow and explicit;
- schema artifacts and docs match the code;
- tests cover the changed behavior;
- the patch remains rebase-friendly over upstream.
