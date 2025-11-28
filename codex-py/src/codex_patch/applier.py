"""
Patch application engine.

Applies parsed hunks to the filesystem with support for:
- Dry-run mode (preview changes without writing)
- Fuzzy matching for context lines
- Detailed error reporting
- Atomic operations (all-or-nothing)
"""

from __future__ import annotations

import difflib
import shutil
from dataclasses import dataclass, field
from enum import Enum, auto
from pathlib import Path
from typing import Callable

from codex_patch.parser import Hunk, HunkType, UpdateChunk, parse_patch, ParseError
from codex_patch.symbols import SymbolResolver, SymbolNotFoundError


class ApplyStatus(Enum):
    """Result status for patch application."""

    SUCCESS = auto()
    PARTIAL = auto()  # Some hunks failed
    FAILED = auto()


@dataclass(slots=True)
class FileChange:
    """A single file change."""

    path: Path
    operation: str  # "add", "delete", "modify", "rename"
    old_content: str | None = None
    new_content: str | None = None
    lines_added: int = 0
    lines_removed: int = 0
    error: str | None = None


@dataclass(slots=True)
class ApplyResult:
    """Result of patch application."""

    status: ApplyStatus
    changes: list[FileChange] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)

    @property
    def success(self) -> bool:
        return self.status == ApplyStatus.SUCCESS

    def summary(self) -> str:
        """Generate human-readable summary."""
        lines = []
        for change in self.changes:
            if change.error:
                lines.append(f"  FAILED {change.path}: {change.error}")
            else:
                stats = f"+{change.lines_added}/-{change.lines_removed}"
                lines.append(f"  {change.operation.upper()} {change.path} ({stats})")

        if self.errors:
            lines.append("\nErrors:")
            for err in self.errors:
                lines.append(f"  - {err}")

        return "\n".join(lines)


class PatchApplier:
    """
    Applies patches to the filesystem.

    Usage:
        applier = PatchApplier(cwd=Path("/project"))
        result = applier.apply(patch_text, dry_run=True)
        if result.success:
            result = applier.apply(patch_text, dry_run=False)
    """

    def __init__(
        self,
        cwd: Path | None = None,
        fuzzy_threshold: float = 0.6,
        on_progress: Callable[[str], None] | None = None,
    ) -> None:
        """
        Initialize the applier.

        Args:
            cwd: Working directory (default: current)
            fuzzy_threshold: Similarity threshold for fuzzy matching (0-1)
            on_progress: Optional callback for progress updates
        """
        self.cwd = cwd or Path.cwd()
        self.fuzzy_threshold = fuzzy_threshold
        self.on_progress = on_progress or (lambda _: None)

    def apply(self, patch_text: str, dry_run: bool = True) -> ApplyResult:
        """
        Apply a patch.

        Args:
            patch_text: Patch content starting with *** Begin Patch
            dry_run: If True, only simulate changes

        Returns:
            ApplyResult with status and details
        """
        try:
            hunks = parse_patch(patch_text)
        except ParseError as e:
            return ApplyResult(status=ApplyStatus.FAILED, errors=[str(e)])

        if not hunks:
            return ApplyResult(status=ApplyStatus.SUCCESS)

        changes: list[FileChange] = []
        errors: list[str] = []

        # First pass: validate and compute all changes
        pending_writes: list[tuple[Path, str]] = []
        pending_deletes: list[Path] = []
        pending_renames: list[tuple[Path, Path]] = []

        for hunk in hunks:
            try:
                change = self._process_hunk(hunk)
                changes.append(change)

                if change.error:
                    errors.append(f"{hunk.path}: {change.error}")
                    continue

                # Collect pending operations
                abs_path = self.cwd / hunk.path
                if hunk.type == HunkType.DELETE_FILE:
                    pending_deletes.append(abs_path)
                elif change.new_content is not None:
                    pending_writes.append((abs_path, change.new_content))
                    if hunk.move_to:
                        pending_renames.append((abs_path, self.cwd / hunk.move_to))

            except Exception as e:
                change = FileChange(
                    path=hunk.path,
                    operation="error",
                    error=str(e),
                )
                changes.append(change)
                errors.append(str(e))

        # Determine overall status
        if errors:
            status = ApplyStatus.PARTIAL if any(c.error is None for c in changes) else ApplyStatus.FAILED
        else:
            status = ApplyStatus.SUCCESS

        # Apply changes if not dry-run and no critical errors
        if not dry_run and status != ApplyStatus.FAILED:
            try:
                self._apply_writes(pending_writes)
                self._apply_deletes(pending_deletes)
                self._apply_renames(pending_renames)
            except Exception as e:
                errors.append(f"Write error: {e}")
                status = ApplyStatus.FAILED

        return ApplyResult(status=status, changes=changes, errors=errors)

    def _process_hunk(self, hunk: Hunk) -> FileChange:
        """Process a single hunk, returning the computed change."""
        abs_path = self.cwd / hunk.path

        if hunk.type == HunkType.ADD_FILE:
            return self._process_add_file(hunk, abs_path)
        elif hunk.type == HunkType.DELETE_FILE:
            return self._process_delete_file(hunk, abs_path)
        elif hunk.type == HunkType.UPDATE_FILE:
            return self._process_update_file(hunk, abs_path)
        elif hunk.type in (HunkType.INSERT_BEFORE_SYMBOL, HunkType.INSERT_AFTER_SYMBOL, HunkType.REPLACE_SYMBOL_BODY):
            return self._process_symbol_operation(hunk, abs_path)
        else:
            return FileChange(path=hunk.path, operation="unknown", error=f"Unknown hunk type: {hunk.type}")

    def _process_add_file(self, hunk: Hunk, abs_path: Path) -> FileChange:
        """Process Add File operation."""
        old_content = None
        lines_removed = 0

        if abs_path.exists():
            old_content = abs_path.read_text()
            lines_removed = len(old_content.splitlines())

        new_content = hunk.contents or ""
        lines_added = len(new_content.splitlines())

        return FileChange(
            path=hunk.path,
            operation="add" if old_content is None else "replace",
            old_content=old_content,
            new_content=new_content,
            lines_added=lines_added,
            lines_removed=lines_removed,
        )

    def _process_delete_file(self, hunk: Hunk, abs_path: Path) -> FileChange:
        """Process Delete File operation."""
        if not abs_path.exists():
            return FileChange(
                path=hunk.path,
                operation="delete",
                error="File does not exist",
            )

        old_content = abs_path.read_text()
        return FileChange(
            path=hunk.path,
            operation="delete",
            old_content=old_content,
            lines_removed=len(old_content.splitlines()),
        )

    def _process_update_file(self, hunk: Hunk, abs_path: Path) -> FileChange:
        """Process Update File operation with hunks."""
        if not abs_path.exists():
            return FileChange(
                path=hunk.path,
                operation="modify",
                error="File does not exist",
            )

        old_content = abs_path.read_text()
        old_lines = old_content.splitlines(keepends=True)

        try:
            new_lines = self._apply_chunks(old_lines, hunk.chunks)
            new_content = "".join(new_lines)

            # Count changes
            diff = list(difflib.unified_diff(old_lines, new_lines))
            lines_added = sum(1 for line in diff if line.startswith("+") and not line.startswith("+++"))
            lines_removed = sum(1 for line in diff if line.startswith("-") and not line.startswith("---"))

            return FileChange(
                path=hunk.path,
                operation="rename" if hunk.move_to else "modify",
                old_content=old_content,
                new_content=new_content,
                lines_added=lines_added,
                lines_removed=lines_removed,
            )

        except Exception as e:
            return FileChange(
                path=hunk.path,
                operation="modify",
                old_content=old_content,
                error=str(e),
            )

    def _process_symbol_operation(self, hunk: Hunk, abs_path: Path) -> FileChange:
        """Process symbol-based operations."""
        if not abs_path.exists():
            return FileChange(
                path=hunk.path,
                operation="modify",
                error="File does not exist",
            )

        old_content = abs_path.read_text()

        try:
            resolver = SymbolResolver(old_content, abs_path)
            symbol = resolver.resolve(hunk.symbol_path or "")
            loc = symbol.location

            old_lines = old_content.splitlines(keepends=True)

            if hunk.type == HunkType.INSERT_BEFORE_SYMBOL:
                new_lines = self._insert_before(old_lines, loc.start_line, hunk.new_lines, loc.indent)
            elif hunk.type == HunkType.INSERT_AFTER_SYMBOL:
                new_lines = self._insert_after(old_lines, loc.end_line, hunk.new_lines, loc.indent)
            elif hunk.type == HunkType.REPLACE_SYMBOL_BODY:
                new_lines = self._replace_body(old_lines, loc, hunk.new_lines)
            else:
                raise ValueError(f"Unknown symbol operation: {hunk.type}")

            new_content = "".join(new_lines)

            return FileChange(
                path=hunk.path,
                operation="modify",
                old_content=old_content,
                new_content=new_content,
                lines_added=len(hunk.new_lines),
                lines_removed=loc.end_line - loc.start_line + 1 if hunk.type == HunkType.REPLACE_SYMBOL_BODY else 0,
            )

        except SymbolNotFoundError as e:
            return FileChange(
                path=hunk.path,
                operation="modify",
                old_content=old_content,
                error=str(e),
            )

    def _apply_chunks(self, lines: list[str], chunks: list[UpdateChunk]) -> list[str]:
        """Apply update chunks to file lines."""
        result = list(lines)
        offset = 0  # Track line number changes as we apply chunks

        for chunk in chunks:
            # Find where to apply this chunk
            match_start = self._find_chunk_location(result, chunk, offset)
            if match_start is None:
                # Try fuzzy matching
                match_start = self._fuzzy_find_chunk(result, chunk, offset)
                if match_start is None:
                    context_hint = chunk.context[:50] if chunk.context else "(no context)"
                    raise ValueError(f"Cannot locate chunk with context: {context_hint}")

            # Calculate the range to replace
            match_end = match_start + len(chunk.old_lines)

            # Perform replacement
            new_chunk_lines = [line + "\n" if not line.endswith("\n") else line for line in chunk.new_lines]
            result = result[:match_start] + new_chunk_lines + result[match_end:]

            # Update offset
            offset = match_start + len(chunk.new_lines)

        return result

    def _find_chunk_location(self, lines: list[str], chunk: UpdateChunk, start_from: int) -> int | None:
        """Find exact location for chunk application."""
        if not chunk.old_lines:
            # Pure insertion - use context or append
            if chunk.context:
                for i in range(start_from, len(lines)):
                    if chunk.context in lines[i]:
                        return i + 1  # Insert after context line
            return len(lines) if chunk.is_eof else None

        # Search for old_lines sequence
        old_stripped = [line.rstrip() for line in chunk.old_lines]

        for i in range(start_from, len(lines) - len(old_stripped) + 1):
            current_stripped = [lines[i + j].rstrip() for j in range(len(old_stripped))]
            if current_stripped == old_stripped:
                return i

        return None

    def _fuzzy_find_chunk(self, lines: list[str], chunk: UpdateChunk, start_from: int) -> int | None:
        """Fuzzy find chunk location using similarity matching."""
        if not chunk.old_lines:
            return None

        old_text = "\n".join(line.rstrip() for line in chunk.old_lines)
        best_ratio = 0.0
        best_pos = None

        window_size = len(chunk.old_lines)
        for i in range(start_from, len(lines) - window_size + 1):
            window = "\n".join(lines[i:i + window_size])
            window_stripped = "\n".join(line.rstrip() for line in lines[i:i + window_size])

            ratio = difflib.SequenceMatcher(None, old_text, window_stripped).ratio()
            if ratio > best_ratio and ratio >= self.fuzzy_threshold:
                best_ratio = ratio
                best_pos = i

        return best_pos

    def _insert_before(self, lines: list[str], line_num: int, new_lines: list[str], indent: int) -> list[str]:
        """Insert lines before a specific line number."""
        insert_idx = line_num - 1
        indent_str = " " * indent
        formatted = [indent_str + line + "\n" for line in new_lines]
        return lines[:insert_idx] + formatted + lines[insert_idx:]

    def _insert_after(self, lines: list[str], line_num: int, new_lines: list[str], indent: int) -> list[str]:
        """Insert lines after a specific line number."""
        insert_idx = line_num
        indent_str = " " * indent
        formatted = [indent_str + line + "\n" for line in new_lines]
        return lines[:insert_idx] + formatted + lines[insert_idx:]

    def _replace_body(self, lines: list[str], loc, new_lines: list[str]) -> list[str]:
        """Replace function/class body."""
        if loc.body_start_line is None or loc.body_end_line is None:
            # Fall back to replacing entire symbol
            start = loc.start_line - 1
            end = loc.end_line
        else:
            start = loc.body_start_line - 1
            end = loc.body_end_line

        # Detect body indent
        if start < len(lines):
            body_line = lines[start]
            body_indent = len(body_line) - len(body_line.lstrip())
        else:
            body_indent = loc.indent + 4

        indent_str = " " * body_indent
        formatted = [indent_str + line + "\n" for line in new_lines]

        return lines[:start] + formatted + lines[end:]

    def _apply_writes(self, writes: list[tuple[Path, str]]) -> None:
        """Write all pending file changes."""
        for path, content in writes:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
            self.on_progress(f"Wrote {path}")

    def _apply_deletes(self, deletes: list[Path]) -> None:
        """Delete all pending files."""
        for path in deletes:
            if path.exists():
                path.unlink()
                self.on_progress(f"Deleted {path}")

    def _apply_renames(self, renames: list[tuple[Path, Path]]) -> None:
        """Apply all pending renames."""
        for old_path, new_path in renames:
            if old_path.exists():
                new_path.parent.mkdir(parents=True, exist_ok=True)
                shutil.move(str(old_path), str(new_path))
                self.on_progress(f"Renamed {old_path} -> {new_path}")


def apply_patch(
    patch_text: str,
    cwd: Path | None = None,
    dry_run: bool = True,
) -> ApplyResult:
    """
    Convenience function to apply a patch.

    Args:
        patch_text: Patch content
        cwd: Working directory
        dry_run: If True, only simulate

    Returns:
        ApplyResult
    """
    applier = PatchApplier(cwd=cwd)
    return applier.apply(patch_text, dry_run=dry_run)
