"""
Codex Patch - Deterministic patch format for AI-assisted code editing.

This module provides a robust implementation of the apply_patch tool format,
supporting file operations (add, delete, update) and symbol-based operations
for precise code modifications.

Format:
    *** Begin Patch
    ... operations ...
    *** End Patch

Operations:
    - Add File: path       Create or replace a file
    - Delete File: path    Remove a file
    - Update File: path    Apply unified diff hunks
    - Insert Before Symbol: file::Symbol::path  Insert lines before symbol
    - Insert After Symbol: file::Symbol::path   Insert lines after symbol
    - Replace Symbol Body: file::Symbol::path   Replace symbol body
"""

from codex_patch.applier import (
    ApplyResult,
    ApplyStatus,
    FileChange,
    PatchApplier,
    apply_patch,
)
from codex_patch.parser import (
    Hunk,
    HunkType,
    ParseError,
    UpdateChunk,
    parse_patch,
)
from codex_patch.symbols import (
    SymbolNotFoundError,
    SymbolPath,
    SymbolResolver,
)

__all__ = [
    # Parser
    "Hunk",
    "HunkType",
    "UpdateChunk",
    "parse_patch",
    "ParseError",
    # Applier
    "PatchApplier",
    "ApplyResult",
    "ApplyStatus",
    "FileChange",
    "apply_patch",
    # Symbols
    "SymbolPath",
    "SymbolResolver",
    "SymbolNotFoundError",
]
