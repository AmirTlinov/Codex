"""MCP (Model Context Protocol) client implementation.

Provides async connection to MCP servers via stdio transport.
Implements JSON-RPC 2.0 protocol for tool discovery and execution.
"""

from __future__ import annotations

import asyncio
import json
import os
from dataclasses import dataclass, field
from typing import Any

from codex_core.config import McpServerConfig


class McpError(Exception):
    """MCP protocol error."""

    def __init__(self, code: int, message: str, data: Any = None) -> None:
        self.code = code
        self.message = message
        self.data = data
        super().__init__(f"MCP error {code}: {message}")


@dataclass(slots=True)
class McpTool:
    """An MCP tool definition."""

    name: str
    description: str | None = None
    input_schema: dict[str, Any] = field(default_factory=dict)

    def to_openai_format(self) -> dict[str, Any]:
        """Convert to OpenAI function calling format."""
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description or "",
                "parameters": self.input_schema,
            },
        }


@dataclass(slots=True)
class McpToolResult:
    """Result from an MCP tool call."""

    content: list[dict[str, Any]]
    is_error: bool = False

    def text(self) -> str:
        """Extract text content as string."""
        parts = []
        for item in self.content:
            if item.get("type") == "text":
                parts.append(item.get("text", ""))
        return "\n".join(parts)


@dataclass
class McpServer:
    """Async connection to an MCP server via stdio."""

    name: str
    config: McpServerConfig
    tools: list[McpTool] = field(default_factory=list)
    _process: asyncio.subprocess.Process | None = None
    _request_id: int = 0
    _reader_task: asyncio.Task[None] | None = None
    _pending: dict[int, asyncio.Future[Any]] = field(default_factory=dict)
    _notifications: asyncio.Queue[dict[str, Any]] = field(default_factory=asyncio.Queue)

    async def connect(self) -> None:
        """Connect to the MCP server."""
        if self.config.transport != "stdio":
            raise ValueError(f"Unsupported transport: {self.config.transport}")

        if not self.config.command:
            raise ValueError("MCP server command not configured")

        # Build environment
        env = {**os.environ, **self.config.env}

        # Start process with async subprocess
        self._process = await asyncio.create_subprocess_exec(
            self.config.command,
            *self.config.args,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=env,
        )

        # Start reader task
        self._reader_task = asyncio.create_task(self._read_loop())

        # Initialize connection
        await self._initialize()

        # Discover tools
        await self._list_tools()

    async def disconnect(self) -> None:
        """Disconnect from the MCP server."""
        if self._reader_task:
            self._reader_task.cancel()
            try:
                await self._reader_task
            except asyncio.CancelledError:
                pass
            self._reader_task = None

        if self._process:
            self._process.terminate()
            try:
                await asyncio.wait_for(self._process.wait(), timeout=5.0)
            except asyncio.TimeoutError:
                self._process.kill()
                await self._process.wait()
            self._process = None

        # Cancel pending requests
        for future in self._pending.values():
            if not future.done():
                future.cancel()
        self._pending.clear()

    async def _read_loop(self) -> None:
        """Read responses from server in background."""
        if not self._process or not self._process.stdout:
            return

        try:
            while True:
                line = await self._process.stdout.readline()
                if not line:
                    break

                try:
                    message = json.loads(line.decode())
                except json.JSONDecodeError:
                    continue

                # Check if response or notification
                if "id" in message:
                    request_id = message["id"]
                    if request_id in self._pending:
                        future = self._pending.pop(request_id)
                        if "error" in message:
                            err = message["error"]
                            future.set_exception(
                                McpError(err.get("code", -1), err.get("message", "Unknown error"), err.get("data"))
                            )
                        else:
                            future.set_result(message.get("result"))
                else:
                    # Notification
                    await self._notifications.put(message)

        except asyncio.CancelledError:
            pass

    async def _send_request(self, method: str, params: dict[str, Any] | None = None) -> Any:
        """Send JSON-RPC request and await response."""
        if not self._process or not self._process.stdin:
            raise RuntimeError("MCP server not connected")

        self._request_id += 1
        request_id = self._request_id

        request: dict[str, Any] = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
        }
        if params is not None:
            request["params"] = params

        # Create future for response
        future: asyncio.Future[Any] = asyncio.get_event_loop().create_future()
        self._pending[request_id] = future

        # Send request
        request_line = json.dumps(request) + "\n"
        self._process.stdin.write(request_line.encode())
        await self._process.stdin.drain()

        # Wait for response with timeout
        try:
            return await asyncio.wait_for(future, timeout=30.0)
        except asyncio.TimeoutError:
            self._pending.pop(request_id, None)
            raise McpError(-1, f"Request {method} timed out")

    def _send_notification(self, method: str, params: dict[str, Any] | None = None) -> None:
        """Send JSON-RPC notification (no response expected)."""
        if not self._process or not self._process.stdin:
            return

        notification: dict[str, Any] = {
            "jsonrpc": "2.0",
            "method": method,
        }
        if params is not None:
            notification["params"] = params

        notification_line = json.dumps(notification) + "\n"
        self._process.stdin.write(notification_line.encode())
        # Note: drain() not awaited for notifications - fire and forget

    async def _initialize(self) -> None:
        """Initialize MCP connection handshake."""
        result = await self._send_request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {},
                },
                "clientInfo": {
                    "name": "codex-py",
                    "version": "0.1.0",
                },
            },
        )

        # Send initialized notification
        self._send_notification("notifications/initialized")

        return result

    async def _list_tools(self) -> None:
        """Discover available tools from server."""
        result = await self._send_request("tools/list")
        tools_data = result.get("tools", []) if result else []

        self.tools = []
        for tool_data in tools_data:
            self.tools.append(
                McpTool(
                    name=tool_data["name"],
                    description=tool_data.get("description"),
                    input_schema=tool_data.get("inputSchema", {}),
                )
            )

    async def call_tool(self, name: str, arguments: dict[str, Any]) -> McpToolResult:
        """Execute a tool on the server."""
        try:
            result = await self._send_request(
                "tools/call",
                {"name": name, "arguments": arguments},
            )
            return McpToolResult(
                content=result.get("content", []) if result else [],
                is_error=result.get("isError", False) if result else False,
            )
        except McpError as e:
            return McpToolResult(
                content=[{"type": "text", "text": str(e)}],
                is_error=True,
            )


class McpClient:
    """Client for managing multiple MCP server connections."""

    def __init__(self) -> None:
        self._servers: dict[str, McpServer] = {}

    async def connect_server(self, name: str, config: McpServerConfig) -> McpServer:
        """Connect to an MCP server."""
        if not config.enabled:
            raise ValueError(f"MCP server {name} is disabled")

        server = McpServer(name=name, config=config)
        await server.connect()
        self._servers[name] = server
        return server

    async def connect_all(self, configs: dict[str, McpServerConfig]) -> list[str]:
        """Connect to all enabled MCP servers. Returns list of connected server names."""
        connected = []
        for name, config in configs.items():
            if config.enabled:
                try:
                    await self.connect_server(name, config)
                    connected.append(name)
                except Exception:
                    # Log but continue with other servers
                    pass
        return connected

    async def disconnect_all(self) -> None:
        """Disconnect from all servers."""
        tasks = [server.disconnect() for server in self._servers.values()]
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        self._servers.clear()

    def get_server(self, name: str) -> McpServer | None:
        """Get a connected server by name."""
        return self._servers.get(name)

    def list_servers(self) -> list[str]:
        """List connected server names."""
        return list(self._servers.keys())

    def get_all_tools(self) -> list[tuple[str, McpTool]]:
        """Get all tools from all connected servers as (server_name, tool) tuples."""
        tools: list[tuple[str, McpTool]] = []
        for name, server in self._servers.items():
            for tool in server.tools:
                tools.append((name, tool))
        return tools

    def get_tools_openai_format(self) -> list[dict[str, Any]]:
        """Get all tools in OpenAI format with mcp__ prefix."""
        tools: list[dict[str, Any]] = []
        for server_name, tool in self.get_all_tools():
            tool_dict = tool.to_openai_format()
            # Use standard MCP tool naming: mcp__<server>__<tool>
            tool_dict["function"]["name"] = f"mcp__{server_name}__{tool.name}"
            tools.append(tool_dict)
        return tools

    async def call_tool(
        self,
        server_name: str,
        tool_name: str,
        arguments: dict[str, Any],
    ) -> McpToolResult:
        """Call a tool on a specific server."""
        server = self._servers.get(server_name)
        if not server:
            return McpToolResult(
                content=[{"type": "text", "text": f"Server not found: {server_name}"}],
                is_error=True,
            )
        return await server.call_tool(tool_name, arguments)

    def parse_tool_name(self, full_name: str) -> tuple[str, str] | None:
        """Parse mcp__server__tool format. Returns (server, tool) or None."""
        if not full_name.startswith("mcp__"):
            return None
        parts = full_name[5:].split("__", 1)
        if len(parts) != 2:
            return None
        return (parts[0], parts[1])

    async def __aenter__(self) -> McpClient:
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self.disconnect_all()
