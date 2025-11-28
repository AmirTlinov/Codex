"""TUI widgets for Codex.

Provides all UI components for the terminal interface.
"""

from codex_tui.widgets.approval import (
    ApprovalOverlay,
    ApprovalQueue,
    ApprovalRequest,
    ApprovalResult,
    ApprovalType,
)
from codex_tui.widgets.chat_widget import (
    ChatWidget,
    CommandWidget,
    MessageWidget,
    ThinkingWidget,
)
from codex_tui.widgets.composer import Composer
from codex_tui.widgets.diff_view import (
    DiffOverlay,
    DiffView,
    FileDiff,
    FileDiffWidget,
    parse_unified_diff,
)
from codex_tui.widgets.markdown_view import (
    MarkdownCodeBlock,
    StreamingMarkdown,
    ThinkingIndicator,
    render_markdown_with_code,
)
from codex_tui.widgets.shell_panel import (
    ShellListWidget,
    ShellLogOverlay,
    ShellPanel,
)

__all__ = [
    # Approval
    "ApprovalOverlay",
    "ApprovalQueue",
    "ApprovalRequest",
    "ApprovalResult",
    "ApprovalType",
    # Chat
    "ChatWidget",
    "CommandWidget",
    "MessageWidget",
    "ThinkingWidget",
    # Composer
    "Composer",
    # Diff
    "DiffOverlay",
    "DiffView",
    "FileDiff",
    "FileDiffWidget",
    "parse_unified_diff",
    # Markdown
    "MarkdownCodeBlock",
    "StreamingMarkdown",
    "ThinkingIndicator",
    "render_markdown_with_code",
    # Shell
    "ShellListWidget",
    "ShellLogOverlay",
    "ShellPanel",
]
