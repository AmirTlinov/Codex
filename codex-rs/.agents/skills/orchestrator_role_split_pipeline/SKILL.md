---
name: orchestrator-role-split-pipeline
description: "Оркестратор → Scouts+Team→Main/Team(implement)→Validator с безопасными контрактами"
ttl_days: 0
---

# Orchestrator role-split pipeline (builder-off mode)

## Trigger
Нужно выполнить задачу итеративно слайсами, сохранив high-signal контекст и fail-closed проверки.

## Outcome
- Основной контур: `Scout -> Orchestrator context decision -> Team/Main implement -> Validator`.
- Scout отдает patch-ready контекст-пак (CODE_REF + excerpt_spec + Mermaid).
- Решение `CONTEXT_APPROVED | CONTEXT_GAPS` принимает оркестратор (без отдельного промежуточного валидатора контекста).
- Team/Main делает минимальный патч по slice; Validator проверяет патч на контракт/verify.
- Делегированный `apply_patch` должен запускать бинарь `codex` (self-invocation), а не `codex_linux_sandbox_exe`; sandbox-wrapper применяется оркестратором отдельно.

## How to request Scout (copy/paste prompt skeleton)
Проси Scout так, чтобы он вернул **контекст‑пак, готовый для патча**:

- Sections: Scope snapshot -> Patch target contract -> Key invariants -> Anchor map -> Excerpt spec -> Mermaid -> Risks -> Unknowns -> Patch readiness.
- Доказательства: `CODE_REF::<crate>::<path>#L<start>-L<end>` + краткие verbatim quote из `context_pack.md`.
- Артефакты: `ScoutReport.md`, `excerpt_spec.yml`, `context_pack.md`.

## Handoff state machine
`discover -> orchestrator_ctx_decision -> implement -> review_patch -> final_accept`

## Pointers
- `core/src/agent/role.rs`
- `core/src/tools/spec.rs`
- `core/src/tools/handlers/collab.rs`
- `core/src/tools/handlers/apply_patch.rs`
- `core/src/tools/runtimes/apply_patch.rs`
- `core/src/tools/router.rs`
- `core/src/tools/js_repl/mod.rs`
- `core/prompt.md`
- `core/config.schema.json`
- `core/tests/suite/request_user_input.rs`
- `core/tests/suite/unified_exec.rs`
- `tui/src/chatwidget.rs`
- `../docs/config.md` (monorepo)
- `.agents/skills/scout_context_pack/SKILL.md`

## Known risk
- Contract drift между skill docs и runtime templates (`core/templates/agents/*.md`).
- Лечится регулярной сверкой handoff и CODE_REF формата.

## Runtime invariants (S9/S10)
- Wake lifecycle (`core/src/orchestration/wake.rs`):
  - timeout обязан эвиктить **все** expired wake_id;
  - escalation в timeout-path только для `ack_required=true`;
  - ack/nack обязаны удалять финальное состояние wake_id.
- Team lifecycle (`core/src/orchestration/lifecycle.rs`):
  - `role + version` immutable: повторная регистрация разрешена только с тем же `prompt_profile`;
  - runbook memory `record_id` immutable в active/archive: no silent overwrite.

## Verify anchors (S3/S4)
- S3 identity:
  - `cargo test -p codex-protocol collab_agent_identity_roundtrip`
  - `cargo test -p codex-core team_profile_config_roundtrip`
- S4 mesh routing/schema:
  - `cargo test -p codex-core collab_message_schema_mentions_and_priority`
  - `cargo test -p codex-core collab_routing_agent_role_all`

## Last verified
Last verified: 2026-02-16
