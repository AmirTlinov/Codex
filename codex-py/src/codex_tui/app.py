"""Main Textual application for Codex TUI.

This is the entry point for the interactive terminal interface.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from rich.text import Text
from textual import on
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Container
from textual.widgets import Footer, Header

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
from codex_tui.widgets.approval import (
    ApprovalOverlay,
    ApprovalQueue,
    ApprovalRequest,
    ApprovalResult,
    ApprovalType,
)
from codex_tui.widgets.chat_widget import ChatWidget
from codex_tui.widgets.composer import Composer


class CodexApp(App[None]):
    """Main Codex TUI application."""

    CSS_PATH = "styles/app.tcss"
    TITLE = "Codex"

    BINDINGS = [
        Binding("ctrl+c", "interrupt", "Interrupt", show=True),
        Binding("ctrl+q", "quit", "Quit", show=True),
        Binding("ctrl+l", "clear", "Clear", show=False),
        Binding("escape", "focus_composer", "Focus Input", show=False),
        Binding("ctrl+s", "toggle_shell_panel", "Shells", show=False),
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
        self._approval_queue = ApprovalQueue()
        self._pending_approval: asyncio.Event | None = None
        self._approval_result: ApprovalResult | None = None

    def compose(self) -> ComposeResult:
        """Compose the UI."""
        yield Header()
        yield Container(
            ChatWidget(id="chat"),
            Composer(id="composer"),
            id="main",
        )
        yield Footer()

    async def on_mount(self) -> None:
        """Initialize when app is mounted."""
        # Create Codex instance
        self._codex = await Codex.create(self.config, self.thread_id)
        await self._codex.__aenter__()

        # Focus composer
        self.query_one("#composer", Composer).focus()

        # Show welcome message
        chat = self.query_one("#chat", ChatWidget)
        chat.add_system_message(
            f"Welcome to Codex! Model: {self.config.model}\n"
            f"Working directory: {self.config.cwd}\n"
            f"Type your message and press Enter to send."
        )

    async def on_unmount(self) -> None:
        """Clean up when app is unmounted."""
        if self._codex:
            await self._codex.__aexit__(None, None, None)

    @on(Composer.Submitted)
    async def handle_submit(self, event: Composer.Submitted) -> None:
        """Handle user input submission."""
        if self._processing:
            return

        text = event.text.strip()
        if not text:
            return

        # Handle special commands
        if text.startswith("/"):
            await self._handle_command(text)
            return

        # Clear composer
        composer = self.query_one("#composer", Composer)
        composer.clear()

        # Add user message to chat
        chat = self.query_one("#chat", ChatWidget)
        chat.add_user_message(text)

        # Process the message
        self._processing = True
        try:
            await self._run_turn(text)
        finally:
            self._processing = False
            composer.focus()

    async def _handle_command(self, text: str) -> None:
        """Handle slash commands."""
        chat = self.query_one("#chat", ChatWidget)
        composer = self.query_one("#composer", Composer)
        composer.clear()

        parts = text[1:].split(maxsplit=1)
        cmd = parts[0].lower()
        args = parts[1] if len(parts) > 1 else ""

        if cmd == "clear":
            chat.clear()
            chat.add_system_message("Chat cleared.")
        elif cmd == "help":
            chat.add_system_message(
                "Available commands:\n"
                "  /clear - Clear chat history\n"
                "  /model - Show current model\n"
                "  /quit - Exit Codex\n"
                "  /help - Show this help"
            )
        elif cmd == "model":
            chat.add_system_message(f"Current model: {self.config.model}")
        elif cmd == "quit":
            self.exit()
        else:
            chat.add_error_message(f"Unknown command: /{cmd}")

    async def _run_turn(self, user_input: str) -> None:
        """Run a turn with the given user input."""
        if not self._codex:
            return

        chat = self.query_one("#chat", ChatWidget)
        current_message_id: str | None = None
        current_message_text: str = ""

        async for event in self._codex.run_turn(user_input):
            if isinstance(event, ThreadStartedEvent):
                self.sub_title = f"Thread: {event.thread_id[:8]}..."

            elif isinstance(event, TurnStartedEvent):
                chat.add_thinking_indicator()

            elif isinstance(event, TurnCompletedEvent):
                chat.remove_thinking_indicator()
                chat.add_usage_info(event.usage)

            elif isinstance(event, TurnFailedEvent):
                chat.remove_thinking_indicator()
                chat.add_error_message(event.error.message)

            elif isinstance(event, ThreadErrorEvent):
                chat.add_error_message(event.message)

            elif isinstance(event, ItemStartedEvent):
                item = event.item
                if isinstance(item.details, AgentMessageItem):
                    current_message_id = item.id
                    current_message_text = item.details.text
                    chat.start_agent_message(item.id, item.details.text)
                elif isinstance(item.details, CommandExecutionItem):
                    chat.add_command_start(item.id, item.details.command)
                elif isinstance(item.details, ReasoningItem):
                    # Handle thinking/reasoning
                    pass

            elif isinstance(event, ItemUpdatedEvent):
                item = event.item
                if isinstance(item.details, AgentMessageItem) and item.id == current_message_id:
                    current_message_text = item.details.text
                    chat.update_agent_message(item.id, item.details.text)

            elif isinstance(event, ItemCompletedEvent):
                item = event.item
                if isinstance(item.details, AgentMessageItem) and item.id == current_message_id:
                    chat.complete_agent_message(item.id, item.details.text)
                    current_message_id = None
                    current_message_text = ""
                elif isinstance(item.details, CommandExecutionItem):
                    chat.add_command_result(
                        item.id,
                        item.details.command,
                        item.details.aggregated_output,
                        item.details.exit_code,
                    )
                elif isinstance(item.details, FileChangeItem):
                    # Show file change notification
                    chat.add_system_message(
                        f"File modified: {item.details.path}"
                    )

    async def request_command_approval(
        self,
        request_id: str,
        command: str,
    ) -> bool:
        """Request approval for command execution.

        Returns True if approved.
        """
        request = self._approval_queue.add_command(
            request_id=request_id,
            command=command,
            description="Codex wants to execute this command",
        )

        if request is None:
            # Auto-approved
            return True

        return await self._show_approval(request)

    async def request_patch_approval(
        self,
        request_id: str,
        path: str,
        diff: str,
    ) -> bool:
        """Request approval for patch application.

        Returns True if approved.
        """
        request = self._approval_queue.add_patch(
            request_id=request_id,
            path=path,
            diff=diff,
            description="Codex wants to apply this change",
        )

        return await self._show_approval(request)

    async def _show_approval(self, request: ApprovalRequest) -> bool:
        """Show approval overlay and wait for decision."""
        self._pending_approval = asyncio.Event()
        self._approval_result = None

        # Push the overlay
        overlay = ApprovalOverlay(request)

        def on_dismiss(result: ApprovalResult) -> None:
            self._approval_result = result
            if self._pending_approval:
                self._pending_approval.set()

        self.push_screen(overlay, on_dismiss)

        # Wait for decision
        await self._pending_approval.wait()

        # Process result
        result = self._approval_result or ApprovalResult.REJECTED
        return self._approval_queue.resolve(request.request_id, result)

    def action_interrupt(self) -> None:
        """Interrupt the current operation."""
        if self._processing:
            # TODO: Implement proper interruption
            chat = self.query_one("#chat", ChatWidget)
            chat.add_system_message("Interruption requested (not yet implemented)")

    def action_clear(self) -> None:
        """Clear the chat history."""
        chat = self.query_one("#chat", ChatWidget)
        chat.clear()

    def action_focus_composer(self) -> None:
        """Focus the composer widget."""
        self.query_one("#composer", Composer).focus()

    def action_toggle_shell_panel(self) -> None:
        """Toggle the shell panel visibility."""
        # TODO: Implement shell panel toggle
        pass


def main() -> None:
    """Main entry point for the TUI."""
    import argparse

    parser = argparse.ArgumentParser(description="Codex TUI")
    parser.add_argument("--model", help="Model to use")
    parser.add_argument("--cd", type=Path, help="Working directory")
    parser.add_argument("--resume", help="Thread ID to resume")
    args = parser.parse_args()

    # Build config
    overrides: dict[str, Any] = {}
    if args.model:
        overrides["model"] = args.model
    if args.cd:
        overrides["cwd"] = args.cd

    config = Config.load(overrides)

    # Run app
    app = CodexApp(config=config, thread_id=args.resume)
    app.run()


if __name__ == "__main__":
    main()
