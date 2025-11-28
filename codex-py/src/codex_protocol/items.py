"""Thread item types for Codex protocol.

These types represent the various items that can appear in a thread,
such as agent messages, command executions, file changes, etc.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class CommandExecutionStatus(str, Enum):
    """Status of a command execution."""

    IN_PROGRESS = "in_progress"
    COMPLETED = "completed"
    FAILED = "failed"
    REJECTED = "rejected"  # User rejected execution


class PatchApplyStatus(str, Enum):
    """Status of a file change operation."""

    COMPLETED = "completed"
    FAILED = "failed"


class PatchChangeKind(str, Enum):
    """Type of file change."""

    ADD = "add"
    DELETE = "delete"
    UPDATE = "update"


class McpToolCallStatus(str, Enum):
    """Status of an MCP tool call."""

    IN_PROGRESS = "in_progress"
    COMPLETED = "completed"
    FAILED = "failed"


@dataclass(slots=True)
class AgentMessageItem:
    """Response from the agent (natural language or JSON for structured output)."""

    text: str

    def to_dict(self) -> dict[str, Any]:
        return {"type": "agent_message", "text": self.text}


@dataclass(slots=True)
class ReasoningItem:
    """Agent's reasoning summary."""

    text: str

    def to_dict(self) -> dict[str, Any]:
        return {"type": "reasoning", "text": self.text}


@dataclass(slots=True)
class CommandExecutionItem:
    """A command executed by the agent."""

    command: str
    aggregated_output: str
    status: CommandExecutionStatus = CommandExecutionStatus.IN_PROGRESS
    exit_code: int | None = None

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "type": "command_execution",
            "command": self.command,
            "aggregated_output": self.aggregated_output,
            "status": self.status.value,
        }
        if self.exit_code is not None:
            d["exit_code"] = self.exit_code
        return d


@dataclass(slots=True)
class FileUpdateChange:
    """A single file change."""

    path: str
    kind: PatchChangeKind

    def to_dict(self) -> dict[str, Any]:
        return {"path": self.path, "kind": self.kind.value}


@dataclass(slots=True)
class FileChangeItem:
    """A set of file changes by the agent."""

    changes: list[FileUpdateChange]
    status: PatchApplyStatus

    def to_dict(self) -> dict[str, Any]:
        return {
            "type": "file_change",
            "changes": [c.to_dict() for c in self.changes],
            "status": self.status.value,
        }


@dataclass(slots=True)
class McpToolCallItemResult:
    """Result payload from an MCP tool invocation."""

    content: list[dict[str, Any]]
    structured_content: Any | None = None

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {"content": self.content}
        if self.structured_content is not None:
            d["structured_content"] = self.structured_content
        return d


@dataclass(slots=True)
class McpToolCallItemError:
    """Error details from a failed MCP tool invocation."""

    message: str

    def to_dict(self) -> dict[str, Any]:
        return {"message": self.message}


@dataclass(slots=True)
class McpToolCallItem:
    """A call to an MCP tool."""

    server: str
    tool: str
    status: McpToolCallStatus = McpToolCallStatus.IN_PROGRESS
    arguments: dict[str, Any] = field(default_factory=dict)
    result: McpToolCallItemResult | None = None
    error: McpToolCallItemError | None = None

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "type": "mcp_tool_call",
            "server": self.server,
            "tool": self.tool,
            "arguments": self.arguments,
            "status": self.status.value,
        }
        if self.result is not None:
            d["result"] = self.result.to_dict()
        if self.error is not None:
            d["error"] = self.error.to_dict()
        return d


@dataclass(slots=True)
class WebSearchItem:
    """A web search request."""

    query: str

    def to_dict(self) -> dict[str, Any]:
        return {"type": "web_search", "query": self.query}


@dataclass(slots=True)
class TodoItem:
    """An item in agent's to-do list."""

    text: str
    completed: bool = False

    def to_dict(self) -> dict[str, Any]:
        return {"text": self.text, "completed": self.completed}


@dataclass(slots=True)
class TodoListItem:
    """Agent's running to-do list."""

    items: list[TodoItem]

    def to_dict(self) -> dict[str, Any]:
        return {"type": "todo_list", "items": [i.to_dict() for i in self.items]}


@dataclass(slots=True)
class ErrorItem:
    """A non-fatal error surfaced as an item."""

    message: str

    def to_dict(self) -> dict[str, Any]:
        return {"type": "error", "message": self.message}


# Union type for all item details
ThreadItemDetails = (
    AgentMessageItem
    | ReasoningItem
    | CommandExecutionItem
    | FileChangeItem
    | McpToolCallItem
    | WebSearchItem
    | TodoListItem
    | ErrorItem
)


@dataclass(slots=True)
class ThreadItem:
    """Canonical representation of a thread item with its payload."""

    id: str
    details: ThreadItemDetails

    def to_dict(self) -> dict[str, Any]:
        d = self.details.to_dict()
        d["id"] = self.id
        # Reorder to have id first (matches Rust serde output)
        return {"id": self.id, **{k: v for k, v in d.items() if k != "id"}}

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), separators=(",", ":"))

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ThreadItem:
        """Parse a ThreadItem from a dictionary."""
        item_id = data["id"]
        item_type = data["type"]

        details: ThreadItemDetails
        match item_type:
            case "agent_message":
                details = AgentMessageItem(text=data["text"])
            case "reasoning":
                details = ReasoningItem(text=data["text"])
            case "command_execution":
                details = CommandExecutionItem(
                    command=data["command"],
                    aggregated_output=data["aggregated_output"],
                    status=CommandExecutionStatus(data["status"]),
                    exit_code=data.get("exit_code"),
                )
            case "file_change":
                changes = [
                    FileUpdateChange(path=c["path"], kind=PatchChangeKind(c["kind"]))
                    for c in data["changes"]
                ]
                details = FileChangeItem(
                    changes=changes,
                    status=PatchApplyStatus(data["status"]),
                )
            case "mcp_tool_call":
                result = None
                if data.get("result"):
                    result = McpToolCallItemResult(
                        content=data["result"]["content"],
                        structured_content=data["result"].get("structured_content"),
                    )
                error = None
                if data.get("error"):
                    error = McpToolCallItemError(message=data["error"]["message"])
                details = McpToolCallItem(
                    server=data["server"],
                    tool=data["tool"],
                    arguments=data.get("arguments", {}),
                    status=McpToolCallStatus(data["status"]),
                    result=result,
                    error=error,
                )
            case "web_search":
                details = WebSearchItem(query=data["query"])
            case "todo_list":
                items = [TodoItem(text=i["text"], completed=i["completed"]) for i in data["items"]]
                details = TodoListItem(items=items)
            case "error":
                details = ErrorItem(message=data["message"])
            case _:
                raise ValueError(f"Unknown item type: {item_type}")

        return cls(id=item_id, details=details)
