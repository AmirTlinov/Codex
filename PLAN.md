# План обновления фоновой оболочки (main2)

## 1. Цели и требования
- Все команды и процессы запускает **только ИИ-агент**. Пользователь не инициирует shell-команды напрямую.
- Агент может держать foreground-команду до 60 с; по истечении лимита core автоматически переводит процесс в фон. Пользователь в любой момент foreground-исполнения может нажать `Ctrl+R`, чтобы немедленно отправить процесс в фон (агент фиксирует событие в чате).
- Агент обязан знать, **кем завершён процесс** (агент, пользователь через TUI kill, автоматический фейл) и реагировать на эти события.
- Пользователь видит в чате одну аккуратную карточку на каждый процесс (без дубликатов по `shell_id`); при нескольких процессах карточки располагаются подряд, а вся детализация статусов идёт в рамках соответствующей карточки. Пользователь также может открыть панель активных процессов (`[Shell]` в футере) стрелками.
- Набор инструментов минимален: `shell_run`, `shell_summary`, `shell_log`, `shell_kill`, `shell_resume` (все вызываются агентом).
- Весь пользовательский интерфейс (карточки, панель `[Shell]`, подсказки клавиш) отображается **на английском языке**.

### Clarifications (TUI expectations)
- **One card per shell_id, multiple shells allowed.** The chat stream always reuses an existing card for the same `shell_id`, but creates additional cards when several commands run concurrently so every process retains its own status/log view.
- **Foreground window = 60 seconds.** Foreground executions stay visible and interactive for the full 60‑second budget; only after that window expires does core auto-promote them to the background.
- **Manual background via `Ctrl+R`.** Whenever a foreground shell is running, the TUI surfaces the `Ctrl+R → background` hint, and pressing the shortcut immediately issues a `BackgroundRequest` event before the 60‑second window elapses.

## 2. Архитектура
```
┌────────────┐     ┌──────────────────────┐     ┌────────────────────┐
│ Foreground │     │ Background Shell Mgr │     │ Presentation Layer │
│ Shell API  │ →→→ │  (core + protocol)   │ →→→ │  (CLI agent + TUI)  │
└────────────┘     └──────────────────────┘     └────────────────────┘
```
1. **Core / Protocol:** управляет реестром процессов и шлёт события с обязательными полями `{call_id, shell_id, start_mode, ended_by, exit_code}`. `ended_by ∈ {agent, user, system}` позволяет агенту понимать источник завершения.
2. **CLI-агент:** единственный инициатор `shell_run`. Он подписывается на события, чтобы знать про фейлы, пользовательские kill и автопромоуты. Историю не парсит — работает только с метаданными и инструментами.
3. **TUI:** чат отображает текст пользователя/агента + карточки процессов (по `shell_id`). Панель `[Shell]` показывает текущие и завершённые процессы, позволяет подсветить и отправить команду (kill/log/resume) от имени агента.

## 3. Изменения в core/protocol
1. **Единый реестр процессов**
   - Таблица по `shell_id`: статус, start_mode, timestamps, `ended_by`, хвосты логов, курсоры для log/summary.
   - `shell_run` всегда запускается неблокирующе; foreground-бюджет 60 с (после чего core инициирует авто-promote). Core также реагирует на ручные запросы `background_request` (срабатывают после `Ctrl+R`).
2. **События**
   - `BackgroundEventEvent` содержит metadata: shell_id, call_id, kind (start/terminate), `ended_by`.
   - `ShellControlEvent` для действий пользователя в TUI/чате (kill через панель, `Ctrl+R` → background) → core отмечает источник (`ended_by=user` для kill, `start_mode=background` для background_request), агент получает уведомление.
3. **Инструменты**
   - `shell_summary` отдаёт только Running по умолчанию; `--completed` / `--failed` добавляют остальные.
   - `shell_log` постраничный: `mode=tail|body|diagnostic`, cursor + лимит 120 строк.
   - `shell_kill`, `shell_resume` возвращают `{result, reason, ended_by}`.

## 4. CLI-агент
1. **Запуск**
   - Агент ставит `run_in_background=true`, если оценка длительности >60 с; короткие (≤60 с) идут в foreground и могут быть вручную перемещены пользователем через `Ctrl+R`. Агент — единственный владелец процессов.
2. **Мониторинг**
   - Агент держит таймауты: если foreground команда живёт >60 с, вызывает `shell_summary`/`shell_log` и описывает автоперевод в системном сообщении. При `background_request` (пользователь нажал `Ctrl+R`) агент немедленно подтверждает перевод и обновляет карточку.
   - Каждое событие `ended_by=user` (например, пользователь нажал Kill в панели) превращается в системную реплику «Пользователь завершил процесс shell-X» и корректирует внутреннее состояние.
3. **Инструменты**
   - Агент никогда не запрашивает полный лог — только режимы tail/diagnostic.
   - После `shell_kill` с ответом `AlreadyFinished*` агент обновляет summary и удаляет ссылку на процесс.

## 5. TUI и UX
1. **Карточка процесса в чате (`ShellCard`)**
   - Заголовок: `● Shell (friendly label)`; цвет точки = статус (зелёный running, серый completed, красный failed).
   - Поля: `Status`, `shell_id`, `Command`, `Reason`, `Last log` (до 2 строк, dim-текст). При смене состояния карточка заменяется на месте без добавления новых строк. Активная foreground-карточка дополнительно показывает подсказку `Ctrl+R → background`.
2. **Полноэкранная панель `[Shell]`**
   - Активация: `[Shell ▾]` или стрелка вниз → панель занимает весь терминал, чат скрывается. Верхняя строка — вкладки `RUNNING`, `COMPLETED`, `FAILED`; переключение ←/→ (или Tab/Shift+Tab).
   - Вся остальная площадь заполнена таблицей: строки вида `> shell-4  sleep 120  осталось 01:43  [автоперевод]`. Активная строка подсвечена обратной заливкой и префиксом `>`; при большом списке используется оконный скролл вокруг выделения.
   - Управление: `↑/↓` — выбор; `Enter` открывает отдельный экран деталей (скролл стрелками, `c` — копирование лога; `Esc`/`Enter`/`q` — назад); `k` — kill (RUNNING); `d` — diagnostic log (окно на 120 строк); `r` — resume (COMPLETED/FAILED); `Ctrl+R` — отправить выделенный foreground-процесс в фон; `Esc`/`q` — выход из режима. Панель не запускает процессы напрямую — все действия транслируются агенту.
3. **Уведомления**
   - Автопромоуты, пользовательские kill и ошибки отображаются в чате приглушённым текстом с указанием `ended_by`. В панели такие процессы получают бейдж «new» до просмотра.

4. **Foreground shortcut (`Ctrl+R`)**
   - За пределами панели подсказка `Ctrl+R → background` отображается в футере, когда есть активная foreground-команда.
   - Нажатие `Ctrl+R` инициирует `ShellControlEvent(background_request)`: core переводит процесс в фон и помечает источник, агент публикует системную реплику и обновляет карточку/панель.

## 6. План миграции
1. **Фаза 1 – core/protocol**
   - Реестр, обязательные метаданные, `ended_by`. Включение через фичфлаг `background_shell_v2`.
   - Обновление интеграционных тестов на core (prompt caching, approvals, seatbelt) под новый формат событий.
2. **Фаза 2 – CLI-агент**
   - Переключение на новый набор инструментов, удаление парсинга истории, добавление цикла опроса.
   - Проверка на сценариях: sleep 120, многопоточность, ошибки `kill`.
3. **Фаза 3 – TUI**
   - Введение `ShellCard` и панели; отказ от дублирующих Exec-ячейки, удаление подсказок “Ctrl+B”.
   - Обновление снапшотов, ручное QA.
4. **Фаза 4 – Cleanup**
   - Удаление legacy toast/hint, документирование в `docs/advanced.md`, включение фичфлага по умолчанию.

## 7. Критерии успеха и метрики
- Агент никогда не блокируется >60 с на любой команде (метрика: среднее время между `shell_run` и возвращением к диалогу).
- Все события `ended_by` корректно отражаются в TUI и логах агента.
- TUI чат содержит ровно одну карточку на процесс; панель `[Shell]` показывает актуальный список без задержек.
- Тесты `cargo test -p codex-tui`, `cargo test -p codex-core`, `cargo test --all-features` зелёные.
- В логах нет массовых запросов `shell_log` >120 строк; режим diagnostic используется только по явному запросу.

## 8. ASCII-наброски интерфейсов

**ShellCard в чате**
```
● Shell (sleep 120)
  └ Status: running · shell-3
    Command: sleep 120
    Reason: auto background (60s budget exceeded)
    Last log:
        (no output)
```
Status colors: `●` green (running), gray (completed), red (failed). On failure:
```
● Failed (sleep 120)
  └ Status: failed (code 1) · shell-3
    Reason: killed by user (Shell panel)
    Last log:
        timeout occurred
```
На активной foreground-карточке выводится подсказка `Ctrl+R → background`.

**Запрос пользователя через `Ctrl+R`**
```
● Shell (npm run build)
  └ Status: running · shell-5
    Command: npm run build
    Reason: moved to background by user (Ctrl+R)
    Last log:
        compiling...
```

**Полноэкранная панель `[Shell]`**
```
[RUNNING] - completed - failed
> shell-4  sleep 120            ETA 01:43   [auto background · 60s]
  shell-2  python countdown     ETA 00:20   [stdout]
  shell-7  tar backup           progress 35% [agent]

←/→ tabs · ↑/↓ select · Enter details · k kill · d diagnostic · r resume · Ctrl+R background · q/Esc exit
```
Enter открывает экран процесса (status/timing/full log; скролл стрелками, `c` копирует лог). `d` показывает диагностическое окно на весь экран (120 строк tail); `q/Esc` → назад в панель, ещё раз `Esc` → чат.
