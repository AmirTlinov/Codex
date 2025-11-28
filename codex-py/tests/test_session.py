"""Tests for session persistence."""

import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from codex_core.session import (
    Session,
    SessionMeta,
    SensitiveFilter,
    Turn,
    cleanup_old_sessions,
    delete_session,
    list_sessions,
)
from codex_protocol.events import Usage
from codex_protocol.items import AgentMessageItem, ThreadItem


class TestSensitiveFilter:
    """Tests for sensitive data filtering."""

    def test_filter_text_simple(self) -> None:
        """Test filtering text with patterns."""
        filter_ = SensitiveFilter.from_patterns([r"password=\S+", r"api_key=\S+"])

        text = "config: password=secret123 api_key=abc123 port=8080"
        result = filter_.filter_text(text)

        assert "password=secret123" not in result
        assert "api_key=abc123" not in result
        assert "port=8080" in result
        assert "[REDACTED]" in result

    def test_filter_dict_recursive(self) -> None:
        """Test filtering dict recursively."""
        filter_ = SensitiveFilter.from_patterns([r"SECRET_\w+"])

        data = {
            "command": "export SECRET_KEY=abc123",
            "nested": {
                "output": "Token: SECRET_TOKEN here",
            },
            "list": ["normal", "SECRET_VALUE"],
            "number": 42,
        }

        result = filter_.filter_dict(data)

        assert "SECRET_KEY" not in result["command"]
        assert "SECRET_TOKEN" not in result["nested"]["output"]
        assert "SECRET_VALUE" not in result["list"][1]
        assert result["number"] == 42


class TestTurn:
    """Tests for Turn serialization."""

    def test_to_dict_basic(self) -> None:
        """Test basic turn serialization."""
        turn = Turn(
            id="turn-1",
            user_input="Hello",
            usage=Usage(input_tokens=10, output_tokens=20),
        )

        data = turn.to_dict()

        assert data["id"] == "turn-1"
        assert data["user_input"] == "Hello"
        assert data["usage"]["input_tokens"] == 10
        assert data["usage"]["output_tokens"] == 20

    def test_to_dict_with_filter(self) -> None:
        """Test turn serialization with sensitive filter."""
        turn = Turn(
            id="turn-1",
            user_input="My password is secret123",
        )
        filter_ = SensitiveFilter.from_patterns([r"password is \S+"])

        data = turn.to_dict(filter_)

        assert "secret123" not in data["user_input"]
        assert "[REDACTED]" in data["user_input"]

    def test_from_dict_roundtrip(self) -> None:
        """Test turn deserialization roundtrip."""
        original = Turn(
            id="turn-1",
            user_input="Test input",
            usage=Usage(input_tokens=5, output_tokens=10),
            started_at=datetime.now(timezone.utc),
        )
        original.completed_at = datetime.now(timezone.utc)

        data = original.to_dict()
        restored = Turn.from_dict(data)

        assert restored.id == original.id
        assert restored.user_input == original.user_input
        assert restored.usage.input_tokens == original.usage.input_tokens
        assert restored.status == "completed"


class TestSession:
    """Tests for Session persistence."""

    def test_new_session(self, tmp_path: Path) -> None:
        """Test creating new session."""
        session = Session.new(
            model="gpt-4",
            cwd=tmp_path,
            auto_save=False,
        )

        assert session.thread_id
        assert session.model == "gpt-4"
        assert session.cwd == tmp_path
        assert len(session.turns) == 0

    def test_save_and_load(self, tmp_path: Path) -> None:
        """Test saving and loading session."""
        session = Session.new(
            model="gpt-4",
            cwd=tmp_path,
            sessions_dir=tmp_path,
        )

        # Add a turn
        turn = session.new_turn("Hello, world!")
        turn.response_items.append(
            ThreadItem(
                id="item-1",
                details=AgentMessageItem(text="Hi there!"),
            )
        )
        session.complete_turn(turn, Usage(input_tokens=5, output_tokens=10))

        # Save
        file_path = session.save(tmp_path)
        assert file_path.exists()

        # Load
        loaded = Session.load(session.thread_id, tmp_path)
        assert loaded is not None
        assert loaded.thread_id == session.thread_id
        assert loaded.model == session.model
        assert len(loaded.turns) == 1
        assert loaded.turns[0].user_input == "Hello, world!"

    def test_auto_save(self, tmp_path: Path) -> None:
        """Test auto-save functionality."""
        session = Session.new(
            model="gpt-4",
            cwd=tmp_path,
            auto_save=True,
            sessions_dir=tmp_path,
        )

        turn = session.new_turn("Test")
        session.complete_turn(turn)

        # Should be auto-saved
        file_path = tmp_path / f"{session.thread_id}.json"
        assert file_path.exists()

    def test_sensitive_filtering_on_save(self, tmp_path: Path) -> None:
        """Test sensitive data is filtered when saving."""
        session = Session.new(
            model="gpt-4",
            cwd=tmp_path,
            sensitive_patterns=[r"API_KEY=\S+"],
            sessions_dir=tmp_path,
        )

        turn = session.new_turn("Set API_KEY=secret123 for auth")
        session.complete_turn(turn)
        session.save(tmp_path)

        # Read raw file and check
        file_path = tmp_path / f"{session.thread_id}.json"
        content = file_path.read_text()
        assert "secret123" not in content
        assert "[REDACTED]" in content

    def test_delete_session(self, tmp_path: Path) -> None:
        """Test deleting a session."""
        session = Session.new(
            model="gpt-4",
            cwd=tmp_path,
            sessions_dir=tmp_path,
        )
        session.save(tmp_path)

        file_path = tmp_path / f"{session.thread_id}.json"
        assert file_path.exists()

        result = session.delete(tmp_path)
        assert result
        assert not file_path.exists()

    def test_conversation_history(self, tmp_path: Path) -> None:
        """Test getting conversation history."""
        session = Session.new(model="gpt-4", cwd=tmp_path)

        turn1 = session.new_turn("Hello")
        turn1.response_items.append(
            ThreadItem(id="1", details=AgentMessageItem(text="Hi!"))
        )
        session.complete_turn(turn1)

        turn2 = session.new_turn("How are you?")
        turn2.response_items.append(
            ThreadItem(id="2", details=AgentMessageItem(text="I'm good!"))
        )
        session.complete_turn(turn2)

        history = session.get_conversation_history()

        assert len(history) == 4
        assert history[0] == {"role": "user", "content": "Hello"}
        assert history[1] == {"role": "assistant", "content": "Hi!"}
        assert history[2] == {"role": "user", "content": "How are you?"}
        assert history[3] == {"role": "assistant", "content": "I'm good!"}

    def test_total_tokens(self, tmp_path: Path) -> None:
        """Test total token counting."""
        session = Session.new(model="gpt-4", cwd=tmp_path)

        turn1 = session.new_turn("Hello")
        session.complete_turn(turn1, Usage(input_tokens=10, output_tokens=20))

        turn2 = session.new_turn("Bye")
        session.complete_turn(turn2, Usage(input_tokens=5, output_tokens=10))

        assert session.total_tokens == 45  # 10+20+5+10


class TestSessionListing:
    """Tests for session listing and cleanup."""

    def test_list_sessions(self, tmp_path: Path) -> None:
        """Test listing sessions."""
        # Create multiple sessions
        for i in range(3):
            session = Session.new(
                model="gpt-4",
                cwd=tmp_path,
                sessions_dir=tmp_path,
            )
            session.title = f"Session {i}"
            session.save(tmp_path)

        sessions = list_sessions(tmp_path)

        assert len(sessions) == 3

    def test_list_sessions_sorted(self, tmp_path: Path) -> None:
        """Test sessions are sorted by last update."""
        # Create sessions with different timestamps
        session1 = Session.new(model="gpt-4", cwd=tmp_path)
        session1.title = "Old"
        session1.save(tmp_path)

        session2 = Session.new(model="gpt-4", cwd=tmp_path)
        session2.title = "New"
        session2.save(tmp_path)

        sessions = list_sessions(tmp_path)

        # Most recent first
        assert sessions[0].title == "New"
        assert sessions[1].title == "Old"

    def test_delete_session_function(self, tmp_path: Path) -> None:
        """Test delete_session helper function."""
        session = Session.new(model="gpt-4", cwd=tmp_path)
        session.save(tmp_path)

        thread_id = session.thread_id
        assert delete_session(thread_id, tmp_path)
        assert not delete_session(thread_id, tmp_path)  # Already deleted

    def test_cleanup_by_count(self, tmp_path: Path) -> None:
        """Test cleaning up sessions by count limit."""
        # Create 5 sessions
        for _ in range(5):
            session = Session.new(model="gpt-4", cwd=tmp_path)
            session.save(tmp_path)

        # Keep only 2
        deleted = cleanup_old_sessions(max_sessions=2, sessions_dir=tmp_path)

        assert deleted == 3
        assert len(list_sessions(tmp_path)) == 2

    def test_cleanup_empty_dir(self, tmp_path: Path) -> None:
        """Test cleanup on empty directory."""
        deleted = cleanup_old_sessions(sessions_dir=tmp_path)
        assert deleted == 0


class TestSessionMeta:
    """Tests for SessionMeta."""

    def test_to_dict_roundtrip(self) -> None:
        """Test SessionMeta serialization roundtrip."""
        original = SessionMeta(
            thread_id="test-id",
            model="gpt-4",
            created_at=datetime.now(timezone.utc),
            last_updated_at=datetime.now(timezone.utc),
            cwd="/home/user/project",
            title="Test Session",
        )

        data = original.to_dict()
        restored = SessionMeta.from_dict(data)

        assert restored.thread_id == original.thread_id
        assert restored.model == original.model
        assert restored.title == original.title
