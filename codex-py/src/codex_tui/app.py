"""Codex TUI - Terminal interface using Textual.

Full-featured TUI similar to codex-rs with:
- Scrollable chat history
- Streaming message display
- Command execution display
- Status bar with model info
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from rich.markdown import Markdown
from rich.text import Text
from textual import on, work
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import ScrollableContainer, Vertical
from textual.widgets import Footer, Input, Static

from codex_core.codex import Codex
from codex_core.config import Config
from codex_protocol.events import (
    ItemCompletedEvent,
    ItemStartedEvent,
    ItemUpdatedEvent,
    ThreadErrorEvent,
    TurnCompletedEvent,
    TurnFailedEvent,
    TurnStartedEvent,
)
from codex_protocol.items import (
    AgentMessageItem,
    CommandExecutionItem,
)


class StatusBar(Static):
    """Status bar showing model and working directory."""

    DEFAULT_CSS = """
    StatusBar {
        dock: top;
        height: 1;
        background: $surface;
        color: $text-muted;
        padding: 0 1;
    }
    """

    def __init__(self, model: str, cwd: Path) -> None:
        super().__init__()
        self.model = model
        self.cwd = cwd

    def compose(self) -> ComposeResult:
        yield Static(f"{self.model} @ {self.cwd}")


class UserMessage(Static):
    """User message display."""

    DEFAULT_CSS = """
    UserMessage {
        height: auto;
        padding: 0 1;
        margin: 1 0 0 0;
        background: $surface;
    }
    """

    def __init__(self, text: str) -> None:
        super().__init__()
        self._text = text

    def compose(self) -> ComposeResult:
        t = Text()
        t.append("› ", style="bold")
        t.append(self._text)
        yield Static(t)


class AssistantMessage(Static):
    """Assistant message display with markdown support."""

    DEFAULT_CSS = """
    AssistantMessage {
        height: auto;
        padding: 0 1;
        margin: 0;
    }
    """

    def __init__(self, text: str = "", message_id: str | None = None) -> None:
        super().__init__()
        self._text = text
        self.message_id = message_id

    def compose(self) -> ComposeResult:
        yield Static(Markdown(self._text) if self._text else "")

    def update_text(self, text: str) -> None:
        """Update the message text."""
        self._text = text
        self.query_one(Static).update(Markdown(text) if text else "")


class CommandCell(Static):
    """Command execution display."""

    DEFAULT_CSS = """
    CommandCell {
        height: auto;
        padding: 0 1;
        margin: 0;
    }
    CommandCell .command-header {
        height: auto;
    }
    CommandCell .command-output {
        height: auto;
        color: $text-muted;
        padding-left: 2;
    }
    """

    def __init__(
        self,
        command: str,
        output: str = "",
        exit_code: int | None = None,
        command_id: str | None = None,
    ) -> None:
        super().__init__()
        self.command = command
        self.output = output
        self.exit_code = exit_code
        self.command_id = command_id

    def compose(self) -> ComposeResult:
        yield Static(self._render_header(), classes="command-header")
        if self.output:
            yield Static(self._render_output(), classes="command-output")

    def _render_header(self) -> Text:
        t = Text()
        if self.exit_code is None:
            t.append("● ", style="bold yellow")
            t.append("Running ", style="yellow")
        elif self.exit_code == 0:
            t.append("● ", style="bold green")
            t.append("Ran ", style="green")
        else:
            t.append("● ", style="bold red")
            t.append("Ran ", style="red")
        t.append(self.command, style="bold")
        if self.exit_code is not None and self.exit_code != 0:
            t.append(f" ({self.exit_code})", style="red")
        return t

    def _render_output(self) -> Text:
        t = Text()
        lines = self.output.strip().split("\n")[:8]
        for i, line in enumerate(lines):
            prefix = "└ " if i == 0 else "  "
            t.append(f"{prefix}{line}\n", style="dim")
        if len(self.output.strip().split("\n")) > 8:
            t.append("  (...)", style="dim italic")
        return t

    def update_result(self, output: str, exit_code: int | None) -> None:
        """Update command with result."""
        self.output = output
        self.exit_code = exit_code
        self.query_one(".command-header", Static).update(self._render_header())
        if output:
            try:
                out_widget = self.query_one(".command-output", Static)
                out_widget.update(self._render_output())
            except Exception:
                self.mount(Static(self._render_output(), classes="command-output"))


class ThinkingIndicator(Static):
    """Thinking/loading indicator."""

    DEFAULT_CSS = """
    ThinkingIndicator {
        height: 1;
        padding: 0 1;
        color: $text-muted;
    }
    """

    def compose(self) -> ComposeResult:
        t = Text()
        t.append("● ", style="bold cyan")
        t.append("Thinking...", style="italic dim")
        yield Static(t)


class ChatHistory(ScrollableContainer):
    """Scrollable chat history container."""

    DEFAULT_CSS = """
    ChatHistory {
        height: 1fr;
        scrollbar-size: 1 1;
    }
    """

    def __init__(self) -> None:
        super().__init__()
        self._messages: dict[str, AssistantMessage] = {}
        self._commands: dict[str, CommandCell] = {}
        self._thinking: ThinkingIndicator | None = None

    def add_user_message(self, text: str) -> None:
        """Add a user message."""
        self.mount(UserMessage(text))
        self.scroll_end(animate=False)

    def add_thinking(self) -> None:
        """Show thinking indicator."""
        if not self._thinking:
            self._thinking = ThinkingIndicator()
            self.mount(self._thinking)
            self.scroll_end(animate=False)

    def remove_thinking(self) -> None:
        """Remove thinking indicator."""
        if self._thinking:
            self._thinking.remove()
            self._thinking = None

    def start_assistant_message(self, message_id: str, text: str = "") -> None:
        """Start streaming an assistant message."""
        self.remove_thinking()
        msg = AssistantMessage(text, message_id)
        self._messages[message_id] = msg
        self.mount(msg)
        self.scroll_end(animate=False)

    def update_assistant_message(self, message_id: str, text: str) -> None:
        """Update a streaming assistant message."""
        if message_id in self._messages:
            self._messages[message_id].update_text(text)
            self.scroll_end(animate=False)

    def complete_assistant_message(self, message_id: str, text: str) -> None:
        """Complete an assistant message."""
        if message_id in self._messages:
            self._messages[message_id].update_text(text)
            self.scroll_end(animate=False)

    def add_command_start(self, command_id: str, command: str) -> None:
        """Add a command that's starting."""
        self.remove_thinking()
        cell = CommandCell(command, command_id=command_id)
        self._commands[command_id] = cell
        self.mount(cell)
        self.scroll_end(animate=False)

    def add_command_result(
        self, command_id: str, command: str, output: str, exit_code: int | None
    ) -> None:
        """Update or add a command with its result."""
        if command_id in self._commands:
            self._commands[command_id].update_result(output, exit_code)
        else:
            cell = CommandCell(command, output, exit_code, command_id)
            self._commands[command_id] = cell
            self.mount(cell)
        self.scroll_end(animate=False)

    def add_error(self, message: str) -> None:
        """Add an error message."""
        self.remove_thinking()
        t = Text()
        t.append("! ", style="bold red")
        t.append(message, style="red")
        self.mount(Static(t, classes="error-message"))
        self.scroll_end(animate=False)


class Composer(Input):
    """Message input composer."""

    DEFAULT_CSS = """
    Composer {
        dock: bottom;
        height: 3;
        border: solid $primary;
        background: $surface;
    }
    Composer:focus {
        border: solid $accent;
    }
    """

    def __init__(self) -> None:
        super().__init__(placeholder="Type a message...")


class CodexApp(App):
    """Main Codex TUI application."""

    TITLE = "Codex"
    CSS = """
    Screen {
        background: $background;
    }
    """

    BINDINGS = [
        Binding("ctrl+c", "quit", "Quit"),
        Binding("ctrl+l", "clear", "Clear"),
        Binding("escape", "cancel", "Cancel", show=False),
    ]

    def __init__(
        self,
        config: Config,
        thread_id: str | None = None,
        initial_prompt: str | None = None,
    ) -> None:
        super().__init__()
        self.config = config
        self.thread_id = thread_id
        self.initial_prompt = initial_prompt
        self._codex: Codex | None = None
        self._running = False

    def compose(self) -> ComposeResult:
        yield StatusBar(self.config.model, self.config.cwd)
        yield ChatHistory()
        yield Composer()
        yield Footer()

    async def on_mount(self) -> None:
        """Initialize on mount."""
        self._codex = await Codex.create(self.config, self.thread_id)
        await self._codex.__aenter__()

        # Focus the composer
        self.query_one(Composer).focus()

        # Run initial prompt if provided
        if self.initial_prompt:
            self.run_turn(self.initial_prompt)

    async def on_unmount(self) -> None:
        """Cleanup on unmount."""
        if self._codex:
            await self._codex.__aexit__(None, None, None)

    @on(Input.Submitted, "Composer")
    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Handle input submission."""
        text = event.value.strip()
        if not text:
            return

        event.input.clear()

        # Handle commands
        if text.startswith("/"):
            cmd = text[1:].split()[0].lower()
            if cmd in ("quit", "q", "exit"):
                self.exit()
                return
            elif cmd == "clear":
                self.action_clear()
                return
            elif cmd == "help":
                chat = self.query_one(ChatHistory)
                chat.mount(
                    Static(Text("/quit /clear /help", style="dim"), classes="help")
                )
                return

        self.run_turn(text)

    @work(exclusive=True)
    async def run_turn(self, user_input: str) -> None:
        """Run a conversation turn."""
        if not self._codex or self._running:
            return

        self._running = True
        chat = self.query_one(ChatHistory)

        try:
            chat.add_user_message(user_input)
            current_msg_id: str | None = None

            async for event in self._codex.run_turn(user_input):
                if isinstance(event, TurnStartedEvent):
                    chat.add_thinking()

                elif isinstance(event, TurnCompletedEvent):
                    chat.remove_thinking()

                elif isinstance(event, TurnFailedEvent):
                    chat.remove_thinking()
                    chat.add_error(event.error.message)

                elif isinstance(event, ThreadErrorEvent):
                    chat.add_error(event.message)

                elif isinstance(event, ItemStartedEvent):
                    item = event.item
                    if isinstance(item.details, AgentMessageItem):
                        current_msg_id = item.id
                        chat.start_assistant_message(item.id, item.details.text)
                    elif isinstance(item.details, CommandExecutionItem):
                        chat.add_command_start(item.id, item.details.command)

                elif isinstance(event, ItemUpdatedEvent):
                    item = event.item
                    if (
                        isinstance(item.details, AgentMessageItem)
                        and item.id == current_msg_id
                    ):
                        chat.update_assistant_message(item.id, item.details.text)

                elif isinstance(event, ItemCompletedEvent):
                    item = event.item
                    if isinstance(item.details, AgentMessageItem):
                        chat.complete_assistant_message(item.id, item.details.text)
                        if item.id == current_msg_id:
                            current_msg_id = None
                    elif isinstance(item.details, CommandExecutionItem):
                        chat.add_command_result(
                            item.id,
                            item.details.command,
                            item.details.aggregated_output or "",
                            item.details.exit_code,
                        )

        except Exception as e:
            chat.add_error(str(e))
        finally:
            self._running = False

    def action_clear(self) -> None:
        """Clear chat history."""
        chat = self.query_one(ChatHistory)
        for child in list(chat.children):
            child.remove()
        chat._messages.clear()
        chat._commands.clear()
        chat._thinking = None

    def action_cancel(self) -> None:
        """Cancel current operation."""
        # TODO: Implement turn cancellation
        pass


def main() -> None:
    """Entry point."""
    import argparse

    parser = argparse.ArgumentParser(description="Codex")
    parser.add_argument("--model", help="Model")
    parser.add_argument("--cd", type=Path, help="Working directory")
    parser.add_argument("--resume", help="Thread ID to resume")
    parser.add_argument("prompt", nargs="*", help="Initial prompt")
    args = parser.parse_args()

    overrides: dict[str, Any] = {}
    if args.model:
        overrides["model"] = args.model
    if args.cd:
        overrides["cwd"] = args.cd

    config = Config.load(overrides)

    initial_prompt = " ".join(args.prompt) if args.prompt else None

    app = CodexApp(config, args.resume, initial_prompt)
    app.run()


if __name__ == "__main__":
    main()
