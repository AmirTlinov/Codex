"""Diff view widget for displaying file changes.

Renders unified diff format with syntax highlighting.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from rich.console import RenderableType
from rich.panel import Panel
from rich.syntax import Syntax
from rich.text import Text
from textual.binding import Binding
from textual.containers import Container, ScrollableContainer
from textual.message import Message
from textual.screen import ModalScreen
from textual.widgets import Static

if TYPE_CHECKING:
    from textual.app import ComposeResult


@dataclass(slots=True)
class DiffHunk:
    """A single hunk in a diff."""

    old_start: int
    old_count: int
    new_start: int
    new_count: int
    lines: list[tuple[str, str]]  # (type, content) - type: ' ', '+', '-'


@dataclass(slots=True)
class FileDiff:
    """Diff for a single file."""

    path: str
    old_path: str | None = None
    is_new: bool = False
    is_deleted: bool = False
    is_binary: bool = False
    hunks: list[DiffHunk] | None = None
    raw_diff: str | None = None


def parse_unified_diff(diff_text: str) -> list[FileDiff]:
    """Parse unified diff format into structured data."""
    files: list[FileDiff] = []
    current_file: FileDiff | None = None
    current_hunk: DiffHunk | None = None

    lines = diff_text.split("\n")
    i = 0

    while i < len(lines):
        line = lines[i]

        # File header
        if line.startswith("--- "):
            old_path = line[4:].split("\t")[0]
            if old_path == "/dev/null":
                old_path = None

            # Next line should be +++
            i += 1
            if i < len(lines) and lines[i].startswith("+++ "):
                new_path = lines[i][4:].split("\t")[0]
                if new_path == "/dev/null":
                    # File deleted
                    current_file = FileDiff(
                        path=old_path or "unknown",
                        is_deleted=True,
                        hunks=[],
                    )
                elif old_path is None:
                    # New file
                    current_file = FileDiff(
                        path=new_path,
                        is_new=True,
                        hunks=[],
                    )
                else:
                    # Modified file
                    current_file = FileDiff(
                        path=new_path,
                        old_path=old_path if old_path != new_path else None,
                        hunks=[],
                    )
                files.append(current_file)

        # Hunk header
        elif line.startswith("@@ "):
            if current_file and current_file.hunks is not None:
                # Parse @@ -old_start,old_count +new_start,new_count @@
                parts = line.split(" ")
                try:
                    old_info = parts[1][1:].split(",")
                    new_info = parts[2][1:].split(",")

                    old_start = int(old_info[0])
                    old_count = int(old_info[1]) if len(old_info) > 1 else 1
                    new_start = int(new_info[0])
                    new_count = int(new_info[1]) if len(new_info) > 1 else 1

                    current_hunk = DiffHunk(
                        old_start=old_start,
                        old_count=old_count,
                        new_start=new_start,
                        new_count=new_count,
                        lines=[],
                    )
                    current_file.hunks.append(current_hunk)
                except (IndexError, ValueError):
                    pass

        # Diff content
        elif current_hunk is not None:
            if line.startswith("+"):
                current_hunk.lines.append(("+", line[1:]))
            elif line.startswith("-"):
                current_hunk.lines.append(("-", line[1:]))
            elif line.startswith(" "):
                current_hunk.lines.append((" ", line[1:]))
            elif line.startswith("\\"):
                # "\ No newline at end of file"
                pass
            elif not line:
                # Empty line in diff context
                current_hunk.lines.append((" ", ""))

        i += 1

    return files


class DiffHunkWidget(Static):
    """Widget displaying a single diff hunk."""

    def __init__(self, hunk: DiffHunk, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self.hunk = hunk

    def render(self) -> RenderableType:
        text = Text()

        # Hunk header
        header = (
            f"@@ -{self.hunk.old_start},{self.hunk.old_count} "
            f"+{self.hunk.new_start},{self.hunk.new_count} @@"
        )
        text.append(header + "\n", style="cyan bold")

        # Diff lines
        for line_type, content in self.hunk.lines:
            if line_type == "+":
                text.append("+" + content + "\n", style="green")
            elif line_type == "-":
                text.append("-" + content + "\n", style="red")
            else:
                text.append(" " + content + "\n", style="dim")

        return text


class FileDiffWidget(Static):
    """Widget displaying diff for a single file."""

    def __init__(self, file_diff: FileDiff, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self.file_diff = file_diff

    def render(self) -> RenderableType:
        fd = self.file_diff

        # File header
        if fd.is_new:
            title = f"NEW: {fd.path}"
            border_style = "green"
        elif fd.is_deleted:
            title = f"DELETED: {fd.path}"
            border_style = "red"
        elif fd.old_path:
            title = f"RENAMED: {fd.old_path} -> {fd.path}"
            border_style = "yellow"
        else:
            title = f"MODIFIED: {fd.path}"
            border_style = "cyan"

        # Build content
        content = Text()

        if fd.is_binary:
            content.append("(binary file)", style="dim italic")
        elif fd.raw_diff:
            # Show raw diff with syntax highlighting
            return Panel(
                Syntax(fd.raw_diff, "diff", theme="monokai"),
                title=title,
                border_style=border_style,
            )
        elif fd.hunks:
            for hunk in fd.hunks:
                # Hunk header
                header = (
                    f"@@ -{hunk.old_start},{hunk.old_count} "
                    f"+{hunk.new_start},{hunk.new_count} @@\n"
                )
                content.append(header, style="cyan bold")

                # Lines
                for line_type, line_content in hunk.lines:
                    if line_type == "+":
                        content.append("+" + line_content + "\n", style="green")
                    elif line_type == "-":
                        content.append("-" + line_content + "\n", style="red")
                    else:
                        content.append(" " + line_content + "\n")
        else:
            content.append("(no changes)", style="dim italic")

        return Panel(content, title=title, border_style=border_style)


class DiffView(ScrollableContainer):
    """Scrollable view of multiple file diffs."""

    DEFAULT_CSS = """
    DiffView {
        height: 1fr;
        padding: 1;
    }

    DiffView FileDiffWidget {
        margin-bottom: 1;
    }
    """

    def __init__(self, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self._diffs: list[FileDiff] = []

    def set_diff(self, diff_text: str) -> None:
        """Set diff content from unified diff text."""
        self._diffs = parse_unified_diff(diff_text)
        self._rebuild()

    def set_diffs(self, diffs: list[FileDiff]) -> None:
        """Set diff content from parsed diffs."""
        self._diffs = diffs
        self._rebuild()

    def _rebuild(self) -> None:
        """Rebuild the view with current diffs."""
        # Clear existing widgets
        for child in list(self.children):
            child.remove()

        # Add new widgets
        for diff in self._diffs:
            self.mount(FileDiffWidget(diff))


class DiffOverlay(ModalScreen[bool]):
    """Modal overlay showing a diff with accept/reject options."""

    DEFAULT_CSS = """
    DiffOverlay {
        align: center middle;
    }

    DiffOverlay > Container {
        width: 90%;
        height: 90%;
        background: $surface;
        border: solid $primary;
    }

    DiffOverlay #diff-header {
        dock: top;
        height: 3;
        background: $primary;
        padding: 0 2;
        content-align: center middle;
    }

    DiffOverlay #diff-content {
        height: 1fr;
    }

    DiffOverlay #diff-footer {
        dock: bottom;
        height: 3;
        layout: horizontal;
        align: center middle;
        background: $surface-darken-1;
        padding: 0 2;
    }

    DiffOverlay .hint {
        margin: 0 2;
    }
    """

    BINDINGS = [
        Binding("y", "accept", "Accept", show=True),
        Binding("n", "reject", "Reject", show=True),
        Binding("escape", "reject", "Cancel", show=False),
    ]

    def __init__(
        self,
        diff_text: str,
        title: str = "Review Changes",
        **kwargs: object,
    ) -> None:
        super().__init__(**kwargs)
        self.diff_text = diff_text
        self.title_text = title

    def compose(self) -> ComposeResult:
        with Container():
            yield Static(self.title_text, id="diff-header")
            diff_view = DiffView(id="diff-content")
            yield diff_view
            with Container(id="diff-footer"):
                yield Static("[y] Accept", classes="hint")
                yield Static("[n] Reject", classes="hint")

    def on_mount(self) -> None:
        """Initialize diff view."""
        diff_view = self.query_one("#diff-content", DiffView)
        diff_view.set_diff(self.diff_text)

    def action_accept(self) -> None:
        """Accept the changes."""
        self.dismiss(True)

    def action_reject(self) -> None:
        """Reject the changes."""
        self.dismiss(False)
