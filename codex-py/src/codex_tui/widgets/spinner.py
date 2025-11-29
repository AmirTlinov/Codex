"""Animated spinner widget for loading indicators."""

from __future__ import annotations

from textual.reactive import reactive
from textual.timer import Timer
from textual.widgets import Static

# Spinner animation frames
SPINNER_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
DOTS_FRAMES = [".", "..", "...", ".."]
PULSE_FRAMES = ["◐", "◓", "◑", "◒"]
THINKING_FRAMES = ["🤔", "💭", "💡", "🧠"]


class SpinnerWidget(Static):
    """Animated spinner widget.

    Shows a spinning animation while thinking/processing.
    """

    DEFAULT_CSS = """
    SpinnerWidget {
        width: auto;
        height: 1;
    }
    """

    frame_index: reactive[int] = reactive(0)

    def __init__(
        self,
        message: str = "Thinking",
        frames: list[str] | None = None,
        style: str = "yellow",
    ) -> None:
        super().__init__()
        self._message = message
        self._frames = frames or SPINNER_FRAMES
        self._style = style
        self._timer_handle: Timer | None = None

    def on_mount(self) -> None:
        """Start animation on mount."""
        self._timer_handle = self.set_interval(0.1, self._advance_frame)

    def on_unmount(self) -> None:
        """Stop animation on unmount."""
        if self._timer_handle:
            self._timer_handle.stop()

    def _advance_frame(self) -> None:
        """Advance to next animation frame."""
        self.frame_index = (self.frame_index + 1) % len(self._frames)

    def watch_frame_index(self, value: int) -> None:
        """Update display when frame changes."""
        frame = self._frames[value]
        self.update(f"[{self._style}]{frame}[/] [{self._style} italic]{self._message}...[/]")

    def set_message(self, message: str) -> None:
        """Update the spinner message."""
        self._message = message
        self.watch_frame_index(self.frame_index)


class ThinkingIndicator(Static):
    """Thinking indicator that shows animated dots."""

    DEFAULT_CSS = """
    ThinkingIndicator {
        height: 1;
        padding: 0 1;
        color: $text-muted;
    }
    """

    frame_index: reactive[int] = reactive(0)
    is_active: reactive[bool] = reactive(False)

    def __init__(self) -> None:
        super().__init__()
        self._timer_handle: Timer | None = None
        self._message = "Thinking"

    def on_mount(self) -> None:
        """Start animation timer."""
        self._timer_handle = self.set_interval(0.3, self._advance_frame)

    def on_unmount(self) -> None:
        """Stop animation."""
        if self._timer_handle:
            self._timer_handle.stop()

    def _advance_frame(self) -> None:
        """Advance animation frame."""
        if self.is_active:
            self.frame_index = (self.frame_index + 1) % len(PULSE_FRAMES)

    def watch_frame_index(self, value: int) -> None:
        """Update display on frame change."""
        if self.is_active:
            frame = PULSE_FRAMES[value]
            self.update(f"[yellow]{frame}[/] [dim italic]{self._message}...[/]")

    def watch_is_active(self, value: bool) -> None:
        """Handle visibility changes."""
        if value:
            self.update(f"[yellow]{PULSE_FRAMES[0]}[/] [dim italic]{self._message}...[/]")
        else:
            self.update("")

    def show(self, message: str = "Thinking") -> None:
        """Show the indicator with optional message."""
        self._message = message
        self.is_active = True

    def hide(self) -> None:
        """Hide the indicator."""
        self.is_active = False


class ProgressIndicator(Static):
    """Progress indicator for long operations."""

    DEFAULT_CSS = """
    ProgressIndicator {
        height: 1;
        padding: 0 1;
    }
    """

    progress: reactive[float] = reactive(0.0)

    def __init__(self, total: int = 100, label: str = "Progress") -> None:
        super().__init__()
        self._total = total
        self._label = label
        self._current = 0

    def set_progress(self, current: int) -> None:
        """Update progress value."""
        self._current = current
        self.progress = current / self._total if self._total > 0 else 0.0

    def watch_progress(self, value: float) -> None:
        """Update display on progress change."""
        percent = int(value * 100)
        bar_width = 20
        filled = int(bar_width * value)
        empty = bar_width - filled

        bar = "█" * filled + "░" * empty
        self.update(f"[dim]{self._label}:[/] [{bar}] [cyan]{percent}%[/]")

    def increment(self, amount: int = 1) -> None:
        """Increment progress by amount."""
        self.set_progress(min(self._current + amount, self._total))

    def complete(self) -> None:
        """Mark as complete."""
        self.set_progress(self._total)
        self.update(f"[green]✓[/] [dim]{self._label} complete[/]")
