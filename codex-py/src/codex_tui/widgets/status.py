"""Status bar widget showing model info and token usage."""

from __future__ import annotations

from datetime import datetime
from typing import TYPE_CHECKING

from rich.text import Text
from textual.reactive import reactive
from textual.widgets import Static

if TYPE_CHECKING:
    pass


class StatusBar(Static):
    """Status bar at the bottom of the screen.

    Shows:
    - Current model name
    - Token usage (input/output)
    - Session duration
    - Current directory
    - Status indicator
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

    status: reactive[str] = reactive("ready")

    def __init__(self) -> None:
        super().__init__()
        self._model = "gpt-4o"
        self._tokens_in = 0
        self._tokens_out = 0
        self._cwd = "~"
        self._start_time = datetime.now()
        self._turn_count = 0

    def on_mount(self) -> None:
        """Initialize display on mount."""
        self._update()
        # Update every second for duration timer
        self.set_interval(1.0, self._update)

    def set_model(self, model: str) -> None:
        """Set the current model name."""
        self._model = model
        self._update()

    def set_tokens(self, input_tokens: int, output_tokens: int) -> None:
        """Update token counts."""
        self._tokens_in = input_tokens
        self._tokens_out = output_tokens
        self._update()

    def add_tokens(self, input_tokens: int, output_tokens: int) -> None:
        """Add to token counts (cumulative)."""
        self._tokens_in += input_tokens
        self._tokens_out += output_tokens
        self._update()

    def set_cwd(self, cwd: str) -> None:
        """Set the current working directory."""
        self._cwd = cwd
        self._update()

    def set_status(self, status: str) -> None:
        """Set status text (ready, thinking, running)."""
        self.status = status

    def increment_turns(self) -> None:
        """Increment conversation turn count."""
        self._turn_count += 1
        self._update()

    def watch_status(self, _value: str) -> None:
        """React to status changes."""
        self._update()

    def _format_duration(self) -> str:
        """Format session duration."""
        delta = datetime.now() - self._start_time
        total_seconds = int(delta.total_seconds())
        hours, remainder = divmod(total_seconds, 3600)
        minutes, seconds = divmod(remainder, 60)

        if hours > 0:
            return f"{hours}h{minutes:02d}m"
        elif minutes > 0:
            return f"{minutes}m{seconds:02d}s"
        else:
            return f"{seconds}s"

    def _format_tokens(self) -> str:
        """Format token count with K suffix for large numbers."""
        def fmt(n: int) -> str:
            if n >= 10000:
                return f"{n // 1000}k"
            elif n >= 1000:
                return f"{n / 1000:.1f}k"
            return str(n)

        return f"{fmt(self._tokens_in)}/{fmt(self._tokens_out)}"

    def _update(self) -> None:
        """Update the status bar display."""
        try:
            self.update(self._build_text())
        except Exception:
            # Widget not yet mounted
            pass

    def _build_text(self) -> Text:
        """Render the status bar content."""
        text = Text()

        # Model indicator
        text.append(" ")
        text.append("●", style="green" if self.status == "ready" else "yellow")
        text.append(" ")
        text.append(self._model, style="bold")

        text.append(" │ ", style="dim")

        # Status
        if self.status == "thinking":
            text.append("◐ thinking", style="yellow italic")
        elif self.status == "running":
            text.append("▶ running", style="blue italic")
        elif self.status == "waiting":
            text.append("⏸ waiting", style="cyan italic")
        else:
            text.append("● ready", style="green")

        text.append(" │ ", style="dim")

        # Tokens
        text.append("tokens ", style="dim")
        text.append(self._format_tokens(), style="cyan")

        text.append(" │ ", style="dim")

        # Duration
        text.append(self._format_duration(), style="dim")

        # Turn count
        if self._turn_count > 0:
            text.append(f" ({self._turn_count} turns)", style="dim")

        text.append(" │ ", style="dim")

        # CWD (truncated if needed)
        cwd_display = self._cwd
        if len(cwd_display) > 30:
            cwd_display = "…" + cwd_display[-29:]
        text.append(cwd_display, style="dim")

        return text

    @property
    def total_tokens(self) -> int:
        """Total tokens used in session."""
        return self._tokens_in + self._tokens_out
