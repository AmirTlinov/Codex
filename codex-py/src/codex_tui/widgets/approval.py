"""Approval dialog for commands and patches."""

from __future__ import annotations

import re
from enum import Enum
from typing import TYPE_CHECKING

from rich.console import RenderableType
from rich.syntax import Syntax
from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widgets import Button, Label, Static

from codex_tui.widgets.renderers import render_diff

if TYPE_CHECKING:
    pass


class ApprovalResult(Enum):
    """Result of approval dialog."""

    APPROVED = "approved"
    REJECTED = "rejected"
    ALWAYS = "always"


# Dangerous command patterns that warrant extra caution
DANGEROUS_PATTERNS = [
    r"\brm\b.*-rf",
    r"\brm\b.*-fr",
    r"\brmdir\b",
    r"\bsudo\b",
    r"\bchmod\b.*777",
    r"\bchown\b",
    r"\bmkfs\b",
    r"\bdd\b.*if=",
    r"\b>\s*/dev/",
    r"\bgit\s+push\s+.*--force",
    r"\bgit\s+reset\s+--hard",
    r"\bdrop\s+database\b",
    r"\btruncate\s+table\b",
    r"\bdelete\s+from\b.*where\s+1",
]


def is_dangerous_command(command: str) -> bool:
    """Check if command matches dangerous patterns."""
    return any(re.search(pattern, command, re.IGNORECASE) for pattern in DANGEROUS_PATTERNS)


class ApprovalDialog(ModalScreen[ApprovalResult]):
    """Modal dialog for approval requests.

    Supports:
    - Command execution approval
    - Patch/file modification approval
    - MCP tool call approval
    """

    DEFAULT_CSS = """
    ApprovalDialog {
        align: center middle;
    }

    #dialog {
        width: 80%;
        max-width: 100;
        height: auto;
        max-height: 85%;
        border: thick $warning;
        background: $surface;
        padding: 1 2;
    }

    #dialog.dangerous {
        border: thick $error;
    }

    #title {
        text-style: bold;
        color: $warning;
        margin-bottom: 1;
    }

    #title.dangerous {
        color: $error;
    }

    #warning {
        color: $error;
        text-style: bold;
        margin-bottom: 1;
        padding: 0 1;
        background: $error 20%;
    }

    #content-scroll {
        margin-bottom: 1;
        height: auto;
        max-height: 25;
        border: solid $primary-darken-2;
    }

    #content {
        padding: 1;
    }

    #buttons {
        height: auto;
        align: center middle;
        margin-top: 1;
    }

    Button {
        margin: 0 1;
        min-width: 12;
    }

    #approve {
        background: $success;
    }

    #approve:focus {
        background: $success-lighten-1;
    }

    #reject {
        background: $error;
    }

    #reject:focus {
        background: $error-lighten-1;
    }

    #always {
        background: $primary;
    }

    #always:focus {
        background: $primary-lighten-1;
    }

    #hint {
        color: $text-muted;
        text-align: center;
        margin-top: 1;
    }
    """

    BINDINGS = [
        Binding("y", "approve", "Approve", show=True),
        Binding("n", "reject", "Reject", show=True),
        Binding("a", "always", "Always", show=True),
        Binding("escape", "reject", "Cancel", show=False),
        Binding("enter", "focus_approve", "Focus", show=False),
    ]

    def __init__(
        self,
        title: str,
        content: str,
        content_type: str = "text",
    ) -> None:
        super().__init__()
        self._title = title
        self._content = content
        self._content_type = content_type
        self._is_dangerous = (
            content_type == "command" and is_dangerous_command(content)
        )

    def compose(self) -> ComposeResult:
        """Create the dialog layout."""
        dialog_classes = "dangerous" if self._is_dangerous else ""

        with Vertical(id="dialog", classes=dialog_classes):
            # Title
            title_classes = "dangerous" if self._is_dangerous else ""
            yield Label(
                f"[bold]{'⚠ ' if self._is_dangerous else ''}{self._title}[/]",
                id="title",
                classes=title_classes,
            )

            # Warning for dangerous commands
            if self._is_dangerous:
                yield Static(
                    "⚠ This command may be destructive! Review carefully.",
                    id="warning",
                )

            # Content display in scrollable area
            with VerticalScroll(id="content-scroll"):
                yield Static(self._build_content(), id="content")

            # Buttons
            with Horizontal(id="buttons"):
                yield Button("[Y]es", id="approve", variant="success")
                yield Button("[N]o", id="reject", variant="error")
                yield Button("[A]lways", id="always", variant="primary")

            # Keyboard hints
            yield Static(
                "[dim]Y[/]es • [dim]N[/]o • [dim]A[/]lways • [dim]Esc[/] cancel",
                id="hint",
            )

    def _build_content(self) -> RenderableType:
        """Build content renderable based on type."""
        if self._content_type == "command":
            text = Text()
            text.append("$ ", style="dim bold")
            if self._is_dangerous:
                text.append(self._content, style="bold red")
            else:
                text.append(self._content, style="bold cyan")
            return text

        elif self._content_type == "diff":
            # Use our rich diff renderer
            return render_diff(self._content)

        elif self._content_type == "json":
            return Syntax(
                self._content,
                "json",
                theme="monokai",
                line_numbers=False,
                word_wrap=True,
            )

        else:
            return Text(self._content)

    def on_mount(self) -> None:
        """Focus appropriate button on mount."""
        # Focus reject button for dangerous commands, approve otherwise
        if self._is_dangerous:
            self.query_one("#reject", Button).focus()
        else:
            self.query_one("#approve", Button).focus()

    def action_approve(self) -> None:
        """Approve the request."""
        self.dismiss(ApprovalResult.APPROVED)

    def action_reject(self) -> None:
        """Reject the request."""
        self.dismiss(ApprovalResult.REJECTED)

    def action_always(self) -> None:
        """Always approve similar requests."""
        self.dismiss(ApprovalResult.ALWAYS)

    def action_focus_approve(self) -> None:
        """Focus the approve button."""
        self.query_one("#approve", Button).focus()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button press."""
        if event.button.id == "approve":
            self.dismiss(ApprovalResult.APPROVED)
        elif event.button.id == "reject":
            self.dismiss(ApprovalResult.REJECTED)
        elif event.button.id == "always":
            self.dismiss(ApprovalResult.ALWAYS)
