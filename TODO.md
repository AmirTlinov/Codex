# SOW BackgroundShell

## Objective
Enable Codex to run every AI-issued shell command asynchronously without freezing the TUI, enforce a 10-second foreground budget, and provide a first-class background process manager (Ctrl+R to promote, Ctrl+B to inspect/manage) backed by a 10 MB ring buffer with one-hour retention.

## Deliverables & Steps

### 1. Protocol & CLI contract
- [x] Extend `ShellToolCallParams` (`codex-rs/protocol/src/models.rs`) with `run_in_background`, `description`, легковесные флаги для управления/просмотра логов (`manage_process`, `tail_lines`), а также `bookmark` поле для короткого алиаса.
- [x] Add `Op::PromoteShell` and `Op::PollBackgroundShell` plus the `EventMsg::ShellPromoted` payload (mirroring repo1) to `codex-rs/protocol/src/protocol.rs`; wire serialization + TS bindings.
- [x] Document the new ops/events in `docs/advanced.md` + CLI README (shortcut table, behavior summary, guardrails).
- [x] Expose new RPC `Op::BackgroundShellSummary` returning компактный список (shell_id, bookmark, команда, статус, tail) для Codex и TUI.

### 2. Core background execution manager
- [x] Introduce `codex-rs/core/src/background_shell.rs` implementing START/READ/KILL per spec:
  - `Map<shell_id, Entry>` storing `UnifiedExec` session id, ring buffer (10 MB cap) with read offsets, status, exit code, timestamps.
  - START_SHELL: spawn via `ShellBackgroundRuntime`, enforce max 10 concurrent sessions, reject forbidden commands, detach from foreground and notify session.
  - READ_OUTPUT: resume `unified_exec` polling, append to ring buffer, honor optional regex filter, advance per-reader offset, expose status/exit.
  - KILL_SHELL: SIGTERM→SIGKILL ladder (5 s), flush output, mark exit, drop resources (auto cleanup after 1 h of terminal state).
- [x] Persist stderr from spawn failures + process errors into the buffer so READ_OUTPUT surfaces diagnostics.
- [ ] Unit-test manager semantics (parallel start, buffer truncation, kill path, regex filters) under `codex-rs/core/tests/suite/background_shell.rs`.

### 3. Foreground shell registry & auto-promotion
- [x] Port `foreground_shell.rs` from repo1, but set `AUTO_PROMOTE_THRESHOLD = 10s`. Track running shells by call_id with `watch` channel to service Ctrl+R promotions.
- [x] On every exec poll:
  - Append stdout/stderr to ring buffer for eventual background adoption.
  - If 10 seconds elapsed and command still running, automatically promote to background (reuse new manager) and emit `EventMsg::ShellPromoted`.
- [x] Expose APIs on `Session` to (a) request promotion (`promote_shell(call_id, description)`) and (b) read/poll backgrounds for the TUI.

### 4. Tool plumbing, execution limits & AI ergonomics
- [x] Update `ShellHandler` to:
  - Respect `run_in_background` flag by routing to background manager immediately.
  - Register every foreground exec with the registry so Ctrl+R/auto promotion can function.
  - Enforce the 10-second foreground window: launch a timer and if the command exceeds it, auto-promote (even if `timeout_ms` is larger).
- [x] Ensure background shells share the same environment + approval flow as foreground commands (including sandbox rewinds, risk metadata).
- [x] Добавить упрощённые ручки (`BackgroundShellHandler` + `shell_summary`, `shell_log`, `shell_kill`) чтобы Codex мог перечислять процессы, читать логи и завершать задачи одним вызовом.
- [x] Формировать ответы в компактном формате (краткое состояние + tail) чтобы не засорять контекст модели.

### 5. Session services & unified exec wiring
- [x] Extend `SessionServices` with `background_shell`, `foreground_shell`, and plumb through constructors/tests.
- [x] Update `codex.rs` turn loop to handle new `Op`s (PromoteShell/PollBackgroundShell) and to emit `ShellPromoted`/`BackgroundEvent` notifications.
- [x] Make `Session::notify_background_event` parseable via `parse_background_event_message` for TUI consumption; ensure notifications fire on start, terminate, kill, and auto-promotion.

- [x] Привязать `Ctrl+R` к `Op::PromoteShell` для последней активной команды и давать обратную связь через ненавязчивые индикаторы (без шума в чате).
- [x] Обновить composer/footer/help подсказки, сохраняя минимализм и когнитивную простоту.

### 7. Safeguards & limits
- [x] Enforce max 10 concurrent background shells (manager-level guard with clear error message in TUI + agent log).
- [ ] Guard commands for `sudo`, `su`, etc., when forced into background (align with command safety rules).
- [x] Ensure child process groups receive SIGTERM then SIGKILL after 5 s, and double-check we reap zombies even when the TUI disconnects.

### 8. Testing & CI
- [ ] Rust unit tests for protocol serde, background manager, promotion flows, and `Session::promote_shell`.
- [ ] Integration tests simulating Ctrl+R/Ctrl+B via TUI event harness (ratatui snapshot) to confirm overlays update.
- [ ] Update `cargo test` targets (`codex-core`, `codex-tui`) and add `just` recipes if needed.
- [x] Cover TUI background indicator/toast/parse logic with unit tests (`background_process.rs`, `chatwidget/tests.rs`).

### 9. Documentation & context hygiene
- [x] Refresh `docs/advanced.md` with the new workflow (Ctrl+R promotion, Ctrl+B manager, auto 10 s limit) and call out the 10 MB buffer + 1 h retention.
- [x] Add `.agents/context/<date>_background_shell_context.jsonl` summary capturing design decisions + references (keep appending per milestone).
- [x] Ensure AGENTS/README tips mention the new shortcuts so users discover them quickly (see README background process section).
