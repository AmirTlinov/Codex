"""Tests for TUI widgets."""

import pytest

from codex_tui.widgets.approval import (
    ApprovalQueue,
    ApprovalRequest,
    ApprovalResult,
    ApprovalType,
)
from codex_tui.widgets.diff_view import (
    DiffHunk,
    FileDiff,
    parse_unified_diff,
)
from codex_tui.widgets.markdown_view import (
    extract_code_blocks,
)


class TestApprovalQueue:
    """Tests for ApprovalQueue."""

    def test_add_command(self) -> None:
        """Test adding a command approval request."""
        queue = ApprovalQueue()
        request = queue.add_command(
            request_id="test-1",
            command="ls -la",
            description="List files",
        )

        assert request is not None
        assert request.request_id == "test-1"
        assert request.approval_type == ApprovalType.COMMAND
        assert request.command == "ls -la"
        assert request.description == "List files"

    def test_add_patch(self) -> None:
        """Test adding a patch approval request."""
        queue = ApprovalQueue()
        request = queue.add_patch(
            request_id="test-2",
            path="src/main.py",
            diff="--- a/src/main.py\n+++ b/src/main.py\n@@ -1 +1 @@\n-old\n+new",
        )

        assert request is not None
        assert request.request_id == "test-2"
        assert request.approval_type == ApprovalType.PATCH
        assert request.patch_path == "src/main.py"

    def test_resolve_approved(self) -> None:
        """Test resolving with approval."""
        queue = ApprovalQueue()
        queue.add_command("test-1", "ls -la")

        result = queue.resolve("test-1", ApprovalResult.APPROVED)
        assert result is True
        assert not queue.has_pending

    def test_resolve_rejected(self) -> None:
        """Test resolving with rejection."""
        queue = ApprovalQueue()
        queue.add_command("test-1", "ls -la")

        result = queue.resolve("test-1", ApprovalResult.REJECTED)
        assert result is False

    def test_always_approve(self) -> None:
        """Test always approve option."""
        queue = ApprovalQueue()
        queue.add_command("test-1", "ls -la")

        queue.resolve("test-1", ApprovalResult.ALWAYS_APPROVE)

        # Next command should be auto-approved
        request = queue.add_command("test-2", "pwd")
        assert request is None  # Auto-approved, no request needed


class TestDiffParsing:
    """Tests for diff parsing."""

    def test_parse_simple_diff(self) -> None:
        """Test parsing a simple unified diff."""
        diff_text = """--- a/file.py
+++ b/file.py
@@ -1,3 +1,3 @@
 line1
-old line
+new line
 line3
"""
        diffs = parse_unified_diff(diff_text)

        assert len(diffs) == 1
        assert diffs[0].path == "b/file.py"
        assert not diffs[0].is_new
        assert not diffs[0].is_deleted
        assert diffs[0].hunks is not None
        assert len(diffs[0].hunks) == 1

        hunk = diffs[0].hunks[0]
        assert hunk.old_start == 1
        assert hunk.old_count == 3
        assert hunk.new_start == 1
        assert hunk.new_count == 3

    def test_parse_new_file(self) -> None:
        """Test parsing diff for a new file."""
        diff_text = """--- /dev/null
+++ b/new_file.py
@@ -0,0 +1,2 @@
+line1
+line2
"""
        diffs = parse_unified_diff(diff_text)

        assert len(diffs) == 1
        assert diffs[0].path == "b/new_file.py"
        assert diffs[0].is_new is True

    def test_parse_deleted_file(self) -> None:
        """Test parsing diff for a deleted file."""
        diff_text = """--- a/old_file.py
+++ /dev/null
@@ -1,2 +0,0 @@
-line1
-line2
"""
        diffs = parse_unified_diff(diff_text)

        assert len(diffs) == 1
        assert diffs[0].is_deleted is True

    def test_parse_multiple_hunks(self) -> None:
        """Test parsing diff with multiple hunks."""
        diff_text = """--- a/file.py
+++ b/file.py
@@ -1,2 +1,2 @@
 line1
-old1
+new1
@@ -10,2 +10,2 @@
 line10
-old2
+new2
"""
        diffs = parse_unified_diff(diff_text)

        assert len(diffs) == 1
        assert diffs[0].hunks is not None
        assert len(diffs[0].hunks) == 2

    def test_parse_multiple_files(self) -> None:
        """Test parsing diff with multiple files."""
        diff_text = """--- a/file1.py
+++ b/file1.py
@@ -1 +1 @@
-old1
+new1
--- a/file2.py
+++ b/file2.py
@@ -1 +1 @@
-old2
+new2
"""
        diffs = parse_unified_diff(diff_text)

        assert len(diffs) == 2
        assert diffs[0].path == "b/file1.py"
        assert diffs[1].path == "b/file2.py"


class TestMarkdownExtraction:
    """Tests for markdown code block extraction."""

    def test_extract_single_code_block(self) -> None:
        """Test extracting a single code block."""
        text = """Some text before

```python
def hello():
    print("Hello")
```

Some text after
"""
        blocks = extract_code_blocks(text)

        assert len(blocks) == 2
        assert "Some text before" in blocks[0][0]
        assert blocks[0][1] == "python"
        assert "def hello():" in blocks[0][2]
        assert "Some text after" in blocks[1][0]

    def test_extract_multiple_code_blocks(self) -> None:
        """Test extracting multiple code blocks."""
        text = """First block:

```python
code1
```

Second block:

```bash
code2
```
"""
        blocks = extract_code_blocks(text)

        # Should have 2 blocks (text before + code for each)
        assert len(blocks) == 2
        assert blocks[0][1] == "python"
        assert "code1" in blocks[0][2]
        assert blocks[1][1] == "bash"
        assert "code2" in blocks[1][2]

    def test_no_code_blocks(self) -> None:
        """Test text with no code blocks."""
        text = "Just plain text"
        blocks = extract_code_blocks(text)

        # Returns empty or single empty entry
        assert len(blocks) == 0 or (len(blocks) == 1 and not blocks[0][2])

    def test_code_block_without_language(self) -> None:
        """Test code block without language specification."""
        text = """```
some code
```
"""
        blocks = extract_code_blocks(text)

        assert len(blocks) >= 1
        # Language defaults to "text"
        assert blocks[0][1] in ("", "text")
