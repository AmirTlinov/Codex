"""Data models matching codex-rs protocol exactly.

These models are designed to serialize to JSON in the exact format
expected by the OpenAI Responses API, matching codex-rs behavior.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class ContentItemType(str, Enum):
    """Content item type for Responses API."""

    INPUT_TEXT = "input_text"
    INPUT_IMAGE = "input_image"
    OUTPUT_TEXT = "output_text"


class ResponseItemType(str, Enum):
    """Response item type for Responses API."""

    MESSAGE = "message"
    FUNCTION_CALL = "function_call"
    FUNCTION_CALL_OUTPUT = "function_call_output"
    LOCAL_SHELL_CALL = "local_shell_call"
    CUSTOM_TOOL_CALL = "custom_tool_call"
    CUSTOM_TOOL_CALL_OUTPUT = "custom_tool_call_output"


@dataclass(slots=True)
class ContentItem:
    """Content item in a message (matches codex-rs ContentItem)."""

    type: ContentItemType
    text: str | None = None
    image_url: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Serialize to dict matching codex-rs JSON format."""
        if self.type == ContentItemType.INPUT_TEXT:
            return {"type": "input_text", "text": self.text}
        elif self.type == ContentItemType.INPUT_IMAGE:
            return {"type": "input_image", "image_url": self.image_url}
        elif self.type == ContentItemType.OUTPUT_TEXT:
            return {"type": "output_text", "text": self.text}
        raise ValueError(f"Unknown content type: {self.type}")

    @classmethod
    def input_text(cls, text: str) -> ContentItem:
        """Create input_text content item."""
        return cls(type=ContentItemType.INPUT_TEXT, text=text)

    @classmethod
    def output_text(cls, text: str) -> ContentItem:
        """Create output_text content item."""
        return cls(type=ContentItemType.OUTPUT_TEXT, text=text)

    @classmethod
    def input_image(cls, image_url: str) -> ContentItem:
        """Create input_image content item."""
        return cls(type=ContentItemType.INPUT_IMAGE, image_url=image_url)


@dataclass(slots=True)
class FunctionCallOutputPayload:
    """Payload for function call output (matches codex-rs).

    Serialization follows Responses API expectations:
    - success → output is a plain string (no nested object)
    - failure → output is an object { content, success: false }
    """

    content: str
    success: bool = True

    def to_json(self) -> str | dict[str, Any]:
        """Serialize to JSON value matching codex-rs format.

        The Responses API expects two different shapes:
        - success: output is a plain string
        - failure: output is an object { content, success: false }
        """
        if self.success:
            return self.content
        return {"content": self.content, "success": False}


@dataclass(slots=True)
class ResponseItem:
    """Response item for Responses API (matches codex-rs ResponseItem)."""

    type: ResponseItemType
    # Message fields
    role: str | None = None
    content: list[ContentItem] = field(default_factory=list)
    # FunctionCall fields
    name: str | None = None
    arguments: str | None = None
    call_id: str | None = None
    # FunctionCallOutput fields
    output: FunctionCallOutputPayload | None = None

    def to_dict(self) -> dict[str, Any]:
        """Serialize to dict matching codex-rs JSON format."""
        if self.type == ResponseItemType.MESSAGE:
            return {
                "type": "message",
                "role": self.role,
                "content": [c.to_dict() for c in self.content],
            }
        elif self.type == ResponseItemType.FUNCTION_CALL:
            return {
                "type": "function_call",
                "name": self.name,
                "arguments": self.arguments,
                "call_id": self.call_id,
            }
        elif self.type == ResponseItemType.FUNCTION_CALL_OUTPUT:
            result: dict[str, Any] = {
                "type": "function_call_output",
                "call_id": self.call_id,
            }
            if self.output:
                result["output"] = self.output.to_json()
            return result
        elif self.type == ResponseItemType.CUSTOM_TOOL_CALL_OUTPUT:
            return {
                "type": "custom_tool_call_output",
                "call_id": self.call_id,
                "output": self.output.content if self.output else "",
            }
        raise ValueError(f"Unknown response item type: {self.type}")

    @classmethod
    def message(cls, role: str, content: list[ContentItem]) -> ResponseItem:
        """Create a message item."""
        return cls(type=ResponseItemType.MESSAGE, role=role, content=content)

    @classmethod
    def user_message(cls, text: str) -> ResponseItem:
        """Create a user message with text content."""
        return cls.message("user", [ContentItem.input_text(text)])

    @classmethod
    def assistant_message(cls, text: str) -> ResponseItem:
        """Create an assistant message with text content."""
        return cls.message("assistant", [ContentItem.output_text(text)])

    @classmethod
    def function_call(cls, name: str, arguments: str, call_id: str) -> ResponseItem:
        """Create a function call item."""
        return cls(
            type=ResponseItemType.FUNCTION_CALL,
            name=name,
            arguments=arguments,
            call_id=call_id,
        )

    @classmethod
    def function_call_output(
        cls, call_id: str, content: str, success: bool = True
    ) -> ResponseItem:
        """Create a function call output item."""
        return cls(
            type=ResponseItemType.FUNCTION_CALL_OUTPUT,
            call_id=call_id,
            output=FunctionCallOutputPayload(content=content, success=success),
        )

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ResponseItem:
        """Parse from API response dict."""
        item_type = data.get("type", "")

        if item_type == "message":
            content_items = []
            for c in data.get("content", []):
                c_type = c.get("type", "")
                if c_type == "input_text":
                    content_items.append(ContentItem.input_text(c.get("text", "")))
                elif c_type == "output_text":
                    content_items.append(ContentItem.output_text(c.get("text", "")))
                elif c_type == "input_image":
                    content_items.append(ContentItem.input_image(c.get("image_url", "")))
            return cls.message(data.get("role", "assistant"), content_items)

        elif item_type == "function_call":
            return cls.function_call(
                name=data.get("name", ""),
                arguments=data.get("arguments", ""),
                call_id=data.get("call_id", ""),
            )

        elif item_type == "local_shell_call":
            # Parse local_shell_call from API response
            return cls(
                type=ResponseItemType.LOCAL_SHELL_CALL,
                call_id=data.get("call_id"),
                name="local_shell",
            )

        elif item_type == "custom_tool_call":
            return cls(
                type=ResponseItemType.CUSTOM_TOOL_CALL,
                call_id=data.get("call_id", ""),
                name=data.get("name", ""),
                arguments=data.get("input", ""),
            )

        # Fallback for unknown types
        return cls(type=ResponseItemType.MESSAGE, role="assistant", content=[])


@dataclass(slots=True)
class ToolSpec:
    """Tool specification for Responses API."""

    type: str  # "function", "local_shell", "web_search", "custom"
    name: str | None = None
    description: str | None = None
    parameters: dict[str, Any] | None = None
    strict: bool = False
    format: str | None = None  # For custom tools

    def to_dict(self) -> dict[str, Any]:
        """Serialize to dict matching codex-rs format."""
        if self.type == "local_shell":
            return {"type": "local_shell"}
        elif self.type == "web_search":
            return {"type": "web_search"}
        elif self.type == "function":
            return {
                "type": "function",
                "name": self.name,
                "description": self.description or "",
                "parameters": self.parameters or {"type": "object", "properties": {}},
                "strict": self.strict,
            }
        elif self.type == "custom":
            result: dict[str, Any] = {
                "type": "custom",
                "name": self.name,
                "description": self.description or "",
            }
            if self.format:
                result["format"] = self.format
            return result
        raise ValueError(f"Unknown tool type: {self.type}")

    @classmethod
    def local_shell(cls) -> ToolSpec:
        """Create local_shell tool."""
        return cls(type="local_shell")

    @classmethod
    def web_search(cls) -> ToolSpec:
        """Create web_search tool."""
        return cls(type="web_search")

    @classmethod
    def function(
        cls,
        name: str,
        description: str,
        parameters: dict[str, Any] | None = None,
        strict: bool = False,
    ) -> ToolSpec:
        """Create function tool."""
        return cls(
            type="function",
            name=name,
            description=description,
            parameters=parameters,
            strict=strict,
        )


@dataclass(slots=True)
class Reasoning:
    """Reasoning configuration for o1/o3 models."""

    effort: str | None = None  # "low", "medium", "high"
    summary: str | None = None  # "none", "auto", "detailed"

    def to_dict(self) -> dict[str, Any]:
        """Serialize to dict, skipping None values."""
        result: dict[str, Any] = {}
        if self.effort:
            result["effort"] = self.effort
        if self.summary:
            result["summary"] = self.summary
        return result


@dataclass(slots=True)
class ResponsesApiRequest:
    """Request format for OpenAI Responses API (matches codex-rs exactly)."""

    model: str
    instructions: str
    input: list[ResponseItem]
    tools: list[ToolSpec] = field(default_factory=list)
    tool_choice: str = "auto"
    parallel_tool_calls: bool = True
    reasoning: Reasoning | None = None  # Required field (serializes as null if None)
    store: bool = False
    stream: bool = True
    include: list[str] = field(default_factory=list)
    prompt_cache_key: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Serialize to dict for JSON request body (matches codex-rs exactly).

        In codex-rs, fields use #[serde(skip_serializing_if = "Option::is_none")]
        so None fields are omitted entirely, not serialized as null.
        """
        result: dict[str, Any] = {
            "model": self.model,
            "instructions": self.instructions,
            "input": [item.to_dict() for item in self.input],
            "tools": [tool.to_dict() for tool in self.tools],
            "tool_choice": self.tool_choice,
            "parallel_tool_calls": self.parallel_tool_calls,
            "store": self.store,
            "stream": self.stream,
            "include": self.include,
        }
        # Optional fields - only include if set (matches codex-rs skip_serializing_if)
        if self.reasoning:
            result["reasoning"] = self.reasoning.to_dict()
        if self.prompt_cache_key:
            result["prompt_cache_key"] = self.prompt_cache_key
        return result
