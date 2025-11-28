"""Codex TUI - lightweight terminal interface.

Minimal design: just chat history + input line.
No headers, no footers, no backgrounds - pure terminal.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from textual import on
from textual.app import App, ComposeResult
from textual.binding import Binding

from codex_core.codex import Codex
from codex_core.config import Config
from codex_protocol.events import (
    ItemCompletedEvent,
    ItemStartedEvent,
    ItemUpdatedEvent,
    ThreadErrorEvent,
    ThreadStartedEvent,
    TurnCompletedEvent,
    TurnFailedEvent,
    TurnStartedEvent,
)
from codex_protocol.items import (
    AgentMessageItem,
    CommandExecutionItem,
    FileChangeItem,
    ReasoningItem,
)
from codex_tui.widgets.chat_widget import ChatWidget
from codex_tui.widgets.composer import Composer


class CodexApp(App[None]):
    """Minimal Codex TUI - just chat + input."""

    CSS = """
    Screen {
        background: transparent;
    }

    ChatWidget {
        height: 1fr;
        background: transparent;
        padding: 0;
        margin: 0;
    }

    Composer {
        dock: bottom;
        height: 1;
        background: transparent;
        border: none;
        padding: 0;
        margin: 0;
    }
    """

    BINDINGS = [
        Binding("ctrl+c", "quit", "Quit", show=False),
        Binding("ctrl+l", "clear", "Clear", show=False),
    ]

    def __init__(
        self,
        config: Config | None = None,
        thread_id: str | None = None,
    ) -> None:
        super().__init__()
        self.config = config or Config.load()
        self.thread_id = thread_id
        self._codex: Codex | None = None
        self._processing = False

    def compose(self) -> ComposeResult:
        """Just chat widget and input - nothing else."""
        yield ChatWidget(id="chat")
        yield Composer(id="composer")

    async def on_mount(self) -> None:
        """Initialize."""
        self._codex = await Codex.create(self.config, self.thread_id)
        await self._codex.__aenter__()
        self.query_one("#composer", Composer).focus()

        # Minimal welcome
        chat = self.query_one("#chat", ChatWidget)
        chat.add_system_message(f"{self.config.model} @ {self.config.cwd}")

    async def on_unmount(self) -> None:
        """Cleanup."""
        if self._codex:
            await self._codex.__aexit__(None, None, None)

    @on(Composer.Submitted)
    async def handle_submit(self, event: Composer.Submitted) -> None:
        """Handle user input."""
        if self._processing:
            return

        text = event.text.strip()
        if not text:
            return

        composer = self.query_one("#composer", Composer)
        chat = self.query_one("#chat", ChatWidget)
        composer.clear()

        # Slash commands
        if text.startswith("/"):
            self._handle_command(text, chat)
            return

        chat.add_user_message(text)

        self._processing = True
        try:
            await self._run_turn(text)
        finally:
            self._processing = False
            composer.focus()

    def _handle_command(self, text: str, chat: ChatWidget) -> None:
        """Handle slash commands."""
        cmd = text[1:].split()[0].lower()

        if cmd == "clear":
            chat.clear()
        elif cmd == "quit" or cmd == "q":
            self.exit()
        elif cmd == "help":
            chat.add_system_message("/clear /quit /help")
        else:
            chat.add_error_message(f"Unknown: {cmd}")

    async def _run_turn(self, user_input: str) -> None:
        """Run a conversation turn."""
        if not self._codex:
            return

        chat = self.query_one("#chat", ChatWidget)
        current_msg_id: str | None = None

        async for event in self._codex.run_turn(user_input):
            if isinstance(event, TurnStartedEvent):
                chat.add_thinking_indicator()

            elif isinstance(event, TurnCompletedEvent):
                chat.remove_thinking_indicator()

            elif isinstance(event, TurnFailedEvent):
                chat.remove_thinking_indicator()
                chat.add_error_message(event.error.message)

            elif isinstance(event, ThreadErrorEvent):
                chat.add_error_message(event.message)

            elif isinstance(event, ItemStartedEvent):
                item = event.item
                if isinstance(item.details, AgentMessageItem):
                    current_msg_id = item.id
                    chat.start_agent_message(item.id, item.details.text)
                elif isinstance(item.details, CommandExecutionItem):
                    chat.add_command_start(item.id, item.details.command)

            elif isinstance(event, ItemUpdatedEvent):
                item = event.item
                if isinstance(item.details, AgentMessageItem) and item.id == current_msg_id:
                    chat.update_agent_message(item.id, item.details.text)

            elif isinstance(event, ItemCompletedEvent):
                item = event.item
                if isinstance(item.details, AgentMessageItem) and item.id == current_msg_id:
                    chat.complete_agent_message(item.id, item.details.text)
                    current_msg_id = None
                elif isinstance(item.details, CommandExecutionItem):
                    chat.add_command_result(
                        item.id,
                        item.details.command,
                        item.details.aggregated_output,
                        item.details.exit_code,
                    )

    def action_clear(self) -> None:
        """Clear chat."""
        self.query_one("#chat", ChatWidget).clear()


def main() -> None:
    """Entry point."""
    import argparse

    parser = argparse.ArgumentParser(description="Codex")
    parser.add_argument("--model", help="Model")
    parser.add_argument("--cd", type=Path, help="Working directory")
    parser.add_argument("--resume", help="Thread ID")
    args = parser.parse_args()

    overrides: dict[str, Any] = {}
    if args.model:
        overrides["model"] = args.model
    if args.cd:
        overrides["cwd"] = args.cd

    config = Config.load(overrides)
    app = CodexApp(config=config, thread_id=args.resume)
    app.run()


if __name__ == "__main__":
    main()
