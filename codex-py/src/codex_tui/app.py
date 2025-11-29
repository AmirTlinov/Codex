"""Codex CLI - inline terminal interface like codex-rs.

Uses rich for formatting but renders inline (no alternate screen).
Matches codex-rs TUI visual style.
"""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path
from typing import Any

from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown
from rich.panel import Panel
from rich.text import Text

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

# Console for inline output
console = Console(highlight=False)


def render_user_message(text: str) -> Text:
    """Render user message with › prefix."""
    result = Text()
    result.append("\n")
    for i, line in enumerate(text.split("\n")):
        if i == 0:
            result.append("› ", style="bold dim")
        else:
            result.append("  ", style="dim")
        result.append(line)
        result.append("\n")
    return result


def render_thinking() -> Text:
    """Render thinking indicator."""
    result = Text()
    result.append("  ")
    result.append("Thinking...", style="dim italic")
    return result


def render_command_start(cmd: str) -> Text:
    """Render command starting."""
    result = Text()
    result.append("  ")
    result.append("$ ", style="dim")
    result.append(cmd, style="bold")
    result.append(" ", style="dim")
    result.append("...", style="dim italic")
    return result


def render_command_complete(cmd: str, output: str | None, exit_code: int | None) -> Text:
    """Render completed command with output."""
    result = Text()
    result.append("  ")

    if exit_code == 0 or exit_code is None:
        result.append("$ ", style="dim")
    else:
        result.append("$ ", style="red dim")

    result.append(cmd, style="bold")

    if exit_code is not None and exit_code != 0:
        result.append(f" (exit {exit_code})", style="red")

    result.append("\n")

    # Output lines with tree prefix
    if output:
        lines = output.strip().split("\n")
        max_lines = 10
        for i, line in enumerate(lines[:max_lines]):
            result.append("  ")
            if i == 0:
                result.append("└ ", style="dim")
            else:
                result.append("  ", style="dim")
            result.append(line, style="dim")
            result.append("\n")

        if len(lines) > max_lines:
            result.append(f"    ... ({len(lines) - max_lines} more lines)\n", style="dim italic")

    return result


def render_assistant_text(text: str) -> Text:
    """Render assistant message text."""
    result = Text()
    for line in text.split("\n"):
        result.append("  ")
        result.append(line)
        result.append("\n")
    return result


def render_error(message: str) -> Text:
    """Render error message."""
    result = Text()
    result.append("  ")
    result.append("Error: ", style="bold red")
    result.append(message, style="red")
    result.append("\n")
    return result


async def run_turn(codex: Codex, user_input: str) -> None:
    """Run a single conversation turn."""
    current_msg_id: str | None = None
    current_text = ""
    live: Live | None = None

    # User message is already shown by console.input() prompt
    # Just add a blank line before assistant response
    console.print()

    try:
        async for event in codex.run_turn(user_input):
            if isinstance(event, TurnStartedEvent):
                # Show thinking
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
                    live = Live(
                        render_assistant_text(current_text),
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

            elif isinstance(event, ItemUpdatedEvent):
                item = event.item
                if isinstance(item.details, AgentMessageItem) and item.id == current_msg_id:
                    current_text = item.details.text
                    if live:
                        live.update(render_assistant_text(current_text))

            elif isinstance(event, ItemCompletedEvent):
                item = event.item

                if isinstance(item.details, AgentMessageItem):
                    # Update live with final text, then stop
                    # Don't print separately - Live already shows the text
                    if live and item.id == current_msg_id:
                        live.update(render_assistant_text(item.details.text))
                        live.stop()
                        live = None
                        current_msg_id = None
                        # Print newline after Live output
                        console.print()

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

    except Exception as e:
        if live:
            live.stop()
        console.print(render_error(str(e)), end="")

    finally:
        if live:
            live.stop()


def print_welcome(config: Config) -> None:
    """Print welcome/status line."""
    t = Text()
    t.append(config.model, style="bold")
    t.append(" @ ", style="dim")
    t.append(str(config.cwd), style="dim")
    console.print(t)
    console.print()


def print_shortcuts() -> None:
    """Print available shortcuts."""
    t = Text()
    t.append("  ", style="dim")
    t.append("? ", style="bold dim")
    t.append("shortcuts  ", style="dim")
    t.append("Ctrl+C ", style="bold dim")
    t.append("quit", style="dim")
    console.print(t)


async def main_loop(config: Config, thread_id: str | None = None) -> None:
    """Main interactive loop."""
    codex = await Codex.create(config, thread_id)
    await codex.__aenter__()

    print_welcome(config)
    print_shortcuts()
    console.print()

    try:
        while True:
            try:
                # Input prompt - simple › prefix
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
                        print_shortcuts()
                        console.print()
                    elif cmd == "help" or cmd == "?":
                        console.print(Text("  /quit  /clear  /help", style="dim"))
                    else:
                        console.print(Text(f"  Unknown command: /{cmd}", style="dim red"))
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
