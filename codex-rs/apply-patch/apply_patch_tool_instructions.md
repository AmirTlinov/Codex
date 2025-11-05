## `apply_patch`

Use the `apply_patch` shell command to edit files.
This binary exists exclusively for Codex CLI’s GPT-5 agent – it is not a human-facing tool, REST API, or general-purpose service.
Your patch language is a stripped‑down, file‑oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high‑level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of six headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename).

Symbol-aware edits let you work relative to AST declarations instead of raw text:

*** Insert Before Symbol: <path::SymbolPath> - insert the provided `+` lines immediately before the symbol definition.
*** Insert After Symbol: <path::SymbolPath> - insert the lines right after the symbol (after its body when available).
*** Replace Symbol Body: <path::SymbolPath> - replace the body/content of the symbol with the provided lines.

May be immediately followed by *** Move to: <new path> if you want to rename the file.
Then one or more “hunks”, each introduced by @@ (optionally followed by a hunk header).
Within a hunk each line starts with:

For instructions on [context_before] and [context_after]:
- By default, show 3 lines of code immediately above and 3 lines immediately below each change. If a change is within 3 lines of a previous change, do NOT duplicate the first change’s [context_after] lines in the second change’s [context_before] lines.
- If 3 lines of context is insufficient to uniquely identify the snippet of code within the file, use the @@ operator to indicate the class or function to which the snippet belongs. For instance, we might have:
@@ class BaseClass
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

- If a code block is repeated so many times in a class or function such that even a single `@@` statement and 3 lines of context cannot uniquely identify the snippet of code, you can use multiple `@@` statements to jump to the right context. For instance:

@@ class BaseClass
@@ 	 def method():
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

The full grammar definition is below:
Patch := Begin { FileOp } End
Begin := "*** Begin Patch" NEWLINE
End := "*** End Patch" NEWLINE
FileOp := AddFile | DeleteFile | UpdateFile | InsertBeforeSymbol | InsertAfterSymbol | ReplaceSymbolBody
AddFile := "*** Add File: " path NEWLINE { "+" line NEWLINE }
DeleteFile := "*** Delete File: " path NEWLINE
UpdateFile := "*** Update File: " path NEWLINE [ MoveTo ] { Hunk }
MoveTo := "*** Move to: " newPath NEWLINE
Hunk := "@@" [ header ] NEWLINE { HunkLine } [ "*** End of File" NEWLINE ]
HunkLine := (" " | "-" | "+") text NEWLINE
InsertBeforeSymbol := "*** Insert Before Symbol: " target NEWLINE { "+" line NEWLINE }
InsertAfterSymbol := "*** Insert After Symbol: " target NEWLINE { "+" line NEWLINE }
ReplaceSymbolBody := "*** Replace Symbol Body: " target NEWLINE { "+" line NEWLINE }
target := path "::" symbol_path
symbol_path := segment { "::" segment }
segment := trimmed_identifier

A full patch can combine several operations:

*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch

It is important to remember:

- You must include a header with your intended action (Add/Delete/Update/Insert Before Symbol/Insert After Symbol/Replace Symbol Body)
- You must prefix new lines with `+` even when creating a new file or supplying symbol edits
- File references can only be relative, NEVER ABSOLUTE.
- Symbol-aware edits support Rust, TypeScript/JavaScript, Go, C++, and Python via precise AST matching with a fuzzy fallback when the AST locator cannot find the declaration. They fail fast if the symbol cannot be located.
- Operation summaries list symbol edits explicitly (e.g. `symbol(replace-body): path :: Symbol (+N, -M)`) with strategy details so downstream automation can reason about the changes without re-parsing code.

You can invoke apply_patch like:

```
shell {"command":["apply_patch","*** Begin Patch\n*** Add File: hello.txt\n+Hello, world!\n*** End Patch\n"]}
```

### Output

On success the tool prints a begin_patch-style summary to stdout so you always know what happened without re-reading the files:

```
Applied operations:
- add: hello.txt (+1)
- update: src/main.rs (+3, -1)
✔ Patch applied successfully.
Formatting:
- cargo fmt (workspace) ✔ 480 ms
Post-checks:
- cargo test -p codex-apply-patch ✔ 3.2 s
```

Each bullet lists the action (`add`, `update`, `move`, or `delete`) plus the per-file line deltas.

Move operations appear as `- move: source -> dest (+added, -removed)` and combine renames with content edits in a single entry. Deletes show their line count (`- delete: path (-N)`). When a patch touches multiple files the summary lists one bullet per file in patch order so you can skim the outcome at a glance. All filesystem updates are applied atomically: every file is written through a temporary file and the original contents are backed up, so a failure automatically rolls the workspace back to its pre-patch state.

If patch verification fails, `apply_patch` prints the diagnostic to stderr and leaves the filesystem unchanged.

On success, any touched files are automatically staged in git (when run inside a repository), so your workspace is ready for a commit without additional `git add` commands.

### Configuration defaults

### CLI usage

- `apply_patch` — применяет патч, автоматически определяя рабочий корень и источники данных.
- `apply_patch dry-run` — выполняет валидацию без записи на диск, вывод совпадает с успешным запуском, но `mode` помечен как `dry-run`.
- `apply_patch explain` — аналог `dry-run`, предназначен для сценариев, где нужно только описание применения.
- `apply_patch amend` — повторно применяет только изменённую часть патча после сбоя (используй `Amendment template`, который выводит CLI).

Никакие дополнительные флаги не требуются: бинарь сразу печатает сводку, JSON-отчёт и диагностику, если что-то пошло не так — без сохранения служебных файлов на диск.

Чтобы запускать без heredoc, достаточно вызвать `apply_patch`, вставить блок `*** Begin Patch` … `*** End Patch`, затем завершить ввод (`Ctrl+D` на Unix, `Ctrl+Z` и Enter на Windows).

#### Conflict hints and batches

При конфликте или других ошибках CLI выводит диагностический блок: путь файла, контекст и diff hint помогают быстро увидеть расхождение, а не применённые содержимое печатается прямо в терминал. Сразу ниже появляется `Amendment template` с готовым `*** Begin Patch` блоком только для проблемных операций — достаточно подправить строки и заново запустить `apply_patch`.

Inline patches may contain multiple `*** Begin Patch` blocks back-to-back. The CLI applies them atomically: if any block fails, no filesystem changes are committed. The human summary and JSON envelope enumerate the operations in order so downstream automation can attribute successes and failures without scraping free-form text.

The machine-readable output printed to stdout always looks like:

```
{"schema":"apply_patch/v2","report":{...}}
```

The `report` object contains:

- `status` — `success` or `failed`.
- `mode` — `apply` or `dry-run`.
- `duration_ms` — end-to-end latency for the run.
- `operations[]` — per-file summaries with fields `action`, `path`, optional `renamed_to`, line deltas (`added`/`removed`), `status` (`applied`/`failed`/`planned`), and optional `message`.
- `formatting[]` — auto-run formatter outcomes with fields `tool`, `scope`, `status`, `duration_ms`, `files[]`, and optional `note`.
- `post_checks[]` — verification commands that ran after the patch, including `name`, `command[]`, `cwd`, `status`, `duration_ms`, optional `note`, `stdout`, and `stderr`.
- `diagnostics[]` — high-level warnings such as skipped formatters or failed post-checks (empty on success).
- `artifacts` — зарезервированный объект для будущих артефактов (по умолчанию пустой).
- `amendment_template` — строка с готовым `*** Begin Patch` блоком для повторного применения проблемных операций (заполняется только при ошибках).
- `errors[]` — high-level errors collected during the run (empty on success).
- `options` — the normalization/preservation settings that were in effect.

When an operation fails, its entry in `operations[]` is marked `failed`, the error message is echoed in `errors[]`, and a conflict hint is written so you can inspect the expectations and actual file contents offline.
