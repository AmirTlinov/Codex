"""Chat widget for displaying conversation history.

Minimal ASCII-style interface without heavy panels.
"""

from __future__ import annotations

from rich.console import RenderableType
from rich.text import Text
from textual.containers import ScrollableContainer
from textual.widgets import Static

from codex_protocol.events import Usage


class MessageWidget(Static):
    """A single message in the chat history."""

    def __init__(
        self,
        content: str,
        role: str,
        message_id: str | None = None,
        **kwargs: object,
    ) -> None:
        super().__init__(**kwargs)
        self.content = content
        self.role = role
        self.message_id = message_id

    def render(self) -> RenderableType:
        text = Text()
        if self.role == "user":
            text.append("> ", style="bold cyan")
            text.append(self.content)
        elif self.role == "assistant":
            text.append(self.content, style="white")
        elif self.role == "system":
            text.append("--- ", style="dim")
            text.append(self.content, style="dim")
            text.append(" ---", style="dim")
        elif self.role == "error":
            text.append("! ", style="bold red")
            text.append(self.content, style="red")
        else:
            text.append(self.content)
        return text


class CommandWidget(Static):
    """Widget for displaying command execution."""

    def __init__(
        self,
        command: str,
        output: str = "",
        exit_code: int | None = None,
        command_id: str | None = None,
        **kwargs: object,
    ) -> None:
        super().__init__(**kwargs)
        self.command = command
        self.output = output
        self.exit_code = exit_code
        self.command_id = command_id

    def render(self) -> RenderableType:
        text = Text()
        # Command line
        text.append("$ ", style="bold yellow")
        text.append(self.command, style="yellow")
        if self.exit_code is not None:
            if self.exit_code == 0:
                text.append(" [ok]", style="green")
            else:
                text.append(f" [exit {self.exit_code}]", style="red")
        elif not self.output:
            text.append(" ...", style="dim")
        text.append("\n")
        # Output
        if self.output:
            for line in self.output.split("\n")[:10]:  # Limit output lines
                text.append("  " + line + "\n", style="dim")
            if self.output.count("\n") > 10:
                text.append("  ... (truncated)\n", style="dim italic")
        return text


class ThinkingWidget(Static):
    """Widget showing that the assistant is thinking."""

    DEFAULT_CSS = """
    ThinkingWidget {
        height: 1;
    }
    """

    def render(self) -> RenderableType:
        return Text("... thinking", style="italic dim")


class UsageWidget(Static):
    """Widget showing token usage."""

    def __init__(self, usage: Usage, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self.usage = usage

    def render(self) -> RenderableType:
        return Text(
            f"[{self.usage.input_tokens}+{self.usage.output_tokens} tokens]",
            style="dim",
        )


class ChatWidget(ScrollableContainer):
    """Scrollable container for chat messages."""

    DEFAULT_CSS = """
    ChatWidget {
        height: 1fr;
        padding: 0 1;
    }
    """

    def __init__(self, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self._messages: dict[str, MessageWidget] = {}
        self._commands: dict[str, CommandWidget] = {}
        self._thinking: ThinkingWidget | None = None

    def add_user_message(self, text: str) -> None:
        """Add a user message."""
        widget = MessageWidget(content=text, role="user")
        self.mount(widget)
        self.scroll_end(animate=False)

    def add_system_message(self, text: str) -> None:
        """Add a system message."""
        widget = MessageWidget(content=text, role="system")
        self.mount(widget)
        self.scroll_end(animate=False)

    def add_error_message(self, text: str) -> None:
        """Add an error message."""
        widget = MessageWidget(content=text, role="error")
        self.mount(widget)
        self.scroll_end(animate=False)

    def start_agent_message(self, message_id: str, text: str) -> None:
        """Start a streaming agent message."""
        widget = MessageWidget(content=text, role="assistant", message_id=message_id)
        self._messages[message_id] = widget
        self.mount(widget)
        self.scroll_end(animate=False)

    def update_agent_message(self, message_id: str, text: str) -> None:
        """Update a streaming agent message."""
        widget = self._messages.get(message_id)
        if widget:
            widget.content = text
            widget.refresh()
            self.scroll_end(animate=False)

    def complete_agent_message(self, message_id: str, text: str) -> None:
        """Complete a streaming agent message."""
        widget = self._messages.get(message_id)
        if widget:
            widget.content = text
            widget.refresh()
            self.scroll_end(animate=False)

    def add_command_start(self, command_id: str, command: str) -> None:
        """Add a command execution start."""
        widget = CommandWidget(command=command, command_id=command_id)
        self._commands[command_id] = widget
        self.mount(widget)
        self.scroll_end(animate=False)

    def add_command_result(
        self,
        command_id: str,
        command: str,
        output: str,
        exit_code: int | None,
    ) -> None:
        """Update a command with its result."""
        widget = self._commands.get(command_id)
        if widget:
            widget.output = output
            widget.exit_code = exit_code
            widget.refresh()
        else:
            widget = CommandWidget(
                command=command,
                output=output,
                exit_code=exit_code,
                command_id=command_id,
            )
            self._commands[command_id] = widget
            self.mount(widget)
        self.scroll_end(animate=False)

    def add_thinking_indicator(self) -> None:
        """Show thinking indicator."""
        if not self._thinking:
            self._thinking = ThinkingWidget()
            self.mount(self._thinking)
            self.scroll_end(animate=False)

    def remove_thinking_indicator(self) -> None:
        """Remove thinking indicator."""
        if self._thinking:
            self._thinking.remove()
            self._thinking = None

    def add_usage_info(self, usage: Usage) -> None:
        """Add usage information."""
        widget = UsageWidget(usage=usage)
        self.mount(widget)
        self.scroll_end(animate=False)

    def clear(self) -> None:
        """Clear all messages."""
        for child in list(self.children):
            child.remove()
        self._messages.clear()
        self._commands.clear()
        self._thinking = None
