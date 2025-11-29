"""Sandbox enforcement for shell commands.

Provides command validation based on sandbox policy:
- read-only: Only allow read operations
- workspace-write: Allow writes only in workspace
- danger-full-access: No restrictions
"""

from __future__ import annotations

import re
import shutil
from dataclasses import dataclass
from enum import Enum
from pathlib import Path


class SandboxPolicy(str, Enum):
    """Sandbox enforcement policy."""

    READ_ONLY = "read-only"
    WORKSPACE_WRITE = "workspace-write"
    DANGER_FULL_ACCESS = "danger-full-access"


@dataclass(slots=True)
class SandboxConfig:
    """Sandbox configuration."""

    policy: SandboxPolicy
    workspace: Path
    writable_roots: list[Path]

    @classmethod
    def default(cls, cwd: Path | None = None) -> SandboxConfig:
        """Create default sandbox config."""
        workspace = cwd or Path.cwd()
        return cls(
            policy=SandboxPolicy.WORKSPACE_WRITE,
            workspace=workspace,
            writable_roots=[workspace],
        )


@dataclass(slots=True)
class SandboxResult:
    """Result of sandbox validation."""

    allowed: bool
    reason: str | None = None
    requires_approval: bool = False


# Commands that are always safe (read-only)
SAFE_COMMANDS = frozenset({
    "ls", "cat", "head", "tail", "less", "more", "wc",
    "pwd", "echo", "date", "whoami", "which", "type",
    "file", "stat", "du", "df", "tree", "find", "grep",
    "awk", "sed", "sort", "uniq", "cut", "tr", "diff",
    "git status", "git log", "git diff", "git show",
    "git branch", "git remote", "git tag",
    "python --version", "python3 --version",
    "node --version", "npm --version",
    "cargo --version", "rustc --version",
    "go version", "java --version",
})

# Patterns for write/modify commands
WRITE_PATTERNS = [
    r"^rm\s",
    r"^mv\s",
    r"^cp\s",
    r"^mkdir\s",
    r"^rmdir\s",
    r"^touch\s",
    r"^chmod\s",
    r"^chown\s",
    r"^ln\s",
    r"^install\s",
    r">\s*[^\s]",  # redirect output
    r">>\s*[^\s]",  # append output
    r"\|\s*tee\s",  # tee to file
    r"^git\s+(add|commit|push|pull|merge|rebase|reset|checkout|switch)",
    r"^npm\s+(install|uninstall|update|publish)",
    r"^pip\s+(install|uninstall)",
    r"^cargo\s+(build|install|publish)",
    r"^make\s",
    r"^cmake\s",
]

# Patterns for network access
NETWORK_PATTERNS = [
    r"^curl\s",
    r"^wget\s",
    r"^ssh\s",
    r"^scp\s",
    r"^rsync\s",
    r"^git\s+(clone|fetch|pull|push)",
    r"^npm\s+(install|publish)",
    r"^pip\s+install",
    r"^cargo\s+(install|publish)",
]


class SandboxValidator:
    """Validates commands against sandbox policy."""

    def __init__(self, config: SandboxConfig) -> None:
        self.config = config
        self._write_patterns = [re.compile(p) for p in WRITE_PATTERNS]
        self._network_patterns = [re.compile(p) for p in NETWORK_PATTERNS]

    def validate(self, command: str) -> SandboxResult:
        """Validate command against sandbox policy.

        Args:
            command: Shell command to validate

        Returns:
            SandboxResult with validation outcome
        """
        cmd = command.strip()

        # Full access - allow everything
        if self.config.policy == SandboxPolicy.DANGER_FULL_ACCESS:
            return SandboxResult(allowed=True)

        # Check if safe command
        if self._is_safe_command(cmd):
            return SandboxResult(allowed=True)

        # Check write operations
        is_write = self._is_write_command(cmd)

        if self.config.policy == SandboxPolicy.READ_ONLY:
            if is_write:
                return SandboxResult(
                    allowed=False,
                    reason="Write operations not allowed in read-only mode",
                    requires_approval=True,
                )
            return SandboxResult(allowed=True)

        # Workspace-write: check if writes target workspace
        if is_write:
            target_paths = self._extract_paths(cmd)
            for path in target_paths:
                if not self._is_in_workspace(path):
                    return SandboxResult(
                        allowed=False,
                        reason=f"Write outside workspace: {path}",
                        requires_approval=True,
                    )

        return SandboxResult(allowed=True)

    def _is_safe_command(self, cmd: str) -> bool:
        """Check if command is known safe."""
        # Get base command
        parts = cmd.split()
        if not parts:
            return True

        base_cmd = parts[0]

        # Check exact matches
        if base_cmd in SAFE_COMMANDS:
            return True

        # Check compound commands (e.g., "git status")
        if len(parts) >= 2:
            compound = f"{parts[0]} {parts[1]}"
            if compound in SAFE_COMMANDS:
                return True

        return False

    def _is_write_command(self, cmd: str) -> bool:
        """Check if command performs write operations."""
        return any(pattern.search(cmd) for pattern in self._write_patterns)

    def _is_network_command(self, cmd: str) -> bool:
        """Check if command requires network access."""
        return any(pattern.search(cmd) for pattern in self._network_patterns)

    def _extract_paths(self, cmd: str) -> list[Path]:
        """Extract file paths from command (basic heuristic)."""
        # This is a simplified extraction
        # In production, should use proper shell parsing
        paths: list[Path] = []

        parts = cmd.split()
        for part in parts[1:]:  # Skip command name
            # Skip flags
            if part.startswith("-"):
                continue
            # Skip common non-paths
            if part in {"&&", "||", "|", ";", ">", ">>", "<"}:
                continue

            # Try to resolve as path
            try:
                if part.startswith("/"):
                    paths.append(Path(part))
                elif part.startswith("~"):
                    paths.append(Path(part).expanduser())
                elif not part.startswith("$"):
                    # Relative path
                    paths.append(self.config.workspace / part)
            except Exception:
                pass

        return paths

    def _is_in_workspace(self, path: Path) -> bool:
        """Check if path is within allowed workspace."""
        try:
            resolved = path.resolve()
        except Exception:
            return False

        # Check workspace
        try:
            resolved.relative_to(self.config.workspace.resolve())
            return True
        except ValueError:
            pass

        # Check writable roots
        for root in self.config.writable_roots:
            try:
                resolved.relative_to(root.resolve())
                return True
            except ValueError:
                pass

        return False


def check_firejail_available() -> bool:
    """Check if firejail is available for real sandboxing."""
    return shutil.which("firejail") is not None


def wrap_with_firejail(
    command: str,
    policy: SandboxPolicy,
    workspace: Path,
) -> str:
    """Wrap command with firejail for actual sandboxing.

    Only works on Linux with firejail installed.
    """
    if not check_firejail_available():
        return command

    if policy == SandboxPolicy.DANGER_FULL_ACCESS:
        return command

    # Build firejail options
    opts = ["firejail", "--quiet"]

    if policy == SandboxPolicy.READ_ONLY:
        opts.append("--read-only=/")
        opts.append(f"--read-only={workspace}")
    elif policy == SandboxPolicy.WORKSPACE_WRITE:
        opts.append("--read-only=/")
        opts.append(f"--read-write={workspace}")

    # Network restriction (could be configurable)
    # opts.append("--net=none")

    opts.append("--")
    opts.append(command)

    return " ".join(opts)
