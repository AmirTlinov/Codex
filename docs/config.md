# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

### Role models for collaboration agents

You can set a separate model for each agent role:

```toml
[agents]
main_model = "gpt-5-mini"            # defaults used by all roles if role-specific model missing
scout_model = "gpt-5-mini"           # context scout
context_validator_model = "gpt-5-mini" # context validation before build
builder_model = "gpt-4.1-mini"       # patch-focused role
post_builder_validator_model = "gpt-5" # patch review after build
validator_model = "gpt-5"            # compatibility/fallback validation role
plan_model = "gpt-5-mini"            # slice-first planning role
```

Sub-agent tools are enabled by default. To disable sub-agents:

```toml
[features]
collab = false
```

Role resolution order:
1. `agents.<role>_model`
2. `agents.main_model`
3. top-level `model`

Runtime behavior defaults:
- `scout` and `context_validator` force a read-only sandbox policy.
- `builder` is read-only and cannot call mutating tools; it can use collaboration tools to coordinate and spawn Scout sub-agents.
- `post_builder_validator` (and `validator` fallback) can apply accepted patches verbatim; patch application still requires a writable sandbox policy for the session.
- `plan` does not force read-only. When the session uses a writable sandbox policy, `plan` is additionally allowed to write plan artifacts under `~/.codex/plans/...`.
- Tool policy is role-aware:
  - `scout`: read-only context tools only (no shell, no `apply_patch`, no sub-agent spawning)
  - `context_validator`: read-only context validation only (same restrictions as `scout`)
  - `builder`: collaboration tools only (`spawn_agent`/`send_input`/`wait`/`resume_agent`/`close_agent`)
  - `builder` can only spawn `scout` agents (all other `agent_type` values are rejected)
  - `post_builder_validator`: validation-oriented tools + `apply_patch` (no shell, no sub-agent spawning)
  - `validator`: compatibility role, same behavior as `post_builder_validator`
  - `plan`: planning tools + constrained `apply_patch` for `PLAN.md` / `slice-*.md` artifacts
  - validator-style roles apply only verbatim Builder patch text; otherwise they must reject with detailed feedback

### Review configuration (`/review`)

You can pin a dedicated local model for `/review` (legacy key):

```toml
review_model = "gpt-5"

[profiles.fast]
model = "gpt-5-mini"
review_model = "gpt-5" # profile-level override for /review
```

`review_model` precedence (local reviewer model):
1. CLI override (`ConfigOverrides.review_model`)
2. `profiles.<name>.review.local.model`
3. `profiles.<name>.review_model`
4. `review.local.model`
5. top-level `review_model`

Preferred (explicit mode + nested config):

```toml
review.mode = "local" # local|remote|hybrid
review.local.model = "gpt-5"
review.remote.provider = "github_codex"
review.remote.trigger = "@codex review"
review.hybrid.policy = "local_first" # local_first|remote_first|required_both
```

Mode precedence:
1. `profiles.<name>.review.mode`
2. `review.mode` (default: `local`)

## Connecting to MCP servers

Codex can connect to MCP servers configured in `~/.codex/config.toml`. See the configuration reference for the latest MCP server options:

- https://developers.openai.com/codex/config-reference

## Apps (Connectors)

Use `$` in the composer to insert a ChatGPT connector; the popover lists accessible
apps. The `/apps` command lists available and installed apps. Connected apps appear first
and are labeled as connected; others are marked as can be installed.

## Notify

Codex can run a notification hook when the agent finishes a turn. See the configuration reference for the latest notification settings:

- https://developers.openai.com/codex/config-reference

## JSON Schema

The generated JSON Schema for `config.toml` lives at `codex-rs/core/config.schema.json`.

## Notices

Codex stores "do not show again" flags for some UI prompts under the `[notice]` table.

Ctrl+C/Ctrl+D quitting uses a ~1 second double-press hint (`ctrl + c again to quit`).
