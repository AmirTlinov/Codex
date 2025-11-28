"""
Symbol resolution for Python code.

Parses Python files and resolves symbol paths like:
    - Class::method
    - function
    - Class::nested_class::method

Uses Python's AST for reliable symbol location.
"""

from __future__ import annotations

import ast
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator


class SymbolNotFoundError(Exception):
    """Symbol could not be found in the source."""

    def __init__(self, symbol_path: str, file_path: Path) -> None:
        self.symbol_path = symbol_path
        self.file_path = file_path
        super().__init__(f"Symbol '{symbol_path}' not found in {file_path}")


@dataclass(slots=True, frozen=True)
class SymbolPath:
    """Parsed symbol path."""

    parts: tuple[str, ...]

    @classmethod
    def parse(cls, path: str) -> SymbolPath:
        """Parse a symbol path string like 'Class::method'."""
        parts = tuple(p.strip() for p in path.split("::") if p.strip())
        if not parts:
            raise ValueError(f"Empty symbol path: {path}")
        return cls(parts=parts)

    def __str__(self) -> str:
        return "::".join(self.parts)


@dataclass(slots=True)
class SymbolLocation:
    """Location of a symbol in source code."""

    start_line: int  # 1-indexed
    end_line: int  # 1-indexed, inclusive
    start_col: int  # 0-indexed
    end_col: int  # 0-indexed
    indent: int  # indentation level (spaces)
    body_start_line: int | None = None  # For functions/classes, where body starts
    body_end_line: int | None = None  # For functions/classes, where body ends


@dataclass(slots=True)
class ResolvedSymbol:
    """A resolved symbol with its location."""

    name: str
    path: SymbolPath
    location: SymbolLocation
    node: ast.AST
    source_lines: list[str]  # The actual source lines


class SymbolResolver:
    """Resolves symbol paths in Python source code."""

    def __init__(self, source: str, file_path: Path | None = None) -> None:
        self.source = source
        self.file_path = file_path or Path("<string>")
        self.lines = source.split("\n")
        self._tree: ast.AST | None = None

    @property
    def tree(self) -> ast.AST:
        """Parse and cache the AST."""
        if self._tree is None:
            self._tree = ast.parse(self.source, filename=str(self.file_path))
        return self._tree

    def resolve(self, symbol_path: str | SymbolPath) -> ResolvedSymbol:
        """
        Resolve a symbol path to its location.

        Args:
            symbol_path: Path like "Class::method" or SymbolPath instance

        Returns:
            ResolvedSymbol with location information

        Raises:
            SymbolNotFoundError: If symbol not found
        """
        if isinstance(symbol_path, str):
            symbol_path = SymbolPath.parse(symbol_path)

        node = self._find_node(self.tree, list(symbol_path.parts))
        if node is None:
            raise SymbolNotFoundError(str(symbol_path), self.file_path)

        location = self._get_location(node)
        source_lines = self.lines[location.start_line - 1:location.end_line]

        return ResolvedSymbol(
            name=symbol_path.parts[-1],
            path=symbol_path,
            location=location,
            node=node,
            source_lines=source_lines,
        )

    def find_all(self, name: str) -> Iterator[ResolvedSymbol]:
        """Find all symbols with the given name at any nesting level."""
        for node in ast.walk(self.tree):
            if self._get_name(node) == name:
                location = self._get_location(node)
                source_lines = self.lines[location.start_line - 1:location.end_line]
                yield ResolvedSymbol(
                    name=name,
                    path=SymbolPath(parts=(name,)),
                    location=location,
                    node=node,
                    source_lines=source_lines,
                )

    def _find_node(self, parent: ast.AST, parts: list[str]) -> ast.AST | None:
        """Recursively find node matching path parts."""
        if not parts:
            return parent

        target = parts[0]
        remaining = parts[1:]

        # Search in parent's body
        body = self._get_body(parent)
        for child in body:
            name = self._get_name(child)
            if name == target:
                if not remaining:
                    return child
                return self._find_node(child, remaining)

        return None

    def _get_body(self, node: ast.AST) -> list[ast.AST]:
        """Get the body of a node (statements it contains)."""
        if isinstance(node, ast.Module):
            return node.body
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            return node.body
        return []

    def _get_name(self, node: ast.AST) -> str | None:
        """Get the name of a definition node."""
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            return node.name
        if isinstance(node, ast.Assign):
            # Handle simple assignments like x = ...
            if len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
                return node.targets[0].id
        return None

    def _get_location(self, node: ast.AST) -> SymbolLocation:
        """Extract location info from AST node."""
        start_line = getattr(node, "lineno", 1)
        end_line = getattr(node, "end_lineno", start_line)
        start_col = getattr(node, "col_offset", 0)
        end_col = getattr(node, "end_col_offset", 0)

        # Calculate indentation from the first line
        if start_line <= len(self.lines):
            first_line = self.lines[start_line - 1]
            indent = len(first_line) - len(first_line.lstrip())
        else:
            indent = 0

        # For functions/classes, find body boundaries
        body_start: int | None = None
        body_end: int | None = None

        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            if node.body:
                first_stmt = node.body[0]
                last_stmt = node.body[-1]
                body_start = getattr(first_stmt, "lineno", None)
                body_end = getattr(last_stmt, "end_lineno", None)

        return SymbolLocation(
            start_line=start_line,
            end_line=end_line,
            start_col=start_col,
            end_col=end_col,
            indent=indent,
            body_start_line=body_start,
            body_end_line=body_end,
        )


def detect_language(file_path: Path) -> str:
    """Detect programming language from file extension."""
    ext = file_path.suffix.lower()
    lang_map = {
        ".py": "python",
        ".pyi": "python",
        ".rs": "rust",
        ".ts": "typescript",
        ".tsx": "typescript",
        ".js": "javascript",
        ".jsx": "javascript",
        ".go": "go",
        ".java": "java",
        ".rb": "ruby",
        ".c": "c",
        ".cpp": "cpp",
        ".cc": "cpp",
        ".h": "c",
        ".hpp": "cpp",
    }
    return lang_map.get(ext, "unknown")
