"""Token counting utilities using tiktoken.

Provides accurate token counting for OpenAI models to track context usage
and trigger auto-compaction when approaching limits.
"""

from __future__ import annotations

import logging
from functools import lru_cache
from typing import TYPE_CHECKING

import tiktoken

if TYPE_CHECKING:
    from codex_core.client import Message

logger = logging.getLogger(__name__)

# Model context limits (tokens)
MODEL_CONTEXT_LIMITS: dict[str, int] = {
    # GPT-4 variants
    "gpt-4": 8_192,
    "gpt-4-32k": 32_768,
    "gpt-4-turbo": 128_000,
    "gpt-4-turbo-preview": 128_000,
    "gpt-4o": 128_000,
    "gpt-4o-mini": 128_000,
    # GPT-5 / Codex variants
    "gpt-5": 256_000,
    "gpt-5.1-codex-max": 256_000,
    "gpt-5-codex": 256_000,
    # GPT-3.5
    "gpt-3.5-turbo": 16_385,
    "gpt-3.5-turbo-16k": 16_385,
    # Default for unknown models
    "default": 128_000,
}

# Encoding names by model family
MODEL_ENCODINGS: dict[str, str] = {
    "gpt-4": "cl100k_base",
    "gpt-4o": "o200k_base",
    "gpt-5": "o200k_base",
    "gpt-3.5": "cl100k_base",
    "default": "o200k_base",
}


@lru_cache(maxsize=8)
def _get_encoding(model: str) -> tiktoken.Encoding:
    """Get tiktoken encoding for model (cached)."""
    # Try exact model match first
    try:
        return tiktoken.encoding_for_model(model)
    except KeyError:
        pass

    # Try model family prefix
    for prefix, encoding_name in MODEL_ENCODINGS.items():
        if model.startswith(prefix):
            return tiktoken.get_encoding(encoding_name)

    # Default to o200k_base (GPT-4o/5 encoding)
    return tiktoken.get_encoding(MODEL_ENCODINGS["default"])


class TokenCounter:
    """Token counter for tracking context usage.

    Uses tiktoken for accurate token counting compatible with OpenAI models.
    Provides methods to count tokens in text and message lists.
    """

    def __init__(self, model: str = "gpt-4o") -> None:
        """Initialize token counter for the specified model.

        Args:
            model: Model name to determine encoding (e.g., "gpt-4o", "gpt-5.1-codex-max")
        """
        self.model = model
        self._encoding = _get_encoding(model)
        self._context_limit = self._get_context_limit(model)

    def _get_context_limit(self, model: str) -> int:
        """Get context limit for model."""
        if model in MODEL_CONTEXT_LIMITS:
            return MODEL_CONTEXT_LIMITS[model]
        # Try prefix match
        for key, limit in MODEL_CONTEXT_LIMITS.items():
            if model.startswith(key):
                return limit
        return MODEL_CONTEXT_LIMITS["default"]

    @property
    def context_limit(self) -> int:
        """Maximum context size in tokens for this model."""
        return self._context_limit

    def count_text(self, text: str) -> int:
        """Count tokens in a text string.

        Args:
            text: Text to count tokens in

        Returns:
            Number of tokens
        """
        return len(self._encoding.encode(text))

    def count_message(self, message: Message) -> int:
        """Count tokens in a single message.

        Accounts for message overhead (role, separators).
        Format: ~4 tokens overhead per message for OpenAI format.

        Args:
            message: Message to count

        Returns:
            Token count including overhead
        """
        # Base message overhead
        overhead = 4  # role, separators
        content_tokens = self.count_text(message.content) if message.content else 0
        return overhead + content_tokens

    def count_messages(self, messages: list[Message]) -> int:
        """Count total tokens in a list of messages.

        Includes per-message overhead and final separator.

        Args:
            messages: List of messages to count

        Returns:
            Total token count
        """
        total = 3  # Initial overhead
        for msg in messages:
            total += self.count_message(msg)
        return total

    def estimate_remaining(self, messages: list[Message]) -> int:
        """Estimate remaining tokens available in context.

        Args:
            messages: Current message list

        Returns:
            Estimated tokens remaining (may be negative if over limit)
        """
        used = self.count_messages(messages)
        return self._context_limit - used

    def is_near_limit(
        self,
        messages: list[Message],
        threshold: float = 0.8,
    ) -> bool:
        """Check if context usage is near the limit.

        Args:
            messages: Current message list
            threshold: Fraction of context to consider "near limit" (0.0-1.0)

        Returns:
            True if usage exceeds threshold
        """
        used = self.count_messages(messages)
        limit = self._context_limit * threshold
        return used >= limit

    def get_usage_info(self, messages: list[Message]) -> dict[str, int]:
        """Get detailed usage information.

        Args:
            messages: Current message list

        Returns:
            Dict with 'used', 'limit', and 'remaining' keys
        """
        used = self.count_messages(messages)
        return {
            "used": used,
            "limit": self._context_limit,
            "remaining": self._context_limit - used,
        }
