"""Approval system for commands and patches.

Implements configurable approval policies:
- never: Auto-approve everything
- always: Always require approval
- unless-allow-listed: Require unless command matches allow list
"""

from __future__ import annotations

import asyncio
import fnmatch
import re
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from enum import Enum, auto
from pathlib import Path
from typing import Any, Protocol


class ApprovalPolicy(str, Enum):
    """Approval policy modes."""

    NEVER = "never"
    ALWAYS = "always"
    UNLESS_ALLOW_LISTED = "unless-allow-listed"


class ApprovalType(Enum):
    """Type of approval requested."""

    COMMAND = auto()
    PATCH = auto()
    MCP_TOOL = auto()


class ApprovalDecision(Enum):
    """User's decision on an approval request."""

    APPROVED = auto()
    REJECTED = auto()
    ALWAYS_APPROVE = auto()  # Auto-approve future similar requests


@dataclass(slots=True, frozen=True)
class ApprovalRequest:
    """Immutable approval request."""

    request_id: str
    approval_type: ApprovalType
    description: str

    # For commands
    command: str | None = None

    # For patches
    patch_path: str | None = None
    patch_diff: str | None = None

    # For MCP tools
    mcp_server: str | None = None
    mcp_tool: str | None = None
    mcp_arguments: dict[str, Any] | None = None


class ApprovalHandler(Protocol):
    """Protocol for handling approval requests."""

    async def request_approval(self, request: ApprovalRequest) -> ApprovalDecision:
        """Request approval from user. Returns decision."""
        ...


class AutoApproveHandler:
    """Handler that auto-approves everything."""

    async def request_approval(self, request: ApprovalRequest) -> ApprovalDecision:
        return ApprovalDecision.APPROVED


class AutoRejectHandler:
    """Handler that auto-rejects everything."""

    async def request_approval(self, request: ApprovalRequest) -> ApprovalDecision:
        return ApprovalDecision.REJECTED


@dataclass
class CommandAllowList:
    """Allow list for commands that don't need approval."""

    patterns: list[str] = field(default_factory=list)
    regex_patterns: list[re.Pattern[str]] = field(default_factory=list)

    # Safe read-only commands
    DEFAULT_PATTERNS: tuple[str, ...] = (
        "ls *",
        "cat *",
        "head *",
        "tail *",
        "wc *",
        "pwd",
        "echo *",
        "date",
        "whoami",
        "which *",
        "type *",
        "file *",
        "stat *",
        "du *",
        "df *",
        "tree *",
        "find * -type *",  # find with -type is usually safe
        "grep *",
        "git status*",
        "git log*",
        "git diff*",
        "git show*",
        "git branch*",
        "git remote*",
        "python --version",
        "python3 --version",
        "node --version",
        "npm --version",
        "cargo --version",
        "rustc --version",
        "go version",
    )

    @classmethod
    def default(cls) -> CommandAllowList:
        """Create default allow list with safe commands."""
        return cls(patterns=list(cls.DEFAULT_PATTERNS))

    def add_pattern(self, pattern: str) -> None:
        """Add glob pattern."""
        self.patterns.append(pattern)

    def add_regex(self, pattern: str) -> None:
        """Add regex pattern."""
        self.regex_patterns.append(re.compile(pattern))

    def is_allowed(self, command: str) -> bool:
        """Check if command matches allow list."""
        # Strip and normalize
        cmd = command.strip()

        # Check glob patterns
        for pattern in self.patterns:
            if fnmatch.fnmatch(cmd, pattern):
                return True

        # Check regex patterns
        for regex in self.regex_patterns:
            if regex.match(cmd):
                return True

        return False


@dataclass
class PathAllowList:
    """Allow list for paths that can be modified without approval."""

    patterns: list[str] = field(default_factory=list)

    def add_pattern(self, pattern: str) -> None:
        """Add glob pattern for paths."""
        self.patterns.append(pattern)

    def is_allowed(self, path: str | Path) -> bool:
        """Check if path is in allow list."""
        path_str = str(path)
        for pattern in self.patterns:
            if fnmatch.fnmatch(path_str, pattern):
                return True
        return False


@dataclass
class ApprovalManager:
    """Manages approval flow with configurable policies."""

    policy: ApprovalPolicy = ApprovalPolicy.UNLESS_ALLOW_LISTED
    handler: ApprovalHandler = field(default_factory=AutoApproveHandler)
    command_allow_list: CommandAllowList = field(default_factory=CommandAllowList.default)
    path_allow_list: PathAllowList = field(default_factory=PathAllowList)

    # Runtime state
    _always_approve_commands: bool = False
    _always_approve_patches: bool = False
    _always_approve_mcp: set[str] = field(default_factory=set)  # server__tool
    _request_counter: int = 0

    def _next_request_id(self) -> str:
        """Generate unique request ID."""
        self._request_counter += 1
        return f"approval-{self._request_counter}"

    async def approve_command(
        self,
        command: str,
        description: str | None = None,
    ) -> bool:
        """Check if command execution is approved."""
        # Never policy - auto-approve
        if self.policy == ApprovalPolicy.NEVER:
            return True

        # Runtime always-approve flag
        if self._always_approve_commands:
            return True

        # Check allow list (unless policy is ALWAYS)
        if self.policy == ApprovalPolicy.UNLESS_ALLOW_LISTED:
            if self.command_allow_list.is_allowed(command):
                return True

        # Request approval
        request = ApprovalRequest(
            request_id=self._next_request_id(),
            approval_type=ApprovalType.COMMAND,
            description=description or f"Execute: {command[:50]}...",
            command=command,
        )

        decision = await self.handler.request_approval(request)

        if decision == ApprovalDecision.ALWAYS_APPROVE:
            self._always_approve_commands = True
            return True

        return decision == ApprovalDecision.APPROVED

    async def approve_patch(
        self,
        path: str | Path,
        diff: str,
        description: str | None = None,
    ) -> bool:
        """Check if patch application is approved."""
        # Never policy - auto-approve
        if self.policy == ApprovalPolicy.NEVER:
            return True

        # Runtime always-approve flag
        if self._always_approve_patches:
            return True

        # Check path allow list
        if self.policy == ApprovalPolicy.UNLESS_ALLOW_LISTED:
            if self.path_allow_list.is_allowed(path):
                return True

        # Request approval
        request = ApprovalRequest(
            request_id=self._next_request_id(),
            approval_type=ApprovalType.PATCH,
            description=description or f"Modify: {path}",
            patch_path=str(path),
            patch_diff=diff,
        )

        decision = await self.handler.request_approval(request)

        if decision == ApprovalDecision.ALWAYS_APPROVE:
            self._always_approve_patches = True
            return True

        return decision == ApprovalDecision.APPROVED

    async def approve_mcp_tool(
        self,
        server: str,
        tool: str,
        arguments: dict[str, Any],
        description: str | None = None,
    ) -> bool:
        """Check if MCP tool call is approved."""
        # Never policy - auto-approve
        if self.policy == ApprovalPolicy.NEVER:
            return True

        tool_key = f"{server}__{tool}"

        # Runtime always-approve for this tool
        if tool_key in self._always_approve_mcp:
            return True

        # Request approval
        request = ApprovalRequest(
            request_id=self._next_request_id(),
            approval_type=ApprovalType.MCP_TOOL,
            description=description or f"MCP: {server}/{tool}",
            mcp_server=server,
            mcp_tool=tool,
            mcp_arguments=arguments,
        )

        decision = await self.handler.request_approval(request)

        if decision == ApprovalDecision.ALWAYS_APPROVE:
            self._always_approve_mcp.add(tool_key)
            return True

        return decision == ApprovalDecision.APPROVED

    def reset_runtime_approvals(self) -> None:
        """Reset runtime always-approve flags."""
        self._always_approve_commands = False
        self._always_approve_patches = False
        self._always_approve_mcp.clear()

    @classmethod
    def from_policy_string(cls, policy: str, handler: ApprovalHandler | None = None) -> ApprovalManager:
        """Create manager from policy string."""
        try:
            policy_enum = ApprovalPolicy(policy)
        except ValueError:
            policy_enum = ApprovalPolicy.UNLESS_ALLOW_LISTED

        if handler is None:
            if policy_enum == ApprovalPolicy.NEVER:
                handler = AutoApproveHandler()
            else:
                # Default to auto-approve in non-interactive mode
                handler = AutoApproveHandler()

        return cls(policy=policy_enum, handler=handler)
