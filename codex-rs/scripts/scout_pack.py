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
                excerpt.get("end_line"),
                f"spec.sections[{i}].excerpts[{j}].end_line",
            )
            if start_line < 1:
                _die(f"spec.sections[{i}].excerpts[{j}]: start_line must be >= 1")
            if end_line < start_line:
                _die(f"spec.sections[{i}].excerpts[{j}]: end_line must be >= start_line")

            range_ctx = f"spec.sections[{i}].excerpts[{j}]"
            prior_ranges = ranges_by_path.setdefault(rel_path, [])
            for prev_start, prev_end, prev_ctx in prior_ranges:
                if start_line == prev_start and end_line == prev_end:
                    _die(f"{range_ctx}: duplicate range for {rel_path}: {start_line}-{end_line} (already used by {prev_ctx})")
                if not (end_line < prev_start or start_line > prev_end):
                    _die(
                        f"{range_ctx}: overlapping range for {rel_path}: {start_line}-{end_line} overlaps {prev_start}-{prev_end} ({prev_ctx})"
                    )
            prior_ranges.append((start_line, end_line, range_ctx))

            language = excerpt.get("language")
            if language is not None and not isinstance(language, str):
                _die(f"spec.sections[{i}].excerpts[{j}].language: expected string")
            if language is not None and language not in {"", "auto"} and not LANG_RE.fullmatch(language.strip()):
                _die(f"spec.sections[{i}].excerpts[{j}].language: invalid value")

            label = excerpt.get("label")
            if label is not None and not isinstance(label, str):
                _die(f"spec.sections[{i}].excerpts[{j}].label: expected string")
            why = excerpt.get("why")
            if why is not None and not isinstance(why, str):
                _die(f"spec.sections[{i}].excerpts[{j}].why: expected string")

            must_include = _expect_optional_str_list(
                excerpt.get("must_include"),
                f"spec.sections[{i}].excerpts[{j}].must_include",
            )

            crate_raw = excerpt.get("crate")
            if crate_raw is None:
                crate = default_crate
            else:
                crate = _expect_crate(crate_raw, f"spec.sections[{i}].excerpts[{j}].crate")

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
            for token in must_include:
                if token not in body:
                    _die(
                        f"spec.sections[{i}].excerpts[{j}].must_include: token not found in excerpt body: {token}"
                    )
            if language is None or language == "auto":
                fence_lang = _guess_language(file_path)
            else:
                fence_lang = language.strip()

            code_ref = f"CODE_REF::{crate}::{rel_path}#L{start_line}-L{end_line}"
            header_label = (label or ex_id or "").strip()
            header = f"**CODE_REF:** `{code_ref}`"
            if header_label:
                header = f"{header} — {header_label}"

            out.append("")
            out.append(header)
            if why and why.strip():
                out.append("")
                out.append(why.strip())
            if must_include:
                out.append("")
                out.append("Must include:")
                for token in must_include:
                    out.append(f"- `{token}`")
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
