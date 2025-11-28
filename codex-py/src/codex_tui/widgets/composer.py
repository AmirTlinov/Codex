"""Composer widget for user input.

Simple Input-based composer - Enter sends, no multiline complexity.
"""

from __future__ import annotations

from textual.message import Message
from textual.widgets import Input


class Composer(Input):
    """Simple input widget for composing messages."""

    DEFAULT_CSS = """
    Composer {
        height: 1;
        border: none;
        background: transparent;
        padding: 0;
    }

    Composer:focus {
        border: none;
    }

    Composer > .input--placeholder {
        color: $text-disabled;
    }
    """

    class Submitted(Message):
        """Message sent when the user submits input."""

        def __init__(self, text: str) -> None:
            self.text = text
            super().__init__()

    def __init__(self, **kwargs: object) -> None:
        super().__init__(placeholder="› Type a message...", **kwargs)
        self._history: list[str] = []
        self._history_index = 0

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Handle Enter key - submit the message."""
        event.stop()
        text = self.value.strip()
        if text:
            # Add to history
            if not self._history or self._history[-1] != text:
                self._history.append(text)
            self._history_index = len(self._history)
            # Post our custom message
            self.post_message(self.Submitted(text))

    def clear(self) -> None:
        """Clear the input."""
        self.value = ""
