"""Approval dialog for commands and patches."""

from __future__ import annotations

from enum import Enum
from typing import TYPE_CHECKING

from rich.syntax import Syntax
from rich.text import Text
from textual.app import ComposeResult
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, Label, Static

if TYPE_CHECKING:
    pass


class ApprovalResult(Enum):
    """Result of approval dialog."""

    APPROVED = "approved"
    REJECTED = "rejected"
    ALWAYS = "always"


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
        max-height: 80%;
        border: thick $warning;
        background: $surface;
        padding: 1 2;
    }

    #title {
        text-style: bold;
        color: $warning;
        margin-bottom: 1;
    }

    #content {
        margin-bottom: 1;
        height: auto;
        max-height: 20;
        overflow-y: auto;
    }

    #buttons {
        height: auto;
        align: center middle;
    }

    Button {
        margin: 0 1;
    }

    #approve {
        background: $success;
    }

    #reject {
        background: $error;
    }

    #always {
        background: $primary;
    }
    """

    BINDINGS = [
        ("y", "approve", "Approve"),
        ("n", "reject", "Reject"),
        ("a", "always", "Always"),
        ("escape", "reject", "Cancel"),
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

    def compose(self) -> ComposeResult:
        """Create the dialog layout."""
        with Vertical(id="dialog"):
            yield Label(f"[bold yellow]{self._title}[/]", id="title")

            # Content display
            if self._content_type == "command":
                yield Static(
                    Text.assemble(
                        ("$ ", "dim"),
                        (self._content, "bold cyan"),
                    ),
                    id="content",
                )
            elif self._content_type == "diff":
                yield Static(
                    Syntax(
                        self._content,
                        "diff",
                        theme="monokai",
                        line_numbers=False,
                    ),
                    id="content",
                )
            elif self._content_type == "json":
                yield Static(
                    Syntax(
                        self._content,
                        "json",
                        theme="monokai",
                        line_numbers=False,
                    ),
                    id="content",
                )
            else:
                yield Static(self._content, id="content")

            with Horizontal(id="buttons"):
                yield Button("[Y]es", id="approve", variant="success")
                yield Button("[N]o", id="reject", variant="error")
                yield Button("[A]lways", id="always", variant="primary")

    def action_approve(self) -> None:
        """Approve the request."""
        self.dismiss(ApprovalResult.APPROVED)

    def action_reject(self) -> None:
        """Reject the request."""
        self.dismiss(ApprovalResult.REJECTED)

    def action_always(self) -> None:
        """Always approve similar requests."""
        self.dismiss(ApprovalResult.ALWAYS)

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button press."""
        if event.button.id == "approve":
            self.dismiss(ApprovalResult.APPROVED)
        elif event.button.id == "reject":
            self.dismiss(ApprovalResult.REJECTED)
        elif event.button.id == "always":
            self.dismiss(ApprovalResult.ALWAYS)
