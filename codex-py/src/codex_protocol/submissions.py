"""Submission types for Codex protocol.

These types represent operations sent from the client to the Codex engine.
They are used for the internal TUI/CLI protocol, not the SDK exec JSONL.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any


class AskForApproval(str, Enum):
    """Policy for command approval."""

    NEVER = "never"
    AUTO_EDIT = "auto-edit"
    UNLESS_ALLOW_LISTED = "unless-allow-listed"
    ALWAYS = "always"


class SandboxPolicy(str, Enum):
    """Policy for sandbox enforcement."""

    NONE = "none"
    READ_ONLY = "read-only"
    IGNORE_FILE_PERMISSIONS = "ignore-file-permissions"
    WORKSPACE_WRITE = "workspace-write"
    WORKSPACE_FULL = "workspace-full"


@dataclass(slots=True)
class TextInput:
    """Text input from the user."""

    text: str

    def to_dict(self) -> dict[str, Any]:
        return {"type": "text", "text": self.text}


@dataclass(slots=True)
class ImageInput:
    """Pre-encoded data URI image."""

    image_url: str

    def to_dict(self) -> dict[str, Any]:
        return {"type": "image", "image_url": self.image_url}


@dataclass(slots=True)
class LocalImageInput:
    """Local image path provided by the user."""

    path: Path

    def to_dict(self) -> dict[str, Any]:
        return {"type": "local_image", "path": str(self.path)}


# Union type for user inputs
UserInput = TextInput | ImageInput | LocalImageInput


def user_input_from_dict(data: dict[str, Any]) -> UserInput:
    """Parse a UserInput from a dictionary."""
    input_type = data.get("type")
    match input_type:
        case "text":
            return TextInput(text=data["text"])
        case "image":
            return ImageInput(image_url=data["image_url"])
        case "local_image":
            return LocalImageInput(path=Path(data["path"]))
        case _:
            raise ValueError(f"Unknown input type: {input_type}")


class BackgroundShellControlAction(str, Enum):
    """Actions for background shell control."""

    KILL = "kill"
    RESUME = "resume"


@dataclass(slots=True)
class InterruptOp:
    """Abort current task."""

    def to_dict(self) -> dict[str, Any]:
        return {"type": "interrupt"}


@dataclass(slots=True)
class UserInputOp:
    """Input from the user."""

    items: list[UserInput]

    def to_dict(self) -> dict[str, Any]:
        return {"type": "user_input", "items": [i.to_dict() for i in self.items]}


@dataclass(slots=True)
class UserTurnOp:
    """User input with additional context for a turn."""

    items: list[UserInput]
    cwd: Path
    approval_policy: AskForApproval
    sandbox_policy: SandboxPolicy
    model: str
    reasoning_effort: str | None = None
    reasoning_summary: str | None = None

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "type": "user_turn",
            "items": [i.to_dict() for i in self.items],
            "cwd": str(self.cwd),
            "approval_policy": self.approval_policy.value,
            "sandbox_policy": self.sandbox_policy.value,
            "model": self.model,
        }
        if self.reasoning_effort:
            d["reasoning_effort"] = self.reasoning_effort
        if self.reasoning_summary:
            d["reasoning_summary"] = self.reasoning_summary
        return d


@dataclass(slots=True)
class ExecApprovalOp:
    """Approval for a command execution."""

    shell_id: int
    approved: bool

    def to_dict(self) -> dict[str, Any]:
        return {"type": "exec_approval", "shell_id": self.shell_id, "approved": self.approved}


@dataclass(slots=True)
class PatchApprovalOp:
    """Approval for a file patch."""

    patch_id: str
    approved: bool

    def to_dict(self) -> dict[str, Any]:
        return {"type": "patch_approval", "patch_id": self.patch_id, "approved": self.approved}


@dataclass(slots=True)
class BackgroundShellControlOp:
    """Control a background shell."""

    shell_id: int
    action: BackgroundShellControlAction

    def to_dict(self) -> dict[str, Any]:
        return {
            "type": "background_shell_control",
            "shell_id": self.shell_id,
            "action": self.action.value,
        }


# Union type for all operations
Op = (
    InterruptOp
    | UserInputOp
    | UserTurnOp
    | ExecApprovalOp
    | PatchApprovalOp
    | BackgroundShellControlOp
)


@dataclass(slots=True)
class Submission:
    """A submission to the Codex engine."""

    id: str
    op: Op

    def to_dict(self) -> dict[str, Any]:
        return {"id": self.id, "op": self.op.to_dict()}

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), separators=(",", ":"))


def parse_op(data: dict[str, Any]) -> Op:
    """Parse an Op from a dictionary."""
    op_type = data.get("type")
    match op_type:
        case "interrupt":
            return InterruptOp()
        case "user_input":
            items = [user_input_from_dict(i) for i in data["items"]]
            return UserInputOp(items=items)
        case "user_turn":
            items = [user_input_from_dict(i) for i in data["items"]]
            return UserTurnOp(
                items=items,
                cwd=Path(data["cwd"]),
                approval_policy=AskForApproval(data["approval_policy"]),
                sandbox_policy=SandboxPolicy(data["sandbox_policy"]),
                model=data["model"],
                reasoning_effort=data.get("reasoning_effort"),
                reasoning_summary=data.get("reasoning_summary"),
            )
        case "exec_approval":
            return ExecApprovalOp(shell_id=data["shell_id"], approved=data["approved"])
        case "patch_approval":
            return PatchApprovalOp(patch_id=data["patch_id"], approved=data["approved"])
        case "background_shell_control":
            return BackgroundShellControlOp(
                shell_id=data["shell_id"],
                action=BackgroundShellControlAction(data["action"]),
            )
        case _:
            raise ValueError(f"Unknown op type: {op_type}")
