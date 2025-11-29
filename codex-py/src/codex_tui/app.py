"""Codex CLI - inline terminal interface like codex-rs.

Uses rich for formatting but renders inline (no alternate screen).
Matches codex-rs TUI visual style with proper message styling.
"""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
from typing import Any

from rich.console import Console
from rich.live import Live
from rich.panel import Panel
from rich.prompt import Prompt
from rich.style import Style
from rich.syntax import Syntax
from rich.text import Text

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

# Console for inline output
console = Console(highlight=False)


class RichApprovalHandler:
    """Interactive approval handler using rich prompts.

    Asks user to approve commands, patches, and MCP tool calls.
    Supports [y]es, [n]o, [a]lways options.
    """

    def __init__(self, console: Console | None = None) -> None:
        self._console = console or Console()

    async def request_approval(self, request: ApprovalRequest) -> ApprovalDecision:
        """Display approval request and get user decision."""
        # Render request details
        self._render_request(request)

        # Get user input
        response = Prompt.ask(
            "[bold]Approve?[/bold]",
            choices=["y", "n", "a"],
            default="y",
            console=self._console,
        )

        if response == "y":
            return ApprovalDecision.APPROVED
        elif response == "a":
            return ApprovalDecision.ALWAYS_APPROVE
        else:
            return ApprovalDecision.REJECTED

    def _render_request(self, request: ApprovalRequest) -> None:
        """Render approval request details."""
        if request.approval_type == ApprovalType.COMMAND:
            self._render_command_request(request)
        elif request.approval_type == ApprovalType.PATCH:
            self._render_patch_request(request)
        elif request.approval_type == ApprovalType.MCP_TOOL:
            self._render_mcp_request(request)

    def _render_command_request(self, request: ApprovalRequest) -> None:
        """Render command approval request."""
        text = Text()
        text.append("\n⚠ ", style="yellow bold")
        text.append("Command requires approval\n", style="bold")
        text.append("  $ ", style="dim")
        text.append(request.command or "", style="bold cyan")
        text.append("\n")
        self._console.print(text)

    def _render_patch_request(self, request: ApprovalRequest) -> None:
        """Render patch approval request."""
        text = Text()
        text.append("\n⚠ ", style="yellow bold")
        text.append("File modification requires approval\n", style="bold")
        text.append("  File: ", style="dim")
        text.append(request.patch_path or "", style="bold")
        text.append("\n")
        self._console.print(text)

        # Show diff if available
        if request.patch_diff:
            syntax = Syntax(
                request.patch_diff,
                "diff",
                theme="monokai",
                line_numbers=False,
            )
            self._console.print(Panel(syntax, title="Changes", border_style="dim"))

    def _render_mcp_request(self, request: ApprovalRequest) -> None:
        """Render MCP tool call approval request."""
        text = Text()
        text.append("\n⚠ ", style="yellow bold")
        text.append("MCP tool call requires approval\n", style="bold")
        text.append("  Server: ", style="dim")
        text.append(request.mcp_server or "", style="bold")
        text.append("\n  Tool: ", style="dim")
        text.append(request.mcp_tool or "", style="bold cyan")
        text.append("\n")
        self._console.print(text)

        # Show arguments if available
        if request.mcp_arguments:
            import json
            args_str = json.dumps(request.mcp_arguments, indent=2)
            syntax = Syntax(args_str, "json", theme="monokai", line_numbers=False)
            self._console.print(Panel(syntax, title="Arguments", border_style="dim"))


def get_user_message_style() -> Style:
    """Get user message background style adapting to terminal colors.

    Matches codex-rs: 10% blend of white/black with terminal background.
    Falls back to subtle gray for terminals that don't support color queries.
    """
    # Try to detect terminal background (simplified - codex-rs uses crossterm queries)
    # Most terminals are dark, so default to dark theme behavior
    colorfgbg = os.environ.get("COLORFGBG", "")
    is_light = False
    if colorfgbg:
        # Format: "fg;bg" where bg > 8 typically means light theme
        parts = colorfgbg.split(";")
        if len(parts) >= 2:
            try:
                bg_color = int(parts[-1])
                is_light = bg_color > 8
            except ValueError:
                pass

    # 10% blend: dark terminal -> subtle white tint, light terminal -> subtle dark tint
    if is_light:
        # Light theme: blend with black (25, 25, 25 approximation of 10% black)
        return Style(bgcolor="grey15")
    else:
        # Dark theme: blend with white (38, 38, 38 approximation of 10% white on black)
        return Style(bgcolor="grey23")


def render_user_message(text: str) -> Text:
    """Render user message with › prefix and background (matches codex-rs UserHistoryCell)."""
    style = get_user_message_style()
    result = Text()

    # Empty line before (with background)
    result.append("\n", style=style)

    # Content lines with prefix
    lines = text.split("\n")
    for i, line in enumerate(lines):
        if i == 0:
            result.append("› ", style=Style(bold=True, dim=True) + style)
        else:
            result.append("  ", style=style)
        result.append(line, style=style)
        result.append("\n", style=style)

    # Empty line after (with background)
    result.append("\n", style=style)

    return result


def render_thinking() -> Text:
    """Render thinking indicator with bullet prefix."""
    result = Text()
    result.append("• ", style="dim")
    result.append("Thinking...", style="dim italic")
    return result


def render_command_start(cmd: str) -> Text:
    """Render command execution starting (matches codex-rs ExecCell 'Running')."""
    result = Text()
    result.append("• ", style="dim")
    result.append("Running ", style="bold")
    result.append(cmd, style="")
    result.append(" ...", style="dim italic")
    return result


def render_command_complete(cmd: str, output: str | None, exit_code: int | None) -> Text:
    """Render completed command with output (matches codex-rs ExecCell 'Ran')."""
    result = Text()

    # Header line: • Ran <command>
    if exit_code == 0 or exit_code is None:
        result.append("• ", style="green bold")
    else:
        result.append("• ", style="red bold")

    result.append("Ran ", style="bold")
    result.append(cmd, style="")

    if exit_code is not None and exit_code != 0:
        result.append(f" (exit {exit_code})", style="red")

    result.append("\n")

    # Output lines with tree prefix (└ for first, spaces for rest)
    if output:
        lines = output.strip().split("\n")
        max_lines = 10
        for i, line in enumerate(lines[:max_lines]):
            if i == 0:
                result.append("  └ ", style="dim")
            else:
                result.append("    ", style="dim")
            result.append(line, style="dim")
            result.append("\n")

        if len(lines) > max_lines:
            result.append(f"    ... ({len(lines) - max_lines} more lines)\n", style="dim italic")

    return result


def render_assistant_text(text: str, is_first_chunk: bool = True) -> Text:
    """Render assistant message text (matches codex-rs AgentMessageCell).

    Uses • prefix for first line, spaces for continuation.
    """
    result = Text()
    lines = text.split("\n")

    for i, line in enumerate(lines):
        if i == 0 and is_first_chunk:
            result.append("• ", style="dim")
        else:
            result.append("  ")
        result.append(line)
        result.append("\n")

    return result


def render_error(message: str) -> Text:
    """Render error message (matches codex-rs new_error_event)."""
    result = Text()
    result.append("■ ", style="red")
    result.append(message, style="red")
    result.append("\n")
    return result


def render_mcp_start(server: str, tool: str) -> Text:
    """Render MCP tool call starting."""
    result = Text()
    result.append("• ", style="dim")
    result.append("Calling ", style="bold")
    result.append(f"{server}.", style="cyan dim")
    result.append(tool, style="cyan")
    result.append(" ...", style="dim italic")
    return result


def render_mcp_complete(mcp: McpToolCallItem) -> Text:
    """Render completed MCP tool call with result."""
    result = Text()

    # Header line: • Called server.tool
    if mcp.status == McpToolCallStatus.COMPLETED:
        result.append("• ", style="green bold")
    else:
        result.append("• ", style="red bold")

    result.append("Called ", style="bold")
    result.append(f"{mcp.server}.", style="cyan dim")
    result.append(mcp.tool, style="cyan")

    if mcp.status == McpToolCallStatus.FAILED:
        result.append(" (failed)", style="red")

    result.append("\n")

    # Show result or error (first line only)
    if mcp.result and mcp.result.content:
        # Extract text from MCP content
        for content_item in mcp.result.content[:1]:
            if isinstance(content_item, dict) and content_item.get("type") == "text":
                text = content_item.get("text", "")
                lines = text.strip().split("\n")
                if lines:
                    result.append("  └ ", style="dim")
                    result.append(lines[0][:80], style="dim")
                    if len(lines) > 1 or len(lines[0]) > 80:
                        result.append("...", style="dim italic")
                    result.append("\n")
                break
    elif mcp.error:
        result.append("  └ ", style="dim")
        result.append(mcp.error.message[:80], style="red dim")
        result.append("\n")

    return result


async def run_turn(codex: Codex, user_input: str) -> None:
    """Run a single conversation turn."""
    current_msg_id: str | None = None
    current_text = ""
    live: Live | None = None
    is_first_message_chunk = True

    # Show user message with proper styling
    console.print(render_user_message(user_input), end="")

    try:
        async for event in codex.run_turn(user_input):
            if isinstance(event, TurnStartedEvent):
                # Show thinking indicator
                live = Live(render_thinking(), console=console, refresh_per_second=4)
                live.start()

            elif isinstance(event, TurnCompletedEvent):
                if live:
                    live.stop()
                    live = None

            elif isinstance(event, TurnFailedEvent):
                if live:
                    live.stop()
                    live = None
                console.print(render_error(event.error.message), end="")

            elif isinstance(event, ThreadErrorEvent):
                if live:
                    live.stop()
                    live = None
                console.print(render_error(event.message), end="")

            elif isinstance(event, ItemStartedEvent):
                item = event.item

                if isinstance(item.details, AgentMessageItem):
                    if live:
                        live.stop()
                        live = None
                    current_msg_id = item.id
                    current_text = item.details.text
                    is_first_message_chunk = True
                    live = Live(
                        render_assistant_text(current_text, is_first_message_chunk),
                        console=console,
                        refresh_per_second=10,
                    )
                    live.start()

                elif isinstance(item.details, CommandExecutionItem):
                    if live:
                        live.stop()
                        live = None
                    live = Live(
                        render_command_start(item.details.command),
                        console=console,
                        refresh_per_second=4,
                    )
                    live.start()

                elif isinstance(item.details, McpToolCallItem):
                    if live:
                        live.stop()
                        live = None
                    live = Live(
                        render_mcp_start(item.details.server, item.details.tool),
                        console=console,
                        refresh_per_second=4,
                    )
                    live.start()

            elif isinstance(event, ItemUpdatedEvent):
                item = event.item
                if isinstance(item.details, AgentMessageItem) and item.id == current_msg_id:
                    current_text = item.details.text
                    if live:
                        live.update(render_assistant_text(current_text, is_first_message_chunk))

            elif isinstance(event, ItemCompletedEvent):
                item = event.item

                if isinstance(item.details, AgentMessageItem):
                    if live and item.id == current_msg_id:
                        live.update(render_assistant_text(item.details.text, is_first_message_chunk))
                        live.stop()
                        live = None
                        current_msg_id = None
                        is_first_message_chunk = False

                elif isinstance(item.details, CommandExecutionItem):
                    if live:
                        live.stop()
                        live = None
                    console.print(
                        render_command_complete(
                            item.details.command,
                            item.details.aggregated_output,
                            item.details.exit_code,
                        ),
                        end="",
                    )

                elif isinstance(item.details, McpToolCallItem):
                    if live:
                        live.stop()
                        live = None
                    console.print(render_mcp_complete(item.details), end="")

    except Exception as e:
        if live:
            live.stop()
        console.print(render_error(str(e)), end="")

    finally:
        if live:
            live.stop()


def print_welcome(config: Config) -> None:
    """Print welcome header box (matches codex-rs SessionHeaderHistoryCell)."""
    # Simplified version - codex-rs has full bordered box
    version = "0.1.0"
    cwd_display = str(config.cwd)

    # Try to relativize to home
    home = Path.home()
    try:
        rel = config.cwd.relative_to(home)
        cwd_display = f"~/{rel}" if str(rel) != "." else "~"
    except ValueError:
        pass

    # Header in bordered box style
    console.print()
    t = Text()
    t.append(">_ ", style="dim")
    t.append("OpenAI Codex", style="bold")
    t.append(f" (v{version})", style="dim")
    console.print(t)
    console.print()

    # Model info
    t = Text()
    t.append("model:     ", style="dim")
    t.append(config.model)
    console.print(t)

    # Directory info
    t = Text()
    t.append("directory: ", style="dim")
    t.append(cwd_display)
    console.print(t)

    console.print()


def print_help_commands() -> None:
    """Print available commands help."""
    console.print(Text("  To get started, describe a task or try one of these commands:", style="dim"))
    console.print()

    commands = [
        ("/help", "show available commands"),
        ("/status", "show current session configuration"),
        ("/clear", "clear the screen"),
        ("/quit", "exit the CLI"),
    ]

    for cmd, desc in commands:
        t = Text()
        t.append("  ")
        t.append(cmd)
        t.append(f" - {desc}", style="dim")
        console.print(t)


async def main_loop(config: Config, thread_id: str | None = None) -> None:
    """Main interactive loop."""
    # Use interactive approval handler for TUI
    approval_handler = RichApprovalHandler(console)
    codex = await Codex.create(config, thread_id, approval_handler=approval_handler)
    await codex.__aenter__()

    print_welcome(config)
    print_help_commands()
    console.print()

    try:
        while True:
            try:
                # Input prompt - simple › prefix matching codex-rs
                user_input = console.input("[bold dim]› [/bold dim]").strip()

                if not user_input:
                    continue

                # Handle slash commands
                if user_input.startswith("/"):
                    cmd = user_input[1:].split()[0].lower() if user_input[1:] else ""

                    if cmd in ("quit", "q", "exit"):
                        break
                    elif cmd == "clear":
                        console.clear()
                        print_welcome(config)
                        print_help_commands()
                        console.print()
                    elif cmd in ("help", "?"):
                        print_help_commands()
                        console.print()
                    elif cmd == "status":
                        t = Text()
                        t.append("• ", style="dim")
                        t.append("Model: ", style="bold")
                        t.append(config.model)
                        console.print(t)
                        console.print()
                    else:
                        t = Text()
                        t.append("• ", style="dim")
                        t.append(f"Unknown command: /{cmd}", style="yellow")
                        console.print(t)
                        console.print()
                    continue

                # Run conversation turn
                await run_turn(codex, user_input)
                console.print()

            except KeyboardInterrupt:
                console.print()
                break
            except EOFError:
                break

    finally:
        await codex.__aexit__(None, None, None)


async def run_single(config: Config, prompt: str) -> None:
    """Run a single prompt non-interactively."""
    codex = await Codex.create(config)
    await codex.__aenter__()

    try:
        await run_turn(codex, prompt)
    finally:
        await codex.__aexit__(None, None, None)


def main() -> None:
    """Entry point."""
    import argparse

    parser = argparse.ArgumentParser(description="Codex CLI")
    parser.add_argument("--model", help="Model to use")
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

    if args.prompt:
        prompt = " ".join(args.prompt)
        asyncio.run(run_single(config, prompt))
    else:
        asyncio.run(main_loop(config, args.resume))


if __name__ == "__main__":
    main()
