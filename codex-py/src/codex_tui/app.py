"""Codex CLI - simple terminal interface using rich.

No heavy TUI framework - just clean terminal I/O like Claude Code.
"""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path
from typing import Any

from rich.console import Console
from rich.live import Live
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

console = Console()


def print_user(text: str) -> None:
    """Print user message."""
    t = Text()
    t.append("› ", style="bold")
    t.append(text)
    console.print(t)


def print_assistant(text: str) -> None:
    """Print assistant message."""
    console.print(text)


def print_system(text: str) -> None:
    """Print system message."""
    console.print(Text(text, style="dim"))


def print_error(text: str) -> None:
    """Print error message."""
    t = Text()
    t.append("! ", style="bold red")
    t.append(text, style="red")
    console.print(t)


def print_command(cmd: str, output: str | None = None, exit_code: int | None = None) -> None:
    """Print command execution."""
    t = Text()

    if exit_code is None:
        t.append("● ", style="bold yellow")
        t.append("Running ", style="yellow")
    elif exit_code == 0:
        t.append("● ", style="bold green")
        t.append("Ran ", style="green")
    else:
        t.append("● ", style="bold red")
        t.append("Ran ", style="red")

    t.append(cmd, style="bold")

    if exit_code is not None and exit_code != 0:
        t.append(f" ({exit_code})", style="red")

    console.print(t)

    if output:
        lines = output.strip().split("\n")[:8]
        for i, line in enumerate(lines):
            prefix = "└ " if i == 0 else "  "
            console.print(Text(f"  {prefix}{line}", style="dim"))
        if len(output.strip().split("\n")) > 8:
            console.print(Text("    (...)", style="dim italic"))


def print_thinking() -> None:
    """Print thinking indicator."""
    console.print(Text("● ", style="bold cyan") + Text("Thinking...", style="italic dim"))


async def run_turn(codex: Codex, user_input: str) -> None:
    """Run a conversation turn."""
    current_msg_id: str | None = None
    current_text = ""
    live: Live | None = None

    async for event in codex.run_turn(user_input):
        if isinstance(event, TurnStartedEvent):
            print_thinking()

        elif isinstance(event, TurnCompletedEvent):
            if live:
                live.stop()
                live = None
            # Clear thinking line
            console.print()

        elif isinstance(event, TurnFailedEvent):
            if live:
                live.stop()
            print_error(event.error.message)

        elif isinstance(event, ThreadErrorEvent):
            print_error(event.message)

        elif isinstance(event, ItemStartedEvent):
            item = event.item
            if isinstance(item.details, AgentMessageItem):
                current_msg_id = item.id
                current_text = item.details.text
                live = Live(Text(current_text), console=console, refresh_per_second=10)
                live.start()
            elif isinstance(item.details, CommandExecutionItem):
                if live:
                    live.stop()
                    live = None
                print_command(item.details.command)

        elif isinstance(event, ItemUpdatedEvent):
            item = event.item
            if isinstance(item.details, AgentMessageItem) and item.id == current_msg_id:
                current_text = item.details.text
                if live:
                    live.update(Text(current_text))

        elif isinstance(event, ItemCompletedEvent):
            item = event.item
            if isinstance(item.details, AgentMessageItem) and item.id == current_msg_id:
                if live:
                    live.stop()
                    live = None
                print_assistant(item.details.text)
                current_msg_id = None
            elif isinstance(item.details, CommandExecutionItem):
                print_command(
                    item.details.command,
                    item.details.aggregated_output,
                    item.details.exit_code,
                )


async def main_loop(config: Config, thread_id: str | None = None) -> None:
    """Main interactive loop."""
    codex = await Codex.create(config, thread_id)
    await codex.__aenter__()

    # Welcome
    print_system(f"{config.model} @ {config.cwd}")
    console.print()

    try:
        while True:
            try:
                # Simple input prompt
                user_input = console.input("[bold]› [/bold]").strip()

                if not user_input:
                    continue

                # Handle commands
                if user_input.startswith("/"):
                    cmd = user_input[1:].split()[0].lower()
                    if cmd in ("quit", "q", "exit"):
                        break
                    elif cmd == "clear":
                        console.clear()
                    elif cmd == "help":
                        print_system("/quit /clear /help")
                    else:
                        print_error(f"Unknown: {cmd}")
                    continue

                # Run turn
                await run_turn(codex, user_input)
                console.print()

            except KeyboardInterrupt:
                console.print()
                break
            except EOFError:
                break

    finally:
        await codex.__aexit__(None, None, None)


def main() -> None:
    """Entry point."""
    import argparse

    parser = argparse.ArgumentParser(description="Codex")
    parser.add_argument("--model", help="Model")
    parser.add_argument("--cd", type=Path, help="Working directory")
    parser.add_argument("--resume", help="Thread ID")
    parser.add_argument("prompt", nargs="*", help="Initial prompt")
    args = parser.parse_args()

    overrides: dict[str, Any] = {}
    if args.model:
        overrides["model"] = args.model
    if args.cd:
        overrides["cwd"] = args.cd

    config = Config.load(overrides)

    # If prompt provided, run non-interactively
    if args.prompt:
        prompt = " ".join(args.prompt)
        asyncio.run(run_single(config, prompt))
    else:
        asyncio.run(main_loop(config, args.resume))


async def run_single(config: Config, prompt: str) -> None:
    """Run a single prompt non-interactively."""
    codex = await Codex.create(config)
    await codex.__aenter__()

    try:
        print_user(prompt)
        await run_turn(codex, prompt)
    finally:
        await codex.__aexit__(None, None, None)


if __name__ == "__main__":
    main()
