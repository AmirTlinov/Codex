"""Codex orchestrator - main engine for running conversations.

This is the core business logic that coordinates:
- Session management
- API interactions
- Tool execution
- Event emission
"""

from __future__ import annotations

import asyncio
import uuid
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from codex_core.client import Message, ModelClient, StreamChunk, ToolCall
from codex_core.config import Config
from codex_core.session import Session, Turn
from codex_protocol.events import (
    ItemCompletedEvent,
    ItemStartedEvent,
    ItemUpdatedEvent,
    ThreadErrorEvent,
    ThreadEvent,
    ThreadStartedEvent,
    TurnCompletedEvent,
    TurnFailedEvent,
    TurnStartedEvent,
    Usage,
)
from codex_protocol.items import (
    AgentMessageItem,
    CommandExecutionItem,
    CommandExecutionStatus,
    ErrorItem,
    FileChangeItem,
    FileUpdateChange,
    PatchApplyStatus,
    PatchChangeKind,
    ThreadItem,
)


@dataclass(slots=True)
class ToolDefinition:
    """Definition of a tool that can be called by the model."""

    name: str
    description: str
    parameters: dict[str, Any]

    def to_openai_format(self) -> dict[str, Any]:
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            },
        }


# Built-in tools
SHELL_TOOL = ToolDefinition(
    name="shell",
    description="Execute a shell command in the working directory.",
    parameters={
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "The shell command to execute",
            },
            "timeout_ms": {
                "type": "integer",
                "description": "Timeout in milliseconds (default: 60000)",
            },
        },
        "required": ["command"],
    },
)

APPLY_PATCH_TOOL = ToolDefinition(
    name="apply_patch",
    description="""Apply a deterministic patch to modify files.

Format:
*** Begin Patch
<operations>
*** End Patch

Operations:
- *** Add File: path         Create/replace file (+ prefixed lines)
- *** Delete File: path      Remove file
- *** Update File: path      Apply diff hunks (@@ context, +/- changes)
- *** Insert Before Symbol: file::Symbol::path  Insert before symbol
- *** Insert After Symbol: file::Symbol::path   Insert after symbol
- *** Replace Symbol Body: file::Symbol::path   Replace symbol body

Example:
*** Begin Patch
*** Update File: src/main.py
@@ def hello():
-    print("hello")
+    print("Hello, World!")
*** End Patch""",
    parameters={
        "type": "object",
        "properties": {
            "patch": {
                "type": "string",
                "description": "The patch content in the deterministic format",
            },
        },
        "required": ["patch"],
    },
)


@dataclass
class Codex:
    """Main Codex orchestrator."""

    config: Config
    session: Session
    _client: ModelClient | None = None
    _event_queue: asyncio.Queue[ThreadEvent] = field(default_factory=asyncio.Queue)
    _tools: list[ToolDefinition] = field(default_factory=list)
    _tool_handlers: dict[str, Any] = field(default_factory=dict)

    @classmethod
    async def create(
        cls,
        config: Config,
        thread_id: str | None = None,
    ) -> Codex:
        """Create a new Codex instance."""
        # Load or create session
        if thread_id:
            session = Session.load(thread_id)
            if not session:
                raise ValueError(f"Session not found: {thread_id}")
        else:
            session = Session.new(model=config.model, cwd=config.cwd)

        codex = cls(config=config, session=session)

        # Register built-in tools
        codex.register_tool(SHELL_TOOL, codex._handle_shell)
        if config.include_apply_patch_tool:
            codex.register_tool(APPLY_PATCH_TOOL, codex._handle_apply_patch)

        return codex

    def register_tool(
        self,
        tool: ToolDefinition,
        handler: Any,
    ) -> None:
        """Register a tool with its handler."""
        self._tools.append(tool)
        self._tool_handlers[tool.name] = handler

    async def __aenter__(self) -> Codex:
        self._client = ModelClient(self.config)
        await self._client.__aenter__()
        return self

    async def __aexit__(self, *args: Any) -> None:
        if self._client:
            await self._client.__aexit__(*args)

    async def events(self) -> AsyncIterator[ThreadEvent]:
        """Yield events from the event queue."""
        while True:
            event = await self._event_queue.get()
            yield event
            if isinstance(event, (TurnCompletedEvent, TurnFailedEvent, ThreadErrorEvent)):
                # Check if there are more events
                if self._event_queue.empty():
                    break

    def _emit(self, event: ThreadEvent) -> None:
        """Emit an event to the queue."""
        self._event_queue.put_nowait(event)

    async def run_turn(self, user_input: str) -> AsyncIterator[ThreadEvent]:
        """Run a single turn with user input and yield events."""
        if not self._client:
            raise RuntimeError("Codex not initialized. Use async context manager.")

        # Emit thread started if this is the first turn
        if not self.session.turns:
            yield ThreadStartedEvent(thread_id=self.session.thread_id)

        # Start the turn
        turn = self.session.new_turn(user_input)
        yield TurnStartedEvent()

        try:
            # Build messages
            messages = self._build_messages(user_input)

            # Get tools in OpenAI format
            tools = [t.to_openai_format() for t in self._tools] if self._tools else None

            # Track response
            response_text = ""
            current_item_id: str | None = None
            usage = Usage()

            # Stream completion
            async for chunk in self._client.stream_completion(messages, tools):
                # Handle content
                if chunk.content:
                    response_text += chunk.content
                    if current_item_id is None:
                        current_item_id = str(uuid.uuid4())
                        item = ThreadItem(
                            id=current_item_id,
                            details=AgentMessageItem(text=response_text),
                        )
                        yield ItemStartedEvent(item=item)
                    else:
                        item = ThreadItem(
                            id=current_item_id,
                            details=AgentMessageItem(text=response_text),
                        )
                        yield ItemUpdatedEvent(item=item)

                # Handle tool calls
                if chunk.tool_calls:
                    for tool_call in chunk.tool_calls:
                        async for event in self._handle_tool_call(tool_call, turn):
                            yield event

                # Handle usage
                if chunk.usage:
                    usage = Usage(
                        input_tokens=chunk.usage.get("prompt_tokens", 0),
                        cached_input_tokens=chunk.usage.get("cached_tokens", 0),
                        output_tokens=chunk.usage.get("completion_tokens", 0),
                    )

            # Complete the agent message item
            if current_item_id:
                item = ThreadItem(
                    id=current_item_id,
                    details=AgentMessageItem(text=response_text),
                )
                turn.response_items.append(item)
                yield ItemCompletedEvent(item=item)

            # Complete the turn
            self.session.complete_turn(turn, usage)
            yield TurnCompletedEvent(usage=usage)

        except Exception as e:
            error_msg = str(e)
            self.session.fail_turn(turn, error_msg)
            yield TurnFailedEvent(error=ThreadErrorEvent(message=error_msg))

    def _build_messages(self, user_input: str) -> list[Message]:
        """Build messages for the API request."""
        messages: list[Message] = []

        # System message with instructions
        system_content = self._build_system_prompt()
        messages.append(Message(role="system", content=system_content))

        # Conversation history
        for history_msg in self.session.get_conversation_history():
            messages.append(Message(role=history_msg["role"], content=history_msg["content"]))

        # Current user input
        messages.append(Message(role="user", content=user_input))

        return messages

    def _build_system_prompt(self) -> str:
        """Build the system prompt with instructions."""
        parts = []

        # Base instructions
        if self.config.base_instructions:
            parts.append(self.config.base_instructions)
        else:
            parts.append(
                "You are Codex, an AI coding assistant. "
                "You help users with software development tasks by executing commands "
                "and modifying files. Be concise and helpful."
            )

        # User instructions from AGENTS.md
        if self.config.user_instructions:
            parts.append(f"\n\nUser Instructions:\n{self.config.user_instructions}")

        # Developer instructions
        if self.config.developer_instructions:
            parts.append(f"\n\nDeveloper Instructions:\n{self.config.developer_instructions}")

        # Working directory context
        parts.append(f"\n\nWorking directory: {self.config.cwd}")

        return "\n".join(parts)

    async def _handle_tool_call(
        self,
        tool_call: ToolCall,
        turn: Turn,
    ) -> AsyncIterator[ThreadEvent]:
        """Handle a tool call from the model."""
        handler = self._tool_handlers.get(tool_call.name)
        if not handler:
            item = ThreadItem(
                id=str(uuid.uuid4()),
                details=ErrorItem(message=f"Unknown tool: {tool_call.name}"),
            )
            yield ItemCompletedEvent(item=item)
            return

        async for event in handler(tool_call, turn):
            yield event

    async def _handle_shell(
        self,
        tool_call: ToolCall,
        turn: Turn,
    ) -> AsyncIterator[ThreadEvent]:
        """Handle shell command execution."""
        command = tool_call.arguments.get("command", "")
        timeout_ms = tool_call.arguments.get("timeout_ms", 60000)

        item_id = str(uuid.uuid4())

        # Start item
        item = ThreadItem(
            id=item_id,
            details=CommandExecutionItem(
                command=command,
                aggregated_output="",
                status=CommandExecutionStatus.IN_PROGRESS,
            ),
        )
        yield ItemStartedEvent(item=item)

        try:
            # Execute command (simplified - real implementation uses PTY)
            process = await asyncio.create_subprocess_shell(
                command,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.STDOUT,
                cwd=self.config.cwd,
            )

            try:
                stdout, _ = await asyncio.wait_for(
                    process.communicate(),
                    timeout=timeout_ms / 1000,
                )
                output = stdout.decode() if stdout else ""
                exit_code = process.returncode or 0
                status = (
                    CommandExecutionStatus.COMPLETED
                    if exit_code == 0
                    else CommandExecutionStatus.FAILED
                )
            except asyncio.TimeoutError:
                process.kill()
                output = "Command timed out"
                exit_code = -1
                status = CommandExecutionStatus.FAILED

            # Complete item
            item = ThreadItem(
                id=item_id,
                details=CommandExecutionItem(
                    command=command,
                    aggregated_output=output,
                    status=status,
                    exit_code=exit_code,
                ),
            )
            turn.response_items.append(item)
            yield ItemCompletedEvent(item=item)

        except Exception as e:
            item = ThreadItem(
                id=item_id,
                details=CommandExecutionItem(
                    command=command,
                    aggregated_output=str(e),
                    status=CommandExecutionStatus.FAILED,
                    exit_code=-1,
                ),
            )
            turn.response_items.append(item)
            yield ItemCompletedEvent(item=item)

    async def _handle_apply_patch(
        self,
        tool_call: ToolCall,
        turn: Turn,
    ) -> AsyncIterator[ThreadEvent]:
        """Handle file patch application."""
        from codex_patch import PatchApplier, ApplyStatus

        patch_text = tool_call.arguments.get("patch", "")

        # Create item for tracking
        item_id = str(uuid.uuid4())

        try:
            # Apply the patch
            applier = PatchApplier(cwd=self.config.cwd)

            # Respect approval policy - dry_run first if needed
            needs_approval = self.config.approval_policy != "never"

            if needs_approval:
                # Dry run to validate
                dry_result = applier.apply(patch_text, dry_run=True)
                if not dry_result.success:
                    item = ThreadItem(
                        id=item_id,
                        details=ErrorItem(message=f"Patch validation failed: {dry_result.summary()}"),
                    )
                    yield ItemCompletedEvent(item=item)
                    return

            # Apply for real
            result = applier.apply(patch_text, dry_run=False)

            # Convert to FileChangeItem format
            changes: list[FileUpdateChange] = []
            for change in result.changes:
                if change.error:
                    continue
                kind_map = {
                    "add": PatchChangeKind.ADD,
                    "replace": PatchChangeKind.UPDATE,
                    "delete": PatchChangeKind.DELETE,
                    "modify": PatchChangeKind.UPDATE,
                    "rename": PatchChangeKind.UPDATE,
                }
                kind = kind_map.get(change.operation, PatchChangeKind.UPDATE)
                changes.append(FileUpdateChange(path=str(change.path), kind=kind))

            status = PatchApplyStatus.COMPLETED if result.success else PatchApplyStatus.FAILED

            item = ThreadItem(
                id=item_id,
                details=FileChangeItem(changes=changes, status=status),
            )
            turn.response_items.append(item)
            yield ItemCompletedEvent(item=item)

        except Exception as e:
            item = ThreadItem(
                id=item_id,
                details=ErrorItem(message=f"Patch error: {e}"),
            )
            turn.response_items.append(item)
            yield ItemCompletedEvent(item=item)
