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
2) `excerpt_spec.yml` (или `.json`) c anchor-first entries (`code_ref` обязателен по умолчанию) + heading/why; verbatim-вырезки строятся автоматически `scripts/scout_pack.py`.
3) `context_pack.md`, сгенерированный через `just scout-pack <excerpt_spec.yml> -o -` и встроенный в финальный ответ.
4) 1–2 Mermaid диаграммы (минимум dependency flow, опционально state/handoff).
5) Инкрементальный map-state (`scout_map.state.json`) + рендер (`scout_map.mmd`) для обновлений без полного переписывания схемы.

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
6) Evidence quotes (verbatim, from `context_pack.md`)
7) Dependency map
8) High-confidence risks / edge cases
9) Missing context items needed before patching
10) Patch readiness gates

## CODE_REF contract
- Каждый ключевой тезис во всех секциях (инвариант/контракт/риск/гейт/gap) обязан иметь минимум один `CODE_REF`.
- Формат:
  - `CODE_REF::<crate>::<repo_relative_path>#L<start>-L<end>`
- Пример:
  - `CODE_REF::codex-core::core/src/tools/router.rs#L140-L260`
- Диапазоны строк: 1-indexed, `end` включительно.
- Каждый тезис обязан иметь короткую verbatim-цитату (1–8 строк) из `context_pack.md`.
- `CODE_REF` без цитаты считается невалидным доказательством.
- Placeholder anchors (`Lx-Ly`, `<start>-<end>`, `...`) запрещены.
- Если файл/диапазон не найден — FAIL и явный `BLOCKER`.
- Для multi-anchor under single heading: несколько anchors в одной секции разрешены, включая разные файлы.
- Mermaid в секции может быть inline (`mermaid`) или из файла (`mermaid_file`), но не одновременно.
- Legacy fallback (`path/start_line/end_line`) разрешён только с `allow_explicit_ranges: true`.

## Quality gates (all required)
- G1 Coverage: все предполагаемые patch touchpoints покрыты anchors.
- G2 Determinism: нет duplicate/overlap anchors в одной зоне.
- G3 Evidence-first: каждый ключевой тезис имеет `CODE_REF` + quote.
- G4 Actionability: из пакета выводится bounded file list для патча.
- G5 Unknowns explicit: неизвестности перечислены как falsifiable gaps.
- G6 Noise budget: нет нерелевантных дампов и лишних файлов.
- G7 Quote-backed claims: нет claim-ов без цитаты из `context_pack.md`.

Финальный статус:
- `Patch readiness: PASS`
- или `Patch readiness: FAIL (Gx, Gy, ...)`

## Anti-noise rules
- Не дампить большие куски кода в отчёт; код идёт через `context_pack.md`.
- Scout не обязан вручную писать вырезки: достаточно anchors (`code_ref`) + описание; генератор пакета сам вставляет код.
- Mermaid обновляется инкрементально через `scout-map-merge` (дельты), а не полным ручным перерисовыванием.
- Не добавлять файлы вне in-scope + 1-hop зависимостей без обоснования.
- Не использовать расплывчатые формулировки без `CODE_REF`.
- Не допускать handoff только с диапазонами строк без реальных цитат.


## Mermaid map workflow (incremental)
1) Инициализация map-state (один раз на slice/подзадачу):
   - `just scout-map-init <state.json> --title "<slice map>" --direction LR`
2) При каждом новом найденном узле/связи:
   - записать delta `.yml/.json` (nodes/edges/remove_nodes/remove_edges)
   - `just scout-map-merge <state.json> <delta.yml>`
3) Перед финальным handoff:
   - `just scout-map-render <state.json> --output "<map.mmd>"`
   - в `excerpt_spec.yml` у нужной секции указать `mermaid_file: "<map.mmd>"`
   - затем `just scout-pack-check ...` и `just scout-pack ...`

Delta schema (short):
- `nodes[]`: `{id, label, shape?}`
- `edges[]`: `{from, to, label?}`
- `remove_nodes[]`: `[id, ...]`
- `remove_edges[]`: `[{from, to, label?}, ...]`

## Templates
- Report: `.agents/skills/scout_context_pack/templates/ScoutReport.md`
- Spec: `.agents/skills/scout_context_pack/templates/excerpt_spec.example.yml`

## Consumption (for Main)
- Быстро вручную: вытянуть verbatim по `CODE_REF` через `mcp__context__file_slice`/`meaning_expand`.
- Автоматически: `just scout-pack <excerpt_spec.yml> -o -` (anchor `code_ref` entries auto-expand в реальные вырезки).
- Валидация без записи: `just scout-pack-check <excerpt_spec.yml>`.

## Last verified
Last verified: 2026-02-18
