# Scout report — role_split: Scout context pack (example)

## 0) Meta
- Repo: `codex-rs` (monorepo: `Codex/`)
- Goal: показать эталонный Scout output с минимальным шумом и patch-ready контекстом.
- Artifacts:
  - `examples/scout_packs/role_split/ScoutReport.md` (this file)
  - `examples/scout_packs/role_split/excerpt_spec.yml` (machine-readable SSOT)
  - `examples/scout_packs/role_split/context_pack.md` (generated verbatim excerpts)

## 1) Scope snapshot
- In scope: excerpt spec + генератор `scout_pack.py` + repo skills/templates + just recipe.
- Out of scope: любые runtime-гейты (apply_patch/tool allowlist/locks) и реальные код-фиксы.

## 2) Patch target contract
- Target: documentation/example only (no code patch in this slice).
- Verify (single repro):
  - `cd codex-rs && just scout-pack-check examples/scout_packs/role_split/excerpt_spec.yml`

## 3) Key invariants / constraints
- Scout не дампит код в отчёт: код идёт через `context_pack.md` (verbatim excerpts). (`CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L1-L282`)
- Диапазоны строк: 1-indexed, `end` включительно. (`CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L1-L282`)
- Генератор fail-closed: любая ошибка пути/диапазона → error, без частичного вывода. (`CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L1-L282`)

## 4) Anchor map
- `CODE_REF::codex-rs::codex-rs/examples/scout_packs/role_split/excerpt_spec.yml#L1-L70` — SSOT excerpt spec.
- `CODE_REF::codex-rs::justfile#L67-L81` — `scout-pack` / `scout-pack-check` recipes.
- `CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L1-L282` — generator implementation.

## 5) Excerpt specs
- File: `examples/scout_packs/role_split/excerpt_spec.yml` (use as a starting point).

## 6) Dependency map
```mermaid
flowchart LR
  Spec[excerpt_spec.yml] --> Gen[scripts/scout_pack.py]
  Gen --> Pack[context_pack.md]
```

## 7) High-confidence risks / edge cases
- Drift: file edits могут сделать ranges невалидными → всегда прогоняй `scout-pack-check` перед handoff. (`CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L1-L282`)

## 8) Missing context items needed before patching
- None (example-only slice).

## 9) Patch readiness gates
- G1 Coverage: PASS
- G2 Determinism: PASS
- G3 Evidence-first: PASS
- G4 Actionability: PASS
- G5 Unknowns explicit: PASS
- G6 Noise budget: PASS

Patch readiness: PASS
