"""History compaction for managing context limits.

Provides intelligent summarization of conversation history when approaching
context limits, preserving recent messages while compacting older ones.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from codex_core.client import Message, ModelClient
    from codex_core.token_counter import TokenCounter

logger = logging.getLogger(__name__)


@dataclass(slots=True)
class CompactionConfig:
    """Configuration for history compaction."""

    # Fraction of context limit to trigger compaction (0.0-1.0)
    trigger_threshold: float = 0.8

    # Target context usage after compaction (fraction)
    target_threshold: float = 0.5

    # Minimum messages to keep uncompacted (recent history)
    min_recent_messages: int = 5

    # Maximum messages to keep uncompacted
    max_recent_messages: int = 10

    # Whether to include tool call results in summary
    include_tool_results: bool = True


class HistoryCompactor:
    """Compacts conversation history by summarizing older messages.

    Keeps recent messages intact while summarizing older ones to reduce
    context usage. Uses the model itself for summarization to maintain
    context quality.
    """

    def __init__(
        self,
        client: ModelClient,
        token_counter: TokenCounter,
        config: CompactionConfig | None = None,
    ) -> None:
        """Initialize history compactor.

        Args:
            client: Model client for summarization requests
            token_counter: Token counter for measuring context usage
            config: Compaction configuration
        """
        self.client = client
        self.token_counter = token_counter
        self.config = config or CompactionConfig()
        self._compaction_count = 0

    def should_compact(self, messages: list[Message]) -> bool:
        """Check if history should be compacted.

        Args:
            messages: Current message list

        Returns:
            True if compaction is recommended
        """
        return self.token_counter.is_near_limit(
            messages,
            threshold=self.config.trigger_threshold,
        )

    def _split_messages(
        self,
        messages: list[Message],
    ) -> tuple[list[Message], list[Message]]:
        """Split messages into old (to compact) and recent (to keep).

        Finds optimal split point respecting min/max recent messages.

        Args:
            messages: Full message list

        Returns:
            Tuple of (old_messages, recent_messages)
        """
        # Filter out system messages (they're handled separately)
        non_system = [m for m in messages if m.role != "system"]
        system_msgs = [m for m in messages if m.role == "system"]

        if len(non_system) <= self.config.min_recent_messages:
            # Not enough messages to compact
            return [], messages

        # Calculate how many recent messages to keep
        # Start with max and reduce if needed
        recent_count = min(self.config.max_recent_messages, len(non_system) - 1)

        # Ensure we have at least some messages to compact
        if len(non_system) - recent_count < 2:
            recent_count = max(
                self.config.min_recent_messages,
                len(non_system) - 2,
            )

        split_point = len(non_system) - recent_count
        old_messages = non_system[:split_point]
        recent_messages = non_system[split_point:]

        return old_messages, system_msgs + recent_messages

    async def _summarize_messages(self, messages: list[Message]) -> str:
        """Generate a summary of messages.

        Args:
            messages: Messages to summarize

        Returns:
            Summary text
        """
        from codex_core.client import Message as Msg

        # Build summarization prompt
        content_parts: list[str] = []
        for msg in messages:
            role_prefix = f"[{msg.role.upper()}]"
            content_parts.append(f"{role_prefix} {msg.content}")

        conversation_text = "\n\n".join(content_parts)

        summarize_prompt = f"""Summarize the following conversation history concisely.
Preserve key context, decisions, code changes, and tool execution results.
Focus on information that would be needed to continue the conversation.

CONVERSATION HISTORY:
{conversation_text}

SUMMARY (be concise but comprehensive):"""

        # Use model to summarize
        summary_messages = [
            Msg(role="system", content="You are a helpful assistant that summarizes conversation history."),
            Msg(role="user", content=summarize_prompt),
        ]

        response = await self.client.complete(summary_messages, tools=None)
        return response.content

    async def compact(
        self,
        messages: list[Message],
    ) -> list[Message]:
        """Compact message history by summarizing older messages.

        Args:
            messages: Current message list

        Returns:
            Compacted message list with summary replacing old messages
        """
        from codex_core.client import Message as Msg

        if not self.should_compact(messages):
            return messages

        old_messages, recent_messages = self._split_messages(messages)

        if not old_messages:
            logger.warning("No messages to compact")
            return messages

        logger.info(
            "Compacting %d messages (keeping %d recent)",
            len(old_messages),
            len(recent_messages),
        )

        # Generate summary
        summary = await self._summarize_messages(old_messages)

        # Create compacted history
        # Keep system messages at the start
        system_msgs = [m for m in recent_messages if m.role == "system"]
        non_system_recent = [m for m in recent_messages if m.role != "system"]

        # Add summary as a system message
        summary_msg = Msg(
            role="system",
            content=f"[CONVERSATION SUMMARY]\n{summary}\n[END SUMMARY]",
        )

        self._compaction_count += 1
        compacted = system_msgs + [summary_msg] + non_system_recent

        # Log results
        old_tokens = self.token_counter.count_messages(messages)
        new_tokens = self.token_counter.count_messages(compacted)
        reduction = old_tokens - new_tokens
        logger.info(
            "Compacted: %d -> %d tokens (saved %d, %.1f%%)",
            old_tokens,
            new_tokens,
            reduction,
            (reduction / old_tokens) * 100 if old_tokens > 0 else 0,
        )

        return compacted

    @property
    def compaction_count(self) -> int:
        """Number of times compaction has been performed."""
        return self._compaction_count


def create_compactor(
    client: ModelClient,
    model: str,
    config: CompactionConfig | None = None,
) -> HistoryCompactor:
    """Create a history compactor with token counter.

    Args:
        client: Model client for summarization
        model: Model name for token counting
        config: Optional compaction config

    Returns:
        Configured HistoryCompactor
    """
    from codex_core.token_counter import TokenCounter

    token_counter = TokenCounter(model)
    return HistoryCompactor(client, token_counter, config)
