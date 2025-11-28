"""Composer - simple input line."""

from __future__ import annotations

from textual.message import Message
from textual.widgets import Input


class Composer(Input):
    """Simple input for messages."""

    DEFAULT_CSS = """
    Composer {
        height: 1;
        background: transparent;
        border: none;
    }
    """

    class Submitted(Message):
        def __init__(self, text: str) -> None:
            self.text = text
            super().__init__()

    def __init__(self, **kwargs: object) -> None:
        super().__init__(placeholder="›", **kwargs)

    def on_input_submitted(self, event: Input.Submitted) -> None:
        event.stop()
        text = self.value.strip()
        if text:
            self.post_message(self.Submitted(text))

    def clear(self) -> None:
        self.value = ""
