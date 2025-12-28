## Lego memory (experimental)

Lego memory keeps long‑lived context as discrete blocks instead of a single chat summary. The working set is compiled deterministically each turn under a token budget, with per‑block degradation (full → summary → label) and stale detection for file‑backed blocks.

### Enable

```toml
[features]
lego_memory = true
```

### Storage layout

Memory is stored per project under `memory.root_dir` (default: `$CODEX_HOME/memory`):

- `memory.log.jsonl` — append‑only event log (block upserts / deletes)
- `snapshot.json` — periodic snapshot for fast load

### Archive budget

When `memory.max_bytes` is set (default 50 MiB), the archive is compacted to a soft cap
(~80% of the limit) by writing a fresh snapshot and truncating the log. If the snapshot
still exceeds the budget, non‑pinned blocks are evicted deterministically (stale →
stashed → active, low priority first, oldest updated_at first). Pinned blocks are never
evicted; a warning is emitted if they alone exceed the limit.

### Context compilation

The compiler:

- includes pinned/active blocks (stashed blocks are skipped unless pinned)
- applies a deterministic workbench selection to keep only the most relevant blocks
- expands one hop across block links (bounded) to surface related archived blocks
- applies block‑level degradation to fit `memory.working_set_token_budget`
- injects memory as an overlay message in the prompt (not recorded in history)

### Context workbench (live selection)

When lego memory is enabled, the workbench keeps a small working set per turn:

- `focus` is updated from the latest user message by default (pinned)
- `plan` is updated when the `update_plan` tool emits a plan update
- selection is deterministic: pinned + focus/goals/constraints/plan + top‑K relevant blocks

The workbench never rewrites the full prompt history; it only curates which blocks enter the
memory overlay.

### Workbench transcript (virtual context window)

If you want the agent to actively manage what enters the model’s context window (as “attention”),
enable the workbench transcript feature:

```toml
[features]
workbench_transcript = true
```

When enabled, Codex compiles a focused prompt transcript for each model call:

- keeps the latest pinned prefix (developer instructions, AGENTS.md instructions, explicit skill injections, environment context)
- keeps only a short tail of the conversation (currently last N user messages + their following items)
- still injects the lego memory overlay (compiled working set) into the prompt

This does not delete the local history; it only changes what is sent to the model.

### Tool output denoise (evidence vs attention)

Tool outputs (logs, diffs, stack traces) are often the biggest source of context noise. When this
feature is enabled, Codex archives large tool outputs as `tool_slice:*` blocks in lego memory and
keeps only a deterministic digest in the prompt transcript.

This preserves a strong invariant:

- the model sees a short, high-signal summary (attention)
- the full output is still available as evidence (via the `memory` tool or by inspecting the rollout)

Enable:

```toml
[features]
lego_memory = true
tool_output_denoise = true
```

Notes:

- The archive blocks are stored as `tool_slice:<call_id>` with status `stashed` and low priority.
- Outputs containing images are not denoised (to avoid breaking `view_image` workflows).
- The prompt digest includes a pointer like `archived=tool_slice:<call_id>` so the model can fetch
  the full output on demand using the `memory` tool.

### BranchMind workbench (project thinking overlay)

If you want Codex to pull a compact “project thinking” snapshot from a BranchMind MCP server and
inject it as a pinned block in the lego memory overlay, enable:

```toml
[features]
branchmind_workbench = true
```

Notes:

- This feature requires `lego_memory = true` (the BranchMind snapshot is injected as part of the memory overlay).
- Codex expects an MCP server named `branchmind` that exposes two tools:
  - `snapshot` (used to fetch a bounded snapshot for the current project workspace)
  - `note` (used to append small “user_focus” notes when the focus block updates)
- When a BranchMind snapshot is available, Codex derives a compact focus summary from it (goal/now/next/verify) and may emit a PlanUpdate event for the UI.
- When `branchmind_workbench` is enabled and the BranchMind `snapshot` tool is present, the `update_plan` tool may be omitted from the model toolset to encourage BranchMind-first task management.
- Tool calls are bounded and time‑limited; failures do not break the turn. Errors are shown only in diagnostics.

### Diagnostics

Use `/context-debug` in the TUI to inspect what is sent to the model:

- enabled feature flags (lego memory / workbench transcript / BranchMind workbench)
- compiled transcript preview (pinned prefix + tail)
- compiled memory overlay (selected blocks + budgets + staleness)
- BranchMind status (workspace id / injected / error)

### Staleness

File‑backed blocks carry fingerprints. When the source changes, blocks are marked stale and only emitted as labels until refreshed.

`memory.staleness` options:

- `git-oid` (default): uses `git hash-object` when available, falls back to mtime+size
- `mtime-size`: uses file mtime (ns) and size

### Memory tool

When `lego_memory` is enabled, the model can call the `memory` tool to manage blocks:

- `upsert` creates or updates a block
- `patch` updates selected fields
- `get` returns a block (full/summary/label)
- `list` returns block summaries
- `delete` removes a block

File sources without fingerprints are filled automatically (best effort) using the configured staleness mode.

### Workspace view and tool catalog

When enabled, the memory overlay includes:

- `/cwd` — current working directory
- `/tools` — local tool roots + MCP/skill tool catalog (descriptions from the first 10 lines of local tool files)
- `/context` — compiled working set
- `/memory` — on‑disk memory location for the project
