# role_split: Scout context pack (example)

Example excerpt spec: package role-split workflow + scout-pack generator into a patch-ready context pack.

## Just recipe: scout-pack

Single command to render a Markdown context pack from this spec.

**CODE_REF:** `justfile:67-81` — justfile: scout-pack

Forwards args to codex-rs/scripts/scout_pack.py (supports -o/--output).

```
# Regenerate the json schema for config.toml from the current config types.
write-config-schema:
    cargo run -p codex-core --bin codex-write-config-schema

# Regenerate vendored app-server protocol schema artifacts.
write-app-server-schema *args:
    cargo run -p codex-app-server-protocol --bin write_schema_fixtures -- "$@"

# Generate a markdown context pack from a Scout excerpt spec (YAML/JSON).
scout-pack *args:
    python3 scripts/scout_pack.py "$@"

# Tail logs from the state SQLite database
log *args:
    if [ "${1:-}" = "--" ]; then shift; fi; cargo run -p codex-state --bin logs_client -- "$@"
```

## codex-rs skills index

Project skill registry in codex-rs.

**CODE_REF:** `codex-rs/.agents/skills/SKILLS.md:1-32` — codex-rs/.agents/skills/SKILLS.md

Shows discoverability of scout-context-pack skill for agents.

```markdown
[LEGEND]

[CONTENT]
# Skills — project memory (.agents/skills)

Эта папка — “долговременная память” проекта для ИИ‑агентов.

## Как добавить skill (норма)

1) Создай файл: `.agents/skills/<skill>/SKILL.md`.
2) В начале файла добавь YAML front matter:

```yaml
---
name: <skill>
description: "TRIGGER → OUTCOME → POINTERS"
ttl_days: 90   # 0 = evergreen (без требования обновлять по TTL)
---
```

3) В тексте skill обязательно укажи строку:

```text
Last verified: YYYY-MM-DD
```

4) Зарегистрируй skill в списке ниже как ссылку на файл.

## Список

- [orchestrator-role-split-pipeline](.agents/skills/orchestrator_role_split_pipeline/SKILL.md)
- [scout-context-pack](.agents/skills/scout_context_pack/SKILL.md)
```

## Orchestrator role-split pipeline

How Orchestrator should request Scout context (anchors + excerpt ranges + Mermaid).

**CODE_REF:** `codex-rs/.agents/skills/orchestrator_role_split_pipeline/SKILL.md:1-49` — orchestrator-role-split-pipeline

Defines the contract between Orchestrator and Scout.

```markdown
---
name: orchestrator-role-split-pipeline
description: "Оркестратор → Scout→ContextValidator→Builder→PostBuilderValidator с безопасными ролевыми контрактами и плейном"
ttl_days: 0
---

# Orchestrator role-split pipeline

## Trigger
Слой роли/планирования (`Default/Plan`) и инструменты TUI/модели требуются для
безопасной, атомарной разработки с валидацией контекста и патчей.

## Outcome
- Подтверждено: роли `Default`, `Scout`, `ContextValidator`, `Builder`,
  `PostBuilderValidator`, `Plan` подключены в runtime.
- Подтверждено: toolset для ролей жестко ограничен и fail-closed.
- Подтверждено: `Plan`/`post_builder`/`validator` пайплайны не допускают невалидный патч.
- Стандарт разведки: Scout отдаёт *контекст‑пак* (anchors + excerpt ranges + Mermaid) вместо код‑дампов,
  чтобы Builder мог патчить без доразведки (см. skill `scout-context-pack`).
- Подтверждено: `view_image` в js_repl с предварительным `spawn_agent("scout")` проходит контракт.
- Подтверждено: плановая схема конфигурации и схема `config.schema.json` синхронизированы.

## How to request Scout (copy/paste prompt skeleton)
Проси Scout так, чтобы он вернул **контекст‑пак, готовый для патча** (без копипасты больших кусков кода):

- Sections: Scope snapshot → Key invariants → Anchor map → Excerpt spec → Mermaid → Risks → Unknowns → Next.
- Доказательства: `CODE_REF` = `path:start-end`.
- Отдельным файлом/блоком: `excerpt_spec.(yml|json)` по шаблону из skill `scout-context-pack`.

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
`agent role` и `plan_dir`-паттерны зависят от корректности настроек `~/.codex/plans`.
Периодически проверять guard-слои и policy для новых API/инструментов.

## Last verified
Last verified: 2026-02-14
```

## Scout context pack contract

Hard rules + templates for a patch-ready scout context pack.

**CODE_REF:** `codex-rs/.agents/skills/scout_context_pack/SKILL.md:1-33` — scout-context-pack

SSOT for required sections and Excerpt spec usage.

```markdown
---
name: scout-context-pack
description: "Scout: собрать patch-ready контекст (anchors + excerpt ranges + Mermaid) без шума"
ttl_days: 0
---

# Scout context pack (anchors + excerpt ranges)

## Trigger
Когда Orchestrator просит “подготовь контекст для патча” / “сделай контекст‑пак”.

## Outcome (what you MUST produce)
1) **Scout report** (Markdown) со строгими секциями и доказательствами через `CODE_REF`.
2) **Excerpt spec** (YAML/JSON) — список verbatim‑вставок кода через `path + start_line + end_line`.
3) 1–2 Mermaid‑диаграммы: dependency flow и (если уместно) state machine/handoff.

## Hard rules
- Любое утверждение “это гейт/инвариант/обход/риск” → обязан приложить `CODE_REF`.
- В отчёте **не** дампить большие куски кода: вместо этого — `CODE_REF` + `excerpt_spec`.
- Диапазоны строк: **1-indexed**, `end_line` включительно.
- Если диапазон/файл не найден → явно пометить как `BLOCKER`.

## Templates
- Report: `.agents/skills/scout_context_pack/templates/ScoutReport.md`
- Spec: `.agents/skills/scout_context_pack/templates/excerpt_spec.example.yml`

## Consumption (for Orchestrator)
- Быстро вручную: вытянуть verbatim по `CODE_REF` через `mcp__context__file_slice`/`meaning_expand`.
- Автоматически: `just scout-pack <excerpt_spec.yml> -o <context_pack.md>`.

## Last verified
Last verified: 2026-02-14
```

## Generator: scout_pack.py

Fail-closed, deterministic pack renderer (YAML/JSON spec → Markdown).

**CODE_REF:** `codex-rs/scripts/scout_pack.py:1-160` — CLI + spec loading + helpers

Entrypoint + YAML/JSON parsing + atomic write primitives.

```python
#!/usr/bin/env python3
"""Generate a Markdown context pack from a Scout excerpt spec (YAML/JSON).

Design goals:
- fail-closed validation (no partial output on error)
- deterministic output (no timestamps, stable ordering)
- excerpt line ranges are 1-indexed and inclusive
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
from pathlib import Path
from typing import Any


def _die(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(2)


def _load_spec(spec_path: Path) -> dict[str, Any]:
    if not spec_path.exists():
        _die(f"spec not found: {spec_path}")

    suffix = spec_path.suffix.lower()
    if suffix == ".json":
        try:
            return json.loads(spec_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as err:
            _die(f"invalid JSON spec: {err}")
    if suffix in {".yml", ".yaml"}:
        try:
            import yaml  # type: ignore[import-not-found]
        except ImportError:
            _die("PyYAML is required for .yml/.yaml specs (pip install pyyaml)")
        data = yaml.safe_load(spec_path.read_text(encoding="utf-8"))
        if data is None:
            _die("YAML spec is empty")
        if not isinstance(data, dict):
            _die("YAML spec must be a mapping at top-level")
        return data

    _die(f"unsupported spec extension: {suffix} (expected .yml/.yaml/.json)")


def _validate_no_unknown_fields(obj: dict[str, Any], allowed: set[str], ctx: str) -> None:
    extra = set(obj.keys()) - allowed
    if extra:
        _die(f"{ctx}: unknown field(s): {', '.join(sorted(extra))}")


def _expect_str(value: Any, ctx: str) -> str:
    if not isinstance(value, str) or not value:
        _die(f"{ctx}: expected non-empty string")
    return value


def _expect_int(value: Any, ctx: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        _die(f"{ctx}: expected integer")
    return value


def _guess_language(path: Path) -> str:
    if path.name == "justfile":
        return ""

    suffix = path.suffix.lower()
    return {
        ".rs": "rust",
        ".md": "markdown",
        ".toml": "toml",
        ".yml": "yaml",
        ".yaml": "yaml",
        ".json": "json",
        ".py": "python",
        ".sh": "bash",
        ".ps1": "powershell",
        ".ts": "typescript",
        ".tsx": "tsx",
        ".js": "javascript",
        ".jsx": "jsx",
    }.get(suffix, "")


def _fence(lang: str, body: str) -> str:
    fence_lang = lang.strip() if lang else ""
    if fence_lang:
        return f"```{fence_lang}\n{body.rstrip('\n')}\n```"
    return f"```\n{body.rstrip('\n')}\n```"


def _atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        newline="\n",
        delete=False,
        dir=path.parent,
        prefix=f".{path.name}.",
        suffix=".tmp",
    ) as tmp:
        tmp.write(content)
        tmp_path = Path(tmp.name)
    os.replace(tmp_path, path)


def _render(spec: dict[str, Any]) -> str:
    _validate_no_unknown_fields(spec, {"version", "title", "summary", "repo_root", "sections"}, "spec")

    version = spec.get("version")
    if version is not None:
        _expect_int(version, "spec.version")

    title = _expect_str(spec.get("title"), "spec.title")
    summary = spec.get("summary")
    if summary is not None and not isinstance(summary, str):
        _die("spec.summary: expected string")

    repo_root_raw = spec.get("repo_root")
    repo_root_str = _expect_str(repo_root_raw, "spec.repo_root")
    repo_root = Path(repo_root_str).resolve()
    if not repo_root.exists() or not repo_root.is_dir():
        _die(f"spec.repo_root: not a directory: {repo_root}")

    sections = spec.get("sections")
    if not isinstance(sections, list) or not sections:
        _die("spec.sections: expected non-empty list")

    out: list[str] = [f"# {title}"]
    if summary:
        out.append("")
        out.append(summary.strip())

    for i, section in enumerate(sections):
        if not isinstance(section, dict):
            _die(f"spec.sections[{i}]: expected mapping")
        _validate_no_unknown_fields(
            section,
            {"heading", "description", "mermaid", "excerpts"},
            f"spec.sections[{i}]",
        )

        heading = _expect_str(section.get("heading"), f"spec.sections[{i}].heading")
        description = section.get("description")
        if description is not None and not isinstance(description, str):
            _die(f"spec.sections[{i}].description: expected string")

        mermaid = section.get("mermaid")
        if mermaid is not None and not isinstance(mermaid, str):
            _die(f"spec.sections[{i}].mermaid: expected string")

        excerpts = section.get("excerpts")
```

**CODE_REF:** `codex-rs/scripts/scout_pack.py:161-282` — validation + renderer

Schema enforcement, path traversal guard, line-range slicing, deterministic markdown.

```python
        if not isinstance(excerpts, list) or not excerpts:
            _die(f"spec.sections[{i}].excerpts: expected non-empty list")

        out.append("")
        out.append(f"## {heading}")
        if description:
            out.append("")
            out.append(description.strip())
        if mermaid and mermaid.strip():
            out.append("")
            out.append(_fence("mermaid", mermaid.strip()))

        for j, excerpt in enumerate(excerpts):
            if not isinstance(excerpt, dict):
                _die(f"spec.sections[{i}].excerpts[{j}]: expected mapping")
            _validate_no_unknown_fields(
                excerpt,
                {"id", "path", "start_line", "end_line", "language", "label", "why"},
                f"spec.sections[{i}].excerpts[{j}]",
            )

            ex_id = excerpt.get("id")
            if ex_id is not None and not isinstance(ex_id, str):
                _die(f"spec.sections[{i}].excerpts[{j}].id: expected string")

            rel_path = _expect_str(excerpt.get("path"), f"spec.sections[{i}].excerpts[{j}].path")
            start_line = _expect_int(
                excerpt.get("start_line"),
                f"spec.sections[{i}].excerpts[{j}].start_line",
            )
            end_line = _expect_int(
                excerpt.get("end_line"),
                f"spec.sections[{i}].excerpts[{j}].end_line",
            )
            if start_line < 1:
                _die(f"spec.sections[{i}].excerpts[{j}]: start_line must be >= 1")
            if end_line < start_line:
                _die(f"spec.sections[{i}].excerpts[{j}]: end_line must be >= start_line")

            language = excerpt.get("language")
            if language is not None and not isinstance(language, str):
                _die(f"spec.sections[{i}].excerpts[{j}].language: expected string")
            if language is not None and language not in {"", "auto"}:
                if not re.fullmatch(r"[A-Za-z0-9_+\.-]+", language.strip()):
                    _die(f"spec.sections[{i}].excerpts[{j}].language: invalid value")

            label = excerpt.get("label")
            if label is not None and not isinstance(label, str):
                _die(f"spec.sections[{i}].excerpts[{j}].label: expected string")
            why = excerpt.get("why")
            if why is not None and not isinstance(why, str):
                _die(f"spec.sections[{i}].excerpts[{j}].why: expected string")

            file_path = (repo_root / rel_path).resolve()
            if not file_path.is_relative_to(repo_root):
                _die(f"excerpt path escapes repo_root: {rel_path}")
            if not file_path.exists() or not file_path.is_file():
                _die(f"excerpt file not found: {rel_path}")

            try:
                text = file_path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                _die(f"excerpt file is not valid UTF-8: {rel_path}")

            text = text.replace("\r\n", "\n")
            lines = text.splitlines(keepends=True)
            if end_line > len(lines):
                _die(
                    f"excerpt range out of bounds for {rel_path}: {start_line}-{end_line} (file has {len(lines)} lines)"
                )

            body = "".join(lines[start_line - 1 : end_line])
            fence_lang = ""
            if language is None or language == "auto":
                fence_lang = _guess_language(file_path)
            else:
                fence_lang = language.strip()

            code_ref = f"{rel_path}:{start_line}-{end_line}"
            header_label = (label or ex_id or "").strip()
            header = f"**CODE_REF:** `{code_ref}`"
            if header_label:
                header = f"{header} — {header_label}"

            out.append("")
            out.append(header)
            if why and why.strip():
                out.append("")
                out.append(why.strip())
            out.append("")
            out.append(_fence(fence_lang, body))

    return "\n".join(out).rstrip("\n") + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("spec", type=Path, help="Path to excerpt spec (.yml/.yaml/.json)")
    parser.add_argument(
        "--output",
        "-o",
        default="-",
        help='Output file path, or "-" for stdout (default: -)',
    )
    args = parser.parse_args()

    spec = _load_spec(args.spec)
    if not isinstance(spec, dict):
        _die("spec: expected mapping at top-level")

    rendered = _render(spec)

    if args.output == "-":
        sys.stdout.write(rendered)
        return

    _atomic_write(Path(args.output), rendered)


if __name__ == "__main__":
    main()
```
