"""Chat widget - minimal Claude Code style."""

from __future__ import annotations

from rich.console import RenderableType
from rich.text import Text
from textual.containers import ScrollableContainer
from textual.widgets import Static

from codex_protocol.events import Usage


class MessageWidget(Static):
    """Single message display."""

    DEFAULT_CSS = "MessageWidget { height: auto; background: transparent; }"

    def __init__(self, content: str, role: str, message_id: str | None = None) -> None:
        super().__init__()
        self.content = content
        self.role = role
        self.message_id = message_id

    def render(self) -> RenderableType:
        t = Text()
        if self.role == "user":
            t.append("› ", style="bold")
            t.append(self.content)
        elif self.role == "assistant":
            t.append(self.content)
        elif self.role == "system":
            t.append(self.content, style="dim")
        elif self.role == "error":
            t.append("! ", style="red bold")
            t.append(self.content, style="red")
        else:
            t.append(self.content)
        return t


class CommandWidget(Static):
    """Command execution display."""

    DEFAULT_CSS = "CommandWidget { height: auto; background: transparent; }"

    def __init__(self, command: str, output: str = "", exit_code: int | None = None, command_id: str | None = None) -> None:
        super().__init__()
        self.command = command
        self.output = output
        self.exit_code = exit_code
        self.command_id = command_id

    def render(self) -> RenderableType:
        t = Text()
        # Status
        if self.exit_code is None:
            t.append("$ ", style="yellow")
            t.append(self.command, style="yellow")
            t.append(" ...")
        elif self.exit_code == 0:
            t.append("$ ", style="green")
            t.append(self.command)
        else:
            t.append("$ ", style="red")
            t.append(self.command)
            t.append(f" [{self.exit_code}]", style="red")

        # Output (truncated)
        if self.output:
            lines = self.output.strip().split("\n")[:5]
            for line in lines:
                t.append("\n  ")
                t.append(line, style="dim")
            if len(self.output.strip().split("\n")) > 5:
                t.append("\n  (...)", style="dim")
        return t


class ThinkingWidget(Static):
    """Thinking indicator."""

    DEFAULT_CSS = "ThinkingWidget { height: auto; background: transparent; }"

    def render(self) -> RenderableType:
        return Text("...", style="dim")


class ChatWidget(ScrollableContainer):
    """Scrollable chat history."""

    DEFAULT_CSS = """
    ChatWidget {
        height: 1fr;
        background: transparent;
    }
    """

    def __init__(self, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self._messages: dict[str, MessageWidget] = {}
        self._commands: dict[str, CommandWidget] = {}
        self._thinking: ThinkingWidget | None = None

    def add_user_message(self, text: str) -> None:
        w = MessageWidget(content=text, role="user")
        self.mount(w)
        self.scroll_end(animate=False)

    def add_system_message(self, text: str) -> None:
        w = MessageWidget(content=text, role="system")
        self.mount(w)
        self.scroll_end(animate=False)

    def add_error_message(self, text: str) -> None:
        w = MessageWidget(content=text, role="error")
        self.mount(w)
        self.scroll_end(animate=False)

    def start_agent_message(self, message_id: str, text: str) -> None:
        w = MessageWidget(content=text, role="assistant", message_id=message_id)
        self._messages[message_id] = w
        self.mount(w)
        self.scroll_end(animate=False)

    def update_agent_message(self, message_id: str, text: str) -> None:
        w = self._messages.get(message_id)
        if w:
            w.content = text
            w.refresh()
            self.scroll_end(animate=False)

    def complete_agent_message(self, message_id: str, text: str) -> None:
        w = self._messages.get(message_id)
        if w:
            w.content = text
            w.refresh()
            self.scroll_end(animate=False)

    def add_command_start(self, command_id: str, command: str) -> None:
        w = CommandWidget(command=command, command_id=command_id)
        self._commands[command_id] = w
        self.mount(w)
        self.scroll_end(animate=False)

    def add_command_result(self, command_id: str, command: str, output: str, exit_code: int | None) -> None:
        w = self._commands.get(command_id)
        if w:
            w.output = output
            w.exit_code = exit_code
            w.refresh()
        else:
            w = CommandWidget(command=command, output=output, exit_code=exit_code, command_id=command_id)
            self._commands[command_id] = w
            self.mount(w)
        self.scroll_end(animate=False)

    def add_thinking_indicator(self) -> None:
        if not self._thinking:
            self._thinking = ThinkingWidget()
            self.mount(self._thinking)
            self.scroll_end(animate=False)

    def remove_thinking_indicator(self) -> None:
        if self._thinking:
            self._thinking.remove()
            self._thinking = None

    def add_usage_info(self, usage: Usage) -> None:
        pass  # Skip usage display - keep it minimal

    def clear(self) -> None:
        for child in list(self.children):
            child.remove()
        self._messages.clear()
        self._commands.clear()
        self._thinking = None
