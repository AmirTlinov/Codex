# role_split: Scout context pack (example)

Example excerpt spec: package role-split workflow + scout-pack generator into a patch-ready context pack.

## Just recipe: scout-pack

Single command to render a Markdown context pack from this spec.

**CODE_REF:** `CODE_REF::codex-rs::justfile#L67-L81` — justfile: scout-pack

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

# Validate a Scout excerpt spec and render to stdout (discarded).
scout-pack-check spec:
    python3 scripts/scout_pack.py "{{spec}}" -o - >/dev/null

```

## codex-rs skills index

Project skill registry in codex-rs.

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/.agents/skills/SKILLS.md#L1-L32` — codex-rs/.agents/skills/SKILLS.md

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

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/.agents/skills/orchestrator_role_split_pipeline/SKILL.md#L1-L47` — orchestrator-role-split-pipeline

Defines the contract between Orchestrator and Scout.

```markdown
---
name: orchestrator-role-split-pipeline
description: "Оркестратор → Scout→ContextValidator→Main(implement)→Validator с безопасными контрактами"
ttl_days: 0
---

# Orchestrator role-split pipeline (builder-off mode)

## Trigger
Нужно выполнить задачу итеративно слайсами, сохранив high-signal контекст и fail-closed проверки.

## Outcome
- Основной контур: `Scout -> ContextValidator -> Main implement -> Validator`.
- Scout отдает patch-ready контекст-пак (CODE_REF + excerpt_spec + Mermaid).
- ContextValidator выдает только `CONTEXT_PACK_APPROVED` или `CONTEXT_PACK_GAPS`.
- Main делает минимальный патч по slice; Validator проверяет патч на контракт/verify.

## How to request Scout (copy/paste prompt skeleton)
Проси Scout так, чтобы он вернул **контекст‑пак, готовый для патча**:

- Sections: Scope snapshot -> Patch target contract -> Key invariants -> Anchor map -> Excerpt spec -> Mermaid -> Risks -> Unknowns -> Patch readiness.
- Доказательства: `CODE_REF::<crate>::<path>#L<start>-L<end>`.
- Артефакты: `ScoutReport.md`, `excerpt_spec.yml`, `context_pack.md`.

## Handoff state machine
`discover -> validate_ctx -> implement -> review_patch -> final_accept`

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
- Contract drift между skill docs и runtime templates (`core/templates/agents/*.md`).
- Лечится регулярной сверкой handoff и CODE_REF формата.

## Last verified
Last verified: 2026-02-14

```

## Scout context pack contract

Hard rules + templates for a patch-ready scout context pack.

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/.agents/skills/scout_context_pack/SKILL.md#L1-L33` — scout-context-pack

SSOT for required sections and Excerpt spec usage.

```markdown
---
name: scout-context-pack
description: "Scout: собрать patch-ready контекст-пак (CODE_REF + excerpts + Mermaid) для прямой реализации Main"
ttl_days: 0
---

# Scout context pack (anchors + excerpt ranges)

## Trigger
Когда Main/Orchestrator просит контекст для атомарного slice-фикса.

## Outcome (what you MUST produce)
1) `ScoutReport.md` со строгими секциями и явным `Patch readiness: PASS|FAIL`.
2) `excerpt_spec.yml` (или `.json`) с line-anchored verbatim-вырезками.
3) `context_pack.md`, сгенерированный через `just scout-pack <excerpt_spec.yml> -o <context_pack.md>`.
4) 1–2 Mermaid диаграммы (минимум dependency flow, опционально state/handoff).

## Required ScoutReport sections (strict order)
1) Scope snapshot
2) Patch target contract
3) Key invariants / constraints
4) Anchor map
5) Excerpt specs
6) Dependency map
7) High-confidence risks / edge cases
8) Missing context items needed before patching
9) Patch readiness gates

## CODE_REF contract
- Каждый ключевой тезис (инвариант/риск/гейт) обязан иметь минимум один `CODE_REF`.
- Формат:
  - `CODE_REF::<crate>::<repo_relative_path>#L<start>-L<end>`
- Пример:

```

## Generator: scout_pack.py

Fail-closed, deterministic pack renderer (YAML/JSON spec → Markdown).

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L1-L160` — CLI + spec loading + helpers

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

CRATE_RE = re.compile(r"[a-z0-9][a-z0-9_-]*")
LANG_RE = re.compile(r"[A-Za-z0-9_+\.-]+")


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


def _expect_optional_str(value: Any, ctx: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        _die(f"{ctx}: expected string")
    if not value.strip():
        _die(f"{ctx}: expected non-empty string")
    return value.strip()


def _expect_optional_str_list(value: Any, ctx: str) -> list[str]:
    if value is None:
        return []
    if not isinstance(value, list) or not value:
        _die(f"{ctx}: expected non-empty list of strings")
    out: list[str] = []
    for i, item in enumerate(value):
        if not isinstance(item, str) or not item.strip():
            _die(f"{ctx}[{i}]: expected non-empty string")
        out.append(item.strip())
    return out


def _expect_crate(value: Any, ctx: str) -> str:
    crate = _expect_str(value, ctx).strip()
    if not CRATE_RE.fullmatch(crate):
        _die(f"{ctx}: invalid crate name")
    return crate


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
        return f"```{fence_lang}\n{body.rstrip('\\n')}\n```"
    return f"```\n{body.rstrip('\\n')}\n```"


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
    _validate_no_unknown_fields(
        spec,
        {
            "version",
            "title",
            "summary",
            "repo_root",
            "default_crate",
            "task_id",
            "slice_id",
            "intent",
            "quality_gates",

```

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L161-L282` — validation + renderer

Schema enforcement, path traversal guard, line-range slicing, deterministic markdown.

```python
            "sections",
        },
        "spec",
    )

    version = spec.get("version")
    if version is not None:
        _expect_int(version, "spec.version")

    title = _expect_str(spec.get("title"), "spec.title")
    summary = spec.get("summary")
    if summary is not None and not isinstance(summary, str):
        _die("spec.summary: expected string")

    default_crate_raw = spec.get("default_crate")
    if default_crate_raw is None:
        default_crate = "codex-rs"
    else:
        default_crate = _expect_crate(default_crate_raw, "spec.default_crate")

    task_id = _expect_optional_str(spec.get("task_id"), "spec.task_id")
    slice_id = _expect_optional_str(spec.get("slice_id"), "spec.slice_id")
    intent = _expect_optional_str(spec.get("intent"), "spec.intent")
    quality_gates = _expect_optional_str_list(spec.get("quality_gates"), "spec.quality_gates")

    repo_root_raw = spec.get("repo_root")
    repo_root_str = _expect_str(repo_root_raw, "spec.repo_root")
    repo_root = Path(repo_root_str).resolve()
    if not repo_root.exists() or not repo_root.is_dir():
        _die(f"spec.repo_root: not a directory: {repo_root}")

    sections = spec.get("sections")
    if not isinstance(sections, list) or not sections:
        _die("spec.sections: expected non-empty list")

    out: list[str] = [f"# {title}"]
    ranges_by_path: dict[str, list[tuple[int, int, str]]] = {}
    if summary:
        out.append("")
        out.append(summary.strip())
    if task_id or slice_id or intent:
        out.append("")
        out.append("## Metadata")
        if task_id:
            out.append(f"- Task: `{task_id}`")
        if slice_id:
            out.append(f"- Slice: `{slice_id}`")
        if intent:
            out.append(f"- Intent: {intent}")
        out.append(f"- Default crate: `{default_crate}`")
    if quality_gates:
        out.append("")
        out.append("## Quality gates")
        for gate in quality_gates:
            out.append(f"- {gate}")

    for i, section in enumerate(sections):
        if not isinstance(section, dict):
            _die(f"spec.sections[{i}]: expected mapping")
        _validate_no_unknown_fields(
            section,
            {"heading", "description", "notes", "mermaid", "excerpts"},
            f"spec.sections[{i}]",
        )

        heading = _expect_str(section.get("heading"), f"spec.sections[{i}].heading")
        description = section.get("description")
        if description is not None and not isinstance(description, str):
            _die(f"spec.sections[{i}].description: expected string")
        notes = section.get("notes")
        if notes is not None and not isinstance(notes, str):
            _die(f"spec.sections[{i}].notes: expected string")

        mermaid = section.get("mermaid")
        if mermaid is not None and not isinstance(mermaid, str):
            _die(f"spec.sections[{i}].mermaid: expected string")

        excerpts = section.get("excerpts")
        if not isinstance(excerpts, list) or not excerpts:
            _die(f"spec.sections[{i}].excerpts: expected non-empty list")

        out.append("")
        out.append(f"## {heading}")
        if description:
            out.append("")
            out.append(description.strip())
        if notes:
            out.append("")
            out.append(notes.strip())
        if mermaid and mermaid.strip():
            out.append("")
            out.append(_fence("mermaid", mermaid.strip()))

        for j, excerpt in enumerate(excerpts):
            if not isinstance(excerpt, dict):
                _die(f"spec.sections[{i}].excerpts[{j}]: expected mapping")
            _validate_no_unknown_fields(
                excerpt,
                {
                    "id",
                    "crate",
                    "path",
                    "start_line",
                    "end_line",
                    "language",
                    "label",
                    "why",
                    "must_include",
                },
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

```
