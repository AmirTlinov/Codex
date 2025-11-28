"""Session management for Codex.

Handles conversation state, turns, and history persistence.
Supports auto-save, session listing, and sensitive data filtering.
"""

from __future__ import annotations

import json
import re
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

from codex_core.config import get_sessions_dir
from codex_protocol.events import Usage
from codex_protocol.items import ThreadItem, parse_thread_item


@dataclass(slots=True)
class SensitiveFilter:
    """Filters sensitive data from session content."""

    patterns: list[re.Pattern[str]] = field(default_factory=list)
    replacement: str = "[REDACTED]"

    @classmethod
    def from_patterns(cls, patterns: list[str]) -> SensitiveFilter:
        """Create filter from string patterns."""
        compiled = [re.compile(p) for p in patterns]
        return cls(patterns=compiled)

    def filter_text(self, text: str) -> str:
        """Remove sensitive content from text."""
        result = text
        for pattern in self.patterns:
            result = pattern.sub(self.replacement, result)
        return result

    def filter_dict(self, data: dict[str, Any]) -> dict[str, Any]:
        """Recursively filter sensitive content from dict."""
        result: dict[str, Any] = {}
        for key, value in data.items():
            if isinstance(value, str):
                result[key] = self.filter_text(value)
            elif isinstance(value, dict):
                result[key] = self.filter_dict(value)
            elif isinstance(value, list):
                result[key] = [
                    self.filter_dict(v) if isinstance(v, dict)
                    else self.filter_text(v) if isinstance(v, str)
                    else v
                    for v in value
                ]
            else:
                result[key] = value
        return result


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

    @property
    def status(self) -> str:
        """Get turn status."""
        if self.error:
            return "failed"
        if self.completed_at:
            return "completed"
        return "in_progress"

    def to_dict(self, filter_: SensitiveFilter | None = None) -> dict[str, Any]:
        """Convert to dict, optionally filtering sensitive data."""
        data = {
            "id": self.id,
            "user_input": self.user_input,
            "response_items": [item.to_dict() for item in self.response_items],
            "usage": self.usage.to_dict(),
            "started_at": self.started_at.isoformat(),
            "completed_at": self.completed_at.isoformat() if self.completed_at else None,
            "error": self.error,
        }
        if filter_:
            return filter_.filter_dict(data)
        return data

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Turn:
        """Parse Turn from dict."""
        items: list[ThreadItem] = []
        for item_data in data.get("response_items", []):
            try:
                items.append(parse_thread_item(item_data))
            except (KeyError, ValueError):
                # Skip invalid items
                pass

        usage = Usage()
        if "usage" in data:
            usage = Usage(
                input_tokens=data["usage"].get("input_tokens", 0),
                output_tokens=data["usage"].get("output_tokens", 0),
                cached_input_tokens=data["usage"].get("cached_input_tokens", 0),
            )

        return cls(
            id=data.get("id", str(uuid.uuid4())),
            user_input=data["user_input"],
            response_items=items,
            usage=usage,
            started_at=datetime.fromisoformat(data["started_at"])
            if "started_at" in data
            else datetime.now(timezone.utc),
            completed_at=datetime.fromisoformat(data["completed_at"])
            if data.get("completed_at")
            else None,
            error=data.get("error"),
        )


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
    """A Codex conversation session with persistence support."""

    thread_id: str
    model: str
    cwd: Path
    turns: list[Turn] = field(default_factory=list)
    created_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    title: str | None = None
    sensitive_filter: SensitiveFilter | None = None
    auto_save: bool = False
    _sessions_dir: Path | None = field(default=None, repr=False)

    @classmethod
    def new(
        cls,
        model: str,
        cwd: Path,
        sensitive_patterns: list[str] | None = None,
        auto_save: bool = False,
        sessions_dir: Path | None = None,
    ) -> Session:
        """Create a new session.

        Args:
            model: Model identifier
            cwd: Working directory
            sensitive_patterns: Regex patterns for sensitive data filtering
            auto_save: Enable auto-save after each turn
            sessions_dir: Custom sessions directory
        """
        filter_ = SensitiveFilter.from_patterns(sensitive_patterns) if sensitive_patterns else None
        return cls(
            thread_id=str(uuid.uuid4()),
            model=model,
            cwd=cwd,
            sensitive_filter=filter_,
            auto_save=auto_save,
            _sessions_dir=sessions_dir,
        )

    @classmethod
    def load(cls, thread_id: str, sessions_dir: Path | None = None) -> Session | None:
        """Load a session from disk by thread ID."""
        if sessions_dir is None:
            sessions_dir = get_sessions_dir()
        if not sessions_dir.exists():
            return None

        # Try direct file first (new format)
        direct_path = sessions_dir / f"{thread_id}.json"
        if direct_path.exists():
            return cls.load_from_file(direct_path)

        # Fallback: search in rollout files (legacy format)
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
        """Load session from a legacy rollout file."""
        with open(file_path) as f:
            lines = f.readlines()

        if not lines:
            raise ValueError("Empty session file")

        meta_data = json.loads(lines[0])
        meta = SessionMeta.from_dict(meta_data)

        session = cls(
            thread_id=meta.thread_id,
            model=meta.model,
            cwd=Path(meta.cwd),
            created_at=meta.created_at,
            title=meta.title,
        )

        # Parse turn data
        for line in lines[1:]:
            if not line.strip():
                continue
            try:
                turn_data = json.loads(line)
                if "user_input" in turn_data:
                    session.turns.append(Turn.from_dict(turn_data))
            except (json.JSONDecodeError, KeyError, ValueError):
                continue

        return session

    @classmethod
    def load_from_file(cls, file_path: Path) -> Session | None:
        """Load session from a specific file."""
        if not file_path.exists():
            return None

        try:
            with open(file_path) as f:
                lines = f.readlines()

            if not lines:
                return None

            meta_data = json.loads(lines[0])
            meta = SessionMeta.from_dict(meta_data)

            session = cls(
                thread_id=meta.thread_id,
                model=meta.model,
                cwd=Path(meta.cwd),
                created_at=meta.created_at,
                title=meta.title,
            )

            for line in lines[1:]:
                if not line.strip():
                    continue
                try:
                    turn_data = json.loads(line)
                    if "user_input" in turn_data:
                        session.turns.append(Turn.from_dict(turn_data))
                except (json.JSONDecodeError, KeyError, ValueError):
                    continue

            return session
        except (json.JSONDecodeError, KeyError):
            return None

    def new_turn(self, user_input: str) -> Turn:
        """Start a new turn."""
        turn = Turn(
            id=str(uuid.uuid4()),
            user_input=user_input,
        )
        self.turns.append(turn)
        return turn

    def complete_turn(self, turn: Turn, usage: Usage | None = None) -> None:
        """Mark a turn as completed and auto-save if enabled."""
        turn.completed_at = datetime.now(timezone.utc)
        if usage:
            turn.usage = usage
        if self.auto_save:
            self.save()

    def fail_turn(self, turn: Turn, error: str) -> None:
        """Mark a turn as failed and auto-save if enabled."""
        turn.completed_at = datetime.now(timezone.utc)
        turn.error = error
        if self.auto_save:
            self.save()

    def save(self, sessions_dir: Path | None = None) -> Path:
        """Save session to disk.

        Args:
            sessions_dir: Directory to save to. Defaults to configured or ~/.codex/sessions
        """
        if sessions_dir is None:
            sessions_dir = self._sessions_dir or get_sessions_dir()
        sessions_dir.mkdir(parents=True, exist_ok=True)

        file_path = sessions_dir / f"{self.thread_id}.json"

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
                f.write(json.dumps(turn.to_dict(self.sensitive_filter)) + "\n")

        return file_path

    def delete(self, sessions_dir: Path | None = None) -> bool:
        """Delete session file from disk."""
        if sessions_dir is None:
            sessions_dir = self._sessions_dir or get_sessions_dir()

        file_path = sessions_dir / f"{self.thread_id}.json"
        if file_path.exists():
            file_path.unlink()
            return True
        return False

    def get_conversation_history(self) -> list[dict[str, Any]]:
        """Get conversation history for API context."""
        messages: list[dict[str, Any]] = []

        for turn in self.turns:
            messages.append({"role": "user", "content": turn.user_input})

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

    @property
    def last_activity(self) -> datetime:
        """Get timestamp of last activity."""
        if self.turns:
            last_turn = self.turns[-1]
            return last_turn.completed_at or last_turn.started_at
        return self.created_at


def list_sessions(sessions_dir: Path | None = None) -> list[SessionMeta]:
    """List all available sessions.

    Returns list of SessionMeta sorted by last_updated_at descending.
    """
    if sessions_dir is None:
        sessions_dir = get_sessions_dir()
    if not sessions_dir.exists():
        return []

    sessions: list[SessionMeta] = []

    for file_path in sessions_dir.glob("*.json"):
        try:
            with open(file_path) as f:
                first_line = f.readline()
                if not first_line:
                    continue
                data = json.loads(first_line)
                if data.get("type") == "session_meta":
                    sessions.append(SessionMeta.from_dict(data))
        except (json.JSONDecodeError, KeyError):
            continue

    # Sort by last updated, newest first
    sessions.sort(key=lambda s: s.last_updated_at, reverse=True)
    return sessions


def delete_session(thread_id: str, sessions_dir: Path | None = None) -> bool:
    """Delete a session by thread ID."""
    if sessions_dir is None:
        sessions_dir = get_sessions_dir()

    file_path = sessions_dir / f"{thread_id}.json"
    if file_path.exists():
        file_path.unlink()
        return True
    return False


def cleanup_old_sessions(
    max_sessions: int = 100,
    max_age_days: int | None = None,
    sessions_dir: Path | None = None,
) -> int:
    """Clean up old sessions, keeping most recent.

    Args:
        max_sessions: Maximum number of sessions to keep
        max_age_days: Delete sessions older than this (optional)
        sessions_dir: Custom sessions directory

    Returns:
        Number of sessions deleted
    """
    sessions = list_sessions(sessions_dir)
    if sessions_dir is None:
        sessions_dir = get_sessions_dir()

    deleted = 0
    now = datetime.now(timezone.utc)

    for i, meta in enumerate(sessions):
        should_delete = False

        # Check count limit
        if i >= max_sessions:
            should_delete = True

        # Check age limit
        if max_age_days is not None:
            age = now - meta.last_updated_at
            if age.days > max_age_days:
                should_delete = True

        if should_delete:
            file_path = sessions_dir / f"{meta.thread_id}.json"
            if file_path.exists():
                file_path.unlink()
                deleted += 1

    return deleted
