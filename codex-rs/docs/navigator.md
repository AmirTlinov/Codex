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
5. Text profile (`SearchProfile::Text`) выполняет триграммный отбор и векторизованный
   `memmem`-скан по блочному текстовому снапшоту: Navigator возвращает `match_count`
   и дифф-подсветки (спаны + diff-маркеры) прямо в `NavHit.context_snippet`, а CLI
   рисует мини-diff без отдельного `rg`.
6. Каждый hit получает `score_reasons`: список сигналов ранжирования (свежесть, owner,
   TODO-хлопки, плановые токены, штрафы lint). CLI показывает их отдельной строкой
   `reasons: fresh · owner=...`, что заменяет ручной анализ `heuristic_score`.
7. References are emitted as two buckets (`definitions`/`usages`) with short previews, so
   UIs can display relevant anchors without re-sorting large arrays.
8. The daemon automatically respawns after crashes or metadata corruption and wipes any
   broken cache on disk. No manual `rm -rf ~/.codex/navigator` steps should be taken.
9. `codex navigator doctor [--project-root <repo>]` теперь печатает компактную health-panel:
   для каждого workspace показываем состояние индекса, риск (green/yellow/red), список
   проблем с подсказками remediation, долю literal fallback, медианные времена текстовых
   сканов и последние прогонки ingest (kind, длительность, сколько файлов/скипов). При
   необходимости можно добавить `--json`, чтобы получить оригинальный отчёт / подобрать
   конкретные пути из coverage. Функциональный хендлер по‑прежнему вызывает Doctor после
   каждой RPC-ошибки и пересказывает резюме модели.
10. `codex navigator profile [--limit N] [--json]` выводит последние поисковые запросы с
   временными метриками (candidate load, matcher, hit assembly, references, facets,
   literal scan/fallback). Это тот же payload, что отдаёт `/v1/nav/profile`, поэтому можно
   либо читать JSON, либо просматривать компактную таблицу прямо в CLI.
11. Профайлер строит `stage hotspots`: агрегирует avg/p95/max по стадиям (matcher, glob filters,
   references, literal scan) за последние ~50 запросов и показывает блок `stage hotspots` прямо
   в CLI. `/v1/nav/profile` возвращает те же данные, поэтому внешние агенты могут мониторить
   регрессии без дополнительного парсинга логов.
12. Каждый ответ выводит "context" блок: слои (core/tui/infra…) и категории (docs/tests/deps)
    с количеством совпадений. Это помогает мгновенно понять распределение результатов и выбрать
    следующий шаг (`codex navigator facet --path ...`, `--tests` и т.п.) без ручного подсчёта.
13. История запросов превратилась в сессионную память: `codex navigator history` показывает
    query preview + топ‑хиты и помечает закреплённые запросы звёздочкой, а теперь ещё и печатает
    готовые команды (`stack/clear/repeat/suggestion[...]`) для каждого индекса, чтобы не держать их в голове.
    Нужен машинный вывод — включайте `--json`: CLI вернёт массив структур с фильтрами/чипами и командами,
    так что внешние инструменты могут подхватывать историю без парсинга текста. Список можно сузить
    `--contains <строка>`, переключиться на закреплённые записи через `--pinned`, а также выполнить действия
    напрямую: `--stack <n>` применяет стек, `--clear-stack <n>` снимает его, `--repeat <n>` повторяет запрос.
    Поэтому работать с историей можно не копируя команды вручную. Команда
    `codex navigator pin` позволяет закреплять (`--index`) и снимать (`--unpin`) записи
    либо вывести список (`--list`). `codex navigator repeat` переиспользует любой запрос из
    истории (`--index`) или из закреплённого списка (`--pinned`) и повторяет его с исходными
    профилями/опциями без ручного ввода.
    Каждая запись истории теперь содержит atlas hint: CLI печатает строку `atlas: core > planner (42 files | next: ...)`,
    JSON-режим возвращает полный `atlas_hint`, а Doctor/flows автоматически показывают последний
    фокус, чтобы сразу понимать, в каком домене работал агент.
14. Фасетные пресеты упрощают возврат к любимым стекам фильтров: `codex navigator facet --preset <name>`
    применяет сохранённый набор (`--save-preset <name>` фиксирует текущие фильтры, `--list-presets`
    показывает доступные, `--delete-preset <name>` удаляет). Пресеты живут рядом с history.json, так что
    все CLI/агенты могут делиться ими без ручного копирования.
15. Каждое `navigator search` теперь возвращает `facet_suggestions`: короткие подсказки вида
    `lang=rust`/`owner=core`, CLI пишет их в sideband/текстовом выводе и показывает готовые команды
    (`codex navigator facet --lang rust`), чтобы быстрее сужать выдачу без чтения Facet Summary.
    Любой suggestion можно применить напрямую: `codex navigator facet --suggestion 0` возьмёт первую
    подсказку из последнего поиска (учитывая undo/history), поэтому переход от "10k результатов" к
    целевому срезу занимает одну команду.
    Если результатов слишком много (hits ≥ лимита или >450 кандидатов), Navigator автоматически
    применяет верхнюю подсказку и повторяет поиск — в консоль выводится `[navigator] auto facet: ...`
    и новый ответ. Теперь цикл продолжается, пока выдача не перестанет быть перегруженной или пока не
    исчерпан лимит из двух автоматических шагов (по умолчанию): CLI/handler переиспользуют те же подсказки,
    но пропускают уже применённые фильтры, поэтому цепочка `lang=rust → tests → owner=core` выполняется
    без участия оператора. Поведение можно отключить переменной `NAVIGATOR_AUTO_FACET=0`, а ручные фильтры
    (активные chips) всегда блокируют авто‑стек, чтобы не перезаписывать пользовательский выбор.
    История хранит сами подсказки, так что `codex navigator facet --suggestion 1` можно вызывать даже
    через несколько шагов, не вспоминая команду; индекс `[n]` соответствует `history[n]`.
    Чтобы не держать любимые комбинации фильтров в голове, используйте `--history-stack <n>` — команда
    подтянет весь стек фильтров из history[n], очистит предыдущие и повторит поиск. Обратная операция —
    `--remove-history-stack <n>`: она добавит remove-* операции для всех языков/категорий/owners/глобов из
    записи и снимет `recent`. Оба варианта пишут хинты (`applied history[0] filters`, `removed history[0] filters`),
    так что понятно, какие комбинации сейчас включены. Те же действия доступны из инструментальных payload'ов:
    `{"action":"history","mode":"stack","index":2,"pinned":false}` или короткое `history stack 2 --pinned`
    переразводят фильтры напрямую через Navigator handler, а `mode="repeat"` повторяет сохранённый запрос без
    CLI. Это позволяет ИИ-агенту переиспользовать прошлые поиски, не прибегая к `rg`/IDE. Если нужно сначала
    посмотреть, что именно лежит в истории, используйте `{"action":"history_list","limit":5,"contains":"planner"}`
    (или короткое `history list --limit 5 --contains planner`). Ответ вернёт массив записей (query_id, возраст,
    фильтры, хиты, suggestions, флаг repeat), так что можно программно выбрать нужный индекс и тут же вызвать
    `history stack`/`history repeat`. Чтобы применить сохранённую facet suggestion без CLI, вызывайте
    `{"action":"history","mode":"suggestion","index":0,"suggestion":1}` или короткое
    `history suggestion --index 0 --suggestion 1`: Navigator наследует фильтры предыдущего поиска, добавляет
    выбранную подсказку и сразу запускает refined запрос.
16. `--focus` управляет уровнем шума в текстовом выводе: `auto` (по умолчанию) подсвечивает
    docs/tests/deps, когда они доминируют; режимы `code/docs/tests/deps/all` можно задавать
    явно. Отфильтрованные хиты не теряются — CLI показывает счётчик suppressed и напоминает
    про `--focus all`, а истории/pin сохраняют выбранный режим, так что повторные вызовы
    восстанавливают то же представление.
17. Появились готовые навигационные сценарии `codex navigator flow <name>`: например,
    `audit-toolchain` проходит по rust-toolchain манифестам и документации, а `trace-feature-flag`
    за один вызов ищет определения и использования флага (`--input flag_name`). Флоу можно
    прогнать в dry-run режиме (`--dry-run`), чтобы увидеть последовательность шагов, либо
    комбинировать с `--focus/--with-refs` для детальной диагностики.
18. Для оценки ранжирования есть `codex navigator eval suite.json`: описываете кейсы в JSON
    (`[ { "name": ..., "query": ..., "expect": [{"pattern": "path", "max_rank": 5}] } ]`),
    и команда прогоняет реальные поиски, проверяя, что целевые файлы попадают в нужный ранг.
    Опционально можно добавить `--snapshot-dir eval_out` чтобы сохранить фактические выдачи.
16. Atlas summary теперь показывает churn score и топ владельцев по каждому узлу, так что
    `codex navigator atlas --summary core` сразу подсветит ответственность и горячие участки.

## Operational Notes

- When integrating with the CLI/TUI, display the streamed diagnostics block verbatim; it is
  the single source of truth for freshness, coverage, and rebuild progress.
- Active filter chips в CLI нумеруются и могут удаляться адресно:
  `codex navigator facet --remove-chip 0` снимет `[0:lang=rust]`, не требуя помнить
  соответствующий `--remove-lang`.
- Planner автоматически выбирает `text` профиль для regex/path/многословных запросов и добавляет
  в hints причину автопереключения; короткие символические запросы остаются на символическом поиске.
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
  path filter and the `files` profile, so вы сразу попадаете в нужный слой; то же самое
  можно сделать из CLI через `codex navigator atlas --jump path/to/domain`, не прибегая к
  freeform-командам.
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
- Coverage diagnostics теперь транслируются напрямую в ранжирование: если файл висит в pending/error
  или попал в `coverage.skipped` (non_utf8/oversize/missing и т.д.), Navigator добавляет
  соответствующий `score_reason` ("coverage pending", "coverage skipped (missing)") и выдаёт
  дополнительный boost, чтобы проблемные области всплывали первыми без ручного просмотра doctor.
- Guardrail-алерты: если риск ≥ yellow или запрос занял дольше `NAVIGATOR_GUARDRAIL_LATENCY_MS`
  (по умолчанию 1500 мс), Navigator логирует предупреждение (`navigator::guardrail`) и, при наличии
  `NAVIGATOR_GUARDRAIL_WEBHOOK`, шлёт JSON-пэйлоад с деталями (risk/issues или статистика запроса).
  Во избежание шума события дросселируются таймером `NAVIGATOR_GUARDRAIL_COOLDOWN_SECS` (300 с по
  умолчанию) и отдельным 60‑секундным cooldown для slow-query. Если в trend_summary появляются новые
  hotspots, guardrail также фиксирует `hotspot spikes +N` в сообщении/вебхуке, так что рост TODO/lint
  шума виден без запуска `insights`. Это закрывает пункт 5.3 roadmap и позволяет получать внешние
  оповещения без опроса Doctor.
- Self-heal: если индекс упал в состояние Failed, либо в coverage накапливается >`NAVIGATOR_SELF_HEAL_ERROR_LIMIT`
  ошибок или >`NAVIGATOR_SELF_HEAL_PENDING_LIMIT` ожидающих файлов, координатор автоматически запускает
  `rebuild_all` (не чаще, чем раз в `NAVIGATOR_SELF_HEAL_COOLDOWN_SECS`, 15 мин по умолчанию). Это
  значит, что большинство “залипших” индексов восстанавливаются без ручного `codex navigator daemon`.
  Переменная `NAVIGATOR_SELF_HEAL_ENABLED=0/1` позволяет полностью отключить механику, а лимиты можно
  подкрутить теми же env переменными.

### Query Profiler

- Каждый поиск теперь записывает покомпонентные тайминги (`stats.stages`): candidate load,
  matcher, hit assembly, references, facets, literal scan/fallback. Эти данные доступны как в
  потоковом `stats`, так и задним числом через профайлер.
- `POST /v1/nav/profile` возвращает последние _N_ сэмплов с query_id, урезанным текстом запроса,
  таймингами и признаками (cache hit, literal fallback, text_mode). Простая CLI-обёртка —
  `codex navigator profile --limit 8` — печатает список и позволяет быстро понять, где тратится
  время. Добавьте `--json`, чтобы сохранить payload как артефакт.
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

### Workspace Insights

- `codex navigator insights` печатает горячие зоны по трём секциям: TODO/attention hotspots,
  lint-риски и ownership gaps (файлы без CODEOWNERS с высоким churn/TODO). По умолчанию команда
  выводит компактный текст (секция → топ N путей с причинами), `--json` возвращает тот же payload,
  что отдаёт `/v1/nav/insights`.
- Флаги `--limit` и `--kind attention|lint|ownership` управляют объёмом и секциями. Фильтры можно
  комбинировать; CLI автоматически удаляет дубликаты и приводит limit к ≥1.
- `--apply N` (N — индекс в печатаемом списке, 1‑based) запускает поиск, сфокусированный на выбранном
  hotspot’е. CLI выставляет `path_globs` на выбранный путь, включает профиль `files` и добавляет hint
  `insights jump: …`, чтобы история могла воспроизвести действие.
- Инструментальный вызов: `{"action":"insights","limit":5,"kinds":["lint_risks"]}`.
  Ответ содержит `generated_at`, массив секций (`kind`, `title`, `summary`) и хиты с метаданными
  (`owners`, `categories`, `line_count`, `score`, `reasons`). Handler возвращает JSON напрямую, так
  что ИИ-агент может читать/транслировать эти данные без участия CLI.
- Комбинируйте insights с `atlas summary` или `facet` для drill-down: например, берём первые lint
  риски и сразу выполняем `codex navigator facet --path <path>` либо `nav <query>` с активным
  owner/категорией на основании выданных сигналов.
- Planner и инструментальный handler автоматически добавляют hint `hotspot: …`, когда поиск запускается
  “с нуля” (без query/filters). Это позволяет агенту мгновенно увидеть самый шумный файл и при желании
  перейти в него через `history suggestion` или `insights --apply` без ручного поиска.
- Каждая выдача `insights` формирует `trend_summary`: сравнение с предыдущим снимком (новые/исчезнувшие
  hotspots). Этот же summary прикладывается к `navigator doctor`/streamed diagnostics, поэтому секция
  health panel теперь показывает «hotspots @ <timestamp>» с количеством новых/закрытых участков.
- Команды `codex navigator flow ...` теперь перед запуском шагов показывают итоги trend_summary, чтобы
  сразу подсветить, где выросли TODO/lint/ownership пики и сто́ит ли менять план до выполнения flow.

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
- Literal text snapshots now stream through a bounded ingest queue: `IndexBuilder` enqueues raw UTF-8 blocks, worker tasks compress them in the background, and fingerprints prevent stale payloads from overwriting fresh files. Symbol updates land immediately while literal storage catches up, so Navigator never blocks on `FileText::from_content`.
- Manual rebuilds (`/index-code`, `codex navigator nav --wait`) still work, but they simply reset the incremental queue.
- You should never need to delete caches when schema changes—self-heal will wipe the corrupted snapshot, rebuild, and keep serving requests while reporting the temporary state via diagnostics.
