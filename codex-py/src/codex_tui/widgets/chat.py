"""Chat history widget - displays conversation with command outputs.

Inspired by codex-rs history_cell.rs architecture.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import Enum
from typing import TYPE_CHECKING, Any

from rich.console import Group, RenderableType
from rich.markdown import Markdown
from rich.panel import Panel
from rich.text import Text
from textual.containers import VerticalScroll
from textual.widgets import Static

from codex_tui.widgets.renderers import (
    render_diff,
    render_error_indicator,
    render_success_indicator,
    render_warning_indicator,
    truncate_output,
)

if TYPE_CHECKING:
    pass


class CellType(Enum):
    """Type of history cell."""

    USER_MESSAGE = "user"
    AGENT_MESSAGE = "agent"
    COMMAND = "command"
    MCP_TOOL = "mcp"
    PATCH = "patch"  # File modification with diff
    ERROR = "error"
    WARNING = "warning"
    SUCCESS = "success"
    SYSTEM = "system"
    REASONING = "reasoning"  # AI thinking/reasoning display


@dataclass
class HistoryCell:
    """A cell in the chat history."""

    cell_type: CellType
    content: str
    timestamp: datetime
    metadata: dict[str, Any] | None = None

    def render(self) -> RenderableType:
        """Render the cell content."""
        if self.cell_type == CellType.USER_MESSAGE:
            return self._render_user()
        elif self.cell_type == CellType.AGENT_MESSAGE:
            return self._render_agent()
        elif self.cell_type == CellType.COMMAND:
            return self._render_command()
        elif self.cell_type == CellType.MCP_TOOL:
            return self._render_mcp()
        elif self.cell_type == CellType.PATCH:
            return self._render_patch()
        elif self.cell_type == CellType.ERROR:
            return self._render_error()
        elif self.cell_type == CellType.WARNING:
            return self._render_warning()
        elif self.cell_type == CellType.SUCCESS:
            return self._render_success()
        elif self.cell_type == CellType.REASONING:
            return self._render_reasoning()
        elif self.cell_type == CellType.SYSTEM:
            return self._render_system()
        return Text(self.content)

    def _render_user(self) -> RenderableType:
        """Render user message with background styling."""
        text = Text()
        text.append("› ", style="bold dim")
        text.append(self.content)
        return Panel(
            text,
            border_style="dim",
            padding=(0, 1),
            title="You",
            title_align="left",
        )

    def _render_agent(self) -> RenderableType:
        """Render agent message with markdown."""
        return Markdown(self.content)

    def _render_command(self) -> RenderableType:
        """Render command execution with output."""
        meta = self.metadata or {}
        command = meta.get("command", "")
        exit_code = meta.get("exit_code", 0)
        status = meta.get("status", "completed")

        # Header
        if status == "in_progress":
            header = Text()
            header.append("● ", style="yellow")
            header.append("Running ", style="bold")
            header.append(command, style="cyan")
            header.append(" ...", style="dim italic")
        elif exit_code == 0:
            header = Text()
            header.append("● ", style="green bold")
            header.append("Ran ", style="bold")
            header.append(command, style="cyan")
        else:
            header = Text()
            header.append("● ", style="red bold")
            header.append("Ran ", style="bold")
            header.append(command, style="cyan")
            header.append(f" (exit {exit_code})", style="red")

        # Output
        if self.content:
            output_lines = self.content.strip().split("\n")
            max_lines = 10
            display_lines = output_lines[:max_lines]

            output_text = Text()
            for i, line in enumerate(display_lines):
                prefix = "  └ " if i == 0 else "    "
                output_text.append(prefix, style="dim")
                output_text.append(line, style="dim")
                output_text.append("\n")

            if len(output_lines) > max_lines:
                output_text.append(
                    f"    ... ({len(output_lines) - max_lines} more lines)\n",
                    style="dim italic",
                )

            return Text.assemble(header, "\n", output_text)

        return header

    def _render_mcp(self) -> RenderableType:
        """Render MCP tool call."""
        meta = self.metadata or {}
        server = meta.get("server", "")
        tool = meta.get("tool", "")
        status = meta.get("status", "completed")
        args = meta.get("arguments")

        text = Text()

        if status == "in_progress":
            text.append("● ", style="yellow")
            text.append("Calling ", style="bold")
            text.append(f"{server}.", style="cyan dim")
            text.append(tool, style="cyan")
            # Show arguments preview if available
            if args:
                args_preview = self._format_args_preview(args)
                if args_preview:
                    text.append(f"({args_preview})", style="dim")
            text.append(" ...", style="dim italic")
        elif status == "completed":
            text.append("● ", style="green bold")
            text.append("Called ", style="bold")
            text.append(f"{server}.", style="cyan dim")
            text.append(tool, style="cyan")

            # Show result preview
            if self.content:
                text.append("\n  └ ", style="dim")
                preview = self.content[:100].replace("\n", " ")
                if len(self.content) > 100:
                    preview += "..."
                text.append(preview, style="dim")
        else:
            text.append("● ", style="red bold")
            text.append("Failed ", style="bold")
            text.append(f"{server}.", style="cyan dim")
            text.append(tool, style="cyan")
            # Show error if in content
            if self.content:
                text.append(f": {self.content[:50]}", style="red dim")

        return text

    def _format_args_preview(self, args: dict[str, Any]) -> str:
        """Format arguments as a short preview string."""
        if not args:
            return ""
        # Show first 2-3 key arguments
        parts = []
        for key, value in list(args.items())[:3]:
            if isinstance(value, str):
                if len(value) > 20:
                    value = value[:17] + "..."
                parts.append(f'{key}="{value}"')
            elif isinstance(value, bool):
                parts.append(f"{key}={str(value).lower()}")
            elif isinstance(value, (int, float)):
                parts.append(f"{key}={value}")
        return ", ".join(parts)

    def _render_patch(self) -> RenderableType:
        """Render a file patch with diff highlighting."""
        meta = self.metadata or {}
        file_path = meta.get("path", "")
        status = meta.get("status", "completed")

        if status == "in_progress":
            text = Text()
            text.append("● ", style="yellow")
            text.append("Applying patch to ", style="bold")
            text.append(file_path, style="cyan")
            text.append(" ...", style="dim italic")
            return text

        # Completed - show diff
        header = Text()
        if status == "completed":
            header.append("● ", style="green bold")
            header.append("Applied patch to ", style="bold")
        else:
            header.append("● ", style="red bold")
            header.append("Failed to patch ", style="bold")
        header.append(file_path, style="cyan")

        if self.content:
            # Render actual diff
            diff_panel = render_diff(self.content, file_path)
            return Group(header, diff_panel)

        return header

    def _render_error(self) -> RenderableType:
        """Render error message."""
        return render_error_indicator(self.content)

    def _render_warning(self) -> RenderableType:
        """Render warning message."""
        return render_warning_indicator(self.content)

    def _render_success(self) -> RenderableType:
        """Render success message."""
        return render_success_indicator(self.content)

    def _render_reasoning(self) -> RenderableType:
        """Render AI reasoning/thinking content."""
        text = Text()
        text.append("💭 ", style="dim")
        text.append("Reasoning: ", style="dim italic")

        # Show truncated reasoning
        content, truncated = truncate_output(self.content, max_lines=5, max_chars=500)
        text.append(content, style="dim")
        if truncated:
            text.append("\n  ...(truncated)", style="dim italic")

        return Panel(
            text,
            border_style="dim",
            padding=(0, 1),
        )

    def _render_system(self) -> RenderableType:
        """Render system message."""
        text = Text()
        text.append("ℹ ", style="blue")
        text.append(self.content, style="blue dim")
        return text


class ChatCell(Static):
    """A single cell in the chat history."""

    DEFAULT_CSS = """
    ChatCell {
        margin: 0 1;
        padding: 0;
    }
    """

    def __init__(self, cell: HistoryCell) -> None:
        super().__init__()
        self.cell = cell

    def compose(self) -> list[Any]:
        """No children - we render directly."""
        return []

    def render(self) -> RenderableType:
        """Render the cell."""
        return self.cell.render()


class ChatWidget(VerticalScroll):
    """Scrollable chat history widget.

    Displays conversation history with:
    - User messages with background
    - Agent messages with markdown
    - Command executions with output
    - MCP tool calls with results
    """

    DEFAULT_CSS = """
    ChatWidget {
        height: 1fr;
        scrollbar-gutter: stable;
    }
    """

    def __init__(self) -> None:
        super().__init__()
        self._cells: list[HistoryCell] = []

    def add_user_message(self, text: str) -> None:
        """Add a user message to the history."""
        cell = HistoryCell(
            cell_type=CellType.USER_MESSAGE,
            content=text,
            timestamp=datetime.now(),
        )
        self._cells.append(cell)
        self.mount(ChatCell(cell))
        self.scroll_end(animate=False)

    def add_agent_message(self, text: str) -> ChatCell:
        """Add an agent message and return the cell for updates."""
        cell = HistoryCell(
            cell_type=CellType.AGENT_MESSAGE,
            content=text,
            timestamp=datetime.now(),
        )
        self._cells.append(cell)
        widget = ChatCell(cell)
        self.mount(widget)
        self.scroll_end(animate=False)
        return widget

    def update_agent_message(self, widget: ChatCell, text: str) -> None:
        """Update an existing agent message."""
        widget.cell.content = text
        widget.refresh()

    def add_command_start(self, command: str, call_id: str) -> ChatCell:
        """Add a command execution starting."""
        cell = HistoryCell(
            cell_type=CellType.COMMAND,
            content="",
            timestamp=datetime.now(),
            metadata={
                "command": command,
                "call_id": call_id,
                "status": "in_progress",
            },
        )
        self._cells.append(cell)
        widget = ChatCell(cell)
        self.mount(widget)
        self.scroll_end(animate=False)
        return widget

    def complete_command(
        self, widget: ChatCell, output: str, exit_code: int
    ) -> None:
        """Complete a command execution."""
        widget.cell.content = output
        widget.cell.metadata = widget.cell.metadata or {}
        widget.cell.metadata["exit_code"] = exit_code
        widget.cell.metadata["status"] = "completed"
        widget.refresh()

    def add_mcp_start(
        self,
        server: str,
        tool: str,
        call_id: str,
        arguments: dict[str, Any] | None = None,
    ) -> ChatCell:
        """Add an MCP tool call starting."""
        cell = HistoryCell(
            cell_type=CellType.MCP_TOOL,
            content="",
            timestamp=datetime.now(),
            metadata={
                "server": server,
                "tool": tool,
                "call_id": call_id,
                "status": "in_progress",
                "arguments": arguments,
            },
        )
        self._cells.append(cell)
        widget = ChatCell(cell)
        self.mount(widget)
        self.scroll_end(animate=False)
        return widget

    def complete_mcp(
        self, widget: ChatCell, result: str, success: bool = True
    ) -> None:
        """Complete an MCP tool call."""
        widget.cell.content = result
        widget.cell.metadata = widget.cell.metadata or {}
        widget.cell.metadata["status"] = "completed" if success else "failed"
        widget.refresh()

    def add_error(self, message: str) -> None:
        """Add an error message."""
        cell = HistoryCell(
            cell_type=CellType.ERROR,
            content=message,
            timestamp=datetime.now(),
        )
        self._cells.append(cell)
        self.mount(ChatCell(cell))
        self.scroll_end(animate=False)

    def add_system(self, message: str) -> None:
        """Add a system message."""
        cell = HistoryCell(
            cell_type=CellType.SYSTEM,
            content=message,
            timestamp=datetime.now(),
        )
        self._cells.append(cell)
        self.mount(ChatCell(cell))
        self.scroll_end(animate=False)

    def add_patch_start(self, file_path: str, call_id: str) -> ChatCell:
        """Add a patch/file modification starting."""
        cell = HistoryCell(
            cell_type=CellType.PATCH,
            content="",
            timestamp=datetime.now(),
            metadata={
                "path": file_path,
                "call_id": call_id,
                "status": "in_progress",
            },
        )
        self._cells.append(cell)
        widget = ChatCell(cell)
        self.mount(widget)
        self.scroll_end(animate=False)
        return widget

    def complete_patch(
        self, widget: ChatCell, diff_content: str, success: bool = True
    ) -> None:
        """Complete a patch with diff content."""
        widget.cell.content = diff_content
        widget.cell.metadata = widget.cell.metadata or {}
        widget.cell.metadata["status"] = "completed" if success else "failed"
        widget.refresh()

    def add_warning(self, message: str) -> None:
        """Add a warning message."""
        cell = HistoryCell(
            cell_type=CellType.WARNING,
            content=message,
            timestamp=datetime.now(),
        )
        self._cells.append(cell)
        self.mount(ChatCell(cell))
        self.scroll_end(animate=False)

    def add_success(self, message: str) -> None:
        """Add a success message."""
        cell = HistoryCell(
            cell_type=CellType.SUCCESS,
            content=message,
            timestamp=datetime.now(),
        )
        self._cells.append(cell)
        self.mount(ChatCell(cell))
        self.scroll_end(animate=False)

    def add_reasoning(self, content: str) -> ChatCell:
        """Add AI reasoning content."""
        cell = HistoryCell(
            cell_type=CellType.REASONING,
            content=content,
            timestamp=datetime.now(),
        )
        self._cells.append(cell)
        widget = ChatCell(cell)
        self.mount(widget)
        self.scroll_end(animate=False)
        return widget
