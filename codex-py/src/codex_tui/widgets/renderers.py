"""Rich renderers for specialized content types.

Provides diff rendering, code highlighting, and other visual formatters.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from rich.console import RenderableType
from rich.panel import Panel
from rich.syntax import Syntax
from rich.text import Text


@dataclass
class DiffStats:
    """Statistics for a diff."""

    additions: int = 0
    deletions: int = 0
    files: int = 0


def render_diff(diff_content: str, title: str | None = None) -> RenderableType:
    """Render a git diff with syntax highlighting.

    Args:
        diff_content: The raw diff text
        title: Optional title (e.g., file path)

    Returns:
        Rich renderable with colored diff
    """
    text = Text()
    stats = DiffStats()

    lines = diff_content.split("\n")
    current_file: str | None = None

    for line in lines:
        # File headers
        if line.startswith("diff --git"):
            stats.files += 1
            # Extract file path from "diff --git a/path b/path"
            match = re.search(r"b/(.+)$", line)
            if match:
                current_file = match.group(1)
            text.append(line + "\n", style="bold blue")

        elif line.startswith("---") or line.startswith("+++"):
            text.append(line + "\n", style="dim")

        elif line.startswith("@@"):
            # Hunk header - show line numbers
            text.append(line + "\n", style="cyan")

        elif line.startswith("+") and not line.startswith("+++"):
            # Addition
            stats.additions += 1
            text.append(line + "\n", style="green")

        elif line.startswith("-") and not line.startswith("---"):
            # Deletion
            stats.deletions += 1
            text.append(line + "\n", style="red")

        elif line.startswith("index ") or line.startswith("new file"):
            text.append(line + "\n", style="dim")

        else:
            # Context line
            text.append(line + "\n", style="dim")

    # Build title with stats
    if title:
        panel_title = title
    elif current_file:
        panel_title = current_file
    else:
        panel_title = "Diff"

    # Add stats to title
    stats_text = []
    if stats.additions:
        stats_text.append(f"+{stats.additions}")
    if stats.deletions:
        stats_text.append(f"-{stats.deletions}")
    if stats_text:
        panel_title = f"{panel_title} ({', '.join(stats_text)})"

    return Panel(
        text,
        title=panel_title,
        title_align="left",
        border_style="yellow",
        padding=(0, 1),
    )


def render_code_block(code: str, language: str = "python") -> RenderableType:
    """Render a code block with syntax highlighting.

    Args:
        code: The source code
        language: Programming language for highlighting

    Returns:
        Rich Syntax object
    """
    return Syntax(
        code.strip(),
        language,
        theme="monokai",
        line_numbers=False,
        word_wrap=True,
    )


def render_json(data: str) -> RenderableType:
    """Render JSON with syntax highlighting."""
    return Syntax(
        data,
        "json",
        theme="monokai",
        line_numbers=False,
        word_wrap=True,
    )


def render_file_path(path: str, exists: bool = True) -> Text:
    """Render a file path with appropriate styling."""
    text = Text()
    if exists:
        text.append("📄 ", style="dim")
        text.append(path, style="cyan underline")
    else:
        text.append("❓ ", style="dim")
        text.append(path, style="yellow")
    return text


def render_thinking_indicator(message: str = "Thinking") -> Text:
    """Render a thinking/processing indicator."""
    text = Text()
    text.append("◐ ", style="yellow bold")
    text.append(message, style="yellow italic")
    text.append("...", style="dim")
    return text


def render_success_indicator(message: str) -> Text:
    """Render a success message."""
    text = Text()
    text.append("✓ ", style="green bold")
    text.append(message, style="green")
    return text


def render_error_indicator(message: str) -> Text:
    """Render an error message."""
    text = Text()
    text.append("✗ ", style="red bold")
    text.append(message, style="red")
    return text


def render_warning_indicator(message: str) -> Text:
    """Render a warning message."""
    text = Text()
    text.append("⚠ ", style="yellow bold")
    text.append(message, style="yellow")
    return text


def truncate_output(output: str, max_lines: int = 15, max_chars: int = 2000) -> tuple[str, bool]:
    """Truncate output to reasonable size.

    Args:
        output: The output text
        max_lines: Maximum number of lines
        max_chars: Maximum number of characters

    Returns:
        Tuple of (truncated_output, was_truncated)
    """
    lines = output.split("\n")
    truncated = False

    # Truncate by lines
    if len(lines) > max_lines:
        lines = lines[:max_lines]
        truncated = True

    result = "\n".join(lines)

    # Truncate by characters
    if len(result) > max_chars:
        result = result[:max_chars]
        truncated = True

    return result, truncated
