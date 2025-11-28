"""
Patch format parser.

Grammar (Lark-style):
    start: begin_patch hunk+ end_patch
    begin_patch: "*** Begin Patch" LF
    end_patch: "*** End Patch" LF

    hunk: add_hunk | delete_hunk | update_hunk | symbol_hunk
    add_hunk: "*** Add File: " path LF add_line+
    delete_hunk: "*** Delete File: " path LF
    update_hunk: "*** Update File: " path LF move_to? chunk+
    symbol_hunk: symbol_header target LF add_line+

    symbol_header: "*** Insert Before Symbol: " | "*** Insert After Symbol: " | "*** Replace Symbol Body: "
    target: path "::" symbol_path

    add_line: "+" line LF
    move_to: "*** Move to: " path LF
    chunk: context? change_line+ eof?
    context: ("@@" | "@@ " text) LF
    change_line: ("+" | "-" | " ") line LF
    eof: "*** End of File" LF
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from enum import Enum, auto
from pathlib import Path
from typing import Iterator


class ParseError(Exception):
    """Patch parsing error."""

    def __init__(self, message: str, line_number: int | None = None) -> None:
        self.line_number = line_number
        if line_number:
            super().__init__(f"Line {line_number}: {message}")
        else:
            super().__init__(message)


class HunkType(Enum):
    """Type of patch operation."""

    ADD_FILE = auto()
    DELETE_FILE = auto()
    UPDATE_FILE = auto()
    INSERT_BEFORE_SYMBOL = auto()
    INSERT_AFTER_SYMBOL = auto()
    REPLACE_SYMBOL_BODY = auto()


@dataclass(slots=True)
class UpdateChunk:
    """A single chunk within an Update File operation."""

    context: str | None = None  # @@ context line
    old_lines: list[str] = field(default_factory=list)
    new_lines: list[str] = field(default_factory=list)
    is_eof: bool = False  # True if chunk ends at EOF


@dataclass(slots=True)
class Hunk:
    """A single patch operation."""

    type: HunkType
    path: Path
    move_to: Path | None = None  # For Update File with rename
    contents: str | None = None  # For Add File
    chunks: list[UpdateChunk] = field(default_factory=list)  # For Update File
    symbol_path: str | None = None  # For symbol operations
    new_lines: list[str] = field(default_factory=list)  # For symbol operations


# Markers
BEGIN_PATCH = "*** Begin Patch"
END_PATCH = "*** End Patch"
ADD_FILE = "*** Add File: "
DELETE_FILE = "*** Delete File: "
UPDATE_FILE = "*** Update File: "
INSERT_BEFORE = "*** Insert Before Symbol: "
INSERT_AFTER = "*** Insert After Symbol: "
REPLACE_BODY = "*** Replace Symbol Body: "
MOVE_TO = "*** Move to: "
CONTEXT_MARKER = "@@"
EOF_MARKER = "*** End of File"


def parse_patch(patch_text: str) -> list[Hunk]:
    """
    Parse a patch string into a list of hunks.

    Args:
        patch_text: The patch content starting with *** Begin Patch

    Returns:
        List of parsed Hunk operations

    Raises:
        ParseError: If the patch format is invalid
    """
    lines = patch_text.split("\n")
    return _Parser(lines).parse()


class _Parser:
    """Internal parser state machine."""

    def __init__(self, lines: list[str]) -> None:
        self.lines = lines
        self.pos = 0
        self.hunks: list[Hunk] = []

    def parse(self) -> list[Hunk]:
        """Parse all hunks from the patch."""
        self._skip_empty()
        self._expect_begin_patch()

        while not self._at_end() and not self._peek_end_patch():
            self._parse_hunk()

        self._expect_end_patch()
        return self.hunks

    def _current_line(self) -> str:
        """Get current line (stripped)."""
        if self.pos >= len(self.lines):
            return ""
        return self.lines[self.pos].rstrip("\r")

    def _current_line_raw(self) -> str:
        """Get current line preserving leading whitespace."""
        if self.pos >= len(self.lines):
            return ""
        return self.lines[self.pos].rstrip("\r")

    def _advance(self) -> str:
        """Advance to next line, return current."""
        line = self._current_line()
        self.pos += 1
        return line

    def _at_end(self) -> bool:
        """Check if at end of input."""
        return self.pos >= len(self.lines)

    def _skip_empty(self) -> None:
        """Skip empty lines."""
        while not self._at_end() and not self._current_line().strip():
            self.pos += 1

    def _peek_end_patch(self) -> bool:
        """Check if current line is end patch."""
        return self._current_line().strip().startswith(END_PATCH)

    def _expect_begin_patch(self) -> None:
        """Expect and consume Begin Patch marker."""
        line = self._current_line().strip()
        if not line.startswith(BEGIN_PATCH):
            raise ParseError("Expected '*** Begin Patch'", self.pos + 1)
        self._advance()

    def _expect_end_patch(self) -> None:
        """Expect and consume End Patch marker."""
        self._skip_empty()
        line = self._current_line().strip()
        if not line.startswith(END_PATCH):
            raise ParseError("Expected '*** End Patch'", self.pos + 1)
        self._advance()

    def _parse_hunk(self) -> None:
        """Parse a single hunk operation."""
        self._skip_empty()
        if self._at_end() or self._peek_end_patch():
            return

        line = self._current_line().strip()
        start_pos = self.pos + 1

        if line.startswith(ADD_FILE):
            self._parse_add_file(line[len(ADD_FILE):].strip())
        elif line.startswith(DELETE_FILE):
            self._parse_delete_file(line[len(DELETE_FILE):].strip())
        elif line.startswith(UPDATE_FILE):
            self._parse_update_file(line[len(UPDATE_FILE):].strip())
        elif line.startswith(INSERT_BEFORE):
            self._parse_symbol_op(HunkType.INSERT_BEFORE_SYMBOL, line[len(INSERT_BEFORE):].strip())
        elif line.startswith(INSERT_AFTER):
            self._parse_symbol_op(HunkType.INSERT_AFTER_SYMBOL, line[len(INSERT_AFTER):].strip())
        elif line.startswith(REPLACE_BODY):
            self._parse_symbol_op(HunkType.REPLACE_SYMBOL_BODY, line[len(REPLACE_BODY):].strip())
        else:
            raise ParseError(f"Unknown operation: {line[:50]}", start_pos)

    def _parse_add_file(self, path: str) -> None:
        """Parse Add File operation."""
        self._advance()  # consume header

        lines: list[str] = []
        while not self._at_end():
            line = self._current_line_raw()
            if line.startswith("+"):
                lines.append(line[1:])  # Remove + prefix
                self._advance()
            elif self._is_operation_header(line) or self._peek_end_patch():
                break
            elif line.strip() == "":
                # Empty line might be part of content or separator
                # Check if next non-empty is an operation
                self._advance()
            else:
                raise ParseError(f"Expected '+' prefixed line in Add File, got: {line[:50]}", self.pos + 1)

        self.hunks.append(Hunk(
            type=HunkType.ADD_FILE,
            path=Path(path),
            contents="\n".join(lines),
        ))

    def _parse_delete_file(self, path: str) -> None:
        """Parse Delete File operation."""
        self._advance()  # consume header
        self.hunks.append(Hunk(
            type=HunkType.DELETE_FILE,
            path=Path(path),
        ))

    def _parse_update_file(self, path: str) -> None:
        """Parse Update File operation with hunks."""
        self._advance()  # consume header

        move_to: Path | None = None
        chunks: list[UpdateChunk] = []

        # Check for Move to
        if not self._at_end():
            line = self._current_line().strip()
            if line.startswith(MOVE_TO):
                move_to = Path(line[len(MOVE_TO):].strip())
                self._advance()

        # Parse chunks
        while not self._at_end() and not self._is_operation_header(self._current_line()) and not self._peek_end_patch():
            chunk = self._parse_update_chunk()
            if chunk:
                chunks.append(chunk)

        if not chunks:
            raise ParseError(f"Update File requires at least one chunk", self.pos + 1)

        self.hunks.append(Hunk(
            type=HunkType.UPDATE_FILE,
            path=Path(path),
            move_to=move_to,
            chunks=chunks,
        ))

    def _parse_update_chunk(self) -> UpdateChunk | None:
        """Parse a single update chunk (@@ ... changes ...)."""
        context: str | None = None
        old_lines: list[str] = []
        new_lines: list[str] = []
        is_eof = False

        # Skip empty lines
        while not self._at_end() and self._current_line().strip() == "":
            self._advance()

        if self._at_end():
            return None

        line = self._current_line()

        # Check for context marker
        if line.strip().startswith(CONTEXT_MARKER):
            ctx = line.strip()
            if ctx == CONTEXT_MARKER or ctx == "@@ ":
                context = None
            else:
                # Extract context after @@
                context = ctx[3:].strip() if len(ctx) > 3 else None
            self._advance()

        # Parse change lines
        while not self._at_end():
            line = self._current_line_raw()
            stripped = line.strip()

            if self._is_operation_header(line) or self._peek_end_patch():
                break
            if stripped.startswith(CONTEXT_MARKER):
                break  # New chunk
            if stripped == EOF_MARKER:
                is_eof = True
                self._advance()
                break

            if line.startswith("+"):
                new_lines.append(line[1:])
                self._advance()
            elif line.startswith("-"):
                old_lines.append(line[1:])
                self._advance()
            elif line.startswith(" "):
                # Context line (unchanged)
                old_lines.append(line[1:])
                new_lines.append(line[1:])
                self._advance()
            elif stripped == "":
                # Empty line - might be separator
                self._advance()
            else:
                # Non-prefixed line - treat as context
                old_lines.append(line)
                new_lines.append(line)
                self._advance()

        if not old_lines and not new_lines:
            return None

        return UpdateChunk(
            context=context,
            old_lines=old_lines,
            new_lines=new_lines,
            is_eof=is_eof,
        )

    def _parse_symbol_op(self, op_type: HunkType, target: str) -> None:
        """Parse symbol-based operation."""
        self._advance()  # consume header

        # Parse target: file::Symbol::path
        if "::" not in target:
            raise ParseError(f"Symbol target must contain '::', got: {target}", self.pos)

        parts = target.split("::", 1)
        file_path = Path(parts[0].strip())
        symbol_path = parts[1].strip()

        # Collect + lines
        lines: list[str] = []
        while not self._at_end():
            line = self._current_line_raw()
            if line.startswith("+"):
                lines.append(line[1:])
                self._advance()
            elif self._is_operation_header(line) or self._peek_end_patch():
                break
            elif line.strip() == "":
                self._advance()
            else:
                raise ParseError(f"Expected '+' line in symbol operation, got: {line[:50]}", self.pos + 1)

        self.hunks.append(Hunk(
            type=op_type,
            path=file_path,
            symbol_path=symbol_path,
            new_lines=lines,
        ))

    def _is_operation_header(self, line: str) -> bool:
        """Check if line is an operation header."""
        stripped = line.strip()
        return any(stripped.startswith(marker) for marker in [
            ADD_FILE, DELETE_FILE, UPDATE_FILE,
            INSERT_BEFORE, INSERT_AFTER, REPLACE_BODY,
            END_PATCH,
        ])
