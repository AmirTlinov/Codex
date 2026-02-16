# Scout report — <topic>

## 0) Meta
- Repo: `codex-rs`
- Task: `<TASK-ID>`
- Slice: `<SLICE-ID>`
- Goal: `<what must be proven before patching>`
- Artifacts:
  - `ScoutReport.md`
  - `excerpt_spec.yml`
  - `context_pack.md`

## 1) Scope snapshot
- In scope: …
- Out of scope: …

## 2) Patch target contract
- Allowed touchpoints: …
- Forbidden touchpoints: …
- Verify command (single repro): `…`

## 3) Key invariants / constraints
Every item MUST have at least one `CODE_REF`.

- `<invariant>` (`CODE_REF::<crate>::<path>#Lx-Ly`)
- `<constraint>` (`CODE_REF::<crate>::<path>#Lx-Ly`)

## 4) Anchor map
- `CODE_REF::<crate>::<path>#Lx-Ly` — why this anchor matters.
- `CODE_REF::<crate>::<path>#Lx-Ly` — …

## 5) Excerpt specs
- Attach or inline `excerpt_spec.yml`.
- Must cover all planned patch touchpoints.

## 6) Dependency map
### 6.1 Flow
```mermaid
flowchart LR
  A[Input] --> B[Core]
  B --> C[Output]
```

### 6.2 Handoff / state (if needed)
```mermaid
stateDiagram-v2
  [*] --> discover
  discover --> validate_ctx
  validate_ctx --> implement
  implement --> review_patch
  review_patch --> final_accept
```

## 7) High-confidence risks / edge cases
Every risk MUST have `CODE_REF`.

- `<risk>` (`CODE_REF::<crate>::<path>#Lx-Ly`) — falsifier: `<cheapest check>`

## 8) Missing context items needed before patching
- `Need more context: <exact missing evidence>`
  - Expected evidence: `CODE_REF::<crate>::<path>#Lx-Ly`
  - Falsifier: `<single-step check>`

## 9) Patch readiness gates
- G1 Coverage: PASS|FAIL
- G2 Determinism: PASS|FAIL
- G3 Evidence-first: PASS|FAIL
- G4 Actionability: PASS|FAIL
- G5 Unknowns explicit: PASS|FAIL
- G6 Noise budget: PASS|FAIL

**Patch readiness: PASS|FAIL**
