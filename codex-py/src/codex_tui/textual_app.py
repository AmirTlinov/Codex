"""Textual-based full-screen TUI for Codex.

This provides a rich terminal interface similar to codex-rs,
with scrollable chat history, command output display, and approval dialogs.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Container
from textual.widgets import Footer, Header, Static

from codex_core.approval import (
    ApprovalDecision,
    ApprovalRequest,
    ApprovalType,
)
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
    McpToolCallItem,
    McpToolCallStatus,
)
from codex_tui.widgets.approval import ApprovalDialog, ApprovalResult
from codex_tui.widgets.chat import ChatCell, ChatWidget
from codex_tui.widgets.commands import CommandPopup, SlashCommand
from codex_tui.widgets.input import InputWidget
from codex_tui.widgets.spinner import ThinkingIndicator
from codex_tui.widgets.status import StatusBar


class TextualApprovalHandler:
    """Approval handler that uses Textual modal dialogs."""

    def __init__(self, app: CodexApp) -> None:
        self._app = app
        self._pending_result: asyncio.Future[ApprovalDecision] | None = None

    async def request_approval(self, request: ApprovalRequest) -> ApprovalDecision:
        """Show approval dialog and wait for result."""
        if request.approval_type == ApprovalType.COMMAND:
            title = "Command requires approval"
            content = request.command or ""
            content_type = "command"
        elif request.approval_type == ApprovalType.PATCH:
            title = "File modification requires approval"
            content = request.patch_diff or f"File: {request.patch_path}"
            content_type = "diff"
        elif request.approval_type == ApprovalType.MCP_TOOL:
            title = f"MCP tool call requires approval: {request.mcp_server}.{request.mcp_tool}"
            import json
            content = json.dumps(request.mcp_arguments, indent=2) if request.mcp_arguments else ""
            content_type = "json"
        else:
            title = "Approval required"
            content = str(request)
            content_type = "text"

        # Create future for result
        self._pending_result = asyncio.get_event_loop().create_future()

        # Show dialog
        dialog = ApprovalDialog(title, content, content_type)

        def on_dismiss(result: ApprovalResult | None) -> None:
            if self._pending_result and not self._pending_result.done():
                if result == ApprovalResult.APPROVED:
                    self._pending_result.set_result(ApprovalDecision.APPROVED)
                elif result == ApprovalResult.ALWAYS:
                    self._pending_result.set_result(ApprovalDecision.ALWAYS_APPROVE)
                else:
                    self._pending_result.set_result(ApprovalDecision.REJECTED)

        self._app.push_screen(dialog, on_dismiss)

        return await self._pending_result


class WelcomeWidget(Static):
    """Welcome header widget."""

    def __init__(self, config: Config, id: str | None = None) -> None:
        super().__init__(id=id)
        self._config = config

    def compose(self) -> ComposeResult:
        """No children."""
        return []

    def render(self) -> Text:
        """Render welcome message."""
        # Get relative path for CWD
        cwd_display = str(self._config.cwd)
        try:
            rel = self._config.cwd.relative_to(Path.home())
            cwd_display = f"~/{rel}" if str(rel) != "." else "~"
        except ValueError:
            pass

        text = Text()
        text.append(">_ ", style="dim")
        text.append("OpenAI Codex", style="bold")
        text.append(" (Python)", style="dim")
        text.append("\n\n")
        text.append("model:     ", style="dim")
        text.append(self._config.model)
        text.append("\n")
        text.append("directory: ", style="dim")
        text.append(cwd_display)
        text.append("\n")

        return text


class CodexApp(App[None]):
    """Main Textual application for Codex.

    Provides full-screen TUI with:
    - Scrollable chat history
    - Command output display
    - Approval dialogs
    - Status bar with token usage
    """

    TITLE = "Codex"
    CSS = """
    Screen {
        background: $background;
    }

    #welcome {
        height: auto;
        padding: 1;
        margin-bottom: 1;
    }

    #chat-container {
        height: 1fr;
    }

    #thinking {
        height: auto;
        padding: 0 1;
        color: $text-muted;
    }
    """

    BINDINGS = [
        Binding("ctrl+c", "quit", "Quit", show=True),
        Binding("ctrl+l", "clear", "Clear", show=True),
        Binding("ctrl+u", "clear_input", "Clear Input", show=False),
        Binding("escape", "cancel", "Cancel", show=False),
        Binding("f1", "help", "Help", show=True),
        Binding("pageup", "scroll_up", "Scroll Up", show=False),
        Binding("pagedown", "scroll_down", "Scroll Down", show=False),
        Binding("home", "scroll_top", "Top", show=False),
        Binding("end", "scroll_bottom", "Bottom", show=False),
    ]

    def __init__(self, config: Config, thread_id: str | None = None) -> None:
        super().__init__()
        self._config = config
        self._conversation_thread_id = thread_id
        self._codex: Codex | None = None
        self._approval_handler: TextualApprovalHandler | None = None
        self._current_task: asyncio.Task[None] | None = None

        # Widget references
        self._chat: ChatWidget | None = None
        self._input: InputWidget | None = None
        self._status: StatusBar | None = None
        self._thinking: ThinkingIndicator | None = None

        # Active cells for updates
        self._active_message: ChatCell | None = None
        self._active_command: ChatCell | None = None
        self._active_mcp: ChatCell | None = None

        # Command popup
        self._command_popup: CommandPopup | None = None

    def compose(self) -> ComposeResult:
        """Create the app layout."""
        yield Header()
        yield WelcomeWidget(self._config, id="welcome")
        yield Container(ChatWidget(), id="chat-container")
        yield ThinkingIndicator()
        yield InputWidget()
        yield StatusBar()
        yield Footer()

    async def on_mount(self) -> None:
        """Initialize the app on mount."""
        # Get widget references
        self._chat = self.query_one(ChatWidget)
        self._input = self.query_one(InputWidget)
        self._status = self.query_one(StatusBar)
        self._thinking = self.query_one(ThinkingIndicator)

        # Configure status bar
        self._status.set_model(self._config.model)

        cwd_display = str(self._config.cwd)
        try:
            rel = self._config.cwd.relative_to(Path.home())
            cwd_display = f"~/{rel}" if str(rel) != "." else "~"
        except ValueError:
            pass
        self._status.set_cwd(cwd_display)

        # Create approval handler
        self._approval_handler = TextualApprovalHandler(self)

        # Create Codex instance
        self._codex = await Codex.create(
            self._config,
            self._conversation_thread_id,
            approval_handler=self._approval_handler,
        )
        await self._codex.__aenter__()

        # Focus input
        self._input.focus()

    async def on_unmount(self) -> None:
        """Cleanup on unmount."""
        if self._codex:
            await self._codex.__aexit__(None, None, None)

    def on_input_widget_submitted(self, event: InputWidget.Submitted) -> None:
        """Handle user input submission."""
        user_input = event.value.strip()

        if not user_input:
            return

        # Hide command popup
        self._hide_command_popup()

        # Handle slash commands
        if user_input.startswith("/"):
            if self._handle_slash_command(user_input):
                return

        # Run conversation turn
        self._current_task = asyncio.create_task(self._run_turn(user_input))

    def _handle_slash_command(self, user_input: str) -> bool:
        """Handle slash command. Returns True if handled locally."""
        cmd = user_input[1:].split()[0].lower() if user_input[1:] else ""

        # Map command strings to enum
        cmd_map = {c.value: c for c in SlashCommand}
        slash_cmd = cmd_map.get(cmd)

        if slash_cmd == SlashCommand.QUIT or slash_cmd == SlashCommand.EXIT:
            self.exit()
            return True
        elif slash_cmd == SlashCommand.CLEAR:
            self.action_clear()
            return True
        elif slash_cmd == SlashCommand.HELP:
            self._show_help()
            return True
        elif slash_cmd == SlashCommand.STATUS:
            self._show_status()
            return True
        elif slash_cmd == SlashCommand.DIFF:
            self._show_diff()
            return True
        elif slash_cmd == SlashCommand.MCP:
            self._show_mcp_tools()
            return True
        # Commands that go to the model
        elif cmd in ("q", "?"):
            # Aliases
            if cmd == "q":
                self.exit()
                return True
            elif cmd == "?":
                self._show_help()
                return True

        return False

    def on_input_widget_show_command_popup(
        self, event: InputWidget.ShowCommandPopup
    ) -> None:
        """Show command completion popup."""
        if not self._command_popup:
            self._command_popup = CommandPopup()
            self.mount(self._command_popup)
        self._command_popup.set_filter(event.filter_text)
        self._command_popup.display = True

    def on_input_widget_hide_command_popup(
        self, event: InputWidget.HideCommandPopup
    ) -> None:
        """Hide command completion popup."""
        self._hide_command_popup()

    def _hide_command_popup(self) -> None:
        """Hide the command popup."""
        if self._command_popup:
            self._command_popup.display = False

    async def _run_turn(self, user_input: str) -> None:
        """Run a conversation turn."""
        if not self._codex or not self._chat:
            return

        # Add user message
        self._chat.add_user_message(user_input)

        # Show thinking
        self._show_thinking(True)
        if self._status:
            self._status.set_status("thinking")

        try:
            async for event in self._codex.run_turn(user_input):
                await self._handle_event(event)
        except Exception as e:
            self._chat.add_error(str(e))
        finally:
            self._show_thinking(False)
            if self._status:
                self._status.set_status("ready")
            self._active_message = None
            self._active_command = None
            self._active_mcp = None

    async def _handle_event(self, event: Any) -> None:
        """Handle a codex event."""
        if not self._chat or not self._status:
            return

        if isinstance(event, TurnStartedEvent):
            pass  # Already showing thinking

        elif isinstance(event, TurnCompletedEvent):
            if event.usage:
                self._status.add_tokens(
                    event.usage.input_tokens,
                    event.usage.output_tokens,
                )
            self._status.increment_turns()

        elif isinstance(event, TurnFailedEvent):
            self._chat.add_error(event.error.message)

        elif isinstance(event, ThreadErrorEvent):
            self._chat.add_error(event.message)

        elif isinstance(event, ItemStartedEvent):
            item = event.item

            if isinstance(item.details, AgentMessageItem):
                self._show_thinking(False)
                self._active_message = self._chat.add_agent_message(item.details.text)

            elif isinstance(item.details, CommandExecutionItem):
                self._show_thinking(False)
                self._status.set_status("running")
                self._active_command = self._chat.add_command_start(
                    item.details.command,
                    item.id,
                )

            elif isinstance(item.details, McpToolCallItem):
                self._show_thinking(False)
                self._status.set_status("running")
                self._active_mcp = self._chat.add_mcp_start(
                    item.details.server,
                    item.details.tool,
                    item.id,
                    item.details.arguments,
                )

        elif isinstance(event, ItemUpdatedEvent):
            item = event.item

            if isinstance(item.details, AgentMessageItem) and self._active_message:
                self._chat.update_agent_message(
                    self._active_message,
                    item.details.text,
                )

        elif isinstance(event, ItemCompletedEvent):
            item = event.item

            if isinstance(item.details, AgentMessageItem):
                if self._active_message:
                    self._chat.update_agent_message(
                        self._active_message,
                        item.details.text,
                    )
                self._active_message = None

            elif isinstance(item.details, CommandExecutionItem):
                if self._active_command:
                    self._chat.complete_command(
                        self._active_command,
                        item.details.aggregated_output or "",
                        item.details.exit_code or 0,
                    )
                self._active_command = None
                self._status.set_status("thinking")

            elif isinstance(item.details, McpToolCallItem):
                if self._active_mcp:
                    result = ""
                    if item.details.result and item.details.result.content:
                        for content in item.details.result.content:
                            if isinstance(content, dict) and content.get("type") == "text":
                                result = content.get("text", "")
                                break
                    self._chat.complete_mcp(
                        self._active_mcp,
                        result,
                        item.details.status == McpToolCallStatus.COMPLETED,
                    )
                self._active_mcp = None
                self._status.set_status("thinking")

    def _show_thinking(self, show: bool, message: str = "Thinking") -> None:
        """Show or hide thinking indicator."""
        if self._thinking:
            if show:
                self._thinking.show(message)
            else:
                self._thinking.hide()

    def _show_help(self) -> None:
        """Show help message."""
        if self._chat:
            self._chat.add_system(
                "Commands:\n"
                "  /help         - Show this help\n"
                "  /clear        - Clear the screen\n"
                "  /status       - Show session info\n"
                "  /diff         - Show git diff\n"
                "  /mcp          - List MCP tools\n"
                "  /quit         - Exit\n"
                "\n"
                "Keyboard shortcuts:\n"
                "  Ctrl+C        - Exit\n"
                "  Ctrl+L        - Clear screen\n"
                "  Ctrl+U        - Clear input\n"
                "  Up/Down       - History / command navigation\n"
                "  Tab           - Complete command\n"
                "  Escape        - Cancel\n"
                "  PageUp/Down   - Scroll chat\n"
                "  F1            - Show help\n"
            )

    def _show_status(self) -> None:
        """Show session status."""
        if self._chat and self._status:
            self._chat.add_system(
                f"Model: {self._config.model}\n"
                f"Directory: {self._config.cwd}\n"
                f"Tokens: {self._status._tokens_in:,} in / {self._status._tokens_out:,} out\n"
            )

    def _show_diff(self) -> None:
        """Show git diff."""
        import subprocess

        try:
            result = subprocess.run(
                ["git", "diff", "--stat"],
                capture_output=True,
                text=True,
                cwd=self._config.cwd,
                timeout=5,
            )
            if result.returncode == 0 and result.stdout.strip():
                if self._chat:
                    self._chat.add_system(f"Git diff:\n{result.stdout}")
            else:
                if self._chat:
                    self._chat.add_system("No changes detected")
        except Exception as e:
            if self._chat:
                self._chat.add_error(f"Failed to get diff: {e}")

    def _show_mcp_tools(self) -> None:
        """Show configured MCP tools."""
        if self._chat:
            if self._codex and hasattr(self._codex, "_mcp_manager"):
                # Get tools from MCP manager if available
                tools_info = "MCP tools are available via configuration."
            else:
                tools_info = "No MCP servers configured."
            self._chat.add_system(f"MCP Tools:\n{tools_info}")

    def action_clear(self) -> None:
        """Clear the chat history."""
        if self._chat:
            self._chat.remove_children()

    def action_clear_input(self) -> None:
        """Clear the input field."""
        if self._input:
            self._input.value = ""

    def action_cancel(self) -> None:
        """Cancel the current operation."""
        if self._current_task and not self._current_task.done():
            self._current_task.cancel()
            if self._chat:
                self._chat.add_warning("Operation cancelled")
            self._show_thinking(False)
            if self._status:
                self._status.set_status("ready")

    def action_help(self) -> None:
        """Show help."""
        self._show_help()

    def action_scroll_up(self) -> None:
        """Scroll chat up."""
        if self._chat:
            self._chat.scroll_up(animate=False)

    def action_scroll_down(self) -> None:
        """Scroll chat down."""
        if self._chat:
            self._chat.scroll_down(animate=False)

    def action_scroll_top(self) -> None:
        """Scroll to top of chat."""
        if self._chat:
            self._chat.scroll_home(animate=False)

    def action_scroll_bottom(self) -> None:
        """Scroll to bottom of chat."""
        if self._chat:
            self._chat.scroll_end(animate=False)


async def run_textual_app(config: Config, thread_id: str | None = None) -> None:
    """Run the Textual TUI application."""
    app = CodexApp(config, thread_id)
    await app.run_async()


def main() -> None:
    """Entry point for Textual TUI."""
    import argparse
    from pathlib import Path

    parser = argparse.ArgumentParser(description="Codex TUI")
    parser.add_argument("--model", help="Model to use")
    parser.add_argument("--cd", type=Path, help="Working directory")
    parser.add_argument("--resume", help="Thread ID to resume")
    args = parser.parse_args()

    overrides: dict[str, Any] = {}
    if args.model:
        overrides["model"] = args.model
    if args.cd:
        overrides["cwd"] = args.cd

    config = Config.load(overrides)

    asyncio.run(run_textual_app(config, args.resume))


if __name__ == "__main__":
    main()
