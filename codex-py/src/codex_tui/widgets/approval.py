"""Approval overlay for command and patch confirmations.

Displays pending approvals and allows user to accept/reject.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum, auto
from typing import TYPE_CHECKING

from rich.console import RenderableType
from rich.panel import Panel
from rich.syntax import Syntax
from rich.text import Text
from textual.binding import Binding
from textual.containers import Container, Vertical
from textual.message import Message
from textual.screen import ModalScreen
from textual.widgets import Button, Static

if TYPE_CHECKING:
    from textual.app import ComposeResult


class ApprovalType(Enum):
    """Type of approval requested."""

    COMMAND = auto()
    PATCH = auto()


@dataclass(slots=True)
class ApprovalRequest:
    """Pending approval request."""

    request_id: str
    approval_type: ApprovalType
    command: str | None = None
    patch_path: str | None = None
    patch_diff: str | None = None
    description: str | None = None


class ApprovalResult(Enum):
    """Result of approval decision."""

    APPROVED = auto()
    REJECTED = auto()
    ALWAYS_APPROVE = auto()


class ApprovalContentWidget(Static):
    """Widget displaying approval content."""

    def __init__(self, request: ApprovalRequest, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self.request = request

    def render(self) -> RenderableType:
        req = self.request

        if req.approval_type == ApprovalType.COMMAND:
            content = Syntax(
                req.command or "",
                "bash",
                theme="monokai",
                line_numbers=False,
                word_wrap=True,
            )
            title = "Execute Command?"
            border_style = "yellow"

        elif req.approval_type == ApprovalType.PATCH:
            content = Syntax(
                req.patch_diff or "",
                "diff",
                theme="monokai",
                line_numbers=True,
                word_wrap=True,
            )
            title = f"Apply Patch: {req.patch_path or 'unknown'}"
            border_style = "cyan"

        else:
            content = Text(str(req))
            title = "Approval Required"
            border_style = "yellow"

        return Panel(
            content,
            title=title,
            subtitle=req.description,
            border_style=border_style,
            padding=(1, 2),
        )


class ApprovalOverlay(ModalScreen[ApprovalResult]):
    """Modal overlay for approval requests."""

    DEFAULT_CSS = """
    ApprovalOverlay {
        align: center middle;
    }

    ApprovalOverlay > Container {
        width: 80%;
        max-width: 100;
        height: auto;
        max-height: 80%;
        background: $surface;
        border: solid $primary;
        padding: 1 2;
    }

    ApprovalOverlay #content {
        height: auto;
        max-height: 60%;
        overflow-y: auto;
    }

    ApprovalOverlay #buttons {
        layout: horizontal;
        height: auto;
        align: center middle;
        margin-top: 1;
    }

    ApprovalOverlay Button {
        margin: 0 1;
    }

    ApprovalOverlay #approve {
        background: $success;
    }

    ApprovalOverlay #reject {
        background: $error;
    }

    ApprovalOverlay #always {
        background: $warning;
    }
    """

    BINDINGS = [
        Binding("y", "approve", "Yes", show=True),
        Binding("n", "reject", "No", show=True),
        Binding("a", "always", "Always", show=True),
        Binding("escape", "reject", "Cancel", show=False),
    ]

    class Decided(Message):
        """Message sent when user decides on approval."""

        def __init__(self, request_id: str, result: ApprovalResult) -> None:
            self.request_id = request_id
            self.result = result
            super().__init__()

    def __init__(self, request: ApprovalRequest, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self.request = request

    def compose(self) -> ComposeResult:
        with Container():
            yield ApprovalContentWidget(self.request, id="content")
            with Container(id="buttons"):
                yield Button("Yes [y]", id="approve", variant="success")
                yield Button("No [n]", id="reject", variant="error")
                if self.request.approval_type == ApprovalType.COMMAND:
                    yield Button("Always [a]", id="always", variant="warning")

    def action_approve(self) -> None:
        """Approve the request."""
        self.dismiss(ApprovalResult.APPROVED)

    def action_reject(self) -> None:
        """Reject the request."""
        self.dismiss(ApprovalResult.REJECTED)

    def action_always(self) -> None:
        """Always approve this type."""
        self.dismiss(ApprovalResult.ALWAYS_APPROVE)

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button clicks."""
        button_id = event.button.id
        if button_id == "approve":
            self.action_approve()
        elif button_id == "reject":
            self.action_reject()
        elif button_id == "always":
            self.action_always()


class ApprovalQueue:
    """Manages pending approval requests."""

    def __init__(self) -> None:
        self._pending: dict[str, ApprovalRequest] = {}
        self._always_approve_commands: bool = False

    def add_command(
        self,
        request_id: str,
        command: str,
        description: str | None = None,
    ) -> ApprovalRequest | None:
        """Add command approval request.

        Returns None if auto-approved.
        """
        if self._always_approve_commands:
            return None

        request = ApprovalRequest(
            request_id=request_id,
            approval_type=ApprovalType.COMMAND,
            command=command,
            description=description,
        )
        self._pending[request_id] = request
        return request

    def add_patch(
        self,
        request_id: str,
        path: str,
        diff: str,
        description: str | None = None,
    ) -> ApprovalRequest:
        """Add patch approval request."""
        request = ApprovalRequest(
            request_id=request_id,
            approval_type=ApprovalType.PATCH,
            patch_path=path,
            patch_diff=diff,
            description=description,
        )
        self._pending[request_id] = request
        return request

    def resolve(self, request_id: str, result: ApprovalResult) -> bool:
        """Resolve a pending request.

        Returns True if approved.
        """
        request = self._pending.pop(request_id, None)
        if not request:
            return False

        if result == ApprovalResult.ALWAYS_APPROVE:
            if request.approval_type == ApprovalType.COMMAND:
                self._always_approve_commands = True
            return True

        return result == ApprovalResult.APPROVED

    def get_pending(self, request_id: str) -> ApprovalRequest | None:
        """Get pending request by ID."""
        return self._pending.get(request_id)

    def clear(self) -> None:
        """Clear all pending requests."""
        self._pending.clear()

    @property
    def has_pending(self) -> bool:
        """Check if there are pending requests."""
        return bool(self._pending)
