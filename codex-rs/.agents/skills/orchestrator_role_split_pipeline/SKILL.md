---
name: orchestrator-role-split-pipeline
description: "Оркестратор → Scout→ContextValidator→Main(implement)→Validator с безопасными контрактами"
ttl_days: 0
---

# Orchestrator role-split pipeline (builder-off mode)

## Trigger
Нужно выполнить задачу итеративно слайсами, сохранив high-signal контекст и fail-closed проверки.

## Outcome
- Основной контур: `Scout -> ContextValidator -> Main implement -> Validator`.
- Scout отдает patch-ready контекст-пак (CODE_REF + excerpt_spec + Mermaid).
- ContextValidator выдает только `CONTEXT_PACK_APPROVED` или `CONTEXT_PACK_GAPS`.
- Main делает минимальный патч по slice; Validator проверяет патч на контракт/verify.

## How to request Scout (copy/paste prompt skeleton)
Проси Scout так, чтобы он вернул **контекст‑пак, готовый для патча**:

- Sections: Scope snapshot -> Patch target contract -> Key invariants -> Anchor map -> Excerpt spec -> Mermaid -> Risks -> Unknowns -> Patch readiness.
- Доказательства: `CODE_REF::<crate>::<path>#L<start>-L<end>`.
- Артефакты: `ScoutReport.md`, `excerpt_spec.yml`, `context_pack.md`.

## Handoff state machine
`discover -> validate_ctx -> implement -> review_patch -> final_accept`

## Pointers
- `core/src/agent/role.rs`
- `core/src/tools/spec.rs`
- `core/src/tools/handlers/collab.rs`
- `core/src/tools/handlers/apply_patch.rs`
- `core/src/tools/router.rs`
- `core/src/tools/js_repl/mod.rs`
- `core/config.schema.json`
- `core/tests/suite/request_user_input.rs`
- `core/tests/suite/unified_exec.rs`
- `tui/src/chatwidget.rs`
- `../docs/config.md` (monorepo)
- `.agents/skills/scout_context_pack/SKILL.md`

## Known risk
- Contract drift между skill docs и runtime templates (`core/templates/agents/*.md`).
- Лечится регулярной сверкой handoff и CODE_REF формата.

## Last verified
Last verified: 2026-02-14
