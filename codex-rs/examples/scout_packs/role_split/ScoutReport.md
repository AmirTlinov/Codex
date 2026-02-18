# Scout report — role_split: quote-backed Scout handoff (example)

## 0) Meta
- Repo: `codex-rs` (monorepo root: `Codex/`)
- Goal: показать anchor-first handoff, где Scout отдает якори + spec, а `context_pack.md` строится автоматически.
- Artifacts:
  - `examples/scout_packs/role_split/ScoutReport.md`
  - `examples/scout_packs/role_split/excerpt_spec.yml`
  - `examples/scout_packs/role_split/context_pack.md`

## 1) Scope snapshot
- In scope: runtime Scout prompt, scout skill/template, fail-closed проверки генератора, just-цепочка `scout-pack-check -> scout-pack`.
- Out of scope: runtime бизнес-логика и патчи product-кода.

## 2) Patch target contract
- Allowed touchpoints: `core/templates/agents/scout.md`, `.agents/skills/scout_context_pack/*`, `scripts/scout_pack.py`, `examples/scout_packs/role_split/*`, `justfile` recipe refs.
- Forbidden touchpoints: production logic за пределами Scout DX.
- Verify (single repro): `cd codex-rs && just scout-pack-check examples/scout_packs/role_split/excerpt_spec.yml`

## 3) Key invariants / constraints
- Scout handoff обязан включать 3 inline-артефакта и stdout-first рендер (`CODE_REF::codex-rs::codex-rs/core/templates/agents/scout.md#L12-L47`).
- Scout передает anchors (`code_ref`) + metadata; verbatim excerpts строит генератор (`CODE_REF::codex-rs::codex-rs/core/templates/agents/scout.md#L32-L33`).
- Claim без цитаты невалиден (`CODE_REF::codex-rs::codex-rs/core/templates/agents/scout.md#L67-L69`).
- Skill gate G7 обязателен (`CODE_REF::codex-rs::codex-rs/.agents/skills/scout_context_pack/SKILL.md#L52-L59`).

## 4) Anchor map
- `CODE_REF::codex-rs::justfile#L75-L81` — рецепты генерации/валидации pack.
- `CODE_REF::codex-rs::codex-rs/core/templates/agents/scout.md#L12-L47` — artifact contract + rendering flow.
- `CODE_REF::codex-rs::codex-rs/core/templates/agents/scout.md#L62-L69` — quote-backed CODE_REF contract.
- `CODE_REF::codex-rs::codex-rs/.agents/skills/scout_context_pack/SKILL.md#L25-L60` — sections + quality gates.
- `CODE_REF::codex-rs::codex-rs/.agents/skills/scout_context_pack/templates/ScoutReport.md#L22-L46` — canonical evidence section.
- `CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L334-L454` — code_ref parsing + fail-closed checks.

## 5) Excerpt specs
- Spec: `examples/scout_packs/role_split/excerpt_spec.yml`.
- Pack generation: `just scout-pack examples/scout_packs/role_split/excerpt_spec.yml -o examples/scout_packs/role_split/context_pack.md`.

## 6) Evidence quotes (verbatim, from `context_pack.md`)
- Claim: Scout не пишет вырезки вручную.
  - CODE_REF: `CODE_REF::codex-rs::codex-rs/core/templates/agents/scout.md#L32-L33`
  - Quote: `"Scout does NOT hand-write verbatim code excerpts. Scout provides anchors (\`code_ref\`) + heading/description/why; \`scripts/scout_pack.py\` expands anchors into real quoted code in \`context_pack.md\`."`
  - Excerpt id: `scout-prompt-artifacts-and-render`
- Claim: CODE_REF без цитаты не проходит контракт.
  - CODE_REF: `CODE_REF::codex-rs::codex-rs/core/templates/agents/scout.md#L67-L69`
  - Quote: `"CODE_REF without a quote is invalid."`
  - Excerpt id: `scout-prompt-quote-backed-code-ref`
- Claim: генератор fail-closed на несовместимом `code_ref`/range и must_include.
  - CODE_REF: `CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L364-L383`
  - Quote: `"code_ref cannot be combined with"`
  - Excerpt id: `scout-pack-validation-overlap-and-must-include`

## 7) Dependency map
```mermaid
flowchart LR
  Spec[excerpt_spec.yml (anchors)] --> Check[just scout-pack-check]
  Check --> Render[just scout-pack -o -]
  Render --> Pack[context_pack.md (verbatim excerpts)]
  Pack --> Report[ScoutReport evidence quotes]
```

## 8) High-confidence risks / edge cases
- Drift ranges: line anchors устаревают после правок; `scout-pack-check` обязан падать до handoff (`CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L444-L447`).
- Шумный spec с широкими диапазонами снижает patch readiness; держим `must_include` на критичных токенах (`CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L450-L454`).

## 9) Missing context items needed before patching
- None for this example slice.

## 10) Patch readiness gates
- G1 Coverage: PASS
- G2 Determinism: PASS
- G3 Evidence-first: PASS
- G4 Actionability: PASS
- G5 Unknowns explicit: PASS
- G6 Noise budget: PASS
- G7 Quote-backed claims: PASS

Patch readiness: PASS
