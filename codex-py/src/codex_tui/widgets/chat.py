"""Chat history widget - displays conversation with command outputs.

Inspired by codex-rs history_cell.rs architecture.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import Enum
from typing import TYPE_CHECKING

from rich.console import RenderableType
from rich.markdown import Markdown
from rich.panel import Panel
from rich.text import Text
from textual.containers import VerticalScroll
from textual.widgets import Static

if TYPE_CHECKING:
    pass


class CellType(Enum):
    """Type of history cell."""

    USER_MESSAGE = "user"
    AGENT_MESSAGE = "agent"
    COMMAND = "command"
    MCP_TOOL = "mcp"
    ERROR = "error"
    SYSTEM = "system"


@dataclass
class HistoryCell:
    """A cell in the chat history."""

    cell_type: CellType
    content: str
    timestamp: datetime
    metadata: dict | None = None

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
        elif self.cell_type == CellType.ERROR:
            return self._render_error()
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

        if status == "in_progress":
            text = Text()
            text.append("● ", style="yellow")
            text.append("Calling ", style="bold")
            text.append(f"{server}.", style="cyan dim")
            text.append(tool, style="cyan")
            text.append(" ...", style="dim italic")
        elif status == "completed":
            text = Text()
            text.append("● ", style="green bold")
            text.append("Called ", style="bold")
            text.append(f"{server}.", style="cyan dim")
            text.append(tool, style="cyan")

            if self.content:
                text.append("\n  └ ", style="dim")
                preview = self.content[:80]
                if len(self.content) > 80:
                    preview += "..."
                text.append(preview, style="dim")
        else:
            text = Text()
            text.append("● ", style="red bold")
            text.append("Failed ", style="bold")
            text.append(f"{server}.", style="cyan dim")
            text.append(tool, style="cyan")

        return text

    def _render_error(self) -> RenderableType:
        """Render error message."""
        text = Text()
        text.append("■ ", style="red bold")
        text.append(self.content, style="red")
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

    def compose(self):
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

    def add_mcp_start(self, server: str, tool: str, call_id: str) -> ChatCell:
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
