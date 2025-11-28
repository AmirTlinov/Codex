"""Shell panel for managing background processes.

Displays running, completed, and failed background shells.
"""

from __future__ import annotations

from enum import Enum, auto
from typing import TYPE_CHECKING

from rich.console import RenderableType
from rich.panel import Panel
from rich.table import Table
from rich.text import Text
from textual.binding import Binding
from textual.containers import Container, Vertical
from textual.message import Message
from textual.reactive import reactive
from textual.widgets import Static, TabbedContent, TabPane

if TYPE_CHECKING:
    from textual.app import ComposeResult

    from codex_shell.background import BackgroundShell, ShellStatus


class ShellTab(Enum):
    """Shell panel tabs."""

    RUNNING = auto()
    COMPLETED = auto()
    FAILED = auto()


class ShellListItem(Static):
    """Single shell item in the list."""

    def __init__(
        self,
        shell_id: str,
        command: str,
        status: str,
        pid: int | None = None,
        exit_code: int | None = None,
        **kwargs: object,
    ) -> None:
        super().__init__(**kwargs)
        self.shell_id = shell_id
        self.command = command
        self.status = status
        self.pid = pid
        self.exit_code = exit_code

    def render(self) -> RenderableType:
        # Status indicator
        if self.status == "running":
            status_text = Text("RUNNING", style="bold green")
        elif self.status == "completed":
            if self.exit_code == 0:
                status_text = Text("OK", style="bold green")
            else:
                status_text = Text(f"EXIT {self.exit_code}", style="bold red")
        elif self.status == "failed":
            status_text = Text("FAILED", style="bold red")
        else:
            status_text = Text(self.status.upper(), style="dim")

        # Build info line
        info = Text()
        info.append(f"[{self.shell_id[:8]}] ", style="dim")
        if self.pid:
            info.append(f"PID:{self.pid} ", style="cyan")
        info.append(status_text)

        # Command (truncated)
        cmd_display = self.command
        if len(cmd_display) > 60:
            cmd_display = cmd_display[:57] + "..."

        return Panel(
            Text(cmd_display, style="bold"),
            subtitle=info,
            border_style="dim",
            padding=(0, 1),
        )


class ShellListWidget(Static):
    """List of shells for a specific status."""

    shells: reactive[list[dict]] = reactive(list, init=False)

    def __init__(self, status_filter: str, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self.status_filter = status_filter
        self.shells = []

    def render(self) -> RenderableType:
        if not self.shells:
            return Text(f"No {self.status_filter} shells", style="dim italic")

        table = Table(box=None, expand=True, show_header=False)
        table.add_column("Shell", ratio=1)

        for shell in self.shells:
            # Status indicator
            status = shell.get("status", "unknown")
            if status == "running":
                indicator = Text(" RUNNING ", style="black on green")
            elif status == "completed":
                exit_code = shell.get("exit_code", 0)
                if exit_code == 0:
                    indicator = Text(" OK ", style="black on green")
                else:
                    indicator = Text(f" EXIT {exit_code} ", style="white on red")
            else:
                indicator = Text(f" {status.upper()} ", style="white on red")

            # Build row
            row = Text()
            row.append(indicator)
            row.append(" ")
            row.append(shell.get("id", "?")[:8], style="dim")
            row.append(" ")

            cmd = shell.get("command", "")
            if len(cmd) > 50:
                cmd = cmd[:47] + "..."
            row.append(cmd)

            table.add_row(row)

        return table

    def update_shells(self, shells: list[dict]) -> None:
        """Update the shell list."""
        self.shells = [s for s in shells if s.get("status") == self.status_filter]
        self.refresh()


class ShellPanel(Container):
    """Panel showing background shell management."""

    DEFAULT_CSS = """
    ShellPanel {
        height: 100%;
        border: solid $primary;
    }

    ShellPanel TabbedContent {
        height: 100%;
    }

    ShellPanel TabPane {
        padding: 1;
    }

    ShellPanel #shell-actions {
        dock: bottom;
        height: 3;
        layout: horizontal;
        align: center middle;
        background: $surface-darken-1;
        padding: 0 1;
    }

    ShellPanel .action-hint {
        margin: 0 2;
    }
    """

    BINDINGS = [
        Binding("k", "kill_shell", "Kill", show=True),
        Binding("l", "view_logs", "Logs", show=True),
        Binding("r", "refresh", "Refresh", show=True),
    ]

    class ShellAction(Message):
        """Message for shell actions."""

        def __init__(self, action: str, shell_id: str) -> None:
            self.action = action
            self.shell_id = shell_id
            super().__init__()

    def __init__(self, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self._shells: list[dict] = []
        self._selected_shell: str | None = None

    def compose(self) -> ComposeResult:
        with TabbedContent():
            with TabPane("Running", id="tab-running"):
                yield ShellListWidget("running", id="list-running")
            with TabPane("Completed", id="tab-completed"):
                yield ShellListWidget("completed", id="list-completed")
            with TabPane("Failed", id="tab-failed"):
                yield ShellListWidget("failed", id="list-failed")

        with Container(id="shell-actions"):
            yield Static("[k] Kill", classes="action-hint")
            yield Static("[l] Logs", classes="action-hint")
            yield Static("[r] Refresh", classes="action-hint")

    def update_shells(self, shells: list[dict]) -> None:
        """Update all shell lists."""
        self._shells = shells

        for status in ["running", "completed", "failed"]:
            widget = self.query_one(f"#list-{status}", ShellListWidget)
            widget.update_shells(shells)

    def action_kill_shell(self) -> None:
        """Kill the selected shell."""
        if self._selected_shell:
            self.post_message(self.ShellAction("kill", self._selected_shell))

    def action_view_logs(self) -> None:
        """View logs for selected shell."""
        if self._selected_shell:
            self.post_message(self.ShellAction("logs", self._selected_shell))

    def action_refresh(self) -> None:
        """Refresh shell list."""
        self.post_message(self.ShellAction("refresh", ""))


class ShellLogOverlay(Container):
    """Overlay showing shell output/logs."""

    DEFAULT_CSS = """
    ShellLogOverlay {
        width: 80%;
        height: 80%;
        background: $surface;
        border: solid $accent;
        padding: 1;
    }

    ShellLogOverlay #log-content {
        height: 1fr;
        overflow-y: auto;
        background: $surface-darken-2;
        padding: 1;
    }

    ShellLogOverlay #log-header {
        dock: top;
        height: 3;
        background: $primary;
        padding: 0 1;
    }
    """

    BINDINGS = [
        Binding("escape", "close", "Close", show=True),
        Binding("q", "close", "Close", show=False),
    ]

    def __init__(
        self,
        shell_id: str,
        command: str,
        output: str,
        **kwargs: object,
    ) -> None:
        super().__init__(**kwargs)
        self.shell_id = shell_id
        self.command = command
        self.output = output

    def compose(self) -> ComposeResult:
        yield Static(
            Text(f"Shell: {self.shell_id[:8]} - {self.command}"),
            id="log-header",
        )
        yield Static(
            Text(self.output or "(no output)", style="dim" if not self.output else ""),
            id="log-content",
        )

    def action_close(self) -> None:
        """Close the overlay."""
        self.remove()
