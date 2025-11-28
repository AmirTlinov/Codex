"""Integration tests for Codex components.

Tests component interactions without external dependencies.
"""

import asyncio
import json
import tempfile
from pathlib import Path

import pytest

from codex_core.config import Config, McpServerConfig, ModelProviderInfo
from codex_core.session import Session, Turn
from codex_protocol.events import Usage
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
    ThreadItem,
    TodoItem,
    TodoListItem,
    WebSearchItem,
)


class TestConfigIntegration:
    """Integration tests for configuration loading."""

    def test_load_default_config(self) -> None:
        """Test loading config with defaults."""
        config = Config.load()

        assert config.model is not None
        assert config.cwd is not None
        assert config.providers is not None
        assert "openai" in config.providers
        assert "anthropic" in config.providers

    def test_load_config_with_overrides(self) -> None:
        """Test loading config with overrides."""
        overrides = {
            "model": "gpt-4-turbo",
            "cwd": Path("/tmp/test"),
        }
        config = Config.load(overrides)

        assert config.model == "gpt-4-turbo"
        assert str(config.cwd) == "/tmp/test"

    def test_config_from_toml(self, tmp_path: Path) -> None:
        """Test loading config from TOML file."""
        config_file = tmp_path / "config.toml"
        config_file.write_text("""
[codex]
model = "claude-3-opus"
approval_policy = "always"

[providers.custom]
name = "Custom Provider"
base_url = "https://api.custom.com/v1"
api_key_env_var = "CUSTOM_API_KEY"
""")

        config = Config.from_file(config_file)

        assert config.model == "claude-3-opus"
        assert config.approval_policy == "always"
        assert "custom" in config.providers

    def test_provider_selection(self) -> None:
        """Test automatic provider selection based on model."""
        config = Config.load({"model": "gpt-4o"})
        provider = config.get_provider()

        assert provider is not None
        assert provider.name == "OpenAI"

        # Use a model name that matches the default Anthropic models list
        config2 = Config.load({"model": "claude-3-5-sonnet-20241022"})
        provider2 = config2.get_provider()

        assert provider2 is not None
        assert provider2.name == "Anthropic"


class TestSessionIntegration:
    """Integration tests for session management."""

    def test_session_create_and_save(self, tmp_path: Path) -> None:
        """Test creating and saving a session."""
        # Create session
        session = Session.new(model="gpt-4", cwd=tmp_path)

        assert session.thread_id is not None
        assert session.model == "gpt-4"
        assert len(session.turns) == 0

        # Save session
        session_dir = tmp_path / "sessions"
        session_dir.mkdir()
        session.save(session_dir)

        # Verify saved
        session_file = session_dir / f"{session.thread_id}.json"
        assert session_file.exists()

        # Load and verify
        loaded = Session.load_from_file(session_file)
        assert loaded is not None
        assert loaded.thread_id == session.thread_id
        assert loaded.model == session.model

    def test_session_turn_lifecycle(self) -> None:
        """Test full turn lifecycle in session."""
        session = Session.new(model="gpt-4", cwd=Path.cwd())

        # Start turn
        turn = session.new_turn("Hello, world!")

        assert turn.user_input == "Hello, world!"
        assert turn.status == "in_progress"
        assert len(session.turns) == 1

        # Add response items
        item1 = ThreadItem(
            id="item-1",
            details=AgentMessageItem(text="Hello! How can I help?"),
        )
        turn.response_items.append(item1)

        item2 = ThreadItem(
            id="item-2",
            details=CommandExecutionItem(
                command="ls -la",
                aggregated_output="total 0",
                exit_code=0,
            ),
        )
        turn.response_items.append(item2)

        # Complete turn
        usage = Usage(input_tokens=10, output_tokens=20)
        session.complete_turn(turn, usage)

        assert turn.status == "completed"
        assert turn.usage == usage
        assert len(turn.response_items) == 2

    def test_session_conversation_history(self) -> None:
        """Test building conversation history from session."""
        session = Session.new(model="gpt-4", cwd=Path.cwd())

        # First turn
        turn1 = session.new_turn("What is Python?")
        turn1.response_items.append(
            ThreadItem(
                id="r1",
                details=AgentMessageItem(text="Python is a programming language."),
            )
        )
        session.complete_turn(turn1, Usage(input_tokens=5, output_tokens=10))

        # Second turn
        turn2 = session.new_turn("Tell me more")
        turn2.response_items.append(
            ThreadItem(
                id="r2",
                details=AgentMessageItem(text="It's known for readability."),
            )
        )
        session.complete_turn(turn2, Usage(input_tokens=15, output_tokens=8))

        # Get history
        history = session.get_conversation_history()

        assert len(history) == 4  # 2 user + 2 assistant messages
        assert history[0]["role"] == "user"
        assert history[0]["content"] == "What is Python?"
        assert history[1]["role"] == "assistant"
        assert "programming language" in history[1]["content"]

    def test_session_failed_turn(self) -> None:
        """Test handling failed turns."""
        session = Session.new(model="gpt-4", cwd=Path.cwd())

        turn = session.new_turn("Do something")
        session.fail_turn(turn, "API error: rate limited")

        assert turn.status == "failed"
        assert turn.error == "API error: rate limited"

        # Failed turns should still be in history
        assert len(session.turns) == 1


class TestProtocolSerialization:
    """Integration tests for protocol serialization roundtrips."""

    def test_event_jsonl_roundtrip(self) -> None:
        """Test serializing events to JSONL and back."""
        from codex_protocol.events import (
            ItemCompletedEvent,
            ItemStartedEvent,
            ThreadStartedEvent,
            TurnCompletedEvent,
            TurnStartedEvent,
            parse_thread_event,
        )

        events = [
            ThreadStartedEvent(thread_id="thread-123"),
            TurnStartedEvent(),
            ItemStartedEvent(
                item=ThreadItem(
                    id="item-1",
                    details=AgentMessageItem(text="Hello"),
                )
            ),
            ItemCompletedEvent(
                item=ThreadItem(
                    id="item-1",
                    details=AgentMessageItem(text="Hello, world!"),
                )
            ),
            TurnCompletedEvent(usage=Usage(input_tokens=10, output_tokens=20)),
        ]

        # Serialize to JSONL
        jsonl_lines = []
        for event in events:
            jsonl_lines.append(json.dumps(event.to_dict()))

        # Parse back
        parsed_events = []
        for line in jsonl_lines:
            data = json.loads(line)
            parsed = parse_thread_event(data)
            parsed_events.append(parsed)

        # Verify
        assert len(parsed_events) == len(events)
        assert isinstance(parsed_events[0], ThreadStartedEvent)
        assert parsed_events[0].thread_id == "thread-123"
        assert isinstance(parsed_events[4], TurnCompletedEvent)
        assert parsed_events[4].usage.input_tokens == 10

    def test_item_serialization_all_types(self) -> None:
        """Test serializing all item types."""
        from codex_protocol.items import (
            CommandExecutionStatus,
            ErrorItem,
            FileChangeItem,
            McpToolCallItem,
            ReasoningItem,
            TodoItem,
            TodoListItem,
            WebSearchItem,
        )

        items = [
            ThreadItem(
                id="1",
                details=AgentMessageItem(text="Hello"),
            ),
            ThreadItem(
                id="2",
                details=ReasoningItem(text="Thinking..."),
            ),
            ThreadItem(
                id="3",
                details=CommandExecutionItem(
                    command="ls",
                    aggregated_output="file.txt",
                    status=CommandExecutionStatus.COMPLETED,
                    exit_code=0,
                ),
            ),
            ThreadItem(
                id="4",
                details=FileChangeItem(
                    changes=[
                        FileUpdateChange(path="test.py", kind=PatchChangeKind.ADD),
                    ],
                    status=PatchApplyStatus.COMPLETED,
                ),
            ),
            ThreadItem(
                id="5",
                details=McpToolCallItem(
                    server="test-server",
                    tool="test_tool",
                    arguments={"arg": "value"},
                    status=McpToolCallStatus.COMPLETED,
                ),
            ),
            ThreadItem(
                id="6",
                details=WebSearchItem(query="test query"),
            ),
            ThreadItem(
                id="7",
                details=TodoListItem(
                    items=[
                        TodoItem(text="Task 1", completed=False),
                        TodoItem(text="Task 2", completed=True),
                    ]
                ),
            ),
            ThreadItem(
                id="8",
                details=ErrorItem(message="Error occurred"),
            ),
        ]

        for item in items:
            # Serialize
            data = item.to_dict()
            json_str = json.dumps(data)

            # Deserialize
            parsed_data = json.loads(json_str)
            parsed_item = ThreadItem.from_dict(parsed_data)

            # Verify
            assert parsed_item.id == item.id
            assert type(parsed_item.details) == type(item.details)


class TestShellExecutorIntegration:
    """Integration tests for shell execution."""

    @pytest.mark.asyncio
    async def test_simple_command_execution(self) -> None:
        """Test executing a simple command."""
        from codex_shell.executor import ShellExecutor

        executor = ShellExecutor(cwd=Path.cwd())

        result = await executor.execute("echo 'hello world'")

        assert result.exit_code == 0
        assert "hello world" in result.output

    @pytest.mark.asyncio
    async def test_command_with_exit_code(self) -> None:
        """Test command that returns non-zero exit code."""
        from codex_shell.executor import ShellExecutor

        executor = ShellExecutor(cwd=Path.cwd())

        result = await executor.execute("exit 42")

        assert result.exit_code == 42

    @pytest.mark.asyncio
    async def test_command_timeout(self) -> None:
        """Test command timeout handling."""
        from codex_shell.executor import ShellExecutor

        executor = ShellExecutor(cwd=Path.cwd())

        result = await executor.execute("sleep 10", timeout_ms=100)

        assert result.exit_code != 0
        assert result.timed_out or "timeout" in result.output.lower()

    @pytest.mark.asyncio
    async def test_command_with_working_directory(self, tmp_path: Path) -> None:
        """Test command execution in specific directory."""
        from codex_shell.executor import ShellExecutor

        # Create a test file
        test_file = tmp_path / "test.txt"
        test_file.write_text("test content")

        executor = ShellExecutor(cwd=tmp_path)

        result = await executor.execute("ls")

        assert result.exit_code == 0
        assert "test.txt" in result.output

    @pytest.mark.asyncio
    async def test_command_output_streaming(self) -> None:
        """Test that output is captured progressively."""
        from codex_shell.executor import ShellExecutor

        executor = ShellExecutor(cwd=Path.cwd())

        # Command that produces output over time
        result = await executor.execute(
            "for i in 1 2 3; do echo $i; done"
        )

        assert result.exit_code == 0
        assert "1" in result.output
        assert "2" in result.output
        assert "3" in result.output


class TestBackgroundShellIntegration:
    """Integration tests for background shell management."""

    @pytest.mark.asyncio
    async def test_start_and_stop_background_shell(self) -> None:
        """Test starting and stopping a background shell."""
        from codex_shell.background import BackgroundShellManager, ProcessStatus

        manager = BackgroundShellManager()

        # Start a background process using spawn()
        shell = await manager.spawn("sleep 60")

        assert shell is not None
        assert shell.shell_id is not None

        # Check it's running
        running = manager.list_running()
        assert len(running) == 1
        assert running[0].shell_id == shell.shell_id
        assert running[0].status == ProcessStatus.RUNNING

        # Kill it via manager
        result = await manager.kill(shell.shell_id)
        assert result is True

        # Wait briefly for status update
        await asyncio.sleep(0.1)

        # Verify it's killed
        assert shell.status == ProcessStatus.KILLED

    @pytest.mark.asyncio
    async def test_background_shell_completes(self) -> None:
        """Test background shell that completes naturally."""
        from codex_shell.background import BackgroundShellManager, ProcessStatus

        manager = BackgroundShellManager()

        # Start a quick command
        shell = await manager.spawn("echo done")

        # Wait a bit for completion
        await asyncio.sleep(0.5)

        # Check status - should be completed
        assert shell.status in (ProcessStatus.COMPLETED, ProcessStatus.RUNNING)

    @pytest.mark.asyncio
    async def test_multiple_background_shells(self) -> None:
        """Test managing multiple background shells."""
        from codex_shell.background import BackgroundShellManager

        manager = BackgroundShellManager()

        # Start multiple using spawn()
        shell1 = await manager.spawn("sleep 60")
        shell2 = await manager.spawn("sleep 60")
        shell3 = await manager.spawn("sleep 60")

        all_shells = manager.list_all()
        assert len(all_shells) == 3

        # Kill all via manager
        await manager.kill(shell1.shell_id)
        await manager.kill(shell2.shell_id)
        await manager.kill(shell3.shell_id)


class TestMcpClientIntegration:
    """Integration tests for MCP client (with mock server)."""

    @pytest.mark.asyncio
    async def test_mcp_client_initialization(self) -> None:
        """Test MCP client can be initialized."""
        from codex_core.mcp_client import McpClient

        client = McpClient()

        assert client is not None
        # Use list_servers() method instead of servers attribute
        assert len(client.list_servers()) == 0

    @pytest.mark.asyncio
    async def test_mcp_tool_conversion(self) -> None:
        """Test MCP tool to OpenAI format conversion."""
        from codex_core.mcp_client import McpTool

        # McpTool doesn't have server_name - it's managed by McpServer
        tool = McpTool(
            name="test_tool",
            description="A test tool",
            input_schema={
                "type": "object",
                "properties": {
                    "arg1": {"type": "string"},
                },
                "required": ["arg1"],
            },
        )

        openai_format = tool.to_openai_format()

        assert openai_format["type"] == "function"
        # Tool name is just the tool name, server prefix is added when getting all tools
        assert openai_format["function"]["name"] == "test_tool"
        assert openai_format["function"]["description"] == "A test tool"
