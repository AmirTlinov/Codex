---
name: scout-context-pack
description: "Scout: собрать patch-ready контекст-пак (CODE_REF + excerpts + Mermaid) для прямой реализации Main"
ttl_days: 0
---

# Scout context pack (anchors + excerpt ranges)

## Trigger
Когда Main/Orchestrator просит контекст для атомарного slice-фикса.

## Outcome (what you MUST produce)
1) `ScoutReport.md` со строгими секциями и явным `Patch readiness: PASS|FAIL`.
2) `excerpt_spec.yml` (или `.json`) с line-anchored verbatim-вырезками.
3) `context_pack.md`, сгенерированный через `just scout-pack <excerpt_spec.yml> -o <context_pack.md>`.
4) 1–2 Mermaid диаграммы (минимум dependency flow, опционально state/handoff).

Drift guard (must stay in sync):
- Runtime Scout prompt template: `core/templates/agents/scout.md`.
- Canonical spec starting points:
  - `.agents/skills/scout_context_pack/templates/excerpt_spec.example.yml`
  - `examples/scout_packs/role_split/excerpt_spec.yml`

## Required ScoutReport sections (strict order)
1) Scope snapshot
2) Patch target contract
3) Key invariants / constraints
4) Anchor map
5) Excerpt specs
6) Dependency map
7) High-confidence risks / edge cases
8) Missing context items needed before patching
9) Patch readiness gates

## CODE_REF contract
- Каждый ключевой тезис (инвариант/риск/гейт) обязан иметь минимум один `CODE_REF`.
- Формат:
  - `CODE_REF::<crate>::<repo_relative_path>#L<start>-L<end>`
- Пример:
  - `CODE_REF::codex-core::core/src/tools/router.rs#L140-L260`
- Диапазоны строк: 1-indexed, `end` включительно.
- Если файл/диапазон не найден — FAIL и явный `BLOCKER`.

## Quality gates (all required)
- G1 Coverage: все предполагаемые patch touchpoints покрыты anchors.
- G2 Determinism: нет duplicate/overlap anchors в одной зоне.
- G3 Evidence-first: каждый риск/инвариант имеет CODE_REF.
- G4 Actionability: из пакета выводится bounded file list для патча.
- G5 Unknowns explicit: неизвестности перечислены как falsifiable gaps.
- G6 Noise budget: нет нерелевантных дампов и лишних файлов.

Финальный статус:
- `Patch readiness: PASS`
- или `Patch readiness: FAIL (Gx, Gy, ...)`

## Anti-noise rules
- Не дампить большие куски кода в отчёт; код идёт через `context_pack.md`.
- Не добавлять файлы вне in-scope + 1-hop зависимостей без обоснования.
- Не использовать расплывчатые формулировки без `CODE_REF`.

## Templates
- Report: `.agents/skills/scout_context_pack/templates/ScoutReport.md`
- Spec: `.agents/skills/scout_context_pack/templates/excerpt_spec.example.yml`

## Consumption (for Main)
- Быстро вручную: вытянуть verbatim по `CODE_REF` через `mcp__context__file_slice`/`meaning_expand`.
- Автоматически: `just scout-pack <excerpt_spec.yml> -o <context_pack.md>`.
- Валидация без записи: `just scout-pack-check <excerpt_spec.yml>`.

## Last verified
Last verified: 2026-02-16
