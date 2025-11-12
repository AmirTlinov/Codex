# Navigator Overview

The canonical usage guide lives in `navigator/navigator_tool_instructions.md` and is
embedded into the tool handler via `include_str!`. Read that file for quick commands,
JSON schemas, profiles, and parsing rules. This page captures the operational pieces
that engineers most often need when integrating Navigator into the CLI, TUI, or other
agents.

## Launching the Daemon

- The CLI/tool handler automatically spawns the daemon (via `codex navigator-daemon`) the
  first time a request arrives. No manual setup is required.
- One daemon manages multiple project roots concurrently through the `WorkspaceRegistry`.
  Passing `--project-root` (or setting `project_root` in payloads) transparently switches
  to the right cached workspace; least-recently-used ones are evicted automatically.
- File-system watchers feed incremental ingest queues, so the index is always up to date.
  If the tree is still rebuilding, diagnostics will report `index_state=building` plus the
  specific coverage gaps.

## Protocol Compatibility

- All requests must set `schema_version: 3`; the daemon self-heals (drops stale snapshots,
  rebuilds, and keeps serving streaming diagnostics) whenever a mismatch is detected.
- `hints` and `stats.autocorrections` are optional and may be absent.

## Observability & Self-Heal

1. Every search streams diagnostics immediately, so the AI sees `index_state`,
   `freshness_secs`, and coverage buckets without waiting for the final response.
2. `fallback_hits` automatically cover pending files, so there is no need to fall back to
   `rg` or other tools. Each entry explains why the file is not indexed yet (pending,
   oversize, non UTF-8, ignored, etc.).
3. Literal fallback kicks in when no symbols match: the daemon searches file contents for
   the raw query text (token-aware) and produces synthesized hits prefixed with
   `literal::`, with `stats.literal_fallback` set to `true`. Every workspace maintains a
   token index *and* a trigram map, so literal fallback always points at the exact files
   containing the string—even in very large trees—without guessing or scanning random
   subsets of the repository.
4. Literal metrics quantify that fallback work: `stats.literal_missing_trigrams`
   surfaces any query grams not yet indexed, `stats.literal_pending_paths` lists the
   pending files that were scanned, and `stats.literal_scanned_files` / `_bytes`
   capture exactly how much literal load the daemon performed. Agents can now tell
   whether “no hits” means “still ingesting” without running `rg`.
5. References are emitted as two buckets (`definitions`/`usages`) with short previews, so
   UIs can display relevant anchors without re-sorting large arrays.
6. The daemon automatically respawns after crashes or metadata corruption and wipes any
   broken cache on disk. No manual `rm -rf ~/.codex/navigator` steps should be taken.
7. `codex navigator doctor [--project-root <repo>]` теперь печатает компактную health-panel:
   для каждого workspace показываем состояние индекса, риск (green/yellow/red), список
   проблем с подсказками remediation, долю literal fallback, медианные времена текстовых
   сканов и последние прогонки ingest (kind, длительность, сколько файлов/скипов). При
   необходимости можно добавить `--json`, чтобы получить оригинальный отчёт / подобрать
   конкретные пути из coverage. Функциональный хендлер по‑прежнему вызывает Doctor после
   каждой RPC-ошибки и пересказывает резюме модели.

## Operational Notes

- When integrating with the CLI/TUI, display the streamed diagnostics block verbatim; it is
  the single source of truth for freshness, coverage, and rebuild progress.
- `codex nav --format text` prints a compact summary (diagnostics, stats, top hits with ids)
  without the large JSON payload, which keeps agent transcripts lightweight when you only
  need the highlights.
- If a request fails, call the Doctor endpoint and surface the structured summary—this is
  already wired in the function tool handler.
- Avoid documenting or implementing ad-hoc cache flush commands; the daemon owns every
  lifecycle transition (spawn, rebuild, eviction, healing).
- For health checks use `codex nav --diagnostics-only <query?>` to suppress hits/JSON and
  simply stream the diagnostics heartbeat; use `--format ndjson` when you need the raw
  daemon events (diagnostics/top_hits/final) without reformatting.
- To focus on specific references, combine `--with-refs` with `--refs-mode definitions` or
  `--refs-mode usages`; the daemon already buckets them separately so no client-side sort is
  needed.
- The TUI listens to the same NDJSON channel as the CLI/tool handler, so the "Exploring"
  exec cell now shows streaming diagnostics and the first batch of hits immediately.
  There's no need for bespoke progress indicators.

### Atlas summaries

- `codex navigator atlas [TARGET]` still renders the full tree, but you can now add
  `--summary` (or use the quick command `atlas summary core`) to collapse the output into a
  breadcrumb + metrics block for the chosen node. Each summary shows file/symbol/doc/test/
  dep counts, recent files, and the new LOC totals so you can compare crates or layers at a
  glance.
- The daemon exposes the same capability via the Navigator tool: send `atlas summary core`
  in a freeform block (or JSON with `{"action":"atlas_summary","target":"core"}`) and
  the function call returns the structured summary payload.
- Quick command `atlas jump path/to/domain` converts into a scoped search that applies the
  path filter and the `files` profile, so you can pivot from the atlas map directly into
  focused symbol/text results without rewriting filters.
- Every `navigator search` response now prints the Atlas focus inline (breadcrumbs + top
  siblings), so you always know где находитесь в дереве без отдельного вызова
  `navigator atlas`.
- `codex navigator facet --from <query-id> --lang rust --tests` позволяет добавлять
  “стековые” фильтры без повторного набора запроса. Команда шлёт refine-запрос к тому же
  query id, так что фильтры применяются к уже отсортированному списку кандидатов.
- Тот же `facet` теперь умеет *снимать* ограничения без ручного `clear`: добавьте флаги
  `--remove-lang <lang>`, `--no-tests|--no-docs|--no-deps` или `--no-recent`, и Navigator
  сам вернётся к ближайшему предку цепочки refine, пересчитает фильтры и обновит hits —
  без shelling‑out в `rg` и без знания исходного запроса.
- Владелец можно задавать через `--owner <handle>` (CLI) или `owner=@team` в freeform: мы
  читаем CODEOWNERS, нормализуем `@handles` и фильтруем/ранжируем файлы конкретных команд;
  `facet --owner foo` и `facet --remove-owner bar` позволяют стековать/сбрасывать ownership.
- Когда owner-фильтр активен, ранжирование получает дополнительный буст для символов этой
  команды, поэтому топ-хиты всегда соответствуют нужным владельцам (не нужно вручную
  перебирать результаты).
- Контекстное ранжирование уже учитывает git churn: мы сжимаем `git log --since=30.days` в
  per-file score, нормализуем (clamp) и прибавляем к эвристике, поэтому горячие файлы
  поднимаются выше даже при слабых fuzzy-совпадениях; attention (TODO/FIXME) остаётся
  вторым усилителем «проблемных» мест.
- Дополнительно Navigator штрафует файлы, где много `#[allow(...)]`: хиты с большим
  числом подавлений получают меньший вес и всплывают отдельной фасетой `lint`, так что
  можно быстро увидеть “чистые” vs “замьюченные” области без чтения линтовых логов.
- The atlas is rebuilt every time the index snapshot changes, so both the tree view and the
  summary metrics stay in sync with coverage/recency signals surfaced in search results.

### Contextual Signals & Facets

- Каждая индексация теперь вытягивает `git log --since=120.days`, вычисляет `freshness_days`
  и сохраняет его в снапшоте. Свежие/недавно отредактированные символы получают заметный boost
  в эвристике ранжирования, поэтому топ-хиты сразу отражают текущий контекст задачи.
- TODO/FIXME маркеры нормализуются в `attention_density` (по KLOC) и дают дополнительный boost
  “горячим” файлам, тогда как обилие `#[allow(...)]` попадает в `lint_density` и штрафует результаты.
- Персональные сигналы: Navigator читает активную ветку (`git rev-parse --abbrev-ref HEAD`) и
  список задач из файла плана, чтобы продвигать релевантные пути. Создайте
  `.agents/current_plan.md` (или задайте абсолютный путь через `NAVIGATOR_PLAN_PATH`) с краткими
  bullet-пунктами — ключевые слова из этого файла (а также из названия ветки и `git diff` против
  origin/main) получают дополнительный вес в `heuristic_score`, а literal-hits из тех же путей
  всплывают выше. Файл опционален: если плана нет, ранжирование просто игнорирует этот сигнал.
- Health‑телеметрия хранится в `~/.codex/navigator/<hash>/health.bin` и пополняется после каждой
  индексации и поискового запроса: фиксируем длительность ingest, количество обработанных/пропущенных
  файлов по причинам, долю literal fallback, объём просканированных файлов и медианный текстовый
  скан. Эти данные попадают в streamed diagnostics и `navigator doctor`, поэтому риск/рекомендации
  переживают перезапуски демона.
- `stats.facets` теперь содержит новые бакеты `freshness` ("0-1d", "2-3d", "4-7d", "8-30d",
  "31-90d", "old") и `attention` ("calm", "low", "medium", "hot"). CLI выводит их в блоке
  `facets:` сразу после languages/categories, так что можно моментально понять, насколько свежи и
  “шумны” найденные места без дополнительного запроса. JSON ответы содержат те же поля для любых
  агентов/интеграций.
- CLI запоминает последний успешный `navigator search` в
  `<CODEX_HOME>/navigator/<hash>/queries/history.json`. Команда `codex navigator facet` теперь по
  умолчанию использует этот query_id (берёт запись `[0]`), поэтому достаточно `codex navigator facet
  --lang rust`; явный `--from <id>` остался для редких случаев. Для повторного использования более
  старых запросов используйте `--history-index N` или простое `--undo` (эквивалент записи `[1]`).
  Просмотреть последние идентификаторы и активные фильтры можно через `codex navigator history`.

## Streaming Diagnostics

- `/v1/nav/search` now streams NDJSON events. Clients should expect this sequence:
  1. `diagnostics` event immediately reports `index_state`, `freshness_secs`, and coverage counts.
  2. `top_hits` event yields the first five ranked hits as soon as scoring finishes.
  3. `final` event carries the full `SearchResponse`, which itself includes the latest `diagnostics` block and any `fallback_hits` produced by the live search engine.
- `fallback_hits` explain results sourced from pending files (e.g., editor just created them). Each entry carries a `CoverageReason` so the model knows *why* the symbol is not indexed yet (oversize, binary, ignored, etc.).
- CLIs and the TUI render these streamed chunks live; you should never have to poll a
  spinner to know whether Navigator is alive.

## Autonomous Indexing & Coverage

- The daemon watches the workspace and performs incremental ingest instead of rebuilding from scratch. Coverage milestones (pending/skipped/errors) are tracked per file and exposed via diagnostics/Doctor.
- Manual rebuilds (`/index-code`, `codex navigator nav --wait`) still work, but they simply reset the incremental queue.
- You should never need to delete caches when schema changes—self-heal will wipe the corrupted snapshot, rebuild, and keep serving requests while reporting the temporary state via diagnostics.
