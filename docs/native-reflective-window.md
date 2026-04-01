# Native reflective window for Codex

## Outcome

Add a native Codex mechanism that lets the main agent keep a narrow active
lane while a bounded internal reflective sidecar periodically scans for:

- blind spots,
- hidden assumptions,
- integration risks,
- subtle contradictions,
- high-upside ideas worth promotion.

The sidecar must not become a second always-on main agent. Its job is to
produce a **small, fresh reflective window** that improves the next real turn.

## Current downstream implementation status

This fork now has a working native downstream slice for two things:

1. the reflective window can run through the internal Codex one-shot path or
   through `claude` CLI;
2. spawned subagents can run through `claude` CLI, including
   `claude-opus-4-6`.

Important boundaries of the current slice:

- external Claude agents are session-local, not resumable from rollout;
- they are text-only by default unless `[claude_cli].tools` is configured;
- reflective Claude runs are forced toolless and read-only;
- reflective Claude scopes its transcript to a bounded recent window and keeps
  an omission marker instead of replaying the whole session forever;
- large delegated prompts are streamed over stdin instead of argv so forked
  parent context and reflective transcripts do not hit `spawn E2BIG`.

## What this is not

This is **not**:

- a permanent full-context clone running on a timer;
- unlimited auxiliary memory;
- public `spawn_agent` orchestration theater;
- another long-lived memory system like repo memories;
- a replacement for the main turn loop.

The right design is:

- one hot path;
- one bounded reflective side channel;
- promotion of only high-signal items back into the hot path.

## Existing Codex surfaces we should reuse

### 1. Internal subagent spawning already exists

Codex already has native internal subagent patterns:

- `codex-rs/core/src/codex_delegate.rs`
- `codex-rs/core/src/guardian/review_session.rs`
- `codex-rs/core/src/tasks/review.rs`

These show three important facts:

1. Codex can spawn internal sub-sessions without exposing them as user-facing
   collab work.
2. Internal sub-sessions can fork parent history.
3. Internal sub-sessions can run with a narrowed config and strict output
   contract.

That is the right base for a reflective sidecar.

### 2. Codex already has a model-visible contextual-fragment surface

The main thread already rebuilds model-visible contextual input in:

- `codex-rs/core/src/codex.rs` -> request assembly in `run_turn`
- `codex-rs/core/src/contextual_user_message.rs`
- `codex-rs/core/src/environment_context.rs`

So the reflective window should be injected as another bounded contextual
fragment, not as random ad-hoc text appended all over the history. In v1 that
is best done as a request-local synthetic contextual fragment instead of a
history-persisted reinjection item.

### 3. Compaction already separates durable baseline from raw history

Relevant surfaces:

- `codex-rs/core/src/compact.rs`
- `codex-rs/core/src/context_manager/history.rs`
- `codex-rs/core/src/state/session.rs`
- `codex-rs/protocol/src/protocol.rs` -> `TurnContextItem`

Compaction already works because Codex distinguishes:

- raw history,
- replacement history,
- a durable reinjection baseline.

The reflective window should follow the same philosophy: survive compaction
through a bounded reinjection baseline rather than relying on long raw history.

### 4. Turn completion already gives a natural trigger point

Relevant surface:

- `codex-rs/core/src/tasks/mod.rs` -> `on_task_finished`

This is the cleanest place to trigger reflective maintenance because:

- the main turn already ended;
- token/tool metrics are known;
- the system can decide whether reflection is worth the extra cost.

But the trigger should schedule reflective maintenance **as detached follow-up
work**, not block `TurnComplete` itself. The user-visible completion path and
`maybe_start_turn_for_pending_work()` must stay fast.

But the reflective path must stay **non-blocking** relative to the next real
user work. If pending input or a new active turn appears, reflective work
should be skipped, deferred, or cancelled rather than delaying the hot path.

## Proposed native architecture

## 1. New internal domain: `core/src/reflective/`

Keep the feature in a dedicated module tree instead of growing `codex.rs`
further.

Suggested split:

- `reflective/mod.rs` - public entrypoints
- `reflective/config.rs` - resolved config and trigger policy
- `reflective/model.rs` - reflective window state and observation types
- `reflective/prompt.rs` - sidecar prompt + output schema
- `reflective/runner.rs` - spawn, gating, and result application
- `reflective/fragment.rs` - model-visible serialization

If the feature grows beyond session orchestration plus prompt/state handling,
move it into a separate crate later. For v1, a focused core module is the
smallest sane surface.

## 2. Session-owned reflective window state

Add bounded state to `SessionState`:

- latest reflective window snapshot;
- last reflective run turn id / timestamp;
- cooldown bookkeeping;
- optional pending reflective task handle if we later allow background overlap.

The reflective window is **session state first**, not raw history first.

That matters because:

- compaction should not erase it;
- decay should be explicit;
- the main lane should see the latest filtered snapshot, not the entire trail.

## 3. Reflective window data model

Use a small explicit model, for example:

```text
ReflectiveWindowState
  - generated_at
  - source_turn_id
  - observations: Vec<ReflectiveObservation>

ReflectiveObservation
  - id
  - category: blind_spot | risk | inconsistency | hypothesis | opportunity
  - note
  - why_it_matters
  - evidence
  - confidence: low | medium | high
  - disposition: watch | verify | promote | discard
  - expires_after_turns
```

Important rules:

- keep at most a small number of observations, for example `3..7`;
- aggressively drop stale `watch` items;
- never allow unbounded accumulation.

## 4. Internal sidecar trigger policy

Do **not** run on a fixed timer.

Use event-driven triggers only. Good v1 triggers:

- after a regular turn completes;
- only when the turn was cognitively heavy enough;
- only when cooldown allows it;
- never during `/review` or `/compact`;
- never recursively inside a reflective sidecar itself.

Good trigger inputs:

- tool-call count,
- token usage,
- compaction happened recently,
- current window age,
- previous reflective run age,
- explicit failure / interruption / repeated retries.

Bad trigger policy:

- "every N seconds",
- "every turn no matter what",
- "spawn whenever the main lane feels hard".

## 5. Sidecar execution model

Use the existing internal subagent machinery, but as a private runtime feature:

- fork from current parent history when useful;
- run ephemeral;
- sandboxed read-only;
- approval policy `never`;
- collab tools disabled;
- no fan-out;
- no public mailbox workflow.
- sidecar events consumed privately, not forwarded into the parent user-visible
  event stream.

This should look closer to guardian than to user-facing multi-agent work.

Important:

- the reflective sidecar should not automatically inherit the current
  reflective window injection, or it risks becoming self-referential and
  overfitting to its own prior hypotheses;
- if prior reflective state is needed, pass it explicitly and minimally rather
  than through the default main-thread reinjection path.

Scheduling rule:

- reflective work should run only while the parent session is idle;
- it must never delay `maybe_start_turn_for_pending_work`;
- if a new regular turn starts, the reflective run should be cancelled or its
  result dropped if stale.

Recommended source marker:

- `SessionSource::SubAgent(SubAgentSource::Other("reflective_sidecar".to_string()))`

If the feature becomes first-class later, add a dedicated `SubAgentSource`
variant.

## 6. Strict output contract

The sidecar should not return prose soup.

Use a strict JSON schema similar to guardian:

```text
ReflectiveReport
  - observations: ReflectiveObservation[]
```

No chain-of-thought persistence. Only actionable reflective outputs.

The parent session should validate and normalize the result before updating the
window.

It should also verify that the report still belongs to the current surviving
history tip. If the source turn was rolled back, replaced, or is no longer an
ancestor of the active thread state, discard the report instead of applying it.

## 7. Model-visible injection

When the window is non-empty, inject one bounded contextual fragment into the
next sampling request input, anchored at the start of the new turn rather than
appended after the live user request.

Recommended shape:

```xml
<reflective_window>
  <generated_at>...</generated_at>
  <source_turn_id>...</source_turn_id>
  <observation category="risk" confidence="high" disposition="verify">
    <note>...</note>
    <why>...</why>
    <evidence>...</evidence>
  </observation>
</reflective_window>
```

Why this shape:

- it is compact;
- it is easy to ignore when empty;
- it matches the existing contextual-fragment model;
- it keeps reflective content distinct from user intent and developer policy.

## 8. Freshness and decay

Decay is mandatory.

Rules:

- `promote` items survive only until they are visibly absorbed into the main
  lane or replaced;
- `watch` items expire fast;
- `verify` items expire if not tested;
- `discard` never gets reinjected.

If decay is missing, the feature becomes cognitive sludge.

## 9. Compaction behavior

The reflective window should survive compaction without relying on the old raw
history surviving.

For v1:

- keep the authoritative window in `SessionState`;
- preserve it across compaction in `SessionState`;
- clear the live window when a new regular turn finishes, then let the next
  reflective refresh repopulate it if still useful;
- insert it again as a synthetic contextual fragment at the current-turn
  boundary on later sampling requests.

For rollback / undo:

- do not trust a session-local reflective snapshot blindly;
- if history is rewound, clear the live reflective window or rebuild it from a
  surviving durable snapshot;
- never keep reflective observations that were produced from turns the user has
  already removed from the thread.

For durability beyond the live session:

- later add an explicit persisted reflective snapshot surface so resume/fork can
  recover the last window without guessing.

## 10. App-server and TUI surface

Do not make UI/API the first slice.

V1 can stay internal and only affect model-visible context.

Later, if needed:

- add a thread item or notification for reflective-window refresh;
- expose lightweight debug visibility in app-server v2;
- optionally show the latest window in TUI diagnostics / thread metadata.

But the core value comes first from better main-lane cognition, not from UI.

## Recommended phased delivery

## Slice 0 - design only

Done in this document.

## Slice 1 - core-only experimental loop

Goal:

- internal reflective sidecar;
- bounded session-local reflective window;
- injection into future turns;
- no resume durability yet;
- clear-on-rollback semantics instead of pretending durability exists;
- no TUI/app-server UI yet.

Files likely touched:

- `codex-rs/core/src/state/session.rs`
- `codex-rs/core/src/codex.rs`
- `codex-rs/core/src/tasks/mod.rs`
- new `codex-rs/core/src/reflective/*`
- `codex-rs/core/src/contextual_user_message.rs`

This is the smallest real end-to-end slice.

## Slice 2 - durability across resume/fork/compaction replay

Goal:

- persist the latest reflective snapshot cleanly;
- reconstruct it on resume and fork;
- keep replay deterministic.

Possible approaches:

1. dedicated rollout item / state model;
2. dedicated contextual fragment plus explicit reconstruction logic.

Do not improvise this in hidden state only.

## Slice 3 - observability and tuning

Goal:

- app-server visibility,
- TUI diagnostics,
- better trigger heuristics,
- configurable model/effort/cooldown,
- metrics for cost vs value.

## Current config surface

Keep the main lane on Codex/OpenAI and point only the reflective sidecar at
Claude:

```toml
[features]
reflective_window = true

reflective_window_agent_type = "claude_reflector"

[agent_roles.claude_reflector]
description = "Claude reflective sidecar"
config_file = "~/.codex/agents/claude-reflector.toml"

[claude_cli]
path = "/home/amir/.npm-global/bin/claude"
permission_mode = "plan"
```

`~/.codex/agents/claude-reflector.toml`:

```toml
agent_backend = "claude_cli"
model = "claude-opus-4-6"
model_reasoning_effort = "high"
```

If you want all spawned subagents to use Claude by default:

```toml
agent_backend = "claude_cli"

[claude_cli]
path = "/home/amir/.npm-global/bin/claude"
permission_mode = "plan"
tools = ["Read", "Glob", "Grep"]
add_dirs = ["/absolute/project/path"]
```

If you want the main lane to stay on Codex/OpenAI but still be able to raise a
specific Claude Opus 4.6 subagent on demand, define a role and spawn that role:

```toml
[agent_roles.claude_worker]
description = "Claude Opus worker for bounded delegated tasks"
config_file = "~/.codex/agents/claude-worker.toml"
```

`~/.codex/agents/claude-worker.toml`:

```toml
agent_backend = "claude_cli"
model = "claude-opus-4-6"

[claude_cli]
permission_mode = "plan"
tools = ["Read", "Glob", "Grep"]
```

Then spawn it with `agent_type = "claude_worker"`.

Notes:

- `reflective_window_agent_type` is the narrow switch for "Claude only for the
  reflective sidecar";
- a bad `reflective_window_agent_type` now emits a startup warning instead of
  quietly degrading into a runtime-only no-op;
- root `agent_backend = "claude_cli"` switches normal spawned subagents too;
- `tools` is opt-in for external Claude agents; when omitted they stay
  text-only;
- `add_dirs` should stay narrow and absolute;
- `claude-opus-4-6` is the downstream default fallback model when a Claude
  backend is selected without an explicit Claude model.

## Failure modes to avoid

### 1. Recursive self-fanout

Reflective sidecars must never spawn more reflective sidecars.

### 2. Permanent background churn

If the feature runs too often, it becomes token burn with weak signal.

### 2.5. Turn-completion regression

If reflection blocks `on_task_finished`, user-visible completion latency gets
worse and queued follow-up work starts later than today.

### 3. Dumping all side thoughts into the main prompt

The point is selective promotion, not duplication.

### 4. Using public collab semantics for a private maintenance lane

This is an internal cognition feature, not normal multi-agent delegation.

### 5. Treating it as memory

Reflective windows are short-lived working memory, not long-term knowledge.

### 6. Sidecar self-rationalization

If the reflective sidecar automatically sees its own previous reflective window
as ordinary reinjected context, it can stabilize on stale narratives instead of
finding fresh blind spots.

### 7. Future-leak after rollback

If a detached reflective run completes after the parent thread was rolled back
or otherwise rewound, applying that stale report leaks "future" conclusions
into the restored past.

## Test plan for the first real implementation slice

- unit tests for trigger gating and cooldown;
- unit tests for decay / bounded retention;
- core snapshot for initial-context injection of `<reflective_window>`;
- compaction test proving reflective state still reappears after `/compact`;
- subagent spawn-config test proving reflective sessions stay read-only,
  `approval_policy = never`, and collab-disabled;
- negative test proving reflective runs do not trigger from review/compact turns.

## Bottom line

The correct native Codex feature is not "give the main agent more thoughts".

It is:

- a narrow main lane,
- an internal reflective maintenance sidecar,
- a tiny dynamic reflective window,
- strict freshness and promotion rules.

That gives Codex a place for non-obvious thinking **inside the runtime**
without turning the whole system into noisy multi-agent theater.
