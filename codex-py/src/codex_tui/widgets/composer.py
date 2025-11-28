"""Composer widget for user input.

Claude Code style: › prefix with minimal chrome.
"""

from __future__ import annotations

from textual.message import Message
from textual.widgets import TextArea


class Composer(TextArea):
    """Text input widget for composing messages (Claude Code style)."""

    DEFAULT_CSS = """
    Composer {
        height: auto;
        min-height: 1;
        max-height: 10;
        padding: 0;
        border: none;
        background: transparent;
    }

    Composer:focus {
        border: none;
    }
    """

    BINDINGS = [
        ("enter", "submit", "Send"),
        ("ctrl+enter", "newline", "New Line"),
        ("up", "history_prev", "Previous"),
        ("down", "history_next", "Next"),
    ]

    class Submitted(Message):
        """Message sent when the user submits input."""

        def __init__(self, text: str) -> None:
            self.text = text
            super().__init__()

    def __init__(self, **kwargs: object) -> None:
        super().__init__(language=None, **kwargs)
        self._history: list[str] = []
        self._history_index = 0

    def action_submit(self) -> None:
        """Submit the current input."""
        text = self.text.strip()
        if text:
            # Add to history
            if not self._history or self._history[-1] != text:
                self._history.append(text)
            self._history_index = len(self._history)

            # Post message
            self.post_message(self.Submitted(text))

    def action_newline(self) -> None:
        """Insert a newline."""
        self.insert("\n")

    def action_history_prev(self) -> None:
        """Go to previous history item."""
        if self._history and self._history_index > 0:
            self._history_index -= 1
            self.text = self._history[self._history_index]
            self.move_cursor((0, len(self.text)))

    def action_history_next(self) -> None:
        """Go to next history item."""
        if self._history_index < len(self._history) - 1:
            self._history_index += 1
            self.text = self._history[self._history_index]
            self.move_cursor((0, len(self.text)))
        elif self._history_index == len(self._history) - 1:
            self._history_index = len(self._history)
            self.clear()

    def clear(self) -> None:
        """Clear the input."""
        self.text = ""
