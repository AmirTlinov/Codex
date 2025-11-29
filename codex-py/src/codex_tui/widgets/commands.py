"""Slash commands system for TUI.

Provides built-in commands and fuzzy-matching popup for command completion.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import TYPE_CHECKING

from rich.text import Text
from textual.containers import Vertical
from textual.message import Message
from textual.widgets import Static

if TYPE_CHECKING:
    pass


class SlashCommand(Enum):
    """Built-in slash commands.

    Order determines presentation order in popup (most used first).
    """

    MODEL = "model"
    SETTINGS = "settings"
    APPROVALS = "approvals"
    REVIEW = "review"
    NEW = "new"
    COMPACT = "compact"
    UNDO = "undo"
    DIFF = "diff"
    STATUS = "status"
    MCP = "mcp"
    CLEAR = "clear"
    HELP = "help"
    QUIT = "quit"
    EXIT = "exit"

    @property
    def description(self) -> str:
        """User-visible description for popup."""
        descriptions = {
            SlashCommand.MODEL: "choose model and reasoning effort",
            SlashCommand.SETTINGS: "open settings hub",
            SlashCommand.APPROVALS: "configure auto-approval rules",
            SlashCommand.REVIEW: "review current changes and find issues",
            SlashCommand.NEW: "start a new conversation",
            SlashCommand.COMPACT: "summarize to prevent context limit",
            SlashCommand.UNDO: "ask to undo last turn",
            SlashCommand.DIFF: "show git diff with untracked files",
            SlashCommand.STATUS: "show session info and token usage",
            SlashCommand.MCP: "list configured MCP tools",
            SlashCommand.CLEAR: "clear chat history",
            SlashCommand.HELP: "show help message",
            SlashCommand.QUIT: "exit Codex",
            SlashCommand.EXIT: "exit Codex",
        }
        return descriptions.get(self, "")

    @property
    def available_during_task(self) -> bool:
        """Whether command can run while a task is in progress."""
        unavailable = {
            SlashCommand.NEW,
            SlashCommand.COMPACT,
            SlashCommand.UNDO,
            SlashCommand.MODEL,
            SlashCommand.APPROVALS,
            SlashCommand.REVIEW,
        }
        return self not in unavailable


@dataclass
class CommandMatch:
    """A matched command with optional highlight indices."""

    command: SlashCommand
    indices: list[int] | None = None
    score: int = 0


def fuzzy_match(text: str, pattern: str) -> tuple[list[int], int] | None:
    """Simple fuzzy matching.

    Returns matched character indices and score (lower is better).
    Returns None if no match.
    """
    if not pattern:
        return [], 0

    text_lower = text.lower()
    pattern_lower = pattern.lower()

    indices: list[int] = []
    pattern_idx = 0
    score = 0

    for i, char in enumerate(text_lower):
        if pattern_idx < len(pattern_lower) and char == pattern_lower[pattern_idx]:
            indices.append(i)
            # Bonus for consecutive matches
            if indices and len(indices) > 1 and indices[-1] == indices[-2] + 1:
                score -= 1
            pattern_idx += 1

    if pattern_idx == len(pattern_lower):
        # Penalty for non-prefix matches
        if indices and indices[0] > 0:
            score += indices[0]
        return indices, score

    return None


def filter_commands(filter_text: str) -> list[CommandMatch]:
    """Filter commands by fuzzy matching.

    Args:
        filter_text: Text to match against command names

    Returns:
        List of matched commands sorted by score
    """
    if not filter_text:
        return [CommandMatch(cmd) for cmd in SlashCommand]

    matches: list[CommandMatch] = []
    for cmd in SlashCommand:
        result = fuzzy_match(cmd.value, filter_text)
        if result:
            indices, score = result
            matches.append(CommandMatch(cmd, indices, score))

    # Sort by score (lower is better)
    matches.sort(key=lambda m: (m.score, m.command.value))
    return matches


class CommandItem(Static):
    """Single command item in popup."""

    DEFAULT_CSS = """
    CommandItem {
        height: 1;
        padding: 0 1;
    }

    CommandItem.selected {
        background: $accent;
    }
    """

    def __init__(self, match: CommandMatch, selected: bool = False) -> None:
        super().__init__()
        self._match = match
        self._selected = selected
        if selected:
            self.add_class("selected")

    def render(self) -> Text:
        """Render command with optional highlights."""
        text = Text()
        text.append("/", style="dim")

        # Render command name with highlights
        name = self._match.command.value
        if self._match.indices:
            for i, char in enumerate(name):
                if i in self._match.indices:
                    text.append(char, style="bold yellow")
                else:
                    text.append(char)
        else:
            text.append(name, style="bold" if self._selected else "")

        text.append("  ", style="dim")
        text.append(self._match.command.description, style="dim italic")

        return text


class CommandPopup(Vertical):
    """Popup showing filtered slash commands.

    Shows available commands with fuzzy filtering as user types.
    """

    DEFAULT_CSS = """
    CommandPopup {
        height: auto;
        max-height: 10;
        background: $surface;
        border: solid $primary;
        padding: 0;
        layer: popup;
    }
    """

    class Selected(Message):
        """Fired when a command is selected."""

        def __init__(self, command: SlashCommand) -> None:
            super().__init__()
            self.command = command

    class Dismissed(Message):
        """Fired when popup is dismissed without selection."""

    def __init__(self) -> None:
        super().__init__()
        self._filter = ""
        self._matches: list[CommandMatch] = []
        self._selected_index = 0
        self._rebuild()

    def _rebuild(self) -> None:
        """Rebuild match list and items."""
        self._matches = filter_commands(self._filter)
        if self._selected_index >= len(self._matches):
            self._selected_index = max(0, len(self._matches) - 1)

    def on_mount(self) -> None:
        """Initialize on mount."""
        self._update_display()

    def set_filter(self, text: str) -> None:
        """Update filter text."""
        # Extract command portion from /command args
        if text.startswith("/"):
            text = text[1:]
        parts = text.split(None, 1)
        self._filter = parts[0] if parts else ""
        self._rebuild()
        self._update_display()

    def _update_display(self) -> None:
        """Update displayed items."""
        self.remove_children()
        for i, match in enumerate(self._matches[:8]):  # Max 8 items
            self.mount(CommandItem(match, selected=(i == self._selected_index)))

    def select_next(self) -> None:
        """Select next item."""
        if self._matches:
            self._selected_index = (self._selected_index + 1) % len(self._matches[:8])
            self._update_display()

    def select_prev(self) -> None:
        """Select previous item."""
        if self._matches:
            self._selected_index = (self._selected_index - 1) % len(self._matches[:8])
            self._update_display()

    def confirm(self) -> SlashCommand | None:
        """Confirm current selection."""
        if self._matches and 0 <= self._selected_index < len(self._matches):
            return self._matches[self._selected_index].command
        return None

    @property
    def has_matches(self) -> bool:
        """Whether there are any matches."""
        return bool(self._matches)

    @property
    def selected_command(self) -> SlashCommand | None:
        """Currently selected command."""
        if self._matches and 0 <= self._selected_index < len(self._matches):
            return self._matches[self._selected_index].command
        return None
