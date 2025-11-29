"""Input widget for user messages with command completion."""

from __future__ import annotations

from textual.binding import Binding
from textual.message import Message
from textual.reactive import reactive
from textual.widgets import Input

from codex_tui.widgets.commands import SlashCommand, filter_commands


class InputWidget(Input):
    """Input widget with history navigation and slash command support.

    Features:
    - Command history with Up/Down navigation
    - Slash command popup with fuzzy filtering
    - Tab completion for commands
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

    InputWidget.command-mode {
        border: solid $warning;
    }
    """

    BINDINGS = [
        Binding("up", "history_prev", "Previous", show=False),
        Binding("down", "history_next", "Next", show=False),
        Binding("tab", "complete", "Complete", show=False),
        Binding("escape", "cancel_command", "Cancel", show=False),
    ]

    is_command_mode: reactive[bool] = reactive(False)

    class UserSubmitted(Message):
        """Message sent when user submits input."""

        def __init__(self, value: str) -> None:
            super().__init__()
            self.value = value

    class CommandSelected(Message):
        """Message sent when a slash command is selected."""

        def __init__(self, command: SlashCommand) -> None:
            super().__init__()
            self.command = command

    class ShowCommandPopup(Message):
        """Request to show command popup."""

        def __init__(self, filter_text: str) -> None:
            super().__init__()
            self.filter_text = filter_text

    class HideCommandPopup(Message):
        """Request to hide command popup."""

    def __init__(self) -> None:
        super().__init__(placeholder="Type a message... (/ for commands)")
        self._history: list[str] = []
        self._history_index = -1
        self._history_temp: str = ""  # Temporary storage while browsing
        self._command_matches: list[SlashCommand] = []
        self._command_index = 0

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Handle input submission."""
        value = event.value.strip()
        if value:
            self._history.append(value)
            self._history_index = -1
            self._history_temp = ""
            self.post_message(self.UserSubmitted(value))
        self.value = ""
        self._exit_command_mode()

    def watch_value(self, new_value: str) -> None:
        """Watch for value changes to handle command mode."""
        if new_value.startswith("/"):
            self._enter_command_mode(new_value)
        else:
            self._exit_command_mode()

    def _enter_command_mode(self, text: str) -> None:
        """Enter command completion mode."""
        self.is_command_mode = True
        self.add_class("command-mode")

        # Get filter text (everything after /)
        filter_text = text[1:].split()[0] if text[1:] else ""
        matches = filter_commands(filter_text)
        self._command_matches = [m.command for m in matches[:8]]
        self._command_index = 0

        self.post_message(self.ShowCommandPopup(filter_text))

    def _exit_command_mode(self) -> None:
        """Exit command completion mode."""
        if self.is_command_mode:
            self.is_command_mode = False
            self.remove_class("command-mode")
            self._command_matches = []
            self._command_index = 0
            self.post_message(self.HideCommandPopup())

    def action_history_prev(self) -> None:
        """Navigate to previous history entry."""
        if self.is_command_mode:
            # Navigate command matches
            if self._command_matches:
                self._command_index = (self._command_index - 1) % len(self._command_matches)
                # Update popup via message
                self.post_message(self.ShowCommandPopup(self.value[1:] if self.value.startswith("/") else ""))
            return

        if not self._history:
            return

        if self._history_index == -1:
            # Save current input
            self._history_temp = self.value
            self._history_index = len(self._history) - 1
        elif self._history_index > 0:
            self._history_index -= 1

        self.value = self._history[self._history_index]
        self.cursor_position = len(self.value)

    def action_history_next(self) -> None:
        """Navigate to next history entry."""
        if self.is_command_mode:
            # Navigate command matches
            if self._command_matches:
                self._command_index = (self._command_index + 1) % len(self._command_matches)
                self.post_message(self.ShowCommandPopup(self.value[1:] if self.value.startswith("/") else ""))
            return

        if self._history_index == -1:
            return

        if self._history_index < len(self._history) - 1:
            self._history_index += 1
            self.value = self._history[self._history_index]
        else:
            # Restore original input
            self._history_index = -1
            self.value = self._history_temp

        self.cursor_position = len(self.value)

    def action_complete(self) -> None:
        """Complete the current command."""
        if self.is_command_mode and self._command_matches:
            cmd = self._command_matches[self._command_index]
            self.value = f"/{cmd.value} "
            self.cursor_position = len(self.value)
            self._exit_command_mode()

    def action_cancel_command(self) -> None:
        """Cancel command mode."""
        if self.is_command_mode:
            self.value = ""
            self._exit_command_mode()

    def select_command(self, command: SlashCommand) -> None:
        """Select a command from popup."""
        self.value = f"/{command.value}"
        self.cursor_position = len(self.value)
        self.post_message(self.UserSubmitted(self.value))
        self.value = ""
        self._exit_command_mode()
