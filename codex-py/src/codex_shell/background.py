"""Background shell manager.

Manages long-running background processes with logging and control.
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from codex_shell.executor import ProcessStatus, ShellExecutor, ShellProcess


@dataclass(slots=True)
class BackgroundShell:
    """A background shell process with metadata."""

    shell_id: int
    command: str
    process: ShellProcess
    started_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    ended_at: datetime | None = None
    log_path: Path | None = None
    _output_buffer: str = ""

    @property
    def status(self) -> ProcessStatus:
        return self.process.status

    @property
    def exit_code(self) -> int | None:
        return self.process.exit_code

    @property
    def output(self) -> str:
        return self._output_buffer

    def append_output(self, data: str) -> None:
        self._output_buffer += data

    def to_dict(self) -> dict[str, Any]:
        return {
            "shell_id": self.shell_id,
            "command": self.command,
            "status": self.status.value,
            "exit_code": self.exit_code,
            "started_at": self.started_at.isoformat(),
            "ended_at": self.ended_at.isoformat() if self.ended_at else None,
            "pid": self.process.pid,
        }


class BackgroundShellManager:
    """Manager for background shell processes."""

    def __init__(self, cwd: Path | None = None, log_dir: Path | None = None) -> None:
        self.cwd = cwd or Path.cwd()
        self.log_dir = log_dir
        self._shells: dict[int, BackgroundShell] = {}
        self._next_id = 1
        self._executor = ShellExecutor(cwd=self.cwd)
        self._tasks: dict[int, asyncio.Task[None]] = {}

    async def spawn(self, command: str) -> BackgroundShell:
        """Spawn a new background shell."""
        shell_id = self._next_id
        self._next_id += 1

        # Spawn process
        process = await self._executor.spawn(command)

        # Create log file if log_dir is set
        log_path = None
        if self.log_dir:
            self.log_dir.mkdir(parents=True, exist_ok=True)
            log_path = self.log_dir / f"shell_{shell_id}.log"

        shell = BackgroundShell(
            shell_id=shell_id,
            command=command,
            process=process,
            log_path=log_path,
        )

        self._shells[shell_id] = shell

        # Start background task to collect output
        task = asyncio.create_task(self._collect_output(shell))
        self._tasks[shell_id] = task

        return shell

    async def _collect_output(self, shell: BackgroundShell) -> None:
        """Collect output from a background shell."""
        log_file = None
        try:
            if shell.log_path:
                log_file = open(shell.log_path, "w")

            async for chunk in self._executor.read_output(shell.process):
                shell.append_output(chunk.data)
                if log_file:
                    log_file.write(chunk.data)
                    log_file.flush()

            # Wait for process to complete
            await shell.process.wait()
            shell.ended_at = datetime.now(timezone.utc)

        finally:
            if log_file:
                log_file.close()

    def get(self, shell_id: int) -> BackgroundShell | None:
        """Get a shell by ID."""
        return self._shells.get(shell_id)

    def list_running(self) -> list[BackgroundShell]:
        """List all running shells."""
        return [s for s in self._shells.values() if s.status == ProcessStatus.RUNNING]

    def list_completed(self) -> list[BackgroundShell]:
        """List all completed shells."""
        return [
            s
            for s in self._shells.values()
            if s.status in (ProcessStatus.COMPLETED, ProcessStatus.FAILED, ProcessStatus.KILLED)
        ]

    def list_all(self) -> list[BackgroundShell]:
        """List all shells."""
        return list(self._shells.values())

    async def kill(self, shell_id: int) -> bool:
        """Kill a background shell."""
        shell = self._shells.get(shell_id)
        if not shell:
            return False

        await shell.process.kill()

        # Cancel the output collection task
        task = self._tasks.get(shell_id)
        if task and not task.done():
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass

        shell.ended_at = datetime.now(timezone.utc)
        return True

    async def cleanup(self) -> None:
        """Clean up all shells and cancel tasks."""
        for shell_id in list(self._shells.keys()):
            await self.kill(shell_id)

        # Wait for all tasks to complete
        if self._tasks:
            await asyncio.gather(*self._tasks.values(), return_exceptions=True)

    def get_summary(self) -> dict[str, Any]:
        """Get a summary of all shells."""
        running = self.list_running()
        completed = self.list_completed()

        return {
            "running": [s.to_dict() for s in running],
            "completed": [s.to_dict() for s in completed],
            "total": len(self._shells),
        }

    async def read_log(
        self,
        shell_id: int,
        mode: str = "tail",
        lines: int = 100,
    ) -> str:
        """Read log from a shell.

        Args:
            shell_id: The shell ID
            mode: "tail" for last N lines, "body" for full log, "head" for first N lines
            lines: Number of lines for tail/head mode
        """
        shell = self._shells.get(shell_id)
        if not shell:
            return ""

        output = shell.output

        if mode == "tail":
            output_lines = output.splitlines()
            return "\n".join(output_lines[-lines:])
        elif mode == "head":
            output_lines = output.splitlines()
            return "\n".join(output_lines[:lines])
        else:  # body
            return output
