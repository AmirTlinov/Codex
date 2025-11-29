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
from typing import Any

from codex_core.approval import ApprovalHandler, ApprovalManager
from codex_core.client import Message, ModelClient, StreamChunk, ToolCall
from codex_core.config import Config
from codex_core.history_compactor import CompactionConfig, HistoryCompactor
from codex_core.mcp_client import McpClient
from codex_core.session import Session, Turn
from codex_core.token_counter import TokenCounter
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
class ToolResult:
    """Result from executing a tool call.

    Contains both the original call info and the execution result.
    This is needed because Responses API expects the tool call item
    to be included in input along with the output.
    """

    call_id: str
    output: str
    success: bool = True
    # Tool call details for building API request
    tool_type: str = "local_shell"  # "local_shell", "function", "custom"
    tool_name: str | None = None  # For function calls
    command: list[str] | None = None  # For local_shell calls


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
    approval_manager: ApprovalManager = field(default_factory=ApprovalManager)
    _client: ModelClient | None = None
    _mcp_client: McpClient | None = None
    _token_counter: TokenCounter | None = None
    _history_compactor: HistoryCompactor | None = None
    _event_queue: asyncio.Queue[ThreadEvent] = field(default_factory=asyncio.Queue)
    _tools: list[ToolDefinition] = field(default_factory=list)
    _tool_handlers: dict[str, Any] = field(default_factory=dict)

    @classmethod
    async def create(
        cls,
        config: Config,
        thread_id: str | None = None,
        approval_handler: ApprovalHandler | None = None,
    ) -> Codex:
        """Create a new Codex instance.

        Args:
            config: Configuration settings
            thread_id: Optional thread ID to resume session
            approval_handler: Optional custom approval handler for interactive mode
        """
        # Load or create session
        if thread_id:
            session = Session.load(thread_id)
            if not session:
                raise ValueError(f"Session not found: {thread_id}")
        else:
            session = Session.new(model=config.model, cwd=config.cwd)

        # Create approval manager from config
        approval_manager = ApprovalManager.from_policy_string(
            config.approval_policy,
            handler=approval_handler,
        )

        codex = cls(
            config=config,
            session=session,
            approval_manager=approval_manager,
        )

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

        # Initialize token counter and history compactor
        self._token_counter = TokenCounter(self.config.model)
        self._history_compactor = HistoryCompactor(
            client=self._client,
            token_counter=self._token_counter,
            config=CompactionConfig(),
        )

        # Connect to MCP servers if configured
        if self.config.mcp_servers:
            self._mcp_client = McpClient()
            await self._mcp_client.connect_all(self.config.mcp_servers)

        return self

    async def __aexit__(self, *args: Any) -> None:
        if self._mcp_client:
            await self._mcp_client.disconnect_all()
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
        """Run a conversation turn with agentic loop.

        This implements the multi-step agentic pattern from codex-rs:
        1. Send user input to model
        2. Process response and execute any tool calls
        3. If there are tool results, send them back to model
        4. Repeat until model responds without tool calls
        """
        if not self._client:
            raise RuntimeError("Codex not initialized. Use async context manager.")

        # Emit thread started if this is the first turn
        if not self.session.turns:
            yield ThreadStartedEvent(thread_id=self.session.thread_id)

        # Start the turn
        turn = self.session.new_turn(user_input)
        yield TurnStartedEvent()

        try:
            # Build initial messages
            messages = self._build_messages(user_input)
            tools = self._get_all_tools()

            # Auto-compact history if approaching context limit
            if self._history_compactor and self._history_compactor.should_compact(messages):
                messages = await self._history_compactor.compact(messages)

            # Collect ALL tool results across agentic loop iterations
            # Each iteration adds to this list (matching codex-rs history accumulation)
            all_tool_history: list[ToolResult] = []
            total_usage = Usage()
            # Track processed call IDs to avoid re-executing the same tool call
            processed_call_ids: set[str] = set()

            # Agentic loop: continue until no tool calls
            while True:
                # Track response for this iteration
                response_text = ""
                current_item_id: str | None = None
                iteration_tool_results: list[ToolResult] = []

                # Stream completion (either initial or with accumulated tool history)
                if all_tool_history:
                    # Continue with ALL accumulated tool results (not just new ones)
                    async for chunk in self._client.stream_completion_with_tool_results(
                        messages, tools, all_tool_history
                    ):
                        async for event in self._process_chunk(
                            chunk, turn, response_text, current_item_id,
                            iteration_tool_results, processed_call_ids
                        ):
                            yield event
                            # Update tracking from yielded events
                            if isinstance(event, (ItemStartedEvent, ItemUpdatedEvent)):
                                if isinstance(event.item.details, AgentMessageItem):
                                    response_text = event.item.details.text
                                    current_item_id = event.item.id

                        # Update usage
                        if chunk.usage:
                            total_usage = Usage(
                                input_tokens=total_usage.input_tokens
                                + chunk.usage.get("prompt_tokens", 0),
                                cached_input_tokens=total_usage.cached_input_tokens
                                + chunk.usage.get("cached_tokens", 0),
                                output_tokens=total_usage.output_tokens
                                + chunk.usage.get("completion_tokens", 0),
                            )
                else:
                    # Initial request
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
                                async for event, result in self._handle_tool_call_with_result(
                                    tool_call, turn, processed_call_ids
                                ):
                                    yield event
                                    if result:
                                        iteration_tool_results.append(result)

                        # Handle usage
                        if chunk.usage:
                            total_usage = Usage(
                                input_tokens=chunk.usage.get("prompt_tokens", 0),
                                cached_input_tokens=chunk.usage.get("cached_tokens", 0),
                                output_tokens=chunk.usage.get("completion_tokens", 0),
                            )

                # Complete the agent message item if we have one
                if current_item_id and response_text:
                    item = ThreadItem(
                        id=current_item_id,
                        details=AgentMessageItem(text=response_text),
                    )
                    turn.response_items.append(item)
                    yield ItemCompletedEvent(item=item)

                # Check if we should continue the loop
                if not iteration_tool_results:
                    # No tool calls, we're done
                    break

                # Add new results to accumulated history (not replace!)
                # This ensures next iteration has FULL history
                all_tool_history.extend(iteration_tool_results)

            # Complete the turn
            self.session.complete_turn(turn, total_usage)
            yield TurnCompletedEvent(usage=total_usage)

        except Exception as e:
            error_msg = str(e)
            self.session.fail_turn(turn, error_msg)
            yield TurnFailedEvent(error=ThreadErrorEvent(message=error_msg))

    async def _process_chunk(
        self,
        chunk: StreamChunk,
        turn: Turn,
        response_text: str,
        current_item_id: str | None,
        tool_results: list[ToolResult],
        processed_call_ids: set[str],
    ) -> AsyncIterator[ThreadEvent]:
        """Process a stream chunk and yield events."""
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
                async for event, result in self._handle_tool_call_with_result(
                    tool_call, turn, processed_call_ids
                ):
                    yield event
                    if result:
                        tool_results.append(result)

    async def _handle_tool_call_with_result(
        self,
        tool_call: ToolCall,
        turn: Turn,
        processed_call_ids: set[str] | None = None,
    ) -> AsyncIterator[tuple[ThreadEvent, ToolResult | None]]:
        """Handle a tool call and return result for agentic loop.

        Args:
            tool_call: The tool call to handle
            turn: Current turn for tracking
            processed_call_ids: Set of already-executed call IDs to skip duplicates
        """
        # Skip already-processed tool calls
        if processed_call_ids is not None and tool_call.id in processed_call_ids:
            return

        # Mark as processed before execution
        if processed_call_ids is not None:
            processed_call_ids.add(tool_call.id)

        # Handle local_shell (Responses API built-in tool)
        if tool_call.name == "local_shell":
            async for event, result in self._handle_local_shell_with_result(tool_call, turn):
                yield event, result
            return

        # Check if this is an MCP tool call
        if self._mcp_client and tool_call.name.startswith("mcp__"):
            async for event in self._handle_mcp_tool_call(tool_call, turn):
                yield event, None
            return

        # Built-in tool handler (shell, apply_patch)
        handler = self._tool_handlers.get(tool_call.name)
        if not handler:
            item = ThreadItem(
                id=str(uuid.uuid4()),
                details=ErrorItem(message=f"Unknown tool: {tool_call.name}"),
            )
            yield ItemCompletedEvent(item=item), ToolResult(
                call_id=tool_call.id,
                output=f"Unknown tool: {tool_call.name}",
                success=False,
            )
            return

        # Execute handler and collect result
        async for event in handler(tool_call, turn):
            # Extract result from completed command execution
            if isinstance(event, ItemCompletedEvent):
                if isinstance(event.item.details, CommandExecutionItem):
                    result = ToolResult(
                        call_id=tool_call.id,
                        output=event.item.details.aggregated_output or "",
                        success=event.item.details.exit_code == 0,
                    )
                    yield event, result
                else:
                    yield event, None
            else:
                yield event, None

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
        """Build the system prompt with instructions.

        Uses the official Codex CLI prompt from prompt.md (same as codex-rs).
        """
        parts = []

        # Base instructions - load from prompt.md (same as codex-rs)
        if self.config.base_instructions:
            parts.append(self.config.base_instructions)
        else:
            parts.append(self._get_base_instructions())

        # User instructions from AGENTS.md
        if self.config.user_instructions:
            parts.append(f"\n\nUser Instructions:\n{self.config.user_instructions}")

        # Developer instructions
        if self.config.developer_instructions:
            parts.append(f"\n\nDeveloper Instructions:\n{self.config.developer_instructions}")

        # NOTE: Working directory is communicated via environment_context XML
        # in the input items (not in the system prompt). This matches codex-rs.

        return "\n".join(parts)

    def _get_base_instructions(self) -> str:
        """Load base instructions from prompt.md (matches codex-rs)."""
        import importlib.resources as resources

        try:
            # Try to load from package resources
            prompt_file = resources.files("codex_core").joinpath("prompt.md")
            return prompt_file.read_text(encoding="utf-8")
        except (FileNotFoundError, TypeError):
            # Fallback to minimal prompt
            return (
                "You are Codex, an AI coding assistant. "
                "You help users with software development tasks by executing commands "
                "and modifying files. Be concise and helpful."
            )

    def _get_all_tools(self) -> list[dict[str, Any]] | None:
        """Get all tools in OpenAI format: built-in + MCP.

        NOTE: For ChatGPT OAuth API (Responses API), we only send built-in tools.
        MCP tools are handled locally but not sent to the API to avoid
        exceeding tool limits or format issues.
        """
        tools: list[dict[str, Any]] = []

        # Built-in tools
        for tool in self._tools:
            tools.append(tool.to_openai_format())

        # MCP tools - only add for non-ChatGPT APIs
        # ChatGPT Responses API has issues with large number of tools
        if self._mcp_client and not self._is_chatgpt_api():
            tools.extend(self._mcp_client.get_tools_openai_format())

        return tools if tools else None

    def _is_chatgpt_api(self) -> bool:
        """Check if using ChatGPT OAuth API."""
        base_url = self.config.get_base_url()
        return "chatgpt.com" in base_url

    async def _handle_tool_call(
        self,
        tool_call: ToolCall,
        turn: Turn,
    ) -> AsyncIterator[ThreadEvent]:
        """Handle a tool call from the model."""
        # Check if this is an MCP tool call
        if self._mcp_client and tool_call.name.startswith("mcp__"):
            async for event in self._handle_mcp_tool_call(tool_call, turn):
                yield event
            return

        # Handle local_shell (Responses API built-in tool)
        if tool_call.name == "local_shell":
            async for event in self._handle_local_shell(tool_call, turn):
                yield event
            return

        # Built-in tool handler
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

    async def _handle_mcp_tool_call(
        self,
        tool_call: ToolCall,
        turn: Turn,
    ) -> AsyncIterator[ThreadEvent]:
        """Handle an MCP tool call."""
        from codex_protocol.items import (
            McpToolCallItem,
            McpToolCallItemError,
            McpToolCallItemResult,
            McpToolCallStatus,
        )

        if not self._mcp_client:
            return

        # Parse tool name: mcp__server__tool
        parsed = self._mcp_client.parse_tool_name(tool_call.name)
        if not parsed:
            item = ThreadItem(
                id=str(uuid.uuid4()),
                details=ErrorItem(message=f"Invalid MCP tool name: {tool_call.name}"),
            )
            yield ItemCompletedEvent(item=item)
            return

        server_name, tool_name = parsed
        item_id = str(uuid.uuid4())

        # Emit started event
        item = ThreadItem(
            id=item_id,
            details=McpToolCallItem(
                server=server_name,
                tool=tool_name,
                arguments=tool_call.arguments,
                status=McpToolCallStatus.IN_PROGRESS,
            ),
        )
        yield ItemStartedEvent(item=item)

        # Execute the tool
        result = await self._mcp_client.call_tool(server_name, tool_name, tool_call.arguments)

        # Emit completed event
        if result.is_error:
            status = McpToolCallStatus.FAILED
            mcp_result = None
            mcp_error = McpToolCallItemError(message=result.text())
        else:
            status = McpToolCallStatus.COMPLETED
            mcp_result = McpToolCallItemResult(content=result.content)
            mcp_error = None

        item = ThreadItem(
            id=item_id,
            details=McpToolCallItem(
                server=server_name,
                tool=tool_name,
                arguments=tool_call.arguments,
                status=status,
                result=mcp_result,
                error=mcp_error,
            ),
        )
        turn.response_items.append(item)
        yield ItemCompletedEvent(item=item)

    async def _handle_local_shell_with_result(
        self,
        tool_call: ToolCall,
        turn: Turn,
    ) -> AsyncIterator[tuple[ThreadEvent, ToolResult | None]]:
        """Handle local_shell calls and return result for agentic loop.

        Responses API returns local_shell_call with action.command as array.
        Format: {"command": ["bash", "-lc", "echo hello"]}

        IMPORTANT: We use subprocess_exec (not shell) to properly pass command array.
        This matches codex-rs spawn behavior where args are passed directly.
        """
        # local_shell uses command array format from Responses API
        command_parts = tool_call.arguments.get("command", [])
        if isinstance(command_parts, list):
            command_list = command_parts
        else:
            command_list = [str(command_parts)]

        # Display command for UI (join for display purposes only)
        display_command = " ".join(command_list)

        item_id = str(uuid.uuid4())

        # Check approval using display command
        approved = await self.approval_manager.approve_command(display_command)
        if not approved:
            item = ThreadItem(
                id=item_id,
                details=CommandExecutionItem(
                    command=display_command,
                    aggregated_output="Command rejected by user",
                    status=CommandExecutionStatus.REJECTED,
                    exit_code=-1,
                ),
            )
            turn.response_items.append(item)
            yield ItemCompletedEvent(item=item), ToolResult(
                call_id=tool_call.id,
                output="Command rejected by user",
                success=False,
                tool_type="local_shell",
                command=command_list,
            )
            return

        # Start item
        item = ThreadItem(
            id=item_id,
            details=CommandExecutionItem(
                command=display_command,
                aggregated_output="",
                status=CommandExecutionStatus.IN_PROGRESS,
            ),
        )
        yield ItemStartedEvent(item=item), None

        try:
            # Execute command using exec (not shell!) to properly pass args
            # This matches codex-rs spawn_child_async behavior
            if command_list:
                program = command_list[0]
                args = command_list[1:] if len(command_list) > 1 else []

                process = await asyncio.create_subprocess_exec(
                    program,
                    *args,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.STDOUT,
                    cwd=self.config.cwd,
                )

                try:
                    stdout, _ = await asyncio.wait_for(
                        process.communicate(),
                        timeout=60.0,  # 60 second timeout
                    )
                    output = stdout.decode() if stdout else ""
                    exit_code = process.returncode or 0
                    status = (
                        CommandExecutionStatus.COMPLETED
                        if exit_code == 0
                        else CommandExecutionStatus.FAILED
                    )
                except TimeoutError:
                    process.kill()
                    output = "Command timed out"
                    exit_code = -1
                    status = CommandExecutionStatus.FAILED
            else:
                output = "Empty command"
                exit_code = -1
                status = CommandExecutionStatus.FAILED

            # Complete item
            item = ThreadItem(
                id=item_id,
                details=CommandExecutionItem(
                    command=display_command,
                    aggregated_output=output,
                    status=status,
                    exit_code=exit_code,
                ),
            )
            turn.response_items.append(item)

            result = ToolResult(
                call_id=tool_call.id,
                output=output,
                success=exit_code == 0,
                tool_type="local_shell",
                command=command_list,
            )
            yield ItemCompletedEvent(item=item), result

        except Exception as e:
            item = ThreadItem(
                id=item_id,
                details=CommandExecutionItem(
                    command=display_command,
                    aggregated_output=str(e),
                    status=CommandExecutionStatus.FAILED,
                    exit_code=-1,
                ),
            )
            turn.response_items.append(item)
            yield ItemCompletedEvent(item=item), ToolResult(
                call_id=tool_call.id,
                output=str(e),
                success=False,
                tool_type="local_shell",
                command=command_list,
            )

    async def _handle_shell(
        self,
        tool_call: ToolCall,
        turn: Turn,
    ) -> AsyncIterator[ThreadEvent]:
        """Handle shell command execution with approval."""
        command = tool_call.arguments.get("command", "")
        timeout_ms = tool_call.arguments.get("timeout_ms", 60000)

        item_id = str(uuid.uuid4())

        # Check approval
        approved = await self.approval_manager.approve_command(command)
        if not approved:
            item = ThreadItem(
                id=item_id,
                details=CommandExecutionItem(
                    command=command,
                    aggregated_output="Command rejected by user",
                    status=CommandExecutionStatus.REJECTED,
                    exit_code=-1,
                ),
            )
            turn.response_items.append(item)
            yield ItemCompletedEvent(item=item)
            return

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
            # Execute command
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
            except TimeoutError:
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
        """Handle file patch application with approval."""
        from codex_patch import PatchApplier

        patch_text = tool_call.arguments.get("patch", "")

        # Create item for tracking
        item_id = str(uuid.uuid4())

        try:
            # Apply the patch in dry-run mode first to validate
            applier = PatchApplier(cwd=self.config.cwd)
            dry_result = applier.apply(patch_text, dry_run=True)

            if not dry_result.success:
                item = ThreadItem(
                    id=item_id,
                    details=ErrorItem(message=f"Patch validation failed: {dry_result.summary()}"),
                )
                yield ItemCompletedEvent(item=item)
                return

            # Request approval for each changed file
            for change in dry_result.changes:
                if change.error:
                    continue

                # Generate diff for approval display
                diff_text = ""
                if change.old_content and change.new_content:
                    import difflib
                    diff_lines = difflib.unified_diff(
                        change.old_content.splitlines(keepends=True),
                        change.new_content.splitlines(keepends=True),
                        fromfile=f"a/{change.path}",
                        tofile=f"b/{change.path}",
                    )
                    diff_text = "".join(diff_lines)
                elif change.new_content:
                    diff_text = f"+++ {change.path}\n" + "\n".join(
                        f"+{line}" for line in change.new_content.splitlines()
                    )

                approved = await self.approval_manager.approve_patch(
                    path=change.path,
                    diff=diff_text,
                    description=f"{change.operation}: {change.path}",
                )
                if not approved:
                    item = ThreadItem(
                        id=item_id,
                        details=ErrorItem(message=f"Patch rejected by user: {change.path}"),
                    )
                    turn.response_items.append(item)
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
