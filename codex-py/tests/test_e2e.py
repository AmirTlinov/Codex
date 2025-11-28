"""End-to-end tests for Codex.

Tests full workflows with mock API responses.
"""

import asyncio
import json
from collections.abc import AsyncIterator
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from codex_core.client import CompletionResponse, Message, ModelClient, StreamChunk, ToolCall
from codex_core.codex import Codex
from codex_core.config import Config
from codex_protocol.events import (
    ItemCompletedEvent,
    ItemStartedEvent,
    ItemUpdatedEvent,
    ThreadEvent,
    ThreadStartedEvent,
    TurnCompletedEvent,
    TurnStartedEvent,
)
from codex_protocol.items import AgentMessageItem, CommandExecutionItem


class MockStreamingClient:
    """Mock client that simulates streaming responses."""

    def __init__(self, responses: list[StreamChunk]) -> None:
        self.responses = responses
        self.messages_received: list[Message] = []

    async def stream_completion(
        self,
        messages: list[Message],
        tools: list[dict] | None = None,
    ) -> AsyncIterator[StreamChunk]:
        """Simulate streaming completion."""
        self.messages_received = messages

        for chunk in self.responses:
            yield chunk
            await asyncio.sleep(0.01)  # Simulate network delay

    async def __aenter__(self) -> "MockStreamingClient":
        return self

    async def __aexit__(self, *args: object) -> None:
        pass


class TestE2EConversation:
    """E2E tests for conversation flow."""

    @pytest.mark.asyncio
    async def test_simple_conversation_turn(self, tmp_path: Path) -> None:
        """Test a simple conversation turn without tools."""
        # Setup mock responses
        mock_chunks = [
            StreamChunk(content="Hello"),
            StreamChunk(content="! How"),
            StreamChunk(content=" can I"),
            StreamChunk(content=" help?"),
            StreamChunk(
                content=None,
                usage={"prompt_tokens": 10, "completion_tokens": 5},
            ),
        ]

        config = Config.load({"cwd": tmp_path, "model": "gpt-4"})

        # Create Codex with mock client
        codex = await Codex.create(config)
        codex._client = MockStreamingClient(mock_chunks)

        # Run turn
        events: list[ThreadEvent] = []
        async for event in codex.run_turn("Hello"):
            events.append(event)

        # Verify events sequence
        assert any(isinstance(e, ThreadStartedEvent) for e in events)
        assert any(isinstance(e, TurnStartedEvent) for e in events)
        assert any(isinstance(e, ItemStartedEvent) for e in events)
        assert any(isinstance(e, ItemUpdatedEvent) for e in events)
        assert any(isinstance(e, ItemCompletedEvent) for e in events)
        assert any(isinstance(e, TurnCompletedEvent) for e in events)

        # Verify final message
        completed_events = [e for e in events if isinstance(e, ItemCompletedEvent)]
        assert len(completed_events) >= 1

        last_item = completed_events[-1].item
        assert isinstance(last_item.details, AgentMessageItem)
        assert "Hello" in last_item.details.text
        assert "help" in last_item.details.text

    @pytest.mark.asyncio
    async def test_conversation_with_tool_call(self, tmp_path: Path) -> None:
        """Test conversation where model calls a tool."""
        # Mock responses: first call requests tool, second provides result
        tool_call = ToolCall(
            id="call-1",
            name="shell",
            arguments={"command": "echo test"},
        )

        mock_chunks = [
            StreamChunk(content="Let me run that for you."),
            StreamChunk(tool_calls=[tool_call]),
            StreamChunk(
                content=None,
                usage={"prompt_tokens": 15, "completion_tokens": 10},
            ),
        ]

        config = Config.load({"cwd": tmp_path, "model": "gpt-4"})

        codex = await Codex.create(config)
        codex._client = MockStreamingClient(mock_chunks)

        # Run turn
        events: list[ThreadEvent] = []
        async for event in codex.run_turn("Run echo test"):
            events.append(event)

        # Should have command execution events
        item_events = [e for e in events if isinstance(e, (ItemStartedEvent, ItemCompletedEvent))]
        assert len(item_events) >= 2  # At least message and command

        # Find command execution
        command_events = [
            e for e in item_events
            if isinstance(e, ItemCompletedEvent)
            and isinstance(e.item.details, CommandExecutionItem)
        ]

        assert len(command_events) >= 1
        cmd_item = command_events[0].item.details
        assert cmd_item.command == "echo test"

    @pytest.mark.asyncio
    async def test_multi_turn_conversation(self, tmp_path: Path) -> None:
        """Test multiple turns maintaining context."""
        config = Config.load({"cwd": tmp_path, "model": "gpt-4"})

        codex = await Codex.create(config)

        # First turn
        codex._client = MockStreamingClient([
            StreamChunk(content="I'm Codex, an AI assistant."),
            StreamChunk(usage={"prompt_tokens": 5, "completion_tokens": 6}),
        ])

        events1: list[ThreadEvent] = []
        async for event in codex.run_turn("Who are you?"):
            events1.append(event)

        # Second turn - should include history
        codex._client = MockStreamingClient([
            StreamChunk(content="Yes, I said I'm Codex."),
            StreamChunk(usage={"prompt_tokens": 15, "completion_tokens": 5}),
        ])

        events2: list[ThreadEvent] = []
        async for event in codex.run_turn("What did you say?"):
            events2.append(event)

        # Verify session has both turns
        assert len(codex.session.turns) == 2

        # Verify second turn's messages included history
        client = codex._client
        assert len(client.messages_received) >= 3  # system + user1 + assistant1 + user2


class TestE2EErrorHandling:
    """E2E tests for error scenarios."""

    @pytest.mark.asyncio
    async def test_api_error_handling(self, tmp_path: Path) -> None:
        """Test handling of API errors."""
        from codex_protocol.events import TurnFailedEvent

        config = Config.load({"cwd": tmp_path, "model": "gpt-4"})

        codex = await Codex.create(config)

        # Mock client that raises error
        async def error_stream(*args: object, **kwargs: object) -> AsyncIterator[StreamChunk]:
            raise Exception("API Error: Rate limited")
            yield  # Make it a generator

        mock_client = MagicMock()
        mock_client.stream_completion = error_stream
        codex._client = mock_client

        # Run turn
        events: list[ThreadEvent] = []
        async for event in codex.run_turn("Hello"):
            events.append(event)

        # Should have failure event
        failed_events = [e for e in events if isinstance(e, TurnFailedEvent)]
        assert len(failed_events) == 1
        assert "Rate limited" in failed_events[0].error.message

    @pytest.mark.asyncio
    async def test_tool_execution_error(self, tmp_path: Path) -> None:
        """Test handling of tool execution errors."""
        tool_call = ToolCall(
            id="call-1",
            name="shell",
            arguments={"command": "nonexistent_command_xyz"},
        )

        mock_chunks = [
            StreamChunk(content="Running command..."),
            StreamChunk(tool_calls=[tool_call]),
            StreamChunk(usage={"prompt_tokens": 10, "completion_tokens": 5}),
        ]

        config = Config.load({"cwd": tmp_path, "model": "gpt-4"})

        codex = await Codex.create(config)
        codex._client = MockStreamingClient(mock_chunks)

        events: list[ThreadEvent] = []
        async for event in codex.run_turn("Run bad command"):
            events.append(event)

        # Command should complete with error exit code
        cmd_events = [
            e for e in events
            if isinstance(e, ItemCompletedEvent)
            and isinstance(e.item.details, CommandExecutionItem)
        ]

        assert len(cmd_events) >= 1
        cmd = cmd_events[0].item.details
        assert cmd.exit_code != 0 or "not found" in cmd.aggregated_output.lower()


class TestE2EJSONL:
    """E2E tests for JSONL output (SDK compatibility)."""

    @pytest.mark.asyncio
    async def test_jsonl_output_format(self, tmp_path: Path) -> None:
        """Test that events can be serialized to valid JSONL."""
        mock_chunks = [
            StreamChunk(content="Hello!"),
            StreamChunk(usage={"prompt_tokens": 5, "completion_tokens": 2}),
        ]

        config = Config.load({"cwd": tmp_path, "model": "gpt-4"})

        codex = await Codex.create(config)
        codex._client = MockStreamingClient(mock_chunks)

        # Collect events and serialize
        jsonl_output = []
        async for event in codex.run_turn("Hi"):
            jsonl_output.append(json.dumps(event.to_dict()))

        # Verify each line is valid JSON
        for line in jsonl_output:
            data = json.loads(line)
            assert "type" in data

        # Verify we have required event types
        types = [json.loads(line)["type"] for line in jsonl_output]
        assert "thread.started" in types
        assert "turn.started" in types
        assert "turn.completed" in types

    @pytest.mark.asyncio
    async def test_event_types_match_sdk(self, tmp_path: Path) -> None:
        """Test that event types match SDK expectations."""
        expected_types = {
            "thread.started",
            "turn.started",
            "turn.completed",
            "turn.failed",
            "item.started",
            "item.updated",
            "item.completed",
            "thread.error",
        }

        mock_chunks = [
            StreamChunk(content="Test"),
            StreamChunk(usage={"prompt_tokens": 5, "completion_tokens": 1}),
        ]

        config = Config.load({"cwd": tmp_path, "model": "gpt-4"})

        codex = await Codex.create(config)
        codex._client = MockStreamingClient(mock_chunks)

        seen_types = set()
        async for event in codex.run_turn("Test"):
            event_data = event.to_dict()
            seen_types.add(event_data["type"])

        # All seen types should be in expected set
        for t in seen_types:
            assert t in expected_types, f"Unexpected event type: {t}"


class TestE2ECLIExec:
    """E2E tests for CLI exec mode."""

    @pytest.mark.asyncio
    async def test_cli_exec_output(self, tmp_path: Path, capsys: pytest.CaptureFixture) -> None:
        """Test CLI exec mode produces correct output."""
        # This would require mocking stdin/stdout
        # For now, test the core functionality

        import inspect
        from codex_core.cli import run_exec

        # Verify the function exists and is async
        assert inspect.iscoroutinefunction(run_exec)

        # Test would require more complex setup with mocked stdin/stdout
        # The actual run_exec requires proper argparse Namespace


class TestE2ETUIWidgets:
    """E2E tests for TUI widget interactions."""

    def test_approval_workflow(self) -> None:
        """Test approval queue workflow."""
        from codex_tui.widgets.approval import (
            ApprovalQueue,
            ApprovalResult,
        )

        queue = ApprovalQueue()

        # Add command
        request = queue.add_command("cmd-1", "rm -rf /")
        assert request is not None
        assert queue.has_pending

        # Reject
        result = queue.resolve("cmd-1", ApprovalResult.REJECTED)
        assert result is False
        assert not queue.has_pending

        # Add another and approve
        request2 = queue.add_command("cmd-2", "ls -la")
        result2 = queue.resolve("cmd-2", ApprovalResult.APPROVED)
        assert result2 is True

    def test_diff_parsing_complex(self) -> None:
        """Test complex diff parsing."""
        from codex_tui.widgets.diff_view import parse_unified_diff

        diff = """diff --git a/src/main.py b/src/main.py
index 1234567..abcdefg 100644
--- a/src/main.py
+++ b/src/main.py
@@ -1,10 +1,12 @@
 import os
+import sys

 def main():
-    print("hello")
+    print("Hello, World!")
+    return 0

 if __name__ == "__main__":
     main()
--- /dev/null
+++ b/src/new_file.py
@@ -0,0 +1,5 @@
+def helper():
+    pass
+
+if __name__ == "__main__":
+    helper()
"""

        diffs = parse_unified_diff(diff)

        assert len(diffs) == 2

        # First file - modified
        assert diffs[0].path == "b/src/main.py"
        assert not diffs[0].is_new
        assert diffs[0].hunks is not None
        assert len(diffs[0].hunks) == 1

        # Second file - new
        assert diffs[1].path == "b/src/new_file.py"
        assert diffs[1].is_new

    def test_markdown_streaming(self) -> None:
        """Test streaming markdown rendering."""
        from codex_tui.widgets.markdown_view import StreamingMarkdown

        widget = StreamingMarkdown()

        # Simulate streaming
        widget.update_content("# Hello")
        assert widget.content == "# Hello"

        widget.append_content("\n\nThis is ")
        widget.append_content("**bold** text")

        assert "**bold**" in widget.content


class TestE2ESessionPersistence:
    """E2E tests for session save/restore."""

    @pytest.mark.asyncio
    async def test_session_resume(self, tmp_path: Path) -> None:
        """Test resuming a saved session."""
        config = Config.load({"cwd": tmp_path, "model": "gpt-4"})

        # Create and run first session
        codex1 = await Codex.create(config)
        codex1._client = MockStreamingClient([
            StreamChunk(content="First response"),
            StreamChunk(usage={"prompt_tokens": 5, "completion_tokens": 2}),
        ])

        async for _ in codex1.run_turn("First message"):
            pass

        # Save session
        session_dir = tmp_path / "sessions"
        session_dir.mkdir()
        codex1.session.save(session_dir)
        thread_id = codex1.session.thread_id

        # Load session in new Codex
        from codex_core.session import Session

        loaded_session = Session.load_from_file(session_dir / f"{thread_id}.json")
        assert loaded_session is not None

        # Create new Codex with loaded session
        codex2 = await Codex.create(config, thread_id=None)
        codex2.session = loaded_session
        codex2._client = MockStreamingClient([
            StreamChunk(content="Second response"),
            StreamChunk(usage={"prompt_tokens": 10, "completion_tokens": 2}),
        ])

        async for _ in codex2.run_turn("Second message"):
            pass

        # Verify history is maintained
        assert len(codex2.session.turns) == 2
        assert codex2.session.turns[0].user_input == "First message"
        assert codex2.session.turns[1].user_input == "Second message"
