"""MCP (Model Context Protocol) client implementation.

Connects to MCP servers defined in config and provides tool access.
"""

from __future__ import annotations

import asyncio
import json
import subprocess
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import Any

from codex_core.config import McpServerConfig


@dataclass(slots=True)
class McpTool:
    """An MCP tool definition."""

    name: str
    description: str | None = None
    input_schema: dict[str, Any] = field(default_factory=dict)

    def to_openai_format(self) -> dict[str, Any]:
        """Convert to OpenAI tool format."""
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


@dataclass
class McpServer:
    """Connection to an MCP server."""

    name: str
    config: McpServerConfig
    tools: list[McpTool] = field(default_factory=list)
    _process: subprocess.Popen[bytes] | None = None
    _request_id: int = 0

    async def connect(self) -> None:
        """Connect to the MCP server."""
        if self.config.transport != "stdio":
            raise ValueError(f"Unsupported transport: {self.config.transport}")

        if not self.config.command:
            raise ValueError("MCP server command not configured")

        # Build command
        cmd = [self.config.command, *self.config.args]

        # Start process
        self._process = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**dict(__import__("os").environ), **self.config.env},
        )

        # Initialize connection
        await self._initialize()

        # List tools
        await self._list_tools()

    async def disconnect(self) -> None:
        """Disconnect from the MCP server."""
        if self._process:
            self._process.terminate()
            try:
                self._process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._process.kill()
            self._process = None

    async def _send_request(self, method: str, params: dict[str, Any] | None = None) -> Any:
        """Send a JSON-RPC request and wait for response."""
        if not self._process or not self._process.stdin or not self._process.stdout:
            raise RuntimeError("MCP server not connected")

        self._request_id += 1
        request = {
            "jsonrpc": "2.0",
            "id": self._request_id,
            "method": method,
        }
        if params:
            request["params"] = params

        # Send request
        request_line = json.dumps(request) + "\n"
        self._process.stdin.write(request_line.encode())
        self._process.stdin.flush()

        # Read response (blocking - should be made async)
        response_line = await asyncio.to_thread(self._process.stdout.readline)
        if not response_line:
            raise RuntimeError("MCP server closed connection")

        response = json.loads(response_line.decode())

        if "error" in response:
            raise RuntimeError(f"MCP error: {response['error']}")

        return response.get("result")

    async def _initialize(self) -> None:
        """Initialize the MCP connection."""
        await self._send_request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "codex-py", "version": "0.1.0"},
            },
        )

        # Send initialized notification
        if self._process and self._process.stdin:
            notification = json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"})
            self._process.stdin.write((notification + "\n").encode())
            self._process.stdin.flush()

    async def _list_tools(self) -> None:
        """List available tools from the server."""
        result = await self._send_request("tools/list")
        tools = result.get("tools", [])

        self.tools = []
        for tool_data in tools:
            self.tools.append(
                McpTool(
                    name=tool_data["name"],
                    description=tool_data.get("description"),
                    input_schema=tool_data.get("inputSchema", {}),
                )
            )

    async def call_tool(self, name: str, arguments: dict[str, Any]) -> McpToolResult:
        """Call a tool on the server."""
        result = await self._send_request(
            "tools/call",
            {"name": name, "arguments": arguments},
        )

        return McpToolResult(
            content=result.get("content", []),
            is_error=result.get("isError", False),
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

    async def disconnect_all(self) -> None:
        """Disconnect from all servers."""
        for server in self._servers.values():
            await server.disconnect()
        self._servers.clear()

    def get_server(self, name: str) -> McpServer | None:
        """Get a connected server by name."""
        return self._servers.get(name)

    def list_servers(self) -> list[str]:
        """List connected server names."""
        return list(self._servers.keys())

    def get_all_tools(self) -> list[tuple[str, McpTool]]:
        """Get all tools from all connected servers."""
        tools: list[tuple[str, McpTool]] = []
        for name, server in self._servers.items():
            for tool in server.tools:
                tools.append((name, tool))
        return tools

    def get_tools_openai_format(self) -> list[dict[str, Any]]:
        """Get all tools in OpenAI format with server prefix."""
        tools: list[dict[str, Any]] = []
        for server_name, tool in self.get_all_tools():
            tool_dict = tool.to_openai_format()
            # Prefix tool name with server name
            tool_dict["function"]["name"] = f"mcp_{server_name}_{tool.name}"
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

    async def __aenter__(self) -> McpClient:
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self.disconnect_all()
