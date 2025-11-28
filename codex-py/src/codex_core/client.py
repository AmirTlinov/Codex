"""API client for chat completions with SSE streaming.

Supports OpenAI and Anthropic APIs with streaming responses.
"""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import Any

import httpx
from httpx_sse import aconnect_sse

from codex_core.config import Config, ModelFamily


@dataclass(slots=True)
class Message:
    """A chat message."""

    role: str
    content: str


@dataclass(slots=True)
class ToolCall:
    """A tool call from the model."""

    id: str
    name: str
    arguments: dict[str, Any]


@dataclass(slots=True)
class StreamChunk:
    """A chunk from the streaming response."""

    content: str | None = None
    tool_calls: list[ToolCall] = field(default_factory=list)
    finish_reason: str | None = None
    usage: dict[str, int] | None = None


@dataclass(slots=True)
class CompletionResponse:
    """Full completion response."""

    content: str
    tool_calls: list[ToolCall]
    finish_reason: str
    usage: dict[str, int]


class ModelClient:
    """Client for chat completion APIs."""

    def __init__(self, config: Config) -> None:
        self.config = config
        self._client: httpx.AsyncClient | None = None

    async def __aenter__(self) -> ModelClient:
        self._client = httpx.AsyncClient(timeout=httpx.Timeout(60.0, connect=10.0))
        return self

    async def __aexit__(self, *args: Any) -> None:
        if self._client:
            await self._client.aclose()

    def _get_headers(self) -> dict[str, str]:
        """Get headers for the API request."""
        api_key = self.config.get_api_key()
        if not api_key:
            raise ValueError("API key not configured")

        headers = {"Content-Type": "application/json"}

        # Different header formats for different providers
        if self.config.model_family == ModelFamily.ANTHROPIC:
            headers["x-api-key"] = api_key
            headers["anthropic-version"] = "2023-06-01"
        else:
            headers["Authorization"] = f"Bearer {api_key}"

        return headers

    def _build_request(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
        stream: bool = True,
    ) -> dict[str, Any]:
        """Build the request payload."""
        if self.config.model_family == ModelFamily.ANTHROPIC:
            return self._build_anthropic_request(messages, tools, stream)
        return self._build_openai_request(messages, tools, stream)

    def _build_openai_request(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
        stream: bool = True,
    ) -> dict[str, Any]:
        """Build OpenAI API request."""
        request: dict[str, Any] = {
            "model": self.config.model,
            "messages": [{"role": m.role, "content": m.content} for m in messages],
            "stream": stream,
        }

        if stream:
            request["stream_options"] = {"include_usage": True}

        if tools:
            request["tools"] = tools

        if self.config.model_max_output_tokens:
            request["max_tokens"] = self.config.model_max_output_tokens

        return request

    def _build_anthropic_request(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
        stream: bool = True,
    ) -> dict[str, Any]:
        """Build Anthropic API request."""
        # Extract system message if present
        system = None
        chat_messages = []
        for m in messages:
            if m.role == "system":
                system = m.content
            else:
                chat_messages.append({"role": m.role, "content": m.content})

        request: dict[str, Any] = {
            "model": self.config.model,
            "messages": chat_messages,
            "max_tokens": self.config.model_max_output_tokens or 4096,
            "stream": stream,
        }

        if system:
            request["system"] = system

        if tools:
            # Convert OpenAI tool format to Anthropic format
            anthropic_tools = []
            for tool in tools:
                if tool.get("type") == "function":
                    func = tool["function"]
                    anthropic_tools.append({
                        "name": func["name"],
                        "description": func.get("description", ""),
                        "input_schema": func.get("parameters", {"type": "object"}),
                    })
            if anthropic_tools:
                request["tools"] = anthropic_tools

        return request

    async def stream_completion(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
    ) -> AsyncIterator[StreamChunk]:
        """Stream chat completion response."""
        if not self._client:
            raise RuntimeError("Client not initialized. Use async context manager.")

        base_url = self.config.get_base_url()
        headers = self._get_headers()
        request_data = self._build_request(messages, tools, stream=True)

        # Determine endpoint
        if self.config.model_family == ModelFamily.ANTHROPIC:
            url = f"{base_url}/messages"
        else:
            url = f"{base_url}/chat/completions"

        async with aconnect_sse(
            self._client, "POST", url, headers=headers, json=request_data
        ) as event_source:
            async for event in event_source.aiter_sse():
                if event.data == "[DONE]":
                    break

                try:
                    data = json.loads(event.data)
                except json.JSONDecodeError:
                    continue

                chunk = self._parse_stream_chunk(data)
                if chunk:
                    yield chunk

    def _parse_stream_chunk(self, data: dict[str, Any]) -> StreamChunk | None:
        """Parse a stream chunk from the API response."""
        if self.config.model_family == ModelFamily.ANTHROPIC:
            return self._parse_anthropic_chunk(data)
        return self._parse_openai_chunk(data)

    def _parse_openai_chunk(self, data: dict[str, Any]) -> StreamChunk | None:
        """Parse OpenAI stream chunk."""
        choices = data.get("choices", [])
        if not choices:
            # Check for usage in final message
            if "usage" in data:
                return StreamChunk(usage=data["usage"])
            return None

        choice = choices[0]
        delta = choice.get("delta", {})

        chunk = StreamChunk(
            content=delta.get("content"),
            finish_reason=choice.get("finish_reason"),
        )

        # Parse tool calls
        if "tool_calls" in delta:
            for tc in delta["tool_calls"]:
                if tc.get("function"):
                    chunk.tool_calls.append(
                        ToolCall(
                            id=tc.get("id", ""),
                            name=tc["function"].get("name", ""),
                            arguments=tc["function"].get("arguments", {}),
                        )
                    )

        if "usage" in data:
            chunk.usage = data["usage"]

        return chunk

    def _parse_anthropic_chunk(self, data: dict[str, Any]) -> StreamChunk | None:
        """Parse Anthropic stream chunk."""
        event_type = data.get("type")

        if event_type == "content_block_delta":
            delta = data.get("delta", {})
            if delta.get("type") == "text_delta":
                return StreamChunk(content=delta.get("text"))
            elif delta.get("type") == "input_json_delta":
                # Tool call argument streaming
                return StreamChunk(content=delta.get("partial_json"))

        elif event_type == "message_delta":
            return StreamChunk(
                finish_reason=data.get("delta", {}).get("stop_reason"),
                usage=data.get("usage"),
            )

        elif event_type == "content_block_start":
            block = data.get("content_block", {})
            if block.get("type") == "tool_use":
                return StreamChunk(
                    tool_calls=[
                        ToolCall(
                            id=block.get("id", ""),
                            name=block.get("name", ""),
                            arguments={},
                        )
                    ]
                )

        return None

    async def complete(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
    ) -> CompletionResponse:
        """Get a complete (non-streaming) response."""
        content_parts: list[str] = []
        all_tool_calls: list[ToolCall] = []
        finish_reason = ""
        usage: dict[str, int] = {}

        async for chunk in self.stream_completion(messages, tools):
            if chunk.content:
                content_parts.append(chunk.content)
            if chunk.tool_calls:
                all_tool_calls.extend(chunk.tool_calls)
            if chunk.finish_reason:
                finish_reason = chunk.finish_reason
            if chunk.usage:
                usage = chunk.usage

        return CompletionResponse(
            content="".join(content_parts),
            tool_calls=all_tool_calls,
            finish_reason=finish_reason,
            usage=usage,
        )
