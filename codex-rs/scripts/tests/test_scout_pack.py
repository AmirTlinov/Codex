from __future__ import annotations

import importlib.util
import io
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from typing import Any


def _load_module():
    module_path = Path(__file__).resolve().parents[1] / "scout_pack.py"
    spec = importlib.util.spec_from_file_location("scout_pack", module_path)
    if spec is None or spec.loader is None:
        raise RuntimeError("failed to load scout_pack module")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ScoutPackRenderTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.module = _load_module()

    def _render(self, spec: dict[str, Any]) -> str:
        return self.module._render(spec)

    def _assert_render_error(self, spec: dict[str, Any], expected: str) -> None:
        stderr = io.StringIO()
        with self.assertRaises(SystemExit) as exc, redirect_stderr(stderr):
            self._render(spec)
        self.assertEqual(exc.exception.code, 2)
        self.assertIn(expected, stderr.getvalue())

    def test_anchor_first_multi_file_multi_anchor_and_mermaid_modes_render(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo_root = Path(temp_dir)
            (repo_root / "a.py").write_text("line1\nline2\nline3\n", encoding="utf-8")
            (repo_root / "docs.md").write_text("alpha\nbeta\n", encoding="utf-8")
            (repo_root / "map.mmd").write_text("flowchart LR\n  X --> Y\n", encoding="utf-8")

            spec = {
                "version": 2,
                "title": "Anchor-first pack",
                "repo_root": str(repo_root),
                "sections": [
                    {
                        "heading": "Combined anchors",
                        "mermaid": "flowchart LR\n  A --> B\n",
                        "anchors": [
                            "CODE_REF::codex-rs::a.py#L1-L2",
                            {
                                "id": "doc-anchor",
                                "code_ref": "CODE_REF::codex-rs::docs.md#L1-L2",
                                "label": "Doc anchor",
                                "why": "Second anchor from another file",
                            },
                        ],
                    },
                    {
                        "heading": "Mermaid from file",
                        "mermaid_file": "map.mmd",
                        "anchors": [
                            {
                                "code_ref": "CODE_REF::codex-rs::a.py#L3-L3",
                            }
                        ],
                    },
                ],
            }

            rendered = self._render(spec)
            self.assertEqual(rendered, self._render(spec))
            self.assertIn("## Combined anchors", rendered)
            self.assertIn("## Mermaid from file", rendered)
            self.assertIn("`CODE_REF::codex-rs::a.py#L1-L2`", rendered)
            self.assertIn("`CODE_REF::codex-rs::docs.md#L1-L2`", rendered)
            self.assertIn("```mermaid\nflowchart LR\n  A --> B", rendered)
            self.assertIn("```mermaid\nflowchart LR\n  X --> Y", rendered)

    def test_explicit_ranges_require_opt_in(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo_root = Path(temp_dir)
            (repo_root / "a.py").write_text("line1\nline2\n", encoding="utf-8")
            spec = {
                "version": 2,
                "title": "No legacy ranges by default",
                "repo_root": str(repo_root),
                "sections": [
                    {
                        "heading": "Ranges",
                        "excerpts": [
                            {
                                "path": "a.py",
                                "start_line": 1,
                                "end_line": 1,
                            }
                        ],
                    }
                ],
            }

            self._assert_render_error(spec, "requires spec.allow_explicit_ranges=true")

    def test_explicit_ranges_work_when_opted_in(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo_root = Path(temp_dir)
            (repo_root / "a.py").write_text("line1\nline2\n", encoding="utf-8")
            spec = {
                "version": 2,
                "title": "Legacy ranges",
                "repo_root": str(repo_root),
                "allow_explicit_ranges": True,
                "sections": [
                    {
                        "heading": "Ranges",
                        "excerpts": [
                            {
                                "path": "a.py",
                                "start_line": 1,
                                "end_line": 1,
                            }
                        ],
                    }
                ],
            }

            rendered = self._render(spec)
            self.assertIn("`CODE_REF::codex-rs::a.py#L1-L1`", rendered)

    def test_duplicate_headings_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo_root = Path(temp_dir)
            (repo_root / "a.py").write_text("line1\nline2\n", encoding="utf-8")
            spec = {
                "version": 2,
                "title": "Duplicate heading",
                "repo_root": str(repo_root),
                "sections": [
                    {
                        "heading": "Same",
                        "anchors": [{"code_ref": "CODE_REF::codex-rs::a.py#L1-L1"}],
                    },
                    {
                        "heading": "Same",
                        "anchors": [{"code_ref": "CODE_REF::codex-rs::a.py#L2-L2"}],
                    },
                ],
            }

            self._assert_render_error(spec, "duplicate heading")


if __name__ == "__main__":
    unittest.main()
