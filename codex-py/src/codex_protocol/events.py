"""Thread event types for Codex protocol.

These types represent the JSONL events emitted by codex exec mode.
They must match the Rust exec_events.rs exactly for SDK compatibility.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

from codex_protocol.items import ThreadItem


@dataclass(slots=True)
class Usage:
    """Token usage statistics for a turn."""

    input_tokens: int = 0
    cached_input_tokens: int = 0
    output_tokens: int = 0

    def to_dict(self) -> dict[str, Any]:
        return {
            "input_tokens": self.input_tokens,
            "cached_input_tokens": self.cached_input_tokens,
            "output_tokens": self.output_tokens,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Usage:
        return cls(
            input_tokens=data.get("input_tokens", 0),
            cached_input_tokens=data.get("cached_input_tokens", 0),
            output_tokens=data.get("output_tokens", 0),
        )


@dataclass(slots=True)
class ThreadErrorEvent:
    """Fatal error emitted by the stream."""

    message: str

    def to_dict(self) -> dict[str, Any]:
        return {"message": self.message}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ThreadErrorEvent:
        return cls(message=data["message"])


@dataclass(slots=True)
class ThreadStartedEvent:
    """Emitted when a new thread is started as the first event."""

    thread_id: str

    def to_dict(self) -> dict[str, Any]:
        return {"type": "thread.started", "thread_id": self.thread_id}


@dataclass(slots=True)
class TurnStartedEvent:
    """Emitted when a turn is started by sending a new prompt to the model."""

    def to_dict(self) -> dict[str, Any]:
        return {"type": "turn.started"}


@dataclass(slots=True)
class TurnCompletedEvent:
    """Emitted when a turn is completed."""

    usage: Usage = field(default_factory=Usage)

    def to_dict(self) -> dict[str, Any]:
        return {"type": "turn.completed", "usage": self.usage.to_dict()}


@dataclass(slots=True)
class TurnFailedEvent:
    """Indicates that a turn failed with an error."""

    error: ThreadErrorEvent

    def to_dict(self) -> dict[str, Any]:
        return {"type": "turn.failed", "error": self.error.to_dict()}


@dataclass(slots=True)
class ItemStartedEvent:
    """Emitted when a new item is added to the thread (typically in progress)."""

    item: ThreadItem

    def to_dict(self) -> dict[str, Any]:
        return {"type": "item.started", "item": self.item.to_dict()}


@dataclass(slots=True)
class ItemUpdatedEvent:
    """Emitted when an item is updated."""

    item: ThreadItem

    def to_dict(self) -> dict[str, Any]:
        return {"type": "item.updated", "item": self.item.to_dict()}


@dataclass(slots=True)
class ItemCompletedEvent:
    """Signals that an item has reached a terminal state."""

    item: ThreadItem

    def to_dict(self) -> dict[str, Any]:
        return {"type": "item.completed", "item": self.item.to_dict()}


# Type alias for all thread events
ThreadEvent = (
    ThreadStartedEvent
    | TurnStartedEvent
    | TurnCompletedEvent
    | TurnFailedEvent
    | ItemStartedEvent
    | ItemUpdatedEvent
    | ItemCompletedEvent
    | ThreadErrorEvent
)


def thread_event_to_dict(event: ThreadEvent) -> dict[str, Any]:
    """Convert any thread event to a dictionary for JSON serialization."""
    if isinstance(event, ThreadErrorEvent):
        # Error events have a different format at the top level
        return {"type": "error", "message": event.message}
    return event.to_dict()


def thread_event_to_json(event: ThreadEvent) -> str:
    """Convert a thread event to a JSON string (JSONL line)."""
    return json.dumps(thread_event_to_dict(event), separators=(",", ":"))


def parse_thread_event(data: dict[str, Any]) -> ThreadEvent:
    """Parse a thread event from a dictionary."""
    event_type = data.get("type")

    match event_type:
        case "thread.started":
            return ThreadStartedEvent(thread_id=data["thread_id"])
        case "turn.started":
            return TurnStartedEvent()
        case "turn.completed":
            return TurnCompletedEvent(usage=Usage.from_dict(data.get("usage", {})))
        case "turn.failed":
            return TurnFailedEvent(error=ThreadErrorEvent.from_dict(data["error"]))
        case "item.started":
            return ItemStartedEvent(item=ThreadItem.from_dict(data["item"]))
        case "item.updated":
            return ItemUpdatedEvent(item=ThreadItem.from_dict(data["item"]))
        case "item.completed":
            return ItemCompletedEvent(item=ThreadItem.from_dict(data["item"]))
        case "error":
            return ThreadErrorEvent(message=data["message"])
        case _:
            raise ValueError(f"Unknown event type: {event_type}")
