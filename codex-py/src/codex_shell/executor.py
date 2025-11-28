"""Shell command executor with PTY support.

Provides process execution with proper terminal handling.
"""

from __future__ import annotations

import asyncio
import os
import pty
import signal
import struct
import termios
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any


class ProcessStatus(str, Enum):
    """Status of a shell process."""

    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    KILLED = "killed"
    TIMEOUT = "timeout"


@dataclass(slots=True)
class ProcessResult:
    """Result of a process execution."""

    exit_code: int
    output: str
    status: ProcessStatus
    pid: int | None = None
    timed_out: bool = False


@dataclass(slots=True)
class OutputChunk:
    """A chunk of process output."""

    data: str
    stream: str = "stdout"  # stdout or stderr


@dataclass
class ShellProcess:
    """A running shell process."""

    pid: int
    command: str
    cwd: Path
    status: ProcessStatus = ProcessStatus.RUNNING
    exit_code: int | None = None
    output: str = ""
    _fd: int | None = None
    _child_pid: int | None = None

    async def wait(self, timeout: float | None = None) -> ProcessResult:
        """Wait for the process to complete."""
        if self._child_pid is None:
            return ProcessResult(
                exit_code=-1,
                output=self.output,
                status=ProcessStatus.FAILED,
                pid=self.pid,
            )

        try:
            if timeout:
                _, status = await asyncio.wait_for(
                    asyncio.to_thread(os.waitpid, self._child_pid, 0),
                    timeout=timeout,
                )
            else:
                _, status = await asyncio.to_thread(os.waitpid, self._child_pid, 0)

            if os.WIFEXITED(status):
                self.exit_code = os.WEXITSTATUS(status)
                self.status = (
                    ProcessStatus.COMPLETED if self.exit_code == 0 else ProcessStatus.FAILED
                )
            elif os.WIFSIGNALED(status):
                self.exit_code = -os.WTERMSIG(status)
                self.status = ProcessStatus.KILLED
            else:
                self.exit_code = -1
                self.status = ProcessStatus.FAILED

        except asyncio.TimeoutError:
            self.status = ProcessStatus.TIMEOUT
            self.exit_code = -1
            await self.kill()
            return ProcessResult(
                exit_code=-1,
                output=self.output + "\nCommand timed out",
                status=ProcessStatus.TIMEOUT,
                pid=self.pid,
                timed_out=True,
            )

        return ProcessResult(
            exit_code=self.exit_code or -1,
            output=self.output,
            status=self.status,
            pid=self.pid,
        )

    async def kill(self) -> None:
        """Kill the process."""
        if self._child_pid:
            try:
                os.kill(self._child_pid, signal.SIGKILL)
                self.status = ProcessStatus.KILLED
            except ProcessLookupError:
                pass

    def close(self) -> None:
        """Close file descriptors."""
        if self._fd is not None:
            try:
                os.close(self._fd)
            except OSError:
                pass


class ShellExecutor:
    """Executor for shell commands with PTY support."""

    def __init__(self, cwd: Path | None = None, env: dict[str, str] | None = None) -> None:
        self.cwd = cwd or Path.cwd()
        self.env = env or dict(os.environ)

    async def execute(
        self,
        command: str,
        timeout_ms: int | None = None,
        use_pty: bool = False,
    ) -> ProcessResult:
        """Execute a command and return the result.

        Args:
            command: Shell command to execute
            timeout_ms: Timeout in milliseconds
            use_pty: If True, use PTY (for interactive commands). Default False for tests.
        """
        if use_pty:
            return await self._execute_with_pty(command, timeout_ms)
        else:
            return await self._execute_simple(command, timeout_ms)

    async def _execute_simple(
        self,
        command: str,
        timeout_ms: int | None = None,
    ) -> ProcessResult:
        """Execute command using subprocess (simpler, works in tests)."""
        timeout = timeout_ms / 1000 if timeout_ms else 60.0

        try:
            proc = await asyncio.create_subprocess_shell(
                command,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.STDOUT,
                cwd=self.cwd,
                env=self.env,
            )

            try:
                stdout, _ = await asyncio.wait_for(
                    proc.communicate(),
                    timeout=timeout,
                )
                output = stdout.decode("utf-8", errors="replace") if stdout else ""
                exit_code = proc.returncode or 0
                status = ProcessStatus.COMPLETED if exit_code == 0 else ProcessStatus.FAILED

                return ProcessResult(
                    exit_code=exit_code,
                    output=output,
                    status=status,
                    pid=proc.pid,
                )

            except asyncio.TimeoutError:
                proc.kill()
                await proc.wait()
                return ProcessResult(
                    exit_code=-1,
                    output="Command timed out",
                    status=ProcessStatus.TIMEOUT,
                    pid=proc.pid,
                    timed_out=True,
                )

        except Exception as e:
            return ProcessResult(
                exit_code=-1,
                output=str(e),
                status=ProcessStatus.FAILED,
            )

    async def _execute_with_pty(
        self,
        command: str,
        timeout_ms: int | None = None,
    ) -> ProcessResult:
        """Execute command using PTY (for interactive commands)."""
        process = await self.spawn(command)

        # Collect output
        output_parts: list[str] = []
        async for chunk in self.read_output(process):
            output_parts.append(chunk.data)
            process.output += chunk.data

        timeout = timeout_ms / 1000 if timeout_ms else None
        result = await process.wait(timeout)
        process.close()

        return result

    async def spawn(self, command: str) -> ShellProcess:
        """Spawn a new shell process."""
        # Create pseudo-terminal
        master_fd, slave_fd = pty.openpty()

        # Fork process
        pid = os.fork()

        if pid == 0:
            # Child process
            os.close(master_fd)

            # Create new session
            os.setsid()

            # Set controlling terminal
            os.dup2(slave_fd, 0)
            os.dup2(slave_fd, 1)
            os.dup2(slave_fd, 2)

            if slave_fd > 2:
                os.close(slave_fd)

            # Change directory
            os.chdir(self.cwd)

            # Execute command
            os.execvpe("/bin/sh", ["/bin/sh", "-c", command], self.env)
        else:
            # Parent process
            os.close(slave_fd)

            process = ShellProcess(
                pid=pid,
                command=command,
                cwd=self.cwd,
                _fd=master_fd,
                _child_pid=pid,
            )

            return process

    async def read_output(
        self,
        process: ShellProcess,
        chunk_size: int = 4096,
    ) -> AsyncIterator[OutputChunk]:
        """Read output from a process."""
        if process._fd is None:
            return

        loop = asyncio.get_event_loop()

        while True:
            try:
                # Non-blocking read
                data = await loop.run_in_executor(
                    None,
                    lambda: self._read_nonblock(process._fd, chunk_size),
                )
                if data:
                    yield OutputChunk(data=data)
                else:
                    # Check if process is still running
                    try:
                        pid, _ = os.waitpid(process._child_pid or 0, os.WNOHANG)
                        if pid != 0:
                            break
                    except ChildProcessError:
                        break
                    await asyncio.sleep(0.01)
            except OSError:
                break

    def _read_nonblock(self, fd: int | None, size: int) -> str:
        """Read from file descriptor non-blocking."""
        if fd is None:
            return ""

        import select

        readable, _, _ = select.select([fd], [], [], 0.1)
        if readable:
            try:
                data = os.read(fd, size)
                return data.decode("utf-8", errors="replace")
            except OSError:
                return ""
        return ""


async def execute_command(
    command: str,
    cwd: Path | None = None,
    timeout_ms: int | None = None,
    env: dict[str, str] | None = None,
) -> ProcessResult:
    """Convenience function to execute a command."""
    executor = ShellExecutor(cwd=cwd, env=env)
    return await executor.execute(command, timeout_ms)
