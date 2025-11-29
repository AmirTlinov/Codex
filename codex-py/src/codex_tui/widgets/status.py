"""Status bar widget showing model info and token usage."""

from __future__ import annotations

from rich.text import Text
from textual.widgets import Static


class StatusBar(Static):
    """Status bar at the bottom of the screen.

    Shows:
    - Current model name
    - Token usage (input/output)
    - Rate limit status
    - Current directory
    """

    DEFAULT_CSS = """
    StatusBar {
        dock: bottom;
        height: 1;
        background: $surface;
        color: $text-muted;
        padding: 0 1;
    }
    """

    def __init__(self) -> None:
        super().__init__()
        self._model = "gpt-4o"
        self._tokens_in = 0
        self._tokens_out = 0
        self._cwd = "~"
        self._status = "ready"

    def set_model(self, model: str) -> None:
        """Set the current model name."""
        self._model = model
        self._update()

    def set_tokens(self, input_tokens: int, output_tokens: int) -> None:
        """Update token counts."""
        self._tokens_in = input_tokens
        self._tokens_out = output_tokens
        self._update()

    def set_cwd(self, cwd: str) -> None:
        """Set the current working directory."""
        self._cwd = cwd
        self._update()

    def set_status(self, status: str) -> None:
        """Set status text (ready, thinking, running)."""
        self._status = status
        self._update()

    def _update(self) -> None:
        """Update the status bar display."""
        self.update(self._render())

    def _render(self) -> Text:
        """Render the status bar content."""
        text = Text()

        # Model
        text.append(" ")
        text.append(self._model, style="bold")
        text.append(" | ", style="dim")

        # Tokens
        text.append(f"{self._tokens_in:,}", style="cyan")
        text.append("/", style="dim")
        text.append(f"{self._tokens_out:,}", style="green")
        text.append(" tokens", style="dim")
        text.append(" | ", style="dim")

        # Status
        if self._status == "thinking":
            text.append("thinking...", style="yellow italic")
        elif self._status == "running":
            text.append("running...", style="blue italic")
        else:
            text.append("ready", style="green")

        text.append(" | ", style="dim")

        # CWD
        text.append(self._cwd, style="dim")

        return text

    def render(self) -> Text:
        """Render the widget."""
        return self._render()
