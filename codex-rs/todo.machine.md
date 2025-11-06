# Upstream Sync & MCP Overhaul — Conflict Resolution Playbook v2

## Миссия и KPI
- **Функция успеха F = 0.35·Correctness + 0.20·Simplicity + 0.20·Performance + 0.15·Cost + 0.10·Risk** (все оценки 0..5).
- Успех ребейза = ветка `Enhancex` без конфликтов поверх `upstream/main` на 2025-10-15, все проверки flagship+++ зелёные (Coverage ≥85% по Statements/Lines, SLO p99 ≤200 мс, ExactlyOnce = CLAIM|OUTBOX, CI без регрессий), все Enhancex-фичи (CLI/TUI wizard, templates, migrations) сохранены.
- Доп KPI: ZeroSecrets, SBOM обновлена, `CLAIM|OUTBOX` соблюдена в коде и документации, конфликты устранены без потери апстрим-патчей.

## Жёсткие ограничения / Non-Negotiables
- DDD + Ports&Adapters + ModularMonolith: слои `domain→shared`, `app→{domain,shared}`, `adapters→{domain,shared}`, `infra→{adapters,domain,shared}`, без циклов.
- NoGodObjects (Class ≤10 методов, ≤16 полей, ≤400 строк); File ≤500 строк; Function ≤80 строк; Cyclomatic ≤10; Cognitive ≤15; Duplication ≤1.5%.
- ExactlyOnce стратегия: **CLAIM|OUTBOX**; Claim_First + Claim_Returning обязательны; конфликтная политика `ReturnExisting | 409_by_flag`; `idempotency_key` частичный уникальный индекс `WHERE NOT NULL`.
- DB время только через now()/clock_timestamp; никакой TimeBucket как primary.
- Outbox reaper TTL ≤60 s; Reconcile план = `external_ref_unique | compensation`.
- SQL DDL: UNIQUE/CHECK/FK обязательны; `CREATE INDEX CONCURRENTLY`; без грязных миграций.
- Zero Legacy/Deadcode/костыли; новые фичи с тестами: unit + property + race (≥max(32, 2×CPU) потоков, ≥3 повторов, seed фиксирован) + crash-edge + tracing.
- Coverage ≥85%; при падении — блокер.
- Observability: Prometheus спецификация в `/metrics`, защищена TLS+Auth, dev-флаг `ALLOW_INSECURE_METRICS_DEV=true`.
- Config доступ только через `config/`; утилиты — в `shared/utils`, чистые, повторное использование ≥2 раз.
- Коммиты — Conventional, подписанные, один логический шаг → commit+push (git CI контракт), никакого rebase squash.
- Патчи: только `begin_patch` с idempotency-key при повторном применении; без разрушительных команд и без удаления чужих изменений.

## Текущий срез ребейза (2025-10-15)
- Ребейз `Enhancex` над `upstream/main`, остановка на `e36eb67d (Add MCP management wizard UX and harden Seatbelt symlink policies)`; впереди ещё 11 коммитов (`git status` подтверждён).
- Конфликтующие файлы: `codex-rs/Cargo.lock`, `codex-rs/cli/Cargo.toml`, `codex-rs/cli/src/lib.rs`, `codex-rs/cli/src/mcp_cmd.rs`, `codex-rs/core/src/mcp/mod.rs`, `codex-rs/tui/src/app.rs`, `codex-rs/tui/src/app_event.rs`, `codex-rs/tui/src/bottom_pane/mod.rs`, `codex-rs/tui/src/chatwidget.rs`, `codex-rs/tui/src/lib.rs`.
- Уже добавлены новые артефакты (staged): `cli/src/mcp/{cli.rs,mod.rs,wizard.rs}`, `core/src/mcp/health.rs`, `tui/src/mcp/*`, обновления `config_types.rs`, миграций, RFC, `CHANGES.md`.
- Working tree доп. модификации: `core/src/config.rs`, `core/src/config_types.rs`, `core/src/mcp/registry.rs`, `tui/src/mcp/...` и др. Untracked: `modes/`, `reports/`, `tmp/`; требуется убедиться, что они не попадут в commit.

## Assumptions (TTL=1h)
- **A1 — снапшоты `tmp/upstream` и `tmp/enhancex` релевантны текущему конфликту.**
  - Best-case: reused → экономия ~0.3 F по cost/simplicity.
  - Worst-case: пересборка + ручная валидация, стоимость +1.5 ч, риск ошибки схемы; F падает по cost −0.3, risk −0.2, correctness −0.1.
- **A2 — новые зависимости CLI/TUI ограничены workspace-версиями (inquire ≥0.7 уже в workspace).**
  - Best-case: зависимостей не добавляем → стабильный Cargo.lock; simplicity +0.2.
  - Worst-case: потребуется внешняя версия (`tracing`, `tracing-subscriber`), тогда надо обновить workspace и SBOM; риск +0.2, cost +0.2.
- **A3 — seatbelt/landlock политики из апстрима не конфликтуют с Enhancex apply-patch изменениями.**
  - Best-case: unit-тесты `codex-apply-patch` зелёные, correctness +0.3.
  - Worst-case: race при создании временных runtime; потребуется рефактор и доп. тесты, риск +0.3, performance −0.2.

## Risk Register (актуализировать перед `git rebase --continue`)
- **R1 (High)**: Несоответствие JSON схемы CLI/TUI wizard vs docs → клиенты ломаются. _Mitigation_: интеграционные тесты `codex mcp list/wizard --json`, doc-sync ревью.
- **R2 (High)**: Неверное преобразование `tool_timeout_ms → sec` в конфигурации вызывает таймауты <CLAIM SLA. _Mitigation_: property-тесты + миграции + double-check default.
- **R3 (Medium)**: TUI event loop не получает новые события MCP → UX замерзает, p99 >200 мс. _Mitigation_: обновить `AppEvent` маршрутизацию + snapshot тесты + perf smoke (`cargo run -p codex-tui -- --mock`).
- **R4 (Medium)**: `Cargo.lock` рассинхронизирован → воспроизводимость сломана. _Mitigation_: regenerate lock, `cargo tree -p codex-cli` diff, SBOM обновить.
- **R5 (Medium)**: Untracked `tmp/`/`reports/` случайно попадут в commit. _Mitigation_: добавить в `.git/info/exclude` или очистить перед staging, зафиксировать в Ops log.

## Workstreams (хронология и зависимости)

### WS0 — Pre-flight & Governance (Status: DONE)
- [x] Проверен remote `upstream` → `https://github.com/openai/codex`.
- [x] Зафиксирована стратегия ExactlyOnce = CLAIM|OUTBOX в `docs/rfcs/0001` и планах.
- [x] Инвентаризированы снапшоты `tmp/upstream`/`tmp/enhancex`; соответствуют текущим конфликтам.
- Output: baseline логи в `.flagship/ops.log`, обновлён KPI чеклист.

### WS1 — Core Config Schema QA (Status: QA HOLD)
**Цель:** Domain слой (`core/config_types.rs`, `config.rs`, migrations, templates) отражает новую схему MCP и проходит тесты.
- [x] Синхронизированы `RawMcpServerConfig`, `McpServerConfig`, `McpTemplate*` (новые поля auth/health/templates/tags, ms→sec) в `core/src/config_types.rs`.
- [x] Обновлены миграции `core/src/config/migrations/mcp.rs` (schema v2, нормализация таймаутов).
- [x] Обновлены шаблоны `core/src/mcp/templates.rs` + registry (инжекция defaults, tags).
- [ ] Добавить property-тест по ms→sec (раздел `core/tests/suite/rmcp_client.rs`) с seed pinning; AC: значения >0, округление floor-поведения понятно.
- [ ] Дополнить unit-тест `McpServerConfig::default` → проверка fallback на DB clock, отсутствие пустых строк в tags.
- [ ] Review docs+RFC на предмет указания новых полей (ссылка в WS5) и обновить ссылки из core (doc-comment).
- Acceptance: `cargo test -p codex-core --all-features`, `cargo test -p codex-core core::config::tests::mcp_*` локально + coverage ≥85% сохраняется.

### WS2 — CLI MCP Wiring (Status: IN PROGRESS)
**Цель:** Консолидация CLI зависимостей и команд под новую схему MCP.
- [x] Разрешить конфликт `codex-rs/cli/Cargo.toml`:
  - Workspace версии `inquire`, `owo-colors`, `supports-color`, Seatbelt deps выровнены; `cargo metadata -q` проходит локально.
- [x] Обновить `codex-rs/cli/src/lib.rs`: экспорт `pub mod mcp;`, публичные Seatbelt/Landlock команды сохранены, doc-tests не ломаются.
- [x] Свести `codex-rs/cli/src/mcp_cmd.rs` c `clap` декларацией: subcommands Serve/Login/Logout/Migrate/Wizard/Import/Doctor синхронизированы, `codex_linux_sandbox_exe` проброшен через `main.rs`.
- [x] Привести `codex-rs/cli/src/mcp/{cli.rs,wizard.rs}` к transport-aware API; ms→sec конверсии закрыты.
- [ ] Добавить негативные property-тесты по невалидному `tool_timeout_ms` (включая <0, overflow, string inputs).
- [x] Починить `cli/tests/mcp_list.rs` ожидания JSON (добавить `display_name`, `category`, `transport`, `auth`, `health`, `template_defaults`).
- [ ] Добавить интеграционные тесты:
  - `cli/tests/mcp_list_json.rs`: `codex mcp list --json`, проверка полей + idempotency ключ.
  - `cli/tests/mcp_wizard_json.rs`: `codex mcp wizard --json --template <id> --apply`, проверка summary `server` блока и миграций.
  - Гарантировать запуск в sandbox-дружественном режиме (`CODEX_SANDBOX_NETWORK_DISABLED=1`).
- [ ] Обновить `CHANGES.md` и `docs/config.md` ссылками на новые CLI команды (см. WS5).
- Acceptance: `cargo check -p codex-cli`, `cargo test -p codex-cli`, интеграционные тесты зелёные ≤200 мс p99. (Текущее состояние: `cargo test -p codex-cli` пройдено 2025-10-16.)

### WS3 — Core MCP Modules & Registry (Status: ACTIVE)
**Цель:** Поддержка `core/src/mcp/mod.rs`, `registry.rs`, `health.rs` с учётом новых transport/helpers.
- [ ] Разрешить конфликт `core/src/mcp/mod.rs`: подключить `health` модуль, реэкспортировать `registry::{validate_server_name, load_registry}` и новые template helpers.
- [ ] Убедиться, что `registry.rs` использует синхронный loader без избыточного runtime: при необходимости вынести helper (todo flagged в самокритике) и покрыть unit-тестом.
- [ ] Проверить, что `health.rs` корректно проверяет endpoints и совместим с wizard (timeouts, auth hints).
- [ ] В `apply-patch` seatbelt тестах (`codex-rs/apply-patch/src/lib.rs`, `exec/tests/suite/apply_patch.rs`) удостовериться: новые политики landlock/sealtbelt учитывают wizard tmp файлы.
- Acceptance: `cargo test -p codex-core core::mcp::*`, `cargo test -p codex-exec --test suite` (особенно apply_patch).

### WS4 — TUI MCP Surface (Status: QA HOLD)
**Цель:** Визуализация менеджера и wizard в TUI, синхронная с CLI.
- [x] Разрешить конфликты:
  - `tui/src/app.rs`: внедрены события и состояние `McpManager`/`Wizard`, подключён `TemplateCatalog`.
  - `tui/src/app_event.rs`: маршрутизация `AppEvent::Mcp*` завершена.
  - `tui/src/bottom_pane/mod.rs`: стек вьюверов поддерживает кастомные экраны.
  - `tui/src/chatwidget.rs`: хуки для менеджера/wizard, fallback на history summary.
  - `tui/src/lib.rs`: экспорт новых модулей и init state.
- [x] Обновить snapshot тесты `tui/src/mcp/snapshots/*.snap` (`cargo test -p codex-tui` → `cargo insta accept -p codex-tui`).
- [ ] Добавить property-тест на переключение форм wizard (валидаторы для `tool_timeout_ms`, auth fields). _Блокер_: нужен deterministic harness.
- [ ] Добавить race-тест (≥64 потоков) для событий UI (`tokio::test(flavor = multi_thread)`, фиксированный seed).
- Acceptance: `cargo test -p codex-tui`; p99 ≤200 мс, snapshots детерминированы.

### WS5 — Docs & Contract Publication (Status: PENDING)
**Цель:** Синхронизировать публичные контракты и документацию.
- [ ] Обновить `docs/config.md`: новая таблица полей (`display_name`, `category`, `template_defaults`, `auth`, `healthcheck`, `tool_timeout_sec`), CLI команды (`mcp wizard`, `mcp migrate`, `mcp doctor`).
- [ ] Освежить `docs/rfcs/0001-mcp-management-overhaul.md`: зафиксировать ExactlyOnce = CLAIM|OUTBOX, idempotency-key, транспорт-aware pipeline, связь с TUI wizard.
- [ ] В `docs/rfcs` добавить раздел о Reconcile (external_ref_unique | compensation) и Outbox TTL.
- [ ] Опубликовать JSON Schema для events (wizard summary, manager state) в `/docs/schemas/` (если ещё не создано) — указать версию `mcp_schema_version = 2`.
- Acceptance: рецензия → docs lint (`markdownlint`, `typos`) чистые.

### WS6 — Testing & Quality Gates (Status: QUEUED)
**Цель:** Защитить качество до завершающего `rebase --continue`.
- [ ] Форматирование: `just fmt` (по умолчанию, без запроса), `just fix -p codex-core`, `just fix -p codex-cli`, `just fix -p codex-tui` (предварительно запросить разрешение у пользователя согласно правилам).
- [ ] Таргетные тесты:
  - `cargo test -p codex-core --all-features`.
  - `cargo test -p codex-cli` (включая интеграционные новые).
  - `cargo test -p codex-tui` + `cargo insta accept -p codex-tui` после ревью снапшотов.
  - `cargo test -p codex-exec --features seatbelt-tests` (для apply-patch).
- [ ] Глобальные проверки (после локальных): `cargo test --workspace --all-features` (по запросу пользователя), `make agent` (fast/full в зависимости от профиля).
- [ ] Coverage: собрать `cargo tarpaulin` или существующий профилировщик, убедиться Statements/Lines ≥85% (зафиксировать в отчёте).
- [ ] Performance smoke: `cargo run -p codex-cli -- mcp list` и `make agent-release` dry-run (если время позволяет) → мониторить p99 ≤200 мс.

### WS7 — Observability & Metrics Compliance (Status: PENDING)
**Цель:** Метрики и ExactlyOnce аналитика согласованы.
- [ ] Обновить `/metrics` спецификацию: добавить `gateway_calls/logical_charges` ratio 1.00±0.01 (15 min), outbox backlog, lock_wait_p99 ≤50 ms.
- [ ] Убедиться, что telemetry код (если затрагивается) публикует новые поля; при необходимости обновить `codex-rs/shared/metrics` (пока без изменений, но проверить `rg "gateway_calls"`).
- [ ] Проверить, что dev флаг `ALLOW_INSECURE_METRICS_DEV=true` документирован и не включается в prod конфиг.
- [ ] SBOM: если зависимости менялись, обновить `tooling/sbom` (скрипт проекта) и приложить вывод в `reports/` (без коммита).

### WS8 — Rebase Finalization & Release Prep (Status: PENDING)
**Цель:** Завершить ребейз, соблюдая git-политику.
- [ ] Очистить untracked `modes/`, `reports/`, `tmp/` или добавить в `.git/info/exclude` (не в gitignore) перед staging.
- [ ] `cargo update -p ...` не делать без необходимости; `cargo metadata` убедиться, что lock обновлено.
- [ ] `cargo generate-lockfile` → resolve `Cargo.lock`, ручной diff (сохранить лог в `reports/cargo-lock-diff.txt`).
- [ ] Проверить `git diff --stat` и `git status` чистоту, затем `git add` конфликтовавшие файлы.
- [ ] Записать шаги/результаты в `.flagship/ops.log` (Claim_First/Returning подтверждены).
- [ ] `git rebase --continue`; если новые конфликты — вернуться к релевантному WS (обновить план).
- [ ] После успешного ребейза: прогнать финальные тесты, подготовить Conventional Commit сообщение (например `feat(cli): align mcp wizard with upstream schema`), подписать `git commit -S` и push.
- [ ] Создать отчет `reports/rebase-summary.md` (не коммитить) с перечислением решённых конфликтов и тестов.

## Verification & Traceability Matrix
- MCP Config Schema → Tests: `core::config_types` unit/property; Docs: `config.md`, RFC.
- CLI Wizard JSON → Tests: `cli/tests/mcp_wizard_json.rs`; Docs: `config.md`; Metrics: None.
- TUI Manager → Tests: snapshots under `tui/src/mcp/snapshots`, race/property tests; Docs: `docs/rfcs/0001` UX раздел.
- Apply Patch Seatbelt → Tests: `exec/tests/suite/apply_patch.rs`; Metrics: lock_wait_p99.

## Tooling Protocol
- Патчи только через `begin_patch --machine --idempotency-key auto` (при повторном прогоне).
- Форматирование — автоматическое `just fmt`; lint/Clippy через `just fix -p <crate>` после подтверждения.
- Snapshot review: `cargo insta pending-snapshots -p codex-tui` перед `accept`.
- Тестовые гонки: использовать `RUST_TEST_THREADS=1` для детерминированных интеграционных тестов и отдельные property гонки со своим harness.
- Все операции логировать в `.flagship/ops.log` (timestamp ISO8601, шаг, результат).

## Exit Criteria Checklist
- [ ] Все workstream задачи в статусе DONE (обновить файл).
- [ ] `git status` чист, `cargo fmt -- --check` нет изменений.
- [ ] Тесты и метрики (Coverage, p99, ExactlyOnce) подтверждены.
- [ ] Документация и схемы обновлены и рецензированы.
- [ ] Ребейз завершён, коммиты подписаны и запушены.
