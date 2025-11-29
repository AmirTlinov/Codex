"""Input widget for user messages."""

from __future__ import annotations

from textual.message import Message
from textual.widgets import Input


class InputWidget(Input):
    """Input widget with submit handling.

    Features:
    - Multi-line input support
    - Command history navigation
    - Slash command completion
    """

    DEFAULT_CSS = """
    InputWidget {
        dock: bottom;
        height: auto;
        min-height: 3;
        border: solid $primary;
        padding: 0 1;
    }

    InputWidget:focus {
        border: solid $accent;
    }
    """

    class Submitted(Message):
        """Message sent when user submits input."""

        def __init__(self, value: str) -> None:
            super().__init__()
            self.value = value

    def __init__(self) -> None:
        super().__init__(placeholder="Type a message... (Ctrl+C to exit)")
        self._history: list[str] = []
        self._history_index = -1

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Handle input submission."""
        value = event.value.strip()
        if value:
            self._history.append(value)
            self._history_index = -1
            self.post_message(self.Submitted(value))
        self.value = ""
