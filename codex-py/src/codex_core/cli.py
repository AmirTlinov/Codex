"""CLI interface for Codex exec mode.

This provides the `codex exec --experimental-json` interface
that the SDK uses to communicate with Codex.
"""

from __future__ import annotations

import argparse
import asyncio
import sys
from pathlib import Path
from typing import Any

from codex_core.codex import Codex
from codex_core.config import Config
from codex_protocol.events import thread_event_to_json


def parse_args() -> argparse.Namespace:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(
        prog="codex",
        description="Codex AI coding assistant",
    )

    subparsers = parser.add_subparsers(dest="command", help="Commands")

    # exec subcommand
    exec_parser = subparsers.add_parser("exec", help="Execute a prompt (headless mode)")
    exec_parser.add_argument(
        "prompt",
        nargs="?",
        help="The prompt to execute (reads from stdin if not provided)",
    )
    exec_parser.add_argument(
        "--experimental-json",
        action="store_true",
        help="Output JSONL events for SDK integration",
    )
    exec_parser.add_argument(
        "--model",
        type=str,
        help="Model to use",
    )
    exec_parser.add_argument(
        "--sandbox",
        type=str,
        choices=["none", "read-only", "workspace-write", "workspace-full"],
        help="Sandbox policy",
    )
    exec_parser.add_argument(
        "--cd",
        type=Path,
        help="Working directory",
    )
    exec_parser.add_argument(
        "--skip-git-repo-check",
        action="store_true",
        help="Skip git repository check",
    )
    exec_parser.add_argument(
        "--output-schema",
        type=Path,
        help="JSON schema file for structured output",
    )
    exec_parser.add_argument(
        "--config",
        action="append",
        dest="config_overrides",
        help="Config override in KEY=VALUE format",
    )
    exec_parser.add_argument(
        "--image",
        action="append",
        dest="images",
        help="Image file to include",
    )
    exec_parser.add_argument(
        "resume",
        nargs="?",
        help="Thread ID to resume",
    )

    return parser.parse_args()


def parse_config_overrides(overrides: list[str] | None) -> dict[str, Any]:
    """Parse config overrides from KEY=VALUE format."""
    result: dict[str, Any] = {}
    if not overrides:
        return result

    for override in overrides:
        if "=" in override:
            key, value = override.split("=", 1)
            # Try to parse as JSON, otherwise keep as string
            try:
                import json

                result[key] = json.loads(value)
            except (json.JSONDecodeError, ValueError):
                result[key] = value

    return result


async def run_exec(args: argparse.Namespace) -> int:
    """Run exec mode."""
    # Build config
    overrides = parse_config_overrides(args.config_overrides)

    if args.model:
        overrides["model"] = args.model
    if args.sandbox:
        overrides["sandbox_policy"] = args.sandbox
    if args.cd:
        overrides["cwd"] = args.cd

    config = Config.load(overrides)

    # Get prompt
    prompt = args.prompt
    if not prompt:
        # Read from stdin
        prompt = sys.stdin.read().strip()

    if not prompt:
        if args.experimental_json:
            print('{"type":"error","message":"No prompt provided"}', flush=True)
        else:
            print("Error: No prompt provided", file=sys.stderr)
        return 1

    # Get thread ID for resume
    thread_id = getattr(args, "resume", None)

    # Run Codex
    try:
        async with await Codex.create(config, thread_id) as codex:
            async for event in codex.run_turn(prompt):
                if args.experimental_json:
                    # Output JSONL for SDK
                    print(thread_event_to_json(event), flush=True)
                else:
                    # Human-readable output
                    _print_event_human(event)

    except Exception as e:
        if args.experimental_json:
            import json

            print(json.dumps({"type": "error", "message": str(e)}), flush=True)
        else:
            print(f"Error: {e}", file=sys.stderr)
        return 1

    return 0


def _print_event_human(event: Any) -> None:
    """Print an event in human-readable format."""
    from codex_protocol.events import (
        ItemCompletedEvent,
        ItemUpdatedEvent,
        ThreadErrorEvent,
        ThreadStartedEvent,
        TurnCompletedEvent,
        TurnFailedEvent,
        TurnStartedEvent,
    )
    from codex_protocol.items import AgentMessageItem, CommandExecutionItem

    if isinstance(event, ThreadStartedEvent):
        print(f"Thread started: {event.thread_id}")
    elif isinstance(event, TurnStartedEvent):
        print("Processing...")
    elif isinstance(event, TurnCompletedEvent):
        print(f"\nCompleted (tokens: {event.usage.input_tokens} in, {event.usage.output_tokens} out)")
    elif isinstance(event, TurnFailedEvent):
        print(f"\nFailed: {event.error.message}")
    elif isinstance(event, ThreadErrorEvent):
        print(f"Error: {event.message}")
    elif isinstance(event, ItemUpdatedEvent):
        if isinstance(event.item.details, AgentMessageItem):
            # Print streaming text
            print(event.item.details.text, end="", flush=True)
    elif isinstance(event, ItemCompletedEvent):
        if isinstance(event.item.details, CommandExecutionItem):
            details = event.item.details
            print(f"\n$ {details.command}")
            print(details.aggregated_output)
            if details.exit_code != 0:
                print(f"Exit code: {details.exit_code}")


def main() -> None:
    """Main entry point."""
    args = parse_args()

    if args.command == "exec":
        exit_code = asyncio.run(run_exec(args))
        sys.exit(exit_code)
    else:
        # Default: show help
        print("Usage: codex exec <prompt>")
        print("       codex exec --experimental-json < input.txt")
        sys.exit(0)


if __name__ == "__main__":
    main()
