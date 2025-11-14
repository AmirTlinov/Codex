## Navigator Flagship Roadmap

This roadmap enumerates the concrete work required to turn Navigator into the primary discovery tool for any repository. Each epic contains rationale, exit criteria, and incremental milestones so we can iterate safely.

### 1. Full-Text Search & Instant Diff Preview

- **Goal:** eliminate dependency on `rg`/IDE for raw text queries; provide diff-style previews with highlighted matches.
- **Milestones:**
  1. ✅ **Streaming literal ingestion:** `IndexBuilder` now streams text payloads through a bounded queue and background workers that compress blocks off the hot path (fingerprint-guarded writes, automatic replay on rebuilds). Exit: background ingest keeps the index hot without blocking symbol updates—all literal storage is eventual and never stalls symbol ingest.
  2. ✅ **Search engine integration:** `text` profile теперь использует триграммные кандидаты + векторизованный memmem-поиск по сжатым блокам, отдаёт `match_count` и точные подсветки (спаны) прямо в `NavHit.context_snippet`, а CLI отображает их без дополнительного `rg`.
  3. ✅ **Diff preview:** `NavHit.context_snippet` теперь содержит diff-маркеры (+/ ) и подсветки, CLI форматирует мини-diff без шума.
  4. ✅ **Benchmark & autopick:** Planner автоматически подбирает `text` профиль для regex/path/многословных запросов и явно документирует причину в hints, чтобы не переключаться на `rg`.
- **Success criteria:** <300 ms P95 for queries over 100 k files; users never shell out to `rg` during internal dogfooding.

### 2. Project Atlas (Global Map & Domain Jump)

- **Goal:** provide an always-on “map” of modules/domains/layers with quick navigation.
- **Milestones:**
  1. **Domain extraction:** parse `Cargo.toml`, `package.json`, `docs/` structure to build a hierarchical taxonomy (crate → module → file).
     - ✅ Snapshot now stores per-file LOC counts and aggregates them through every node so summaries expose size + recency, not just counts.
  2. **Atlas API:** expose `/v1/nav/atlas` returning tree nodes with metadata (LOC, owners, churn, docs). Cache in snapshot.
     - ✅ `/v1/nav/atlas` теперь содержит LOC/recency + owners/churn/док/тест/dep и CLI выводит эти метрики.
  3. **Jump commands:** extend freeform parser with `atlas` verbs: `atlas summary core`, `atlas jump tui/history`.
     - ✅ `atlas summary` now available via CLI + freeform payloads; jump verb rewrites into a scoped search. Still need richer `jump` UX (breadcrumbs in planner, interactive chips).
  4. **UI surfacing:** show breadcrumbs + sibling modules in CLI outputs so users see where they are in the map.
     - ✅ `navigator atlas --summary` и сами search-ответы (CLI/TUI/JSON) теперь печатают Atlas-фокус + ближайшие модули, поэтому отдельный atlas-вызов нужен только для глубоких обзоров.
     - ✅ История, Doctor и flow-команды автоматически подхватывают последний atlas hint, выводя краткий breadcrumb (`core > planner · 42 files`) прямо в тексте/JSON, так что оператору не нужно спрашивать “где мы сейчас?”.
- **Success criteria:** navigation requests referencing domain names resolve without manual `find`/`ls`; onboarding users can orient themselves within minutes.

### 3. Contextual Ranking & Intent Signals

- **Goal:** rank results by relevance to current work (recency, TODO density, failing tests, ownership).
- **Milestones:**
  1. **Signal ingestion:** collect git recency, reviewer comments, TODO/FIXME counts, lint warnings; store per-file scores.
     - ✅ Снапшот теперь содержит `freshness_days`, `attention_density`, `lint_density`; builder заполняет их на каждом ingest через git log + TODO/#[allow] сканирование.
  2. **Ranking model:** add scoring pipeline combining fuzzy score + context bonuses; make weights configurable via config file.
     - ✅ `heuristic_score` использует новые сигналы: свежие/недавно правленные файлы получают boost, TODO-насыщенные блоки всплывают выше, а lint-heavy результаты штрафуются.
     - ✅ Coverage diagnostics подмешивают pending/errors/skipped причины в ранжирование: проблемные пути получают bonus и читаемый `score_reason` (например, `coverage skipped (missing)`), поэтому health-сигналы видны прямо в выдаче.
  3. **Personal context:** integrate plan/task files so active epics boost relevant files.
     - ✅ Navigator читает `.agents/current_plan.md` (или `NAVIGATOR_PLAN_PATH`) и активную ветку, вытягивает ключевые токены/изменённые пути и добавляет соответствующий boost в `heuristic_score` и literal hits.
  4. ✅ **Evaluation harness:** `codex navigator eval suite.json` прогоняет записанные кейсы, проверяет ранги и может сохранять snapshots для оффлайн A/B сравнения.
- **Success criteria:** ≥80 % of manual reorder actions disappear in daily use; top hit matches intent in qualitative reviews.

### 4. Faceted Exploration (Stacked Filters)

- **Goal:** offer zero-cost filtering by language, layer, difficulty, ownership, doc/test categories.
- **Milestones:**
  1. **Facet metadata:** augment `IndexSnapshot` with per-file attributes (layer, service, complexity score, owner).
  2. **Facet API:** extend `SearchStats` with a `facets` section (each facet → buckets + counts). Provide CLI commands to apply/remove facets interactively.
     - ✅ `navigator search` уже отдаёт фасеты, и CLI/TUI их печатают; осталось добавить приоритизацию по сложности/слою.
     - ✅ Добавлены свежие `freshness`/`attention` бакеты (0–1d, 2–3d,… / calm, hot), и CLI сразу печатает их в блоке facets.
  3. **Interactive loop:** add incremental `facet add lang=rust` commands that reuse previous `query_id`.
     - ✅ `codex navigator facet --from <query-id> --lang rust --tests` реализовано и подхватывается freeform `facet from=... lang=...`.
  4. **UX polish:** display active filters + suggestion chips to avoid cognitive overload.
     - ✅ CLI рисует filter-chips и автоматически переиспользует последний query_id, так что `codex navigator facet --lang rust` продолжает предыдущий поиск без ручного `--from`.
     - ✅ SearchResponse теперь возвращает facet_suggestions, CLI печатает готовые команды (`--lang foo`, `--tests`, `--owner team`), поэтому сужать выдачу можно за один шаг без чтения facets блока.
     - ✅ Когда выдача перегружена (hits ≥ limit или candidate_size > 450) и фильтры ещё не применялись, CLI автоматически запускает `facet` с верхней подсказкой, печатает `[navigator] auto facet: …` и повторяет поиск; включается по умолчанию и управляется `NAVIGATOR_AUTO_FACET`.
     - ✅ Автофасет теперь выполняет до двух последовательных шагов (пропуская уже применённые фильтры) и добавляет подсказки в историю, так что цепочки `lang=rust → tests → owner=core` происходят без участия оператора.
     - ✅ История хранит готовые стеки фильтров: `codex navigator facet --history-stack <n>` переиспользует комбинацию, `--remove-history-stack <n>` снимает её целиком, поэтому включение/снятие фильтров занимает одну команду.
     - ✅ `codex navigator history` показывает готовые команды (stack/clear/repeat/suggestion) и поддерживает `--json`, так что фильтры можно переиспользовать автоматически без парсинга.
     - ✅ Navigator handler понимает `history` и `history_list` payload'ы, поэтому ИИ может как вызывать stack/repeat напрямую, так и получать JSON с последними запросами/подсказками без CLI.
     - ✅ Добавлен режим `history suggestion`, позволяющий запускать сохранённые facet suggestions напрямую через инструмент (индекс истории + индекс подсказки) без ручного `facet --suggestion`.
     - ✅ Freeform parser + Navigator handler понимают `history` действия (stack/clear/repeat, pinned), поэтому ИИ-агент повторяет или модифицирует прошлые поиски напрямую через инструмент без вызова CLI.
- **Success criteria:** users can drill from >10 k hits to <20 hits in ≤3 commands without retyping the query.

### 5. Index Health & Regression Monitoring

- **Goal:** spot ingest lag, skipped files, or schema drift before users notice.
- **Milestones:**
  1. ✅ **Telemetry core:** ingest + search метрики теперь сохраняются в `health.bin`, фиксируют длительность full/delta прогонов, причины skip, literal fallback rate, медианные текстовые сканы и объёмы чтения. Данные переживают рестарт демона и используются в diagnostics/Doctor.
  2. ✅ **Health panel:** Doctor и streamed diagnostics показывают risk (green/yellow/red), список issues с remediation, резюме literal fallback и последние ingest прогонки; CLI печатает панель по умолчанию, `--json` возвращает сырой отчёт.
  3. ✅ **Guardrails:** health risk ≥ yellow или ленивые запросы (> `NAVIGATOR_GUARDRAIL_LATENCY_MS`) теперь порождают предупреждения и, при настроенном `NAVIGATOR_GUARDRAIL_WEBHOOK`, уходят во внешний JSON-webhook; срабатывания дросселируются `NAVIGATOR_GUARDRAIL_COOLDOWN_SECS` и отдельным 60s cooldown для slow-query.
  4. ✅ **Self-heal:** при state=Failed или при превышении лимитов pending/errors координатор автоматически запускает rebuild (дросселируется `NAVIGATOR_SELF_HEAL_COOLDOWN_SECS`), так что индексы восстанавливаются без ручного вмешательства.
- **Success criteria:** zero “why is navigator stale?” incidents; health panel always green or explains mitigation.

### 6. Query Profiler & Performance Studio

- **Goal:** give immediate insight into where time is spent (cache hit, matcher, literal scan, HTTP).
- **Milestones:**
  1. ✅ **Profiling hooks:** `run_search` / `run_text_search` теперь собирают тайминги (candidate load, matcher, hit assembly, references, facets, literal scan/fallback) и кладут их в `stats.stages`.
  2. ✅ **`/profiler` endpoint:** `/v1/nav/profile` возвращает последние _N_ сэмплов (query_id, урезанный запрос, тайминги, cache hit/literal flags) для любого workspace.
  3. ✅ **CLI view:** `codex navigator profile --limit N [--json]` печатает эти же сэмплы и подсвечивает “виноватые” стадии, так что bottlenecks видны без ручных логов.
  4. ✅ **Optimization backlog:** profiler агрегирует stage hotspots (avg/p95/max по последним запросам), `/v1/nav/profile` и `codex navigator profile` показывают виноватые стадии, так что регрессии matcher/glob/io фиксируются до жалоб пользователей.
- **Success criteria:** performance regressions get detected within one commit; engineers can self-serve bottleneck analysis.

### 7. UX Accelerators & Guided Workflows

- **Goal:** remove friction by surfacing next actions and automating common navigation playbooks.
- **Milestones:**
  1. ✅ **Command palette flows:** `codex navigator flow audit-toolchain|trace-feature-flag` запускает готовые цепочки, поддерживает `--input`, dry-run и наследует фокус/refs.
  2. ✅ **Context banners:** каждый ответ печатает блок `context:` (слои core/tui/... + категории docs/tests/deps) и показывает счётчики; CLI подсвечивает их рядом со stats.
  3. ✅ **Session memory:** history теперь хранит параметры поиска + превью хитов, `codex navigator repeat` повторяет любой запрос (включая закреплённые), а `codex navigator pin` позволяет закреплять/снимать и перечислять избранные цепочки.
  4. ✅ **Focus mode:** CLI `--focus` (auto/code/docs/tests/deps/all) фильтрует вывод, показывает suppressed-счётчики и сохраняется в history/repeat, так что шум от нецелевых категорий исчез.
- **Success criteria:** average navigation flow shrinks to ≤2 commands; subjective “cognitive load” score drops in dogfooding surveys.

### 8. Workspace Insights & Hotspot Briefings

- **Goal:** furnish instant situational awareness (TODO clusters, lint risks, unowned churn) so the agent never has to run ad-hoc repo scans.
- **Milestones:**
  1. ✅ **Baseline sections + tooling:** `/v1/nav/insights` aggregates attention/lint/ownership hotspots, CLI `codex navigator insights` prints them, and freeform payload `{"action":"insights"}` lets the handler fetch the same JSON.
  2. ✅ **Planner integration:** empty searches now receive a `hotspot: …` hint before planning (CLI + handler), and `codex navigator insights --apply N` spins up a focused search on the selected hotspot without retyping filters.
  3. ✅ **Trend tracking:** insight history хранит последние снимки, вычисляет `trend_summary` (новые/закрытые hotspots) и добавляет его в `insights`, streamed diagnostics и `navigator doctor` health panel.
- **Success criteria:** agents kick off work by reading insights instead of running blind searches; onboarding to a new repo takes <30 seconds because hotspots + atlas jumps cover the heavy lifting.

### Execution Guidance

- **Iteration cadence:** treat each epic as a 1–2 week slice with demoable value; keep this TODO updated after each milestone.
- **Quality bar:** every feature ships with planner/CLI documentation, unit + integration tests, and benchmarking notes.
- **Adoption:** once a milestone lands, dogfood it immediately inside Codex CLI and capture feedback under `.agents/context/`.
- **UX contract:** TUI остаётся исключительно пользовательским слоем: никаких кнопок управления Navigator, только короткий статус вроде `Navigator: <intent>` без лишнего шума. Navigator, CLI, facet/atlas/hints — это рабочий инструмент ИИ-агента, который должен обеспечивать полную навигацию по проекту без IDE/rg.
