"""API client for chat completions and Responses API with SSE streaming.

Supports OpenAI Chat Completions, OpenAI Responses API (ChatGPT), and Anthropic.
Includes retry logic with exponential backoff for rate limits and transient errors.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any

import httpx
from httpx_sse import aconnect_sse

logger = logging.getLogger(__name__)

from codex_core.config import Config, ModelFamily
from codex_core.models import ResponseItem, ResponsesApiRequest, ToolSpec

# Load GPT-5 Codex prompt (matches codex-rs gpt_5_codex_prompt.md)
_PROMPT_PATH = Path(__file__).parent / "gpt_5_codex_prompt.md"
GPT_5_CODEX_INSTRUCTIONS = _PROMPT_PATH.read_text(encoding="utf-8") if _PROMPT_PATH.exists() else ""


class WireApi(str, Enum):
    """API wire format to use."""

    CHAT = "chat"  # OpenAI Chat Completions API
    RESPONSES = "responses"  # OpenAI Responses API (ChatGPT backend)


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


@dataclass(slots=True)
class RetryConfig:
    """Configuration for retry logic with exponential backoff."""

    max_retries: int = 3
    base_delay: float = 0.5  # seconds
    max_delay: float = 30.0  # seconds

    def calculate_delay(self, attempt: int, retry_after: float | None = None) -> float:
        """Calculate delay for the given attempt.

        If retry_after is provided (from API response), use it.
        Otherwise, use exponential backoff: base * 2^attempt.
        """
        if retry_after is not None:
            return min(retry_after, self.max_delay)
        delay = self.base_delay * (2**attempt)
        return min(delay, self.max_delay)


@dataclass(slots=True)
class RateLimitSnapshot:
    """Snapshot of rate limit state from API response headers."""

    requests_remaining: int | None = None
    tokens_remaining: int | None = None
    requests_limit: int | None = None
    tokens_limit: int | None = None
    reset_requests_ms: int | None = None
    reset_tokens_ms: int | None = None

    @classmethod
    def from_headers(cls, headers: httpx.Headers) -> RateLimitSnapshot:
        """Parse rate limit info from response headers."""

        def _parse_int(val: str | None) -> int | None:
            if val is None:
                return None
            try:
                return int(val)
            except ValueError:
                return None

        def _parse_reset_ms(val: str | None) -> int | None:
            """Parse reset time like '1m30s' or '500ms' to milliseconds."""
            if val is None:
                return None
            # Try parsing as plain seconds
            try:
                return int(float(val) * 1000)
            except ValueError:
                pass
            # Try parsing duration format like "1m30s" or "500ms"
            total_ms = 0
            parts = re.findall(r"(\d+(?:\.\d+)?)(ms|s|m|h)", val)
            for num, unit in parts:
                n = float(num)
                if unit == "ms":
                    total_ms += int(n)
                elif unit == "s":
                    total_ms += int(n * 1000)
                elif unit == "m":
                    total_ms += int(n * 60 * 1000)
                elif unit == "h":
                    total_ms += int(n * 60 * 60 * 1000)
            return total_ms if total_ms > 0 else None

        return cls(
            requests_remaining=_parse_int(
                headers.get("x-ratelimit-remaining-requests")
            ),
            tokens_remaining=_parse_int(headers.get("x-ratelimit-remaining-tokens")),
            requests_limit=_parse_int(headers.get("x-ratelimit-limit-requests")),
            tokens_limit=_parse_int(headers.get("x-ratelimit-limit-tokens")),
            reset_requests_ms=_parse_reset_ms(
                headers.get("x-ratelimit-reset-requests")
            ),
            reset_tokens_ms=_parse_reset_ms(headers.get("x-ratelimit-reset-tokens")),
        )

    @property
    def is_low(self) -> bool:
        """Check if we're running low on rate limit quota."""
        if self.tokens_remaining is not None and self.tokens_remaining < 1000:
            return True
        if self.requests_remaining is not None and self.requests_remaining < 5:
            return True
        return False


def _parse_retry_after(response: httpx.Response) -> float | None:
    """Parse retry delay from response.

    Checks Retry-After header and error message body for delay hints.
    Returns delay in seconds or None.
    """
    # Check Retry-After header
    retry_after = response.headers.get("retry-after")
    if retry_after:
        try:
            return float(retry_after)
        except ValueError:
            pass

    # Check error body for "try again in X.XXXs" pattern (ChatGPT API style)
    try:
        body = response.text
        match = re.search(r"try again in (\d+(?:\.\d+)?)\s*s", body, re.IGNORECASE)
        if match:
            return float(match.group(1))
    except Exception:
        pass

    return None


def _is_retryable_error(error: Exception) -> bool:
    """Check if error is retryable (transient)."""
    if isinstance(error, httpx.HTTPStatusError):
        status = error.response.status_code
        # Retry on rate limit (429) and server errors (5xx)
        return status == 429 or 500 <= status < 600
    if isinstance(error, (httpx.ReadTimeout, httpx.ConnectTimeout, httpx.ConnectError)):
        return True
    return False


class ModelClient:
    """Client for chat completion APIs."""

    def __init__(
        self,
        config: Config,
        retry_config: RetryConfig | None = None,
    ) -> None:
        self.config = config
        self.retry_config = retry_config or RetryConfig()
        self._client: httpx.AsyncClient | None = None
        self._last_rate_limit: RateLimitSnapshot | None = None

    async def __aenter__(self) -> ModelClient:
        self._client = httpx.AsyncClient(timeout=httpx.Timeout(60.0, connect=10.0))
        return self

    async def __aexit__(self, *args: Any) -> None:
        if self._client:
            await self._client.aclose()

    def _get_wire_api(self) -> WireApi:
        """Determine which API wire format to use."""
        # ChatGPT OAuth uses Responses API
        if self.config.auth and self.config.auth.is_chatgpt_auth():
            return WireApi.RESPONSES
        return WireApi.CHAT

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

        # Required for ChatGPT backend API (matches codex-rs)
        if self.config.auth and self.config.auth.is_chatgpt_auth():
            headers["originator"] = "codex_cli_rs"
            headers["User-Agent"] = "codex_cli_rs/1.0"

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

        wire_api = self._get_wire_api()
        if wire_api == WireApi.RESPONSES:
            return self._build_responses_request(messages, tools, stream)
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

    def _build_responses_request(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
        stream: bool = True,
    ) -> dict[str, Any]:
        """Build OpenAI Responses API request (ChatGPT backend).

        Responses API uses a different format than Chat Completions:
        - `instructions` instead of system message
        - `input` array of ResponseItem objects instead of `messages`

        This matches codex-rs ResponsesApiRequest format exactly.
        """
        # Use GPT-5 Codex instructions (matches codex-rs gpt_5_codex_prompt.md)
        # ChatGPT backend requires these specific instructions
        instructions = GPT_5_CODEX_INSTRUCTIONS
        input_items: list[ResponseItem] = []

        # Add environment_context as first input item (matches codex-rs)
        # This tells the model about the working directory
        env_context = self._build_environment_context()
        input_items.append(ResponseItem.user_message(env_context))

        for m in messages:
            # Skip system messages - we use GPT_5_CODEX_INSTRUCTIONS instead
            if m.role == "system":
                continue
            elif m.role == "user":
                input_items.append(ResponseItem.user_message(m.content))
            elif m.role == "assistant":
                input_items.append(ResponseItem.assistant_message(m.content))

        # Convert tools to ToolSpec format
        tool_specs: list[ToolSpec] = []
        if tools:
            for tool in tools:
                tool_type = tool.get("type")
                if tool_type == "function":
                    func = tool["function"]
                    # Special handling: convert "shell" function to local_shell built-in
                    if func["name"] == "shell":
                        tool_specs.append(ToolSpec.local_shell())
                    else:
                        tool_specs.append(ToolSpec.function(
                            name=func["name"],
                            description=func.get("description", ""),
                            parameters=func.get("parameters", {"type": "object", "properties": {}}),
                            strict=func.get("strict", False),
                        ))
                elif tool_type == "local_shell":
                    tool_specs.append(ToolSpec.local_shell())
                elif tool_type == "web_search":
                    tool_specs.append(ToolSpec.web_search())

        # Build request using ResponsesApiRequest model (matches codex-rs exactly)
        # codex-rs always uses tool_choice="auto" regardless of whether tools are present
        request = ResponsesApiRequest(
            model=self.config.model,
            instructions=instructions,
            input=input_items,
            tools=tool_specs,
            tool_choice="auto",  # Always "auto" like codex-rs
            parallel_tool_calls=True,
            store=False,
            stream=stream,
            include=[],
            prompt_cache_key=None,
        )

        return request.to_dict()

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

    def _build_environment_context(self) -> str:
        """Build environment context XML (matches codex-rs EnvironmentContext).

        This tells the model about the working directory and policies.
        Format matches codex-rs exactly:
        <environment_context>
          <cwd>/path/to/working/directory</cwd>
          <approval_policy>...</approval_policy>
          <sandbox_mode>...</sandbox_mode>
        </environment_context>
        """
        lines = ["<environment_context>"]
        lines.append(f"  <cwd>{self.config.cwd}</cwd>")
        lines.append(f"  <approval_policy>{self.config.approval_policy}</approval_policy>")
        lines.append(f"  <sandbox_mode>{self.config.sandbox_policy}</sandbox_mode>")
        lines.append("</environment_context>")
        return "\n".join(lines)

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
        wire_api = self._get_wire_api()

        # Determine endpoint
        if self.config.model_family == ModelFamily.ANTHROPIC:
            url = f"{base_url}/messages"
        elif wire_api == WireApi.RESPONSES:
            url = f"{base_url}/responses"
        else:
            url = f"{base_url}/chat/completions"

        # ChatGPT backend API doesn't set Content-Type: text/event-stream
        # so we parse SSE manually instead of using httpx_sse
        if wire_api == WireApi.RESPONSES:
            async for chunk in self._stream_responses_api(url, headers, request_data):
                yield chunk
        else:
            async for chunk in self._stream_chat_api_with_retry(
                url, headers, request_data, wire_api
            ):
                yield chunk

    async def stream_completion_with_tool_results(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None,
        tool_results: list[Any],  # list[ToolResult] from codex.py
    ) -> AsyncIterator[StreamChunk]:
        """Stream completion with tool results for agentic loop.

        This sends tool execution results back to the model for follow-up.
        Used in the agentic loop when the model made tool calls.
        """
        if not self._client:
            raise RuntimeError("Client not initialized. Use async context manager.")

        wire_api = self._get_wire_api()

        if wire_api == WireApi.RESPONSES:
            # For Responses API, add function_call_output items to input
            async for chunk in self._stream_responses_with_results(
                messages, tools, tool_results
            ):
                yield chunk
        else:
            # For Chat Completions API, add tool results as assistant/tool messages
            async for chunk in self._stream_chat_with_results(
                messages, tools, tool_results
            ):
                yield chunk

    async def _stream_responses_with_results(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None,
        tool_results: list[Any],
    ) -> AsyncIterator[StreamChunk]:
        """Stream Responses API with tool results.

        For each tool result, we must add BOTH:
        1. The original tool call item (local_shell_call, function_call, etc.)
        2. The function_call_output with the result

        This matches codex-rs behavior where items_to_record_in_conversation_history
        includes both the call and its output.

        IMPORTANT: Unlike initial request, we build input manually here to avoid
        duplicating user messages. The input structure is:
        - env_context (always first)
        - user_message (the current turn's input)
        - tool_call + output pairs (accumulated history)
        """
        base_url = self.config.get_base_url()
        headers = self._get_headers()

        # Build input items manually to avoid duplication
        # Only include env_context + last user message + tool results
        input_items: list[dict[str, Any]] = []

        # 1. Add environment context (always first)
        env_context = self._build_environment_context()
        input_items.append({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": env_context}],
        })

        # 2. Add only the LAST user message (current turn's input)
        # Messages list structure: [system, ...history..., current_user_input]
        # We only want the current user input, not previous history
        for msg in reversed(messages):
            if msg.role == "user":
                input_items.append({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": msg.content}],
                })
                break

        # 3. Add tool call + output pairs (accumulated history)
        for result in tool_results:
            # First add the tool call item
            if result.tool_type == "local_shell":
                call_item = {
                    "type": "local_shell_call",
                    "call_id": result.call_id,
                    "status": "completed",
                    "action": {
                        "type": "exec",
                        "command": result.command or [],
                        "env": {},  # Required by API
                    },
                }
                input_items.append(call_item)
            elif result.tool_type == "function":
                call_item = {
                    "type": "function_call",
                    "call_id": result.call_id,
                    "name": result.tool_name or "",
                    "arguments": result.arguments or "{}",
                }
                input_items.append(call_item)

            # Then add the output (always plain string, matching codex-rs)
            output_item = {
                "type": "function_call_output",
                "call_id": result.call_id,
                "output": result.output,
            }
            input_items.append(output_item)

        # Build tools list
        tool_specs: list[ToolSpec] = []
        if tools:
            for tool in tools:
                tool_type = tool.get("type")
                if tool_type == "function":
                    func = tool["function"]
                    if func["name"] == "shell":
                        tool_specs.append(ToolSpec.local_shell())
                    else:
                        tool_specs.append(ToolSpec.function(
                            name=func["name"],
                            description=func.get("description", ""),
                            parameters=func.get("parameters", {"type": "object", "properties": {}}),
                            strict=func.get("strict", False),
                        ))
                elif tool_type == "local_shell":
                    tool_specs.append(ToolSpec.local_shell())
                elif tool_type == "web_search":
                    tool_specs.append(ToolSpec.web_search())

        # Build request manually
        request_data: dict[str, Any] = {
            "model": self.config.model,
            "instructions": GPT_5_CODEX_INSTRUCTIONS,
            "input": input_items,
            "tools": [t.to_dict() for t in tool_specs],
            "tool_choice": "auto",
            "parallel_tool_calls": True,
            "store": False,
            "stream": True,
            "include": [],
        }

        url = f"{base_url}/responses"

        async for chunk in self._stream_responses_api(url, headers, request_data):
            yield chunk

    async def _stream_chat_with_results(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None,
        tool_results: list[Any],
    ) -> AsyncIterator[StreamChunk]:
        """Stream Chat Completions API with tool results."""
        base_url = self.config.get_base_url()
        headers = self._get_headers()

        # Build base request with existing messages
        request_data = self._build_request(messages, tools, stream=True)

        # Add tool results as tool role messages
        for result in tool_results:
            tool_message = {
                "role": "tool",
                "tool_call_id": result.call_id,
                "content": result.output,
            }
            request_data["messages"].append(tool_message)

        url = f"{base_url}/chat/completions"

        async for chunk in self._stream_chat_api_with_retry(
            url, headers, request_data, WireApi.CHAT
        ):
            yield chunk

    async def _stream_chat_api_with_retry(
        self,
        url: str,
        headers: dict[str, str],
        request_data: dict[str, Any],
        wire_api: WireApi,
    ) -> AsyncIterator[StreamChunk]:
        """Stream from Chat Completions/Anthropic API with retry logic.

        Uses httpx_sse for proper SSE parsing.

        Implements retry with exponential backoff for:
        - Rate limits (429)
        - Server errors (5xx)
        - Timeouts and connection errors
        """
        last_error: Exception | None = None

        for attempt in range(self.retry_config.max_retries + 1):
            try:
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

                        chunk = self._parse_stream_chunk(data, wire_api, event.event)
                        if chunk:
                            yield chunk
                return  # Success

            except (httpx.HTTPStatusError, httpx.ReadTimeout, httpx.ConnectTimeout, httpx.ConnectError) as e:
                last_error = e

                if not _is_retryable_error(e):
                    raise

                if attempt >= self.retry_config.max_retries:
                    logger.error(
                        "Max retries (%d) exceeded for API request",
                        self.retry_config.max_retries,
                    )
                    raise

                retry_after: float | None = None
                if isinstance(e, httpx.HTTPStatusError):
                    retry_after = _parse_retry_after(e.response)

                delay = self.retry_config.calculate_delay(attempt, retry_after)
                logger.warning(
                    "API request failed (attempt %d/%d), retrying in %.1fs: %s",
                    attempt + 1,
                    self.retry_config.max_retries + 1,
                    delay,
                    str(e),
                )
                await asyncio.sleep(delay)

        if last_error:
            raise last_error

    async def _stream_responses_api(
        self,
        url: str,
        headers: dict[str, str],
        request_data: dict[str, Any],
    ) -> AsyncIterator[StreamChunk]:
        """Stream from Responses API with retry logic and manual SSE parsing.

        ChatGPT backend API doesn't return Content-Type: text/event-stream,
        so httpx_sse fails. We parse SSE manually here.

        Implements retry with exponential backoff for:
        - Rate limits (429)
        - Server errors (5xx)
        - Timeouts and connection errors
        """
        last_error: Exception | None = None

        for attempt in range(self.retry_config.max_retries + 1):
            try:
                async for chunk in self._stream_responses_api_raw(
                    url, headers, request_data
                ):
                    yield chunk
                return  # Success - exit retry loop

            except (httpx.HTTPStatusError, httpx.ReadTimeout, httpx.ConnectTimeout, httpx.ConnectError) as e:
                last_error = e

                if not _is_retryable_error(e):
                    raise

                if attempt >= self.retry_config.max_retries:
                    logger.error(
                        "Max retries (%d) exceeded for API request",
                        self.retry_config.max_retries,
                    )
                    raise

                # Calculate delay
                retry_after: float | None = None
                if isinstance(e, httpx.HTTPStatusError):
                    retry_after = _parse_retry_after(e.response)

                delay = self.retry_config.calculate_delay(attempt, retry_after)
                logger.warning(
                    "API request failed (attempt %d/%d), retrying in %.1fs: %s",
                    attempt + 1,
                    self.retry_config.max_retries + 1,
                    delay,
                    str(e),
                )
                await asyncio.sleep(delay)

        # Should not reach here, but raise last error if we do
        if last_error:
            raise last_error

    async def _stream_responses_api_raw(
        self,
        url: str,
        headers: dict[str, str],
        request_data: dict[str, Any],
    ) -> AsyncIterator[StreamChunk]:
        """Raw streaming from Responses API without retry.

        Parses SSE manually because ChatGPT backend doesn't set proper Content-Type.
        """
        async with self._client.stream(
            "POST", url, headers=headers, json=request_data
        ) as response:
            # Capture rate limit info from headers
            self._last_rate_limit = RateLimitSnapshot.from_headers(response.headers)
            if self._last_rate_limit.is_low:
                logger.warning(
                    "Rate limit running low: %d tokens, %d requests remaining",
                    self._last_rate_limit.tokens_remaining or 0,
                    self._last_rate_limit.requests_remaining or 0,
                )

            response.raise_for_status()

            event_name: str | None = None
            data_lines: list[str] = []

            async for line in response.aiter_lines():
                line = line.strip()

                if not line:
                    # Empty line = end of event
                    if data_lines:
                        data_str = "\n".join(data_lines)
                        if data_str == "[DONE]":
                            break

                        try:
                            data = json.loads(data_str)
                            chunk = self._parse_responses_chunk(data, event_name)
                            if chunk:
                                yield chunk
                        except json.JSONDecodeError:
                            pass

                        event_name = None
                        data_lines = []
                    continue

                if line.startswith("event:"):
                    event_name = line[6:].strip()
                elif line.startswith("data:"):
                    data_lines.append(line[5:].strip())

    @property
    def last_rate_limit(self) -> RateLimitSnapshot | None:
        """Get the rate limit snapshot from the last API response."""
        return self._last_rate_limit

    def _parse_stream_chunk(
        self,
        data: dict[str, Any],
        wire_api: WireApi = WireApi.CHAT,
        event_name: str | None = None,
    ) -> StreamChunk | None:
        """Parse a stream chunk from the API response."""
        if self.config.model_family == ModelFamily.ANTHROPIC:
            return self._parse_anthropic_chunk(data)
        if wire_api == WireApi.RESPONSES:
            return self._parse_responses_chunk(data, event_name)
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

    def _parse_responses_chunk(
        self, data: dict[str, Any], event_name: str | None
    ) -> StreamChunk | None:
        """Parse OpenAI Responses API stream chunk.

        Responses API uses SSE event names (matches codex-rs):
        - response.created - turn started
        - response.output_text.delta - text content streaming
        - response.output_item.added - new item started
        - response.output_item.done - completed output item
        - response.completed - turn done with usage
        - response.failed - error occurred
        """
        # Handle by event name (preferred)
        if event_name == "response.output_text.delta":
            delta = data.get("delta", "")
            if delta:
                return StreamChunk(content=delta)

        elif event_name == "response.output_item.done":
            # Parse completed output item (matches codex-rs ResponseItem parsing)
            item = data.get("item", {})
            item_type = item.get("type")

            if item_type == "message":
                # Message text was already streamed via response.output_text.delta
                # Don't return content again to avoid duplication
                pass

            elif item_type == "function_call":
                # Function tool call completed
                args_str = item.get("arguments", "{}")
                try:
                    arguments = json.loads(args_str) if args_str else {}
                except json.JSONDecodeError:
                    arguments = {"raw": args_str}
                return StreamChunk(
                    tool_calls=[
                        ToolCall(
                            id=item.get("call_id", ""),
                            name=item.get("name", ""),
                            arguments=arguments,
                        )
                    ]
                )

            elif item_type == "local_shell_call":
                # Local shell call from Responses API (codex-rs built-in tool)
                action = item.get("action", {})
                command = action.get("command", [])
                return StreamChunk(
                    tool_calls=[
                        ToolCall(
                            id=item.get("call_id", ""),
                            name="local_shell",
                            arguments={"command": command},
                        )
                    ]
                )

            elif item_type == "custom_tool_call":
                # Custom tool call (MCP tools, etc.)
                return StreamChunk(
                    tool_calls=[
                        ToolCall(
                            id=item.get("call_id", ""),
                            name=item.get("name", ""),
                            arguments={"input": item.get("input", "")},
                        )
                    ]
                )

        elif event_name == "response.completed":
            # Extract usage from completed response
            response = data.get("response", {})
            usage = response.get("usage", {})
            if usage:
                return StreamChunk(
                    finish_reason="stop",
                    usage={
                        "prompt_tokens": usage.get("input_tokens", 0),
                        "completion_tokens": usage.get("output_tokens", 0),
                        "total_tokens": usage.get("total_tokens", 0),
                    },
                )

        elif event_name == "response.failed":
            # Error occurred - extract message
            response = data.get("response", {})
            error = response.get("error", {})
            error_msg = error.get("message", "Unknown error")
            return StreamChunk(content=f"[Error: {error_msg}]", finish_reason="error")

        # Fallback: check data type field (some SSE implementations put type in data)
        data_type = data.get("type", "")
        if data_type == "response.output_text.delta":
            delta = data.get("delta", "")
            if delta:
                return StreamChunk(content=delta)

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
