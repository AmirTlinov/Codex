"""Integration tests for ChatGPT OAuth API.

These tests verify that codex-py makes correct API calls to the ChatGPT
backend, matching codex-rs behavior exactly.

Tests are skipped if ChatGPT OAuth is not configured.
"""

import asyncio
import json
import os
from typing import Any

import pytest

from codex_core.client import Message, ModelClient
from codex_core.config import Config
from codex_core.models import ResponseItem, ResponsesApiRequest, ToolSpec


def load_prompt_md() -> str:
    """Load prompt.md (required for valid API requests)."""
    import importlib.resources as resources

    prompt_file = resources.files("codex_core").joinpath("prompt.md")
    return prompt_file.read_text(encoding="utf-8")


def is_chatgpt_oauth_available() -> bool:
    """Check if ChatGPT OAuth is configured."""
    config = Config.load()
    return bool(config.auth and config.auth.is_chatgpt_auth())


# Skip all tests in this module if ChatGPT OAuth is not available
pytestmark = pytest.mark.skipif(
    not is_chatgpt_oauth_available(),
    reason="ChatGPT OAuth not configured (no access_token in ~/.codex/auth.json)",
)


class TestResponsesApiRequestFormat:
    """Tests for Responses API request format matching codex-rs."""

    def test_request_has_no_reasoning_field_when_none(self) -> None:
        """Verify reasoning field is omitted (not null) when not set."""
        request = ResponsesApiRequest(
            model="gpt-5.1-codex-max",
            instructions="Test instructions",
            input=[ResponseItem.user_message("Hello")],
            tools=[],
        )

        data = request.to_dict()

        # reasoning should be omitted entirely, not set to null
        assert "reasoning" not in data

    def test_request_has_auto_tool_choice(self) -> None:
        """Verify tool_choice is always 'auto' like codex-rs."""
        request = ResponsesApiRequest(
            model="gpt-5.1-codex-max",
            instructions="Test",
            input=[],
            tools=[],  # Empty tools
        )

        data = request.to_dict()

        assert data["tool_choice"] == "auto"

    def test_request_omits_prompt_cache_key_when_none(self) -> None:
        """Verify prompt_cache_key is omitted when not set."""
        request = ResponsesApiRequest(
            model="test",
            instructions="test",
            input=[],
            prompt_cache_key=None,
        )

        data = request.to_dict()

        assert "prompt_cache_key" not in data

    def test_local_shell_tool_format(self) -> None:
        """Verify local_shell tool serializes correctly."""
        tool = ToolSpec.local_shell()
        data = tool.to_dict()

        assert data == {"type": "local_shell"}

    def test_function_tool_format(self) -> None:
        """Verify function tool serializes correctly."""
        tool = ToolSpec.function(
            name="test_func",
            description="A test function",
            parameters={"type": "object", "properties": {"arg": {"type": "string"}}},
            strict=False,
        )
        data = tool.to_dict()

        assert data["type"] == "function"
        assert data["name"] == "test_func"
        assert data["description"] == "A test function"
        assert "parameters" in data
        assert data["strict"] is False


class TestChatGPTApiIntegration:
    """Live API tests for ChatGPT OAuth backend."""

    @pytest.mark.asyncio
    async def test_simple_streaming_response(self) -> None:
        """Test that a simple request gets a streaming response."""
        config = Config.load()
        instructions = load_prompt_md()

        async with ModelClient(config) as client:
            messages = [
                Message(role="system", content=instructions),
                Message(role="user", content="Reply with exactly: 'TEST_OK'"),
            ]

            request_data = client._build_responses_request(messages, tools=[], stream=True)

            url = f"{config.get_base_url()}/responses"
            headers = client._get_headers()

            full_text = ""
            async with client._client.stream(
                "POST", url, headers=headers, json=request_data
            ) as response:
                assert response.status_code == 200, f"Expected 200, got {response.status_code}"

                async for line in response.aiter_lines():
                    if line.startswith("data:"):
                        data = line[5:].strip()
                        if data and data != "[DONE]":
                            try:
                                parsed = json.loads(data)
                                if "delta" in parsed:
                                    full_text += parsed["delta"]
                            except json.JSONDecodeError:
                                pass

            # The response should contain some text
            assert len(full_text) > 0, "Expected non-empty response"

    @pytest.mark.asyncio
    async def test_request_matches_codex_rs_format(self) -> None:
        """Verify request format matches codex-rs exactly."""
        config = Config.load()
        instructions = load_prompt_md()

        async with ModelClient(config) as client:
            messages = [
                Message(role="system", content=instructions),
                Message(role="user", content="Hi"),
            ]

            request_data = client._build_responses_request(messages, tools=[], stream=True)

            # Required fields
            assert "model" in request_data
            assert "instructions" in request_data
            assert "input" in request_data
            assert "tools" in request_data
            assert "tool_choice" in request_data
            assert "parallel_tool_calls" in request_data
            assert "store" in request_data
            assert "stream" in request_data
            assert "include" in request_data

            # Field values
            assert request_data["tool_choice"] == "auto"
            assert request_data["store"] is False
            assert request_data["stream"] is True
            assert isinstance(request_data["include"], list)

            # reasoning should NOT be present (not null, just absent)
            assert "reasoning" not in request_data

            # Input format
            assert len(request_data["input"]) == 1
            user_msg = request_data["input"][0]
            assert user_msg["type"] == "message"
            assert user_msg["role"] == "user"
            assert user_msg["content"][0]["type"] == "input_text"

    @pytest.mark.asyncio
    async def test_headers_match_codex_rs(self) -> None:
        """Verify headers match codex-rs for ChatGPT backend."""
        config = Config.load()

        async with ModelClient(config) as client:
            headers = client._get_headers()

            # Required headers for ChatGPT backend
            assert headers.get("originator") == "codex_cli_rs"
            assert "codex_cli_rs" in headers.get("User-Agent", "")
            assert "Bearer " in headers.get("Authorization", "")
            assert headers.get("Content-Type") == "application/json"

    @pytest.mark.asyncio
    async def test_instructions_must_be_full_prompt(self) -> None:
        """Verify that short instructions are rejected by API."""
        config = Config.load()

        async with ModelClient(config) as client:
            # Try with short instructions (should fail)
            messages = [
                Message(role="system", content="Be helpful"),  # Too short!
                Message(role="user", content="Hi"),
            ]

            request_data = client._build_responses_request(messages, tools=[], stream=True)

            url = f"{config.get_base_url()}/responses"
            headers = client._get_headers()

            async with client._client.stream(
                "POST", url, headers=headers, json=request_data
            ) as response:
                # API should reject short instructions
                assert response.status_code == 400
                body = await response.aread()
                error = json.loads(body)
                assert "Instructions are not valid" in error.get("detail", "")


class TestModelClientWithChatGPT:
    """Tests for ModelClient with ChatGPT OAuth."""

    @pytest.mark.asyncio
    async def test_client_initialization(self) -> None:
        """Test that ModelClient initializes correctly for ChatGPT."""
        config = Config.load()

        async with ModelClient(config) as client:
            assert client._client is not None
            assert client.config.auth is not None
            assert client.config.auth.is_chatgpt_auth()

    @pytest.mark.asyncio
    async def test_base_url_for_chatgpt(self) -> None:
        """Verify base URL is ChatGPT backend when using OAuth."""
        config = Config.load()

        assert "chatgpt.com/backend-api/codex" in config.get_base_url()
