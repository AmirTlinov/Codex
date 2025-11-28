"""Session management for Codex.

Handles conversation state, turns, and history.
"""

from __future__ import annotations

import json
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from codex_core.config import get_sessions_dir
from codex_protocol.events import Usage
from codex_protocol.items import ThreadItem


@dataclass(slots=True)
class Turn:
    """A single turn in the conversation."""

    id: str
    user_input: str
    response_items: list[ThreadItem] = field(default_factory=list)
    usage: Usage = field(default_factory=Usage)
    started_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    completed_at: datetime | None = None
    error: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "user_input": self.user_input,
            "response_items": [item.to_dict() for item in self.response_items],
            "usage": self.usage.to_dict(),
            "started_at": self.started_at.isoformat(),
            "completed_at": self.completed_at.isoformat() if self.completed_at else None,
            "error": self.error,
        }


@dataclass(slots=True)
class SessionMeta:
    """Metadata for a session."""

    thread_id: str
    model: str
    created_at: datetime
    last_updated_at: datetime
    cwd: str
    title: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "type": "session_meta",
            "thread_id": self.thread_id,
            "model": self.model,
            "created_at": self.created_at.isoformat(),
            "last_updated_at": self.last_updated_at.isoformat(),
            "cwd": self.cwd,
            "title": self.title,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> SessionMeta:
        return cls(
            thread_id=data["thread_id"],
            model=data["model"],
            created_at=datetime.fromisoformat(data["created_at"]),
            last_updated_at=datetime.fromisoformat(data["last_updated_at"]),
            cwd=data["cwd"],
            title=data.get("title"),
        )


@dataclass
class Session:
    """A Codex conversation session."""

    thread_id: str
    model: str
    cwd: Path
    turns: list[Turn] = field(default_factory=list)
    created_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    title: str | None = None

    @classmethod
    def new(cls, model: str, cwd: Path) -> Session:
        """Create a new session."""
        return cls(
            thread_id=str(uuid.uuid4()),
            model=model,
            cwd=cwd,
        )

    @classmethod
    def load(cls, thread_id: str) -> Session | None:
        """Load a session from disk by thread ID."""
        sessions_dir = get_sessions_dir()
        if not sessions_dir.exists():
            return None

        # Find session file
        for file_path in sessions_dir.glob("rollout-*.jsonl"):
            try:
                with open(file_path) as f:
                    first_line = f.readline()
                    if not first_line:
                        continue
                    data = json.loads(first_line)
                    if data.get("type") == "session_meta" and data.get("thread_id") == thread_id:
                        return cls._load_from_file(file_path)
            except (json.JSONDecodeError, KeyError):
                continue

        return None

    @classmethod
    def _load_from_file(cls, file_path: Path) -> Session:
        """Load session from a rollout file."""
        with open(file_path) as f:
            lines = f.readlines()

        if not lines:
            raise ValueError("Empty session file")

        # First line is session meta
        meta_data = json.loads(lines[0])
        meta = SessionMeta.from_dict(meta_data)

        session = cls(
            thread_id=meta.thread_id,
            model=meta.model,
            cwd=Path(meta.cwd),
            created_at=meta.created_at,
            title=meta.title,
        )

        # Parse remaining lines as events/items
        # (Simplified - full implementation would reconstruct turns)
        return session

    def new_turn(self, user_input: str) -> Turn:
        """Start a new turn."""
        turn = Turn(
            id=str(uuid.uuid4()),
            user_input=user_input,
        )
        self.turns.append(turn)
        return turn

    def complete_turn(self, turn: Turn, usage: Usage | None = None) -> None:
        """Mark a turn as completed."""
        turn.completed_at = datetime.now(timezone.utc)
        if usage:
            turn.usage = usage

    def fail_turn(self, turn: Turn, error: str) -> None:
        """Mark a turn as failed."""
        turn.completed_at = datetime.now(timezone.utc)
        turn.error = error

    def save(self) -> Path:
        """Save session to disk."""
        sessions_dir = get_sessions_dir()
        sessions_dir.mkdir(parents=True, exist_ok=True)

        timestamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
        file_path = sessions_dir / f"rollout-{timestamp}-{self.thread_id[:8]}.jsonl"

        meta = SessionMeta(
            thread_id=self.thread_id,
            model=self.model,
            created_at=self.created_at,
            last_updated_at=datetime.now(timezone.utc),
            cwd=str(self.cwd),
            title=self.title,
        )

        with open(file_path, "w") as f:
            f.write(json.dumps(meta.to_dict()) + "\n")
            for turn in self.turns:
                f.write(json.dumps(turn.to_dict()) + "\n")

        return file_path

    def get_conversation_history(self) -> list[dict[str, Any]]:
        """Get conversation history for API context."""
        messages: list[dict[str, Any]] = []

        for turn in self.turns:
            # User message
            messages.append({"role": "user", "content": turn.user_input})

            # Assistant response (aggregate from items)
            response_text = ""
            for item in turn.response_items:
                item_dict = item.to_dict()
                if item_dict.get("type") == "agent_message":
                    response_text += item_dict.get("text", "")

            if response_text:
                messages.append({"role": "assistant", "content": response_text})

        return messages

    @property
    def total_tokens(self) -> int:
        """Get total tokens used in this session."""
        return sum(
            turn.usage.input_tokens + turn.usage.output_tokens for turn in self.turns
        )
