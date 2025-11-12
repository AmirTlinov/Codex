## Navigator Flagship Roadmap

This roadmap enumerates the concrete work required to turn Navigator into the primary discovery tool for any repository. Each epic contains rationale, exit criteria, and incremental milestones so we can iterate safely.

### 1. Full-Text Search & Instant Diff Preview

- **Goal:** eliminate dependency on `rg`/IDE for raw text queries; provide diff-style previews with highlighted matches.
- **Milestones:**
  1. **Streaming literal ingestion:** extend `IndexBuilder` to store per-line offsets + compressed text blocks for every file (respect ignore rules). Exit: background ingest keeps index hot without blocking symbol updates.
  2. **Search engine integration:** add a `text` profile that bypasses symbol matching and scans the text blocks via trigram filters + vectorized scanning; include match counts + highlight spans.
  3. **Diff preview:** update `NavHit` to optionally include a `context_snippet` payload (line numbers + emphasis markers). Extend CLI renderer to show miniature diffs.
  4. **Benchmark & autopick:** wire adaptive planner logic that chooses literal vs text index based on query entropy (<3 chars, regex-like, etc.).
- **Success criteria:** <300 ms P95 for queries over 100 k files; users never shell out to `rg` during internal dogfooding.

### 2. Project Atlas (Global Map & Domain Jump)

- **Goal:** provide an always-on “map” of modules/domains/layers with quick navigation.
- **Milestones:**
  1. **Domain extraction:** parse `Cargo.toml`, `package.json`, `docs/` structure to build a hierarchical taxonomy (crate → module → file).
     - ✅ Snapshot now stores per-file LOC counts and aggregates them through every node so summaries expose size + recency, not just counts.
  2. **Atlas API:** expose `/v1/nav/atlas` returning tree nodes with metadata (LOC, owners, churn, docs). Cache in snapshot.
     - ✅ `/v1/nav/atlas` already includes LOC + doc/test/dep metrics; owners/churn remain TBD.
  3. **Jump commands:** extend freeform parser with `atlas` verbs: `atlas summary core`, `atlas jump tui/history`.
     - ✅ `atlas summary` now available via CLI + freeform payloads; jump verb rewrites into a scoped search. Still need richer `jump` UX (breadcrumbs in planner, interactive chips).
  4. **UI surfacing:** show breadcrumbs + sibling modules in CLI outputs so users see where they are in the map.
     - ✅ `navigator atlas --summary` и сами search-ответы (CLI/TUI/JSON) теперь печатают Atlas-фокус + ближайшие модули, поэтому отдельный atlas-вызов нужен только для глубоких обзоров.
- **Success criteria:** navigation requests referencing domain names resolve without manual `find`/`ls`; onboarding users can orient themselves within minutes.

### 3. Contextual Ranking & Intent Signals

- **Goal:** rank results by relevance to current work (recency, TODO density, failing tests, ownership).
- **Milestones:**
  1. **Signal ingestion:** collect git recency, reviewer comments, TODO/FIXME counts, lint warnings; store per-file scores.
     - ✅ Снапшот теперь содержит `freshness_days`, `attention_density`, `lint_density`; builder заполняет их на каждом ingest через git log + TODO/#[allow] сканирование.
  2. **Ranking model:** add scoring pipeline combining fuzzy score + context bonuses; make weights configurable via config file.
     - ✅ `heuristic_score` использует новые сигналы: свежие/недавно правленные файлы получают boost, TODO-насыщенные блоки всплывают выше, а lint-heavy результаты штрафуются.
  3. **Personal context:** integrate plan/task files so active epics boost relevant files.
     - ✅ Navigator читает `.agents/current_plan.md` (или `NAVIGATOR_PLAN_PATH`) и активную ветку, вытягивает ключевые токены/изменённые пути и добавляет соответствующий boost в `heuristic_score` и literal hits.
  4. **Evaluation harness:** snapshot search sessions and assert ordering improvements (A/B tests offline).
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
- **Success criteria:** users can drill from >10 k hits to <20 hits in ≤3 commands without retyping the query.

### 5. Index Health & Regression Monitoring

- **Goal:** spot ingest lag, skipped files, or schema drift before users notice.
- **Milestones:**
  1. **Telemetry core:** collect ingest duration, skipped paths, literal fallback rates, error trends; persist summary snapshots.
  2. **Health panel:** extend `doctor` endpoint with risk levels + auto-remediation tips; CLI displays warnings upfront.
  3. **Guardrails:** add alerting hooks (log + optional webhook) when coverage drops or latency spikes.
  4. **Self-heal:** implement automatic rebuild/compaction when corruption or backlog detected, with progress reporting.
- **Success criteria:** zero “why is navigator stale?” incidents; health panel always green or explains mitigation.

### 6. Query Profiler & Performance Studio

- **Goal:** give immediate insight into where time is spent (cache hit, matcher, literal scan, HTTP).
- **Milestones:**
  1. **Profiling hooks:** record per-stage timings inside `run_search` and embed in `SearchStats`.
  2. **`/profiler` endpoint:** stream aggregated timings for last _N_ queries; support flamegraph export.
  3. **CLI view:** add `navigator profile last` command showing stage breakdown + bottleneck hints.
  4. **Optimization backlog:** use profiler data to prioritize hotspots (matching, glob filters, IO) and track regressions.
- **Success criteria:** performance regressions get detected within one commit; engineers can self-serve bottleneck analysis.

### 7. UX Accelerators & Guided Workflows

- **Goal:** remove friction by surfacing next actions and automating common navigation playbooks.
- **Milestones:**
  1. **Command palette flows:** prebuilt macros like “audit toolchain”, “trace feature flag” that chain multiple searches.
  2. **Context banners:** when a query returns hits across layers, highlight counts (“3 core, 12 tui, 4 docs”) with quick jump links.
  3. **Session memory:** persist recent queries, open hits, accepted facets; allow `repeat last` and `pin query`.
  4. **Focus mode:** collapse noisy metadata, emphasize the most relevant info depending on query intent (code vs docs vs config).
- **Success criteria:** average navigation flow shrinks to ≤2 commands; subjective “cognitive load” score drops in dogfooding surveys.

### Execution Guidance

- **Iteration cadence:** treat each epic as a 1–2 week slice with demoable value; keep this TODO updated after each milestone.
- **Quality bar:** every feature ships with planner/CLI documentation, unit + integration tests, and benchmarking notes.
- **Adoption:** once a milestone lands, dogfood it immediately inside Codex CLI and capture feedback under `.agents/context/`.
- **UX contract:** TUI остаётся пользовательским слоем: минимум когнитивного шума, только итоговые подсказки (hits, активные фильтры, atlas breadcrumbs). Все продвинутые возможности (`navigator` CLI, facet команды, atlas карты) предназначены для ИИ-оператора, чтобы ускорять его ориентирование.
