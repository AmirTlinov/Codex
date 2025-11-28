"""Tests for Codex protocol types.

These tests verify JSON serialization matches Rust serde output for SDK compatibility.
"""

import json

import pytest

from codex_protocol.events import (
    ItemCompletedEvent,
    ItemStartedEvent,
    ItemUpdatedEvent,
    ThreadErrorEvent,
    ThreadStartedEvent,
    TurnCompletedEvent,
    TurnFailedEvent,
    TurnStartedEvent,
    Usage,
    parse_thread_event,
    thread_event_to_json,
)
from codex_protocol.items import (
    AgentMessageItem,
    CommandExecutionItem,
    CommandExecutionStatus,
    ErrorItem,
    FileChangeItem,
    FileUpdateChange,
    McpToolCallItem,
    McpToolCallStatus,
    PatchApplyStatus,
    PatchChangeKind,
    ReasoningItem,
    ThreadItem,
    TodoItem,
    TodoListItem,
    WebSearchItem,
)


class TestThreadEvents:
    """Test ThreadEvent serialization."""

    def test_thread_started(self) -> None:
        event = ThreadStartedEvent(thread_id="abc-123")
        result = thread_event_to_json(event)
        expected = '{"type":"thread.started","thread_id":"abc-123"}'
        assert result == expected

    def test_turn_started(self) -> None:
        event = TurnStartedEvent()
        result = thread_event_to_json(event)
        expected = '{"type":"turn.started"}'
        assert result == expected

    def test_turn_completed(self) -> None:
        event = TurnCompletedEvent(
            usage=Usage(input_tokens=100, cached_input_tokens=50, output_tokens=200)
        )
        result = thread_event_to_json(event)
        data = json.loads(result)
        assert data["type"] == "turn.completed"
        assert data["usage"]["input_tokens"] == 100
        assert data["usage"]["cached_input_tokens"] == 50
        assert data["usage"]["output_tokens"] == 200

    def test_turn_failed(self) -> None:
        event = TurnFailedEvent(error=ThreadErrorEvent(message="Something went wrong"))
        result = thread_event_to_json(event)
        data = json.loads(result)
        assert data["type"] == "turn.failed"
        assert data["error"]["message"] == "Something went wrong"

    def test_error_event(self) -> None:
        event = ThreadErrorEvent(message="Fatal error")
        result = thread_event_to_json(event)
        expected = '{"type":"error","message":"Fatal error"}'
        assert result == expected


class TestThreadItems:
    """Test ThreadItem serialization."""

    def test_agent_message(self) -> None:
        item = ThreadItem(id="item-1", details=AgentMessageItem(text="Hello, world!"))
        result = item.to_json()
        data = json.loads(result)
        assert data["id"] == "item-1"
        assert data["type"] == "agent_message"
        assert data["text"] == "Hello, world!"

    def test_reasoning(self) -> None:
        item = ThreadItem(id="item-2", details=ReasoningItem(text="Thinking..."))
        result = item.to_json()
        data = json.loads(result)
        assert data["id"] == "item-2"
        assert data["type"] == "reasoning"
        assert data["text"] == "Thinking..."

    def test_command_execution_in_progress(self) -> None:
        item = ThreadItem(
            id="item-3",
            details=CommandExecutionItem(
                command="ls -la",
                aggregated_output="",
                status=CommandExecutionStatus.IN_PROGRESS,
            ),
        )
        result = item.to_json()
        data = json.loads(result)
        assert data["id"] == "item-3"
        assert data["type"] == "command_execution"
        assert data["command"] == "ls -la"
        assert data["status"] == "in_progress"
        assert "exit_code" not in data

    def test_command_execution_completed(self) -> None:
        item = ThreadItem(
            id="item-4",
            details=CommandExecutionItem(
                command="echo hello",
                aggregated_output="hello\n",
                status=CommandExecutionStatus.COMPLETED,
                exit_code=0,
            ),
        )
        result = item.to_json()
        data = json.loads(result)
        assert data["id"] == "item-4"
        assert data["type"] == "command_execution"
        assert data["exit_code"] == 0
        assert data["status"] == "completed"

    def test_file_change(self) -> None:
        item = ThreadItem(
            id="item-5",
            details=FileChangeItem(
                changes=[
                    FileUpdateChange(path="src/main.py", kind=PatchChangeKind.UPDATE),
                    FileUpdateChange(path="src/new.py", kind=PatchChangeKind.ADD),
                ],
                status=PatchApplyStatus.COMPLETED,
            ),
        )
        result = item.to_json()
        data = json.loads(result)
        assert data["id"] == "item-5"
        assert data["type"] == "file_change"
        assert len(data["changes"]) == 2
        assert data["changes"][0]["path"] == "src/main.py"
        assert data["changes"][0]["kind"] == "update"
        assert data["status"] == "completed"

    def test_mcp_tool_call(self) -> None:
        item = ThreadItem(
            id="item-6",
            details=McpToolCallItem(
                server="filesystem",
                tool="read_file",
                arguments={"path": "/tmp/test.txt"},
                status=McpToolCallStatus.IN_PROGRESS,
            ),
        )
        result = item.to_json()
        data = json.loads(result)
        assert data["id"] == "item-6"
        assert data["type"] == "mcp_tool_call"
        assert data["server"] == "filesystem"
        assert data["tool"] == "read_file"
        assert data["arguments"]["path"] == "/tmp/test.txt"

    def test_web_search(self) -> None:
        item = ThreadItem(id="item-7", details=WebSearchItem(query="python async tutorial"))
        result = item.to_json()
        data = json.loads(result)
        assert data["id"] == "item-7"
        assert data["type"] == "web_search"
        assert data["query"] == "python async tutorial"

    def test_todo_list(self) -> None:
        item = ThreadItem(
            id="item-8",
            details=TodoListItem(
                items=[
                    TodoItem(text="Read file", completed=True),
                    TodoItem(text="Write code", completed=False),
                ]
            ),
        )
        result = item.to_json()
        data = json.loads(result)
        assert data["id"] == "item-8"
        assert data["type"] == "todo_list"
        assert len(data["items"]) == 2
        assert data["items"][0]["text"] == "Read file"
        assert data["items"][0]["completed"] is True

    def test_error_item(self) -> None:
        item = ThreadItem(id="item-9", details=ErrorItem(message="Something went wrong"))
        result = item.to_json()
        data = json.loads(result)
        assert data["id"] == "item-9"
        assert data["type"] == "error"
        assert data["message"] == "Something went wrong"


class TestItemEvents:
    """Test item-related events."""

    def test_item_started(self) -> None:
        item = ThreadItem(id="item-1", details=AgentMessageItem(text="Starting..."))
        event = ItemStartedEvent(item=item)
        result = thread_event_to_json(event)
        data = json.loads(result)
        assert data["type"] == "item.started"
        assert data["item"]["id"] == "item-1"
        assert data["item"]["type"] == "agent_message"

    def test_item_updated(self) -> None:
        item = ThreadItem(id="item-1", details=AgentMessageItem(text="Updated text"))
        event = ItemUpdatedEvent(item=item)
        result = thread_event_to_json(event)
        data = json.loads(result)
        assert data["type"] == "item.updated"
        assert data["item"]["text"] == "Updated text"

    def test_item_completed(self) -> None:
        item = ThreadItem(
            id="item-1",
            details=CommandExecutionItem(
                command="ls",
                aggregated_output="file1\nfile2\n",
                status=CommandExecutionStatus.COMPLETED,
                exit_code=0,
            ),
        )
        event = ItemCompletedEvent(item=item)
        result = thread_event_to_json(event)
        data = json.loads(result)
        assert data["type"] == "item.completed"
        assert data["item"]["exit_code"] == 0


class TestParsing:
    """Test parsing events from JSON."""

    def test_parse_thread_started(self) -> None:
        data = {"type": "thread.started", "thread_id": "test-123"}
        event = parse_thread_event(data)
        assert isinstance(event, ThreadStartedEvent)
        assert event.thread_id == "test-123"

    def test_parse_turn_completed(self) -> None:
        data = {
            "type": "turn.completed",
            "usage": {"input_tokens": 100, "cached_input_tokens": 0, "output_tokens": 50},
        }
        event = parse_thread_event(data)
        assert isinstance(event, TurnCompletedEvent)
        assert event.usage.input_tokens == 100

    def test_parse_item_started(self) -> None:
        data = {
            "type": "item.started",
            "item": {"id": "item-1", "type": "agent_message", "text": "Hello"},
        }
        event = parse_thread_event(data)
        assert isinstance(event, ItemStartedEvent)
        assert isinstance(event.item.details, AgentMessageItem)
        assert event.item.details.text == "Hello"

    def test_roundtrip(self) -> None:
        """Test that serialization and parsing are inverse operations."""
        original = ThreadItem(
            id="test-item",
            details=CommandExecutionItem(
                command="echo test",
                aggregated_output="test\n",
                status=CommandExecutionStatus.COMPLETED,
                exit_code=0,
            ),
        )
        json_str = original.to_json()
        parsed = ThreadItem.from_dict(json.loads(json_str))
        assert parsed.id == original.id
        assert isinstance(parsed.details, CommandExecutionItem)
        assert parsed.details.command == "echo test"
        assert parsed.details.exit_code == 0
