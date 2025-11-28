"""Streaming markdown renderer widget.

Handles incremental markdown rendering during streaming.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import TYPE_CHECKING

from rich.console import RenderableType
from rich.markdown import Markdown
from rich.syntax import Syntax
from rich.text import Text
from textual.reactive import reactive
from textual.widgets import Static

if TYPE_CHECKING:
    pass


@dataclass(slots=True)
class CodeBlock:
    """Represents a fenced code block."""

    language: str
    content: str
    complete: bool = False


class StreamingMarkdown(Static):
    """Widget that renders markdown incrementally.

    Handles partial code blocks and incomplete markdown gracefully.
    """

    DEFAULT_CSS = """
    StreamingMarkdown {
        height: auto;
    }
    """

    content: reactive[str] = reactive("", init=False)

    def __init__(self, initial_content: str = "", **kwargs: object) -> None:
        super().__init__(**kwargs)
        self.content = initial_content

    def update_content(self, text: str) -> None:
        """Update the markdown content."""
        self.content = text
        self.refresh()

    def append_content(self, text: str) -> None:
        """Append to the markdown content."""
        self.content += text
        self.refresh()

    def render(self) -> RenderableType:
        if not self.content:
            return Text()

        # Check for incomplete code blocks
        # A code block starts with ``` and ends with ```
        content = self.content
        fence_pattern = re.compile(r"```(\w*)\n")
        fence_matches = list(fence_pattern.finditer(content))

        # Count opening and closing fences
        open_fences = len(fence_matches)
        close_fences = content.count("\n```")

        if open_fences > close_fences:
            # We have an incomplete code block
            # Find the last incomplete block
            last_fence = fence_matches[-1]
            before_block = content[: last_fence.start()]
            block_content = content[last_fence.end() :]
            language = last_fence.group(1) or "text"

            # Render what we have
            parts = []

            # Render completed markdown before the code block
            if before_block.strip():
                parts.append(Markdown(before_block))

            # Render incomplete code block with syntax highlighting
            # Add a visual indicator that it's streaming
            code_with_cursor = block_content + "\u2588"  # Block cursor

            syntax = Syntax(
                code_with_cursor,
                language,
                theme="monokai",
                line_numbers=True,
                word_wrap=True,
            )
            parts.append(syntax)

            if len(parts) == 1:
                return parts[0]

            # Combine parts
            from rich.console import Group

            return Group(*parts)

        # Complete content - render as normal markdown
        try:
            return Markdown(content)
        except Exception:
            # Fallback to plain text if markdown parsing fails
            return Text(content)


class MarkdownCodeBlock(Static):
    """Widget for rendering a single code block with syntax highlighting."""

    DEFAULT_CSS = """
    MarkdownCodeBlock {
        height: auto;
        margin: 1 0;
    }
    """

    def __init__(
        self,
        code: str,
        language: str = "text",
        **kwargs: object,
    ) -> None:
        super().__init__(**kwargs)
        self.code = code
        self.language = language

    def render(self) -> RenderableType:
        return Syntax(
            self.code,
            self.language,
            theme="monokai",
            line_numbers=True,
            word_wrap=True,
        )


class ThinkingIndicator(Static):
    """Widget showing streaming thinking/reasoning content."""

    DEFAULT_CSS = """
    ThinkingIndicator {
        height: auto;
        border: dashed $primary;
        padding: 1;
        margin: 1 0;
    }
    """

    content: reactive[str] = reactive("", init=False)

    def __init__(self, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self.content = ""

    def update_content(self, text: str) -> None:
        """Update thinking content."""
        self.content = text
        self.refresh()

    def render(self) -> RenderableType:
        if not self.content:
            return Text("Thinking...", style="italic dim")

        text = Text()
        text.append("Thinking:\n", style="bold cyan")
        text.append(self.content, style="dim")
        return text


def extract_code_blocks(markdown_text: str) -> list[tuple[str, str, str]]:
    """Extract code blocks from markdown.

    Returns list of (before, language, code) tuples.
    """
    blocks: list[tuple[str, str, str]] = []
    pattern = re.compile(r"```(\w*)\n(.*?)```", re.DOTALL)

    last_end = 0
    for match in pattern.finditer(markdown_text):
        before = markdown_text[last_end : match.start()]
        language = match.group(1) or "text"
        code = match.group(2)
        blocks.append((before, language, code))
        last_end = match.end()

    # Add remaining text after last code block
    remaining = markdown_text[last_end:]
    if remaining.strip():
        blocks.append((remaining, "", ""))

    return blocks


def render_markdown_with_code(markdown_text: str) -> RenderableType:
    """Render markdown with proper code block syntax highlighting.

    This is used for final rendering (non-streaming).
    """
    # Check if there are any code blocks
    if "```" not in markdown_text:
        return Markdown(markdown_text)

    # Extract and render code blocks separately
    from rich.console import Group

    parts: list[RenderableType] = []
    blocks = extract_code_blocks(markdown_text)

    for before, language, code in blocks:
        if before.strip():
            parts.append(Markdown(before))
        if code:
            parts.append(
                Syntax(
                    code,
                    language,
                    theme="monokai",
                    line_numbers=True,
                    word_wrap=True,
                )
            )

    if not parts:
        return Text()
    if len(parts) == 1:
        return parts[0]

    return Group(*parts)
